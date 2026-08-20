package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

func newTestDB(t *testing.T) *sql.DB {
	t.Helper()
	db, _, err := NewSQLiteDB(":memory:", slog.Default())
	if err != nil {
		t.Fatalf("opening test database: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return db
}

func TestNewSQLiteDB_CreatesTables(t *testing.T) {
	db := newTestDB(t)

	tables := []string{"agents", "chat_sessions", "chat_messages", "integrations", "user_settings", "schema_migrations", "claude_session_cache", "claude_subagent_cache", "claude_cache_metadata", "notification_log", "scheduled_tasks", "job_history", "trigger_rules", "telegram_processed_updates", "model_pricing", "model_pricing_tier"}
	for _, table := range tables {
		var name string
		err := db.QueryRowContext(context.Background(), "SELECT name FROM sqlite_master WHERE type='table' AND name=?", table).Scan(&name)
		if err != nil {
			t.Errorf("table %q not found: %v", table, err)
		}
	}
}

func TestNewSQLiteDB_MigrationVersion(t *testing.T) {
	db := newTestDB(t)

	var version int
	err := db.QueryRowContext(context.Background(), "SELECT MAX(version) FROM schema_migrations").Scan(&version)
	if err != nil {
		t.Fatalf("querying version: %v", err)
	}
	if version != 30 {
		t.Errorf("expected version 30, got %d", version)
	}
}

func TestNewSQLiteDB_FreshDBFlag(t *testing.T) {
	db, fresh, err := NewSQLiteDB(":memory:", slog.Default())
	if err != nil {
		t.Fatalf("opening database: %v", err)
	}
	defer db.Close()

	if !fresh {
		t.Error("expected freshDB=true for new database")
	}
}

// --- Agent Store Tests ---

func TestSQLiteAgentStore_CRUD(t *testing.T) {
	ctx := context.Background()
	db := newTestDB(t)
	store := NewSQLiteAgentStore(db)

	// List empty
	agents, err := store.List(ctx)
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(agents) != 0 {
		t.Fatalf("expected 0 agents, got %d", len(agents))
	}

	// Save
	agent := &config.AgentConfig{
		Name:         "Test Agent",
		Slug:         "test-agent",
		Description:  "A test agent",
		Model:        "claude-sonnet-4-6",
		Thinking:     "adaptive",
		SystemPrompt: "You are helpful.",
		Capabilities: config.AgentCapabilities{
			BuiltIn: []string{"current_time"},
			MCP:     map[string]config.MCPCap{"server1": {Tools: []string{"tool1"}}},
		},
	}
	if saveErr := store.Save(ctx, agent); saveErr != nil {
		t.Fatalf("save: %v", saveErr)
	}

	// Get
	got, err := store.Get(ctx, "test-agent")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got == nil {
		t.Fatal("expected agent, got nil")
	}
	if got.Name != "Test Agent" {
		t.Errorf("expected name 'Test Agent', got %q", got.Name)
	}
	if len(got.Capabilities.BuiltIn) != 1 || got.Capabilities.BuiltIn[0] != "current_time" {
		t.Errorf("expected built-in 'current_time', got %v", got.Capabilities.BuiltIn)
	}
	if got.Capabilities.MCP["server1"].Tools[0] != "tool1" {
		t.Errorf("unexpected MCP capabilities: %v", got.Capabilities.MCP)
	}

	// Get not found
	missing, err := store.Get(ctx, "nonexistent")
	if err != nil {
		t.Fatalf("get nonexistent: %v", err)
	}
	if missing != nil {
		t.Error("expected nil for nonexistent agent")
	}

	// Update (upsert)
	agent.Description = "Updated description"
	if updateErr := store.Save(ctx, agent); updateErr != nil {
		t.Fatalf("save update: %v", updateErr)
	}
	got, _ = store.Get(ctx, "test-agent")
	if got.Description != "Updated description" {
		t.Errorf("expected updated description, got %q", got.Description)
	}

	// List
	agents, err = store.List(ctx)
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(agents) != 1 {
		t.Fatalf("expected 1 agent, got %d", len(agents))
	}

	// Delete
	if err := store.Delete(ctx, "test-agent"); err != nil {
		t.Fatalf("delete: %v", err)
	}
	agents, _ = store.List(ctx)
	if len(agents) != 0 {
		t.Errorf("expected 0 agents after delete, got %d", len(agents))
	}

	// Delete not found
	if err := store.Delete(ctx, "nonexistent"); err == nil {
		t.Error("expected error deleting nonexistent agent")
	}
}

func TestSQLiteAgentStore_ValidationErrors(t *testing.T) {
	ctx := context.Background()
	db := newTestDB(t)
	store := NewSQLiteAgentStore(db)

	if err := store.Save(ctx, &config.AgentConfig{Slug: "test"}); err == nil {
		t.Error("expected error for missing name")
	}
	if err := store.Save(ctx, &config.AgentConfig{Name: "Test"}); err == nil {
		t.Error("expected error for missing slug")
	}
}

// --- Chat Store Tests ---

func TestSQLiteChatStore_CRUD(t *testing.T) {
	ctx := context.Background()
	db := newTestDB(t)
	store := NewSQLiteChatStore(db)

	// List empty
	sessions, err := store.ListSessions(ctx)
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(sessions) != 0 {
		t.Fatalf("expected 0 sessions, got %d", len(sessions))
	}

	// Create
	session, err := store.CreateSession(ctx, NewSessionParams{
		AgentSlug:         "test-agent",
		WorkingDir:        "/tmp/work",
		Model:             "claude-sonnet-4-6",
		SettingsProfileID: "profile1",
		PermissionMode:    "plan",
	})
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	if session.ID == "" {
		t.Error("expected non-empty session ID")
	}
	if session.Title != "New Chat" {
		t.Errorf("expected title 'New Chat', got %q", session.Title)
	}
	if session.AgentSlug != "test-agent" {
		t.Errorf("expected agent slug 'test-agent', got %q", session.AgentSlug)
	}

	// Get
	got, err := store.GetSession(ctx, session.ID)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got == nil {
		t.Fatal("expected session, got nil")
	}
	if got.Model != "claude-sonnet-4-6" {
		t.Errorf("expected model, got %q", got.Model)
	}
	if got.PermissionMode != "plan" {
		t.Errorf("expected permission mode 'plan', got %q", got.PermissionMode)
	}

	// Get not found
	missing, err := store.GetSession(ctx, "nonexistent")
	if err != nil {
		t.Fatalf("get nonexistent: %v", err)
	}
	if missing != nil {
		t.Error("expected nil for nonexistent session")
	}

	// Append messages
	msg1 := ChatMessage{
		Role:      "user",
		Content:   "Hello",
		Timestamp: time.Now().UTC(),
	}
	msg2 := ChatMessage{
		Role:      "assistant",
		Content:   "Hi there!",
		Timestamp: time.Now().UTC(),
		Blocks: []MessageBlock{
			{Type: "text", Text: "Hi there!"},
			{Type: "tool_use", ID: "t1", Name: "current_time", Input: json.RawMessage(`{"tz":"UTC"}`)},
		},
	}
	if appendErr := store.AppendMessage(ctx, session.ID, msg1); appendErr != nil {
		t.Fatalf("append msg1: %v", appendErr)
	}
	if appendErr := store.AppendMessage(ctx, session.ID, msg2); appendErr != nil {
		t.Fatalf("append msg2: %v", appendErr)
	}

	// GetSessionWithMessages
	gotSession, messages, err := store.GetSessionWithMessages(ctx, session.ID)
	if err != nil {
		t.Fatalf("get with messages: %v", err)
	}
	if gotSession == nil {
		t.Fatal("expected session")
	}
	if len(messages) != 2 {
		t.Fatalf("expected 2 messages, got %d", len(messages))
	}
	if messages[0].Role != "user" || messages[0].Content != "Hello" {
		t.Errorf("unexpected first message: %+v", messages[0])
	}
	if len(messages[1].Blocks) != 2 {
		t.Fatalf("expected 2 blocks in second message, got %d", len(messages[1].Blocks))
	}
	if messages[1].Blocks[1].Name != "current_time" {
		t.Errorf("expected tool name 'current_time', got %q", messages[1].Blocks[1].Name)
	}

	// Update session
	session.Title = "Updated Title"
	session.TotalInputTokens = 100
	session.TotalOutputTokens = 50
	session.UpdatedAt = time.Now().UTC()
	if err := store.UpdateSession(ctx, session); err != nil {
		t.Fatalf("update: %v", err)
	}
	got, _ = store.GetSession(ctx, session.ID)
	if got.Title != "Updated Title" {
		t.Errorf("expected updated title, got %q", got.Title)
	}
	if got.TotalInputTokens != 100 {
		t.Errorf("expected 100 input tokens, got %d", got.TotalInputTokens)
	}

	// List sessions
	sessions, _ = store.ListSessions(ctx)
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions))
	}

	// Delete (cascade should delete messages too)
	if err := store.DeleteSession(ctx, session.ID); err != nil {
		t.Fatalf("delete: %v", err)
	}
	sessions, _ = store.ListSessions(ctx)
	if len(sessions) != 0 {
		t.Errorf("expected 0 sessions after delete, got %d", len(sessions))
	}

	// Verify messages were cascade-deleted
	var count int
	_ = db.QueryRowContext(context.Background(), "SELECT COUNT(*) FROM chat_messages").Scan(&count)
	if count != 0 {
		t.Errorf("expected 0 messages after cascade delete, got %d", count)
	}

	// Delete not found
	if err := store.DeleteSession(ctx, "nonexistent"); err == nil {
		t.Error("expected error deleting nonexistent session")
	}
}

func TestSQLiteChatStore_GetSessionWithMessages_NotFound(t *testing.T) {
	ctx := context.Background()
	db := newTestDB(t)
	store := NewSQLiteChatStore(db)

	session, messages, err := store.GetSessionWithMessages(ctx, "nonexistent")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if session != nil {
		t.Error("expected nil session")
	}
	if messages != nil {
		t.Error("expected nil messages")
	}
}

// --- Integration Store Tests ---

func TestSQLiteIntegrationStore_CRUD(t *testing.T) {
	ctx := context.Background()
	db := newTestDB(t)
	store := NewSQLiteIntegrationStore(db)

	// List empty
	integrations, err := store.List(ctx)
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(integrations) != 0 {
		t.Fatalf("expected 0 integrations, got %d", len(integrations))
	}

	now := time.Now().UTC()
	cfg := &config.IntegrationConfig{
		ID:          "google-1",
		Name:        "Google Workspace",
		Type:        "google",
		Enabled:     true,
		Credentials: json.RawMessage(`{"client_id":"client-id","client_secret":"client-secret"}`),
		Services: map[string]config.ServiceConfig{
			"calendar": {Enabled: true, Tools: []string{"list_events"}},
		},
		CreatedAt: now,
		UpdatedAt: now,
	}

	// Save
	if saveErr := store.Save(ctx, cfg); saveErr != nil {
		t.Fatalf("save: %v", saveErr)
	}

	// Get
	got, err := store.Get(ctx, "google-1")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got == nil {
		t.Fatal("expected integration, got nil")
	}
	if !got.Enabled {
		t.Error("expected enabled=true")
	}
	var gotCreds config.GoogleCredentials
	if err := got.ParseCredentials(&gotCreds); err != nil {
		t.Fatalf("parsing credentials: %v", err)
	}
	if gotCreds.ClientID != "client-id" {
		t.Errorf("expected client-id, got %q", gotCreds.ClientID)
	}
	if got.Services["calendar"].Tools[0] != "list_events" {
		t.Errorf("unexpected tools: %v", got.Services["calendar"].Tools)
	}
	if got.IsAuthenticated() {
		t.Error("expected no auth for new integration")
	}

	// Get not found
	missing, err := store.Get(ctx, "nonexistent")
	if err != nil {
		t.Fatalf("get nonexistent: %v", err)
	}
	if missing != nil {
		t.Error("expected nil for nonexistent integration")
	}

	// Update
	cfg.Name = "Updated Name"
	if err := store.Save(ctx, cfg); err != nil {
		t.Fatalf("save update: %v", err)
	}
	got, _ = store.Get(ctx, "google-1")
	if got.Name != "Updated Name" {
		t.Errorf("expected updated name, got %q", got.Name)
	}

	// Delete
	if err := store.Delete(ctx, "google-1"); err != nil {
		t.Fatalf("delete: %v", err)
	}
	integrations, _ = store.List(ctx)
	if len(integrations) != 0 {
		t.Errorf("expected 0 after delete, got %d", len(integrations))
	}

	// Delete not found
	if err := store.Delete(ctx, "nonexistent"); err == nil {
		t.Error("expected error deleting nonexistent integration")
	}
}

func TestSQLiteIntegrationStore_SaveRequiresID(t *testing.T) {
	ctx := context.Background()
	db := newTestDB(t)
	store := NewSQLiteIntegrationStore(db)

	err := store.Save(ctx, &config.IntegrationConfig{Name: "No ID"})
	if err == nil {
		t.Error("expected error for missing ID")
	}
}

// --- Settings Store Tests ---

func TestSQLiteSettingsStore_LoadSave(t *testing.T) {
	db := newTestDB(t)
	store := NewSQLiteSettingsStore(db)

	// Load defaults (no row exists yet) — returns zero-value settings;
	// SettingsManager is responsible for filling in defaults.
	settings, err := store.Load()
	if err != nil {
		t.Fatalf("load defaults: %v", err)
	}
	if settings.DefaultWorkingDir != "" {
		t.Errorf("expected empty default working dir on fresh load, got %q", settings.DefaultWorkingDir)
	}

	// Save
	settings.DefaultWorkingDir = "/some/work/dir"
	settings.DefaultModel = "test-model"
	settings.OnboardingComplete = true
	settings.AppearanceDarkMode = true
	settings.AppearanceFontSize = 14
	settings.AppearanceFontFamily = "monospace"
	if saveErr := store.Save(settings); saveErr != nil {
		t.Fatalf("save: %v", saveErr)
	}

	// Load saved
	got, err := store.Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if got.DefaultModel != "test-model" {
		t.Errorf("expected 'test-model', got %q", got.DefaultModel)
	}
	if !got.OnboardingComplete {
		t.Error("expected onboarding_complete=true")
	}
	if !got.AppearanceDarkMode {
		t.Error("expected dark_mode=true")
	}
	if got.AppearanceFontSize != 14 {
		t.Errorf("expected font size 14, got %d", got.AppearanceFontSize)
	}
	if got.AppearanceFontFamily != "monospace" {
		t.Errorf("expected font family 'monospace', got %q", got.AppearanceFontFamily)
	}
}

// TestSQLiteSettingsStore_DataAnalytics covers the Data & Analytics fields,
// which decide what every reported figure covers. The hidden list is stored as
// JSON, so a round-trip is the only thing that proves the encode/decode pair
// agree.
func TestSQLiteSettingsStore_DataAnalytics(t *testing.T) {
	db := newTestDB(t)
	store := NewSQLiteSettingsStore(db)

	// A fresh row has neither: no project is hidden, and the threshold reads
	// as "not chosen" rather than as zero minutes.
	fresh, err := store.Load()
	if err != nil {
		t.Fatalf("load defaults: %v", err)
	}
	if len(fresh.HiddenProjects) != 0 {
		t.Errorf("expected no hidden projects on a fresh load, got %v", fresh.HiddenProjects)
	}
	if fresh.IdleGapThresholdMinutes != 0 {
		t.Errorf("expected an unset threshold on a fresh load, got %d", fresh.IdleGapThresholdMinutes)
	}

	fresh.HiddenProjects = []string{"/home/me/scratch", "/home/me/experiments"}
	fresh.IdleGapThresholdMinutes = 25
	if err := store.Save(fresh); err != nil {
		t.Fatalf("save: %v", err)
	}

	got, err := store.Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if len(got.HiddenProjects) != 2 ||
		got.HiddenProjects[0] != "/home/me/scratch" ||
		got.HiddenProjects[1] != "/home/me/experiments" {
		t.Errorf("hidden projects round-tripped as %v", got.HiddenProjects)
	}
	if got.IdleGapThresholdMinutes != 25 {
		t.Errorf("idle threshold round-tripped as %d, want 25", got.IdleGapThresholdMinutes)
	}

	// Unhiding everything must clear the list, not leave the previous one
	// behind: the column is NOT NULL, so an empty save has to write valid JSON.
	got.HiddenProjects = nil
	if err := store.Save(got); err != nil {
		t.Fatalf("save cleared list: %v", err)
	}
	cleared, err := store.Load()
	if err != nil {
		t.Fatalf("load cleared list: %v", err)
	}
	if len(cleared.HiddenProjects) != 0 {
		t.Errorf("expected no hidden projects after clearing, got %v", cleared.HiddenProjects)
	}
}

// --- FS Migration Tests ---

func TestMigrateFromFS(t *testing.T) {
	dataDir := t.TempDir()

	// Create agents dir with a YAML file
	agentsDir := filepath.Join(dataDir, "agents")
	if err := os.MkdirAll(agentsDir, 0750); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(agentsDir, "hello.yaml"), []byte(`name: Hello
slug: hello
model: claude-sonnet-4-6
thinking: adaptive
system_prompt: You are helpful.
capabilities:
  built_in:
    - current_time
`), 0600); err != nil {
		t.Fatal(err)
	}

	// Create chats dir with a JSONL file
	chatsDir := filepath.Join(dataDir, "chats")
	if err := os.MkdirAll(chatsDir, 0750); err != nil {
		t.Fatal(err)
	}
	now := time.Now().UTC()
	sessionLine := mustJSON(t, map[string]interface{}{
		"type": "session", "id": "sess-1", "title": "Test Chat",
		"agent_slug": "hello", "created_at": now, "updated_at": now,
	})
	messageLine := mustJSON(t, map[string]interface{}{
		"type": "message", "role": "user", "content": "Hello",
		"timestamp": now, "blocks": []interface{}{},
	})
	if err := os.WriteFile(
		filepath.Join(chatsDir, "sess-1.jsonl"),
		[]byte(sessionLine+"\n"+messageLine+"\n"),
		0600,
	); err != nil {
		t.Fatal(err)
	}

	// Create settings.json
	if err := os.WriteFile(
		filepath.Join(dataDir, "settings.json"),
		[]byte(`{"default_working_dir":"/tmp/test","default_model":"test-model","onboarding_complete":true}`),
		0600,
	); err != nil {
		t.Fatal(err)
	}

	db := newTestDB(t)
	logger := slog.Default()

	if err := MigrateFromFS(db, dataDir, logger); err != nil {
		t.Fatalf("migration failed: %v", err)
	}

	// Verify agent
	agentStore := NewSQLiteAgentStore(db)
	agent, err := agentStore.Get(context.Background(), "hello")
	if err != nil {
		t.Fatalf("get agent: %v", err)
	}
	if agent == nil || agent.Name != "Hello" {
		t.Errorf("unexpected agent: %+v", agent)
	}

	// Verify chat session and messages
	chatStore := NewSQLiteChatStore(db)
	session, messages, err := chatStore.GetSessionWithMessages(context.Background(), "sess-1")
	if err != nil {
		t.Fatalf("get session: %v", err)
	}
	if session == nil || session.Title != "Test Chat" {
		t.Errorf("unexpected session: %+v", session)
	}
	if len(messages) != 1 || messages[0].Content != "Hello" {
		t.Errorf("unexpected messages: %+v", messages)
	}

	// Verify settings
	settingsStore := NewSQLiteSettingsStore(db)
	settings, err := settingsStore.Load()
	if err != nil {
		t.Fatalf("load settings: %v", err)
	}
	if settings.DefaultWorkingDir != "/tmp/test" {
		t.Errorf("expected /tmp/test, got %q", settings.DefaultWorkingDir)
	}
	if settings.DefaultModel != "test-model" {
		t.Errorf("expected test-model, got %q", settings.DefaultModel)
	}
	if !settings.OnboardingComplete {
		t.Error("expected onboarding_complete=true")
	}

	// Verify old FS data was cleaned up
	if _, err := os.Stat(agentsDir); !os.IsNotExist(err) {
		t.Error("expected agents directory to be removed after migration")
	}
	if _, err := os.Stat(chatsDir); !os.IsNotExist(err) {
		t.Error("expected chats directory to be removed after migration")
	}
	if _, err := os.Stat(filepath.Join(dataDir, "settings.json")); !os.IsNotExist(err) {
		t.Error("expected settings.json to be removed after migration")
	}
}

func TestMigrateFromFS_Idempotent(t *testing.T) {
	dataDir := t.TempDir()

	agentsDir := filepath.Join(dataDir, "agents")
	if err := os.MkdirAll(agentsDir, 0750); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(agentsDir, "test.yaml"), []byte("name: Test\nslug: test\n"), 0600); err != nil {
		t.Fatal(err)
	}

	db := newTestDB(t)
	logger := slog.Default()

	// Run twice — should not fail
	if err := MigrateFromFS(db, dataDir, logger); err != nil {
		t.Fatalf("first migration: %v", err)
	}
	if err := MigrateFromFS(db, dataDir, logger); err != nil {
		t.Fatalf("second migration: %v", err)
	}

	// Should still have exactly 1 agent
	store := NewSQLiteAgentStore(db)
	agents, _ := store.List(context.Background())
	if len(agents) != 1 {
		t.Errorf("expected 1 agent, got %d", len(agents))
	}
}

func TestHasFSData(t *testing.T) {
	// Empty dir
	emptyDir := t.TempDir()
	if HasFSData(emptyDir) {
		t.Error("expected no FS data in empty dir")
	}

	// Dir with agents
	dataDir := t.TempDir()
	agentsDir := filepath.Join(dataDir, "agents")
	if err := os.MkdirAll(agentsDir, 0750); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(agentsDir, "test.yaml"), []byte("name: Test\nslug: test\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if !HasFSData(dataDir) {
		t.Error("expected FS data with agents dir")
	}
}

// --- Helpers ---

func mustJSON(t *testing.T, v interface{}) string {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("marshaling JSON: %v", err)
	}
	return string(b)
}

// LoadUserSettingsReadOnly exists so a CLI can read a stored preference without
// owning the schema's lifecycle: a command that migrated could upgrade a
// database out from under a running `agento web`.
func TestLoadUserSettingsReadOnly(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "agento.db")

	db, _, err := NewSQLiteDB(dbPath, slog.Default())
	if err != nil {
		t.Fatalf("creating db: %v", err)
	}
	want := config.UserSettings{
		ClaudeConfigDir:  "/opt/personal/.claude",
		ClaudeConfigDirs: []string{"/opt/second/.claude"},
	}
	if err := NewSQLiteSettingsStore(db).Save(want); err != nil {
		t.Fatalf("saving: %v", err)
	}
	if err := db.Close(); err != nil {
		t.Fatalf("closing: %v", err)
	}

	got, err := LoadUserSettingsReadOnly(dbPath, slog.Default())
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if got.ClaudeConfigDir != want.ClaudeConfigDir {
		t.Errorf("ClaudeConfigDir = %q, want %q", got.ClaudeConfigDir, want.ClaudeConfigDir)
	}
	if len(got.ClaudeConfigDirs) != 1 || got.ClaudeConfigDirs[0] != "/opt/second/.claude" {
		t.Errorf("ClaudeConfigDirs = %v, want %v", got.ClaudeConfigDirs, want.ClaudeConfigDirs)
	}
}

// schemaVersion reads the recorded migration version without migrating, which
// is the whole point: reading it back through NewSQLiteDB would migrate the
// database and report head no matter what the loader did.
func schemaVersion(t *testing.T, dbPath string) int {
	t.Helper()
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("opening %s: %v", dbPath, err)
	}
	defer func() { _ = db.Close() }()
	var v int
	if err := db.QueryRowContext(context.Background(),
		"SELECT COALESCE(MAX(version), 0) FROM schema_migrations").Scan(&v); err != nil {
		t.Fatalf("reading schema version: %v", err)
	}
	return v
}

// The load must never migrate. #244 forbids it because `agento ask` shares the
// file with a possibly-running `agento web`, and upgrading a schema out from
// under a live server is not a CLI's business.
//
// The fixture is an *old* database, and the version is read back with a plain
// sql.Open. Both matter: against a head database, or read back through
// NewSQLiteDB, the assertion passes even for a loader that does migrate.
func TestLoadUserSettingsReadOnly_DoesNotMigrate(t *testing.T) {
	const oldVersion = 19
	dbPath := filepath.Join(t.TempDir(), "agento.db")

	db, err := ApplyMigrationsUpTo(dbPath, oldVersion)
	if err != nil {
		t.Fatalf("building an old database: %v", err)
	}
	if err := db.Close(); err != nil {
		t.Fatalf("closing: %v", err)
	}
	if got := schemaVersion(t, dbPath); got != oldVersion {
		t.Fatalf("fixture is at version %d, want %d", got, oldVersion)
	}

	// The read itself fails — v19 predates the column — which is exactly the
	// degradation path the caller relies on.
	if _, err := LoadUserSettingsReadOnly(dbPath, slog.Default()); err == nil {
		t.Error("expected an error reading settings from a schema that predates the column")
	}

	if got := schemaVersion(t, dbPath); got != oldVersion {
		t.Errorf("schema moved from %d to %d — the read-only load migrated", oldVersion, got)
	}
}

// An absent database must yield an error the caller can ignore, never a panic
// and never a migrated schema.
func TestLoadUserSettingsReadOnly_UnmigratedDatabase(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "fresh.db")

	if _, err := LoadUserSettingsReadOnly(dbPath, slog.Default()); err == nil {
		t.Error("expected an error reading settings from an unmigrated database")
	}

	// `fresh` is "migration 1 was applied in this call", so a loader that had
	// migrated would make it false.
	db, fresh, err := NewSQLiteDB(dbPath, slog.Default())
	if err != nil {
		t.Fatalf("opening: %v", err)
	}
	defer func() { _ = db.Close() }()
	if !fresh {
		t.Error("the read-only load created schema it should not have")
	}
}

// TestSessionInsights_RebuiltTableKeepsEveryColumn guards migration 29, which
// could not add the second key column in place — SQLite cannot alter a primary
// key, so the table is created anew and the rows copied across.
//
// That shape has one failure mode and it is silent: a column left out of the
// new DDL or out of the INSERT list is simply gone, and the only complaint
// comes much later from whichever read still selects it. The column set is
// therefore spelled out here rather than derived, for the same reason
// sqlite_test.go hardcodes the schema version — a list computed from the table
// agrees with itself no matter what the table lost.
func TestSessionInsights_RebuiltTableKeepsEveryColumn(t *testing.T) {
	db := newTestDB(t)

	rows, err := db.QueryContext(context.Background(),
		`SELECT name FROM pragma_table_info('session_insights')`)
	if err != nil {
		t.Fatalf("reading table info: %v", err)
	}
	defer func() { _ = rows.Close() }()

	got := map[string]bool{}
	for rows.Next() {
		var name string
		if scanErr := rows.Scan(&name); scanErr != nil {
			t.Fatalf("scanning column: %v", scanErr)
		}
		got[name] = true
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("reading columns: %v", err)
	}

	// Every column migrations 1, 20, 22, 24 and 26 built, plus 29's own.
	// `claude_working_time_ms` is migration 24's rename of `thinking_time_ms`.
	want := []string{
		"session_id", "project_path", "processor_version", "scanned_at",
		"turn_count", "steps_per_turn_avg", "autonomy_score",
		"tool_calls_total", "tool_breakdown", "tool_error_rate",
		"total_duration_ms", "claude_working_time_ms", "active_duration_ms",
		"cache_hit_rate", "tokens_per_turn_avg", "cost_estimate_usd",
		"tool_error_count", "has_errors",
		"max_consecutive_tool_calls", "longest_autonomous_chain",
		"avg_user_response_time_ms", "avg_claude_response_time_ms",
		"session_type",
		"skill_breakdown", "plugin_breakdown", "mcp_server_breakdown",
		"mcp_tool_breakdown", "effort_breakdown", "unattributed_calls",
		"agent_breakdown",
	}
	for _, col := range want {
		if !got[col] {
			t.Errorf("column %q did not survive the rebuild", col)
		}
	}
	if len(got) != len(want) {
		t.Errorf("table has %d columns, want %d — a column was added without "+
			"being listed here", len(got), len(want))
	}

	// The whole point of the migration.
	var pk []string
	pkRows, err := db.QueryContext(context.Background(),
		`SELECT name FROM pragma_table_info('session_insights') WHERE pk > 0 ORDER BY pk`)
	if err != nil {
		t.Fatalf("reading primary key: %v", err)
	}
	defer func() { _ = pkRows.Close() }()
	for pkRows.Next() {
		var name string
		if scanErr := pkRows.Scan(&name); scanErr != nil {
			t.Fatalf("scanning pk column: %v", scanErr)
		}
		pk = append(pk, name)
	}
	if got, want := strings.Join(pk, ","), "session_id,project_path"; got != want {
		t.Errorf("primary key = %q, want %q", got, want)
	}
}

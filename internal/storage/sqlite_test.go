package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

func newTestDB(t *testing.T) *sql.DB {
	t.Helper()
	db, _, err := NewSQLiteDB(":memory:")
	if err != nil {
		t.Fatalf("opening test database: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return db
}

func TestNewSQLiteDB_CreatesTables(t *testing.T) {
	db := newTestDB(t)

	tables := []string{"agents", "chat_sessions", "chat_messages", "integrations", "user_settings", "schema_migrations", "claude_session_cache", "claude_cache_metadata", "notification_log"}
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
	if version != 3 {
		t.Errorf("expected version 3, got %d", version)
	}
}

func TestNewSQLiteDB_FreshDBFlag(t *testing.T) {
	db, fresh, err := NewSQLiteDB(":memory:")
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
	db := newTestDB(t)
	store := NewSQLiteAgentStore(db)

	// List empty
	agents, err := store.List()
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
	if saveErr := store.Save(agent); saveErr != nil {
		t.Fatalf("save: %v", saveErr)
	}

	// Get
	got, err := store.Get("test-agent")
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
	missing, err := store.Get("nonexistent")
	if err != nil {
		t.Fatalf("get nonexistent: %v", err)
	}
	if missing != nil {
		t.Error("expected nil for nonexistent agent")
	}

	// Update (upsert)
	agent.Description = "Updated description"
	if updateErr := store.Save(agent); updateErr != nil {
		t.Fatalf("save update: %v", updateErr)
	}
	got, _ = store.Get("test-agent")
	if got.Description != "Updated description" {
		t.Errorf("expected updated description, got %q", got.Description)
	}

	// List
	agents, err = store.List()
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(agents) != 1 {
		t.Fatalf("expected 1 agent, got %d", len(agents))
	}

	// Delete
	if err := store.Delete("test-agent"); err != nil {
		t.Fatalf("delete: %v", err)
	}
	agents, _ = store.List()
	if len(agents) != 0 {
		t.Errorf("expected 0 agents after delete, got %d", len(agents))
	}

	// Delete not found
	if err := store.Delete("nonexistent"); err == nil {
		t.Error("expected error deleting nonexistent agent")
	}
}

func TestSQLiteAgentStore_ValidationErrors(t *testing.T) {
	db := newTestDB(t)
	store := NewSQLiteAgentStore(db)

	if err := store.Save(&config.AgentConfig{Slug: "test"}); err == nil {
		t.Error("expected error for missing name")
	}
	if err := store.Save(&config.AgentConfig{Name: "Test"}); err == nil {
		t.Error("expected error for missing slug")
	}
}

// --- Chat Store Tests ---

func TestSQLiteChatStore_CRUD(t *testing.T) {
	db := newTestDB(t)
	store := NewSQLiteChatStore(db)

	// List empty
	sessions, err := store.ListSessions()
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(sessions) != 0 {
		t.Fatalf("expected 0 sessions, got %d", len(sessions))
	}

	// Create
	session, err := store.CreateSession("test-agent", "/tmp/work", "claude-sonnet-4-6", "profile1")
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
	got, err := store.GetSession(session.ID)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got == nil {
		t.Fatal("expected session, got nil")
	}
	if got.Model != "claude-sonnet-4-6" {
		t.Errorf("expected model, got %q", got.Model)
	}

	// Get not found
	missing, err := store.GetSession("nonexistent")
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
	if appendErr := store.AppendMessage(session.ID, msg1); appendErr != nil {
		t.Fatalf("append msg1: %v", appendErr)
	}
	if appendErr := store.AppendMessage(session.ID, msg2); appendErr != nil {
		t.Fatalf("append msg2: %v", appendErr)
	}

	// GetSessionWithMessages
	gotSession, messages, err := store.GetSessionWithMessages(session.ID)
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
	if err := store.UpdateSession(session); err != nil {
		t.Fatalf("update: %v", err)
	}
	got, _ = store.GetSession(session.ID)
	if got.Title != "Updated Title" {
		t.Errorf("expected updated title, got %q", got.Title)
	}
	if got.TotalInputTokens != 100 {
		t.Errorf("expected 100 input tokens, got %d", got.TotalInputTokens)
	}

	// List sessions
	sessions, _ = store.ListSessions()
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions))
	}

	// Delete (cascade should delete messages too)
	if err := store.DeleteSession(session.ID); err != nil {
		t.Fatalf("delete: %v", err)
	}
	sessions, _ = store.ListSessions()
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
	if err := store.DeleteSession("nonexistent"); err == nil {
		t.Error("expected error deleting nonexistent session")
	}
}

func TestSQLiteChatStore_GetSessionWithMessages_NotFound(t *testing.T) {
	db := newTestDB(t)
	store := NewSQLiteChatStore(db)

	session, messages, err := store.GetSessionWithMessages("nonexistent")
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
	db := newTestDB(t)
	store := NewSQLiteIntegrationStore(db)

	// List empty
	integrations, err := store.List()
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(integrations) != 0 {
		t.Fatalf("expected 0 integrations, got %d", len(integrations))
	}

	now := time.Now().UTC()
	cfg := &config.IntegrationConfig{
		ID:      "google-1",
		Name:    "Google Workspace",
		Type:    "google",
		Enabled: true,
		Credentials: config.GoogleCredentials{
			ClientID:     "client-id",
			ClientSecret: "client-secret",
		},
		Services: map[string]config.ServiceConfig{
			"calendar": {Enabled: true, Tools: []string{"list_events"}},
		},
		CreatedAt: now,
		UpdatedAt: now,
	}

	// Save
	if saveErr := store.Save(cfg); saveErr != nil {
		t.Fatalf("save: %v", saveErr)
	}

	// Get
	got, err := store.Get("google-1")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got == nil {
		t.Fatal("expected integration, got nil")
	}
	if !got.Enabled {
		t.Error("expected enabled=true")
	}
	if got.Credentials.ClientID != "client-id" {
		t.Errorf("expected client-id, got %q", got.Credentials.ClientID)
	}
	if got.Services["calendar"].Tools[0] != "list_events" {
		t.Errorf("unexpected tools: %v", got.Services["calendar"].Tools)
	}
	if got.Auth != nil {
		t.Error("expected nil auth for new integration")
	}

	// Get not found
	missing, err := store.Get("nonexistent")
	if err != nil {
		t.Fatalf("get nonexistent: %v", err)
	}
	if missing != nil {
		t.Error("expected nil for nonexistent integration")
	}

	// Update
	cfg.Name = "Updated Name"
	if err := store.Save(cfg); err != nil {
		t.Fatalf("save update: %v", err)
	}
	got, _ = store.Get("google-1")
	if got.Name != "Updated Name" {
		t.Errorf("expected updated name, got %q", got.Name)
	}

	// Delete
	if err := store.Delete("google-1"); err != nil {
		t.Fatalf("delete: %v", err)
	}
	integrations, _ = store.List()
	if len(integrations) != 0 {
		t.Errorf("expected 0 after delete, got %d", len(integrations))
	}

	// Delete not found
	if err := store.Delete("nonexistent"); err == nil {
		t.Error("expected error deleting nonexistent integration")
	}
}

func TestSQLiteIntegrationStore_SaveRequiresID(t *testing.T) {
	db := newTestDB(t)
	store := NewSQLiteIntegrationStore(db)

	err := store.Save(&config.IntegrationConfig{Name: "No ID"})
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
	agent, err := agentStore.Get("hello")
	if err != nil {
		t.Fatalf("get agent: %v", err)
	}
	if agent == nil || agent.Name != "Hello" {
		t.Errorf("unexpected agent: %+v", agent)
	}

	// Verify chat session and messages
	chatStore := NewSQLiteChatStore(db)
	session, messages, err := chatStore.GetSessionWithMessages("sess-1")
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
	agents, _ := store.List()
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

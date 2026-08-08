package claudesessions

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// writeSubagentJSONL creates <dir>/<sessionID>/subagents/<agentID>.jsonl with a
// user + assistant pair. Every event carries isSidechain=true, matching what
// Claude Code actually writes into sub-agent transcripts.
func writeSubagentJSONL(
	t *testing.T, dir, sessionID, agentID string, ts time.Time, in, out int,
) string {
	t.Helper()
	subagentsDir := filepath.Join(dir, sessionID, "subagents")
	if err := os.MkdirAll(subagentsDir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	fp := filepath.Join(subagentsDir, agentID+".jsonl")

	userMsg, _ := json.Marshal(rawEvent{
		Type: "user", SessionID: sessionID, Timestamp: ts, IsSidechain: true,
		Message: &rawMessage{Role: "user", Content: json.RawMessage(`"delegate this"`)},
	})
	assistantMsg, _ := json.Marshal(rawEvent{
		Type: "assistant", SessionID: sessionID, Timestamp: ts.Add(time.Second), IsSidechain: true,
		Message: &rawMessage{
			Role: "assistant", Model: "claude-haiku-4-5",
			Content: json.RawMessage(`[{"type":"text","text":"done"}]`),
			Usage:   &rawUsage{InputTokens: in, OutputTokens: out},
		},
	})

	var data []byte
	data = append(data, userMsg...)
	data = append(data, '\n')
	data = append(data, assistantMsg...)
	data = append(data, '\n')

	if err := os.WriteFile(fp, data, 0600); err != nil {
		t.Fatalf("write subagent jsonl: %v", err)
	}
	return fp
}

// writeSubagentMeta writes the agent-<id>.meta.json sidecar next to a transcript.
func writeSubagentMeta(t *testing.T, dir, sessionID, agentID, agentType, description, toolUseID string) {
	t.Helper()
	fp := filepath.Join(dir, sessionID, "subagents", agentID+".meta.json")
	data, err := json.Marshal(map[string]string{
		"agentType": agentType, "description": description, "toolUseId": toolUseID,
	})
	if err != nil {
		t.Fatalf("marshal meta: %v", err)
	}
	if err := os.WriteFile(fp, data, 0600); err != nil {
		t.Fatalf("write meta: %v", err)
	}
}

// setupSubagentProject creates a project dir with one parent session and
// returns (projectDir, logger). HOME is redirected to a temp dir.
func setupSubagentProject(t *testing.T, sessionID string, ts time.Time) string {
	t.Helper()
	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")
	writeJSONL(t, projectDir, sessionID, ts)
	return projectDir
}

func findSession(t *testing.T, sessions []ClaudeSessionSummary, id string) ClaudeSessionSummary {
	t.Helper()
	for _, s := range sessions {
		if s.SessionID == id {
			return s
		}
	}
	t.Fatalf("session %q not found in %d results", id, len(sessions))
	return ClaudeSessionSummary{}
}

// TestIncrementalScan_RollsUpSubagentUsage covers the core acceptance criterion:
// a session with N sub-agent files reports subagent_count == N and a
// subagent_usage equal to the summed assistant usage of those files, while its
// own main-thread Usage is untouched.
func TestIncrementalScan_RollsUpSubagentUsage(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "session-abc", ts)
	writeSubagentJSONL(t, projectDir, "session-abc", "agent-001", ts.Add(time.Minute), 100, 200)
	writeSubagentJSONL(t, projectDir, "session-abc", "agent-002", ts.Add(2*time.Minute), 30, 7)

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}

	s := findSession(t, sessions, "session-abc")
	if s.SubagentCount != 2 {
		t.Errorf("expected subagent_count 2, got %d", s.SubagentCount)
	}
	if s.SubagentUsage.InputTokens != 130 {
		t.Errorf("expected subagent input 130, got %d", s.SubagentUsage.InputTokens)
	}
	if s.SubagentUsage.OutputTokens != 207 {
		t.Errorf("expected subagent output 207, got %d", s.SubagentUsage.OutputTokens)
	}
	// Main-thread usage keeps its original meaning.
	if s.Usage.InputTokens != 10 || s.Usage.OutputTokens != 20 {
		t.Errorf("main-thread usage changed: got %+v", s.Usage)
	}
	total := s.TotalUsage()
	if total.InputTokens != 140 || total.OutputTokens != 227 {
		t.Errorf("expected total usage 140/227, got %d/%d", total.InputTokens, total.OutputTokens)
	}
}

// TestIncrementalScan_NoSubagentsDirUnchanged pins the regression guarantee:
// a session that never delegated behaves exactly as before.
func TestIncrementalScan_NoSubagentsDirUnchanged(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	setupSubagentProject(t, "session-plain", ts)

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}

	s := findSession(t, sessions, "session-plain")
	if s.SubagentCount != 0 {
		t.Errorf("expected subagent_count 0, got %d", s.SubagentCount)
	}
	if s.SubagentUsage != (TokenUsage{}) {
		t.Errorf("expected zero subagent usage, got %+v", s.SubagentUsage)
	}
	if s.TotalUsage() != s.Usage {
		t.Errorf("TotalUsage should equal Usage with no sub-agents: %+v vs %+v", s.TotalUsage(), s.Usage)
	}
}

// TestSubagentMeta_PopulatesAndDegrades checks the sidecar is read when present
// and that a missing one leaves the columns empty without losing the transcript.
func TestSubagentMeta_PopulatesAndDegrades(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "session-meta", ts)
	writeSubagentJSONL(t, projectDir, "session-meta", "agent-withmeta", ts.Add(time.Minute), 5, 5)
	writeSubagentMeta(t, projectDir, "session-meta", "agent-withmeta",
		"general-purpose", "Read issue 179 in full", "tool_abc123")
	// agent-nometa deliberately has no sidecar.
	writeSubagentJSONL(t, projectDir, "session-meta", "agent-nometa", ts.Add(2*time.Minute), 1, 1)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}

	subagents, err := ListSubagents(db, logger, "session-meta")
	if err != nil {
		t.Fatalf("ListSubagents: %v", err)
	}
	if len(subagents) != 2 {
		t.Fatalf("expected 2 sub-agents, got %d", len(subagents))
	}

	byID := map[string]ClaudeSubagent{}
	for _, sa := range subagents {
		byID[sa.AgentID] = sa
	}

	withMeta := byID["agent-withmeta"]
	if withMeta.AgentType != "general-purpose" {
		t.Errorf("expected agent_type general-purpose, got %q", withMeta.AgentType)
	}
	if withMeta.Description != "Read issue 179 in full" {
		t.Errorf("unexpected description %q", withMeta.Description)
	}
	if withMeta.ToolUseID != "tool_abc123" {
		t.Errorf("unexpected tool_use_id %q", withMeta.ToolUseID)
	}

	noMeta := byID["agent-nometa"]
	if noMeta.AgentType != "" || noMeta.Description != "" || noMeta.ToolUseID != "" {
		t.Errorf("expected empty meta columns, got %+v", noMeta)
	}
	// The transcript itself must survive a missing sidecar.
	if noMeta.Usage.InputTokens != 1 || noMeta.MessageCount == 0 {
		t.Errorf("transcript lost with missing sidecar: %+v", noMeta)
	}
}

// TestSubagentSummary_CountsSidechainUsers guards the correction to the issue's
// premise: every sub-agent event is isSidechain, so the shared summary reader
// would otherwise count assistant turns only.
func TestSubagentSummary_CountsSidechainUsers(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "session-count", ts)
	fp := writeSubagentJSONL(t, projectDir, "session-count", "agent-x", ts, 1, 2)

	sub, err := readSubagentSummary("session-count", "/tmp", fp, testLogger)
	if err != nil || sub == nil {
		t.Fatalf("readSubagentSummary: %v", err)
	}
	if sub.MessageCount != 2 {
		t.Errorf("expected 2 messages (user + assistant), got %d", sub.MessageCount)
	}

	// The parent-session reader must still drop sidechain user turns.
	main, err := readSessionSummary("session-count", "/tmp", fp, testLogger)
	if err != nil || main == nil {
		t.Fatalf("readSessionSummary: %v", err)
	}
	if main.MessageCount != 1 {
		t.Errorf("expected sidechain user dropped for parent read, got %d", main.MessageCount)
	}
}

// TestIncrementalScan_SubagentIncremental asserts that touching one sub-agent
// transcript re-reads only that row: every other row keeps its stored mtime.
func TestIncrementalScan_SubagentIncremental(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "session-inc", ts)
	touched := writeSubagentJSONL(t, projectDir, "session-inc", "agent-touched", ts, 10, 10)
	writeSubagentJSONL(t, projectDir, "session-inc", "agent-stable", ts, 20, 20)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	before, err := loadCachedEntries(db, logger)
	if err != nil {
		t.Fatalf("loadCachedEntries: %v", err)
	}

	time.Sleep(10 * time.Millisecond)
	// Rewrite with different usage so a re-read is observable.
	writeSubagentJSONL(t, projectDir, "session-inc", "agent-touched", ts, 99, 99)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("second scan: %v", err)
	}

	after, err := loadCachedEntries(db, logger)
	if err != nil {
		t.Fatalf("loadCachedEntries: %v", err)
	}

	for path, beforeEntry := range before {
		afterEntry, ok := after[path]
		if !ok {
			t.Fatalf("cache row disappeared for %s", path)
		}
		changed := !beforeEntry.mtime.Equal(afterEntry.mtime)
		if path == touched && !changed {
			t.Errorf("touched sub-agent file was not re-read")
		}
		if path != touched && changed {
			t.Errorf("unrelated row %s was re-read (mtime changed)", path)
		}
	}

	// And the re-read actually picked up the new numbers.
	sessions, err := loadAllSessions(db, logger)
	if err != nil {
		t.Fatalf("loadAllSessions: %v", err)
	}
	s := findSession(t, sessions, "session-inc")
	if s.SubagentUsage.InputTokens != 119 {
		t.Errorf("expected subagent input 99+20=119, got %d", s.SubagentUsage.InputTokens)
	}
}

// TestIncrementalScan_DeletesSubagentRows covers removal of a session's
// subagents/ directory.
func TestIncrementalScan_DeletesSubagentRows(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "session-del", ts)
	writeSubagentJSONL(t, projectDir, "session-del", "agent-gone", ts, 10, 10)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}
	if subs, _ := ListSubagents(db, logger, "session-del"); len(subs) != 1 {
		t.Fatalf("expected 1 sub-agent before deletion, got %d", len(subs))
	}

	if err := os.RemoveAll(filepath.Join(projectDir, "session-del")); err != nil {
		t.Fatalf("remove subagents dir: %v", err)
	}

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}

	subs, err := ListSubagents(db, logger, "session-del")
	if err != nil {
		t.Fatalf("ListSubagents: %v", err)
	}
	if len(subs) != 0 {
		t.Errorf("expected sub-agent rows removed, got %d", len(subs))
	}
	// The parent session itself survives.
	s := findSession(t, sessions, "session-del")
	if s.SubagentCount != 0 {
		t.Errorf("expected subagent_count 0 after deletion, got %d", s.SubagentCount)
	}
}

// TestIncrementalScan_SubagentPreservesCustomTitle asserts the user-defined
// fields on the parent row survive a scan that writes sub-agent rows.
func TestIncrementalScan_SubagentPreservesCustomTitle(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "session-title", ts)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	cache := NewCache(db, logger)
	if err := cache.UpdateCustomTitle("session-title", "Preserved Title"); err != nil {
		t.Fatalf("UpdateCustomTitle: %v", err)
	}
	if err := cache.UpdateFavorite("session-title", true); err != nil {
		t.Fatalf("UpdateFavorite: %v", err)
	}

	// A sub-agent appearing afterwards inserts new rows for this session.
	writeSubagentJSONL(t, projectDir, "session-title", "agent-new", ts.Add(time.Minute), 5, 5)

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}

	s := findSession(t, sessions, "session-title")
	if s.CustomTitle != "Preserved Title" {
		t.Errorf("custom_title lost: got %q", s.CustomTitle)
	}
	if !s.IsFavorite {
		t.Error("is_favorite lost")
	}
	if s.SubagentCount != 1 {
		t.Errorf("expected subagent_count 1, got %d", s.SubagentCount)
	}
}

// TestSubagentFiles lists a session's transcripts and returns nil when the
// session delegated nothing.
func TestSubagentFiles(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "session-files", ts)
	parent := filepath.Join(projectDir, "session-files.jsonl")

	if got := SubagentFiles("session-files", parent); got != nil {
		t.Errorf("expected nil for a session with no subagents/, got %v", got)
	}

	writeSubagentJSONL(t, projectDir, "session-files", "agent-b", ts, 1, 1)
	writeSubagentJSONL(t, projectDir, "session-files", "agent-a", ts, 1, 1)
	writeSubagentMeta(t, projectDir, "session-files", "agent-a", "explore", "d", "t")

	got := SubagentFiles("session-files", parent)
	if len(got) != 2 {
		t.Fatalf("expected 2 transcripts, got %d: %v", len(got), got)
	}
	// Sorted, and the .meta.json sidecar is not mistaken for a transcript.
	if filepath.Base(got[0]) != "agent-a.jsonl" || filepath.Base(got[1]) != "agent-b.jsonl" {
		t.Errorf("unexpected file list: %v", got)
	}
}

// TestBuildAnalytics_IncludesSubagentTokens asserts aggregate reporting rises by
// the delegated contribution rather than ignoring it.
func TestBuildAnalytics_IncludesSubagentTokens(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{{
		SessionID:     "s1",
		Model:         "claude-sonnet-4-6",
		StartTime:     ts,
		LastActivity:  ts,
		Usage:         TokenUsage{InputTokens: 100, OutputTokens: 50},
		SubagentCount: 1,
		SubagentUsage: TokenUsage{InputTokens: 10, OutputTokens: 5},
	}}

	summary, _ := buildSummary(sessions)
	if summary.TotalInputTokens != 110 {
		t.Errorf("expected total input 110, got %d", summary.TotalInputTokens)
	}
	if summary.TotalOutputTokens != 55 {
		t.Errorf("expected total output 55, got %d", summary.TotalOutputTokens)
	}
	if summary.TotalTokens != 165 {
		t.Errorf("expected total tokens 165, got %d", summary.TotalTokens)
	}
}

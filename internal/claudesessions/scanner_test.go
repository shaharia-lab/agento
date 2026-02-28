package claudesessions

import (
	"database/sql"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/storage"
)

func setupTestDB(t *testing.T) *sql.DB {
	t.Helper()
	dbPath := filepath.Join(t.TempDir(), "test.db")
	db, _, err := storage.NewSQLiteDB(dbPath)
	if err != nil {
		t.Fatalf("failed to create test db: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return db
}

// writeJSONL creates a minimal JSONL session file with the given session ID and timestamp.
func writeJSONL(t *testing.T, dir, sessionID string, ts time.Time) string {
	t.Helper()
	if err := os.MkdirAll(dir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	fp := filepath.Join(dir, sessionID+".jsonl")

	userMsg, _ := json.Marshal(rawEvent{
		Type:      "user",
		SessionID: sessionID,
		Timestamp: ts,
		CWD:       "/tmp",
		Message: &rawMessage{
			Role:    "user",
			Content: json.RawMessage(`"hello world"`),
		},
	})
	assistantMsg, _ := json.Marshal(rawEvent{
		Type:      "assistant",
		SessionID: sessionID,
		Timestamp: ts.Add(time.Second),
		Message: &rawMessage{
			Role:    "assistant",
			Model:   "claude-sonnet-4-6",
			Content: json.RawMessage(`[{"type":"text","text":"hi"}]`),
			Usage:   &rawUsage{InputTokens: 10, OutputTokens: 20},
		},
	})

	data := append(userMsg, '\n')
	data = append(data, assistantMsg...)
	data = append(data, '\n')

	if err := os.WriteFile(fp, data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}
	return fp
}

func TestIncrementalScan_NewFiles(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-001", ts)
	writeJSONL(t, projectDir, "session-002", ts.Add(time.Hour))

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}

	if len(sessions) != 2 {
		t.Fatalf("expected 2 sessions, got %d", len(sessions))
	}

	// Verify order: most recent first.
	if sessions[0].SessionID != "session-002" {
		t.Errorf("expected session-002 first, got %s", sessions[0].SessionID)
	}
	if sessions[1].SessionID != "session-001" {
		t.Errorf("expected session-001 second, got %s", sessions[1].SessionID)
	}

	// Verify token counts.
	if sessions[0].Usage.InputTokens != 10 {
		t.Errorf("expected 10 input tokens, got %d", sessions[0].Usage.InputTokens)
	}
}

func TestIncrementalScan_ModifiedFiles(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-001", ts)

	// First scan.
	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("first scan: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions))
	}
	if sessions[0].MessageCount != 2 {
		t.Errorf("expected 2 messages, got %d", sessions[0].MessageCount)
	}

	// Modify the file — add more messages with a new mtime.
	time.Sleep(10 * time.Millisecond) // Ensure mtime changes.
	writeJSONL(t, projectDir, "session-001", ts.Add(2*time.Hour))

	// Second scan should pick up the modified file.
	sessions, err = IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions))
	}
	// The rewritten file has the same structure (2 messages).
	if sessions[0].MessageCount != 2 {
		t.Errorf("expected 2 messages after modification, got %d", sessions[0].MessageCount)
	}
}

func TestIncrementalScan_DeletedFiles(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	fp := writeJSONL(t, projectDir, "session-001", ts)
	writeJSONL(t, projectDir, "session-002", ts)

	// First scan.
	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("first scan: %v", err)
	}
	if len(sessions) != 2 {
		t.Fatalf("expected 2 sessions, got %d", len(sessions))
	}

	// Delete one file.
	if err := os.Remove(fp); err != nil {
		t.Fatalf("remove: %v", err)
	}

	// Second scan should reflect the deletion.
	sessions, err = IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session after deletion, got %d", len(sessions))
	}
	if sessions[0].SessionID != "session-002" {
		t.Errorf("expected session-002 to remain, got %s", sessions[0].SessionID)
	}
}

func TestIncrementalScan_UnchangedFiles(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-001", ts)

	// First scan.
	sessions1, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("first scan: %v", err)
	}
	if len(sessions1) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions1))
	}

	// Second scan — file unchanged, should still return same data.
	sessions2, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}
	if len(sessions2) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions2))
	}
	if sessions2[0].SessionID != sessions1[0].SessionID {
		t.Errorf("session ID mismatch: %s vs %s", sessions1[0].SessionID, sessions2[0].SessionID)
	}
}

func TestIncrementalScan_EmptyProjectsDir(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	// Don't create the projects dir.

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	if len(sessions) != 0 {
		t.Fatalf("expected 0 sessions, got %d", len(sessions))
	}
}

func TestCache_ListAndInvalidate(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-001", ts)

	cache := NewCache(db, logger)

	// First List() should trigger a scan.
	sessions := cache.List()
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions))
	}

	// Second List() should be fast (from cache).
	sessions = cache.List()
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session on cache hit, got %d", len(sessions))
	}

	// Invalidate and add a new file.
	writeJSONL(t, projectDir, "session-002", ts.Add(time.Hour))
	cache.Invalidate()

	sessions = cache.List()
	if len(sessions) != 2 {
		t.Fatalf("expected 2 sessions after invalidate, got %d", len(sessions))
	}
}

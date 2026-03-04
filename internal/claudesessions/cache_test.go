package claudesessions

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestCache_UpdateAndGetCustomTitle(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-abc", ts)

	cache := NewCache(db, logger)
	// Populate the cache.
	sessions := cache.List()
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions))
	}

	// Initially no custom title.
	if got := cache.GetCustomTitle("session-abc"); got != "" {
		t.Errorf("expected empty custom title, got %q", got)
	}

	// Set a custom title.
	if err := cache.UpdateCustomTitle("session-abc", "My Title"); err != nil {
		t.Fatalf("UpdateCustomTitle: %v", err)
	}

	// Verify it can be retrieved.
	if got := cache.GetCustomTitle("session-abc"); got != "My Title" {
		t.Errorf("expected %q, got %q", "My Title", got)
	}
}

func TestCache_GetCustomTitle_UnknownSession(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	cache := NewCache(db, logger)
	// Should return empty string without error for unknown session.
	if got := cache.GetCustomTitle("nonexistent-id"); got != "" {
		t.Errorf("expected empty string for unknown session, got %q", got)
	}
}

func TestCache_UpdateCustomTitle_Overwrite(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-abc", ts)

	cache := NewCache(db, logger)
	cache.List() // populate

	if err := cache.UpdateCustomTitle("session-abc", "First Title"); err != nil {
		t.Fatalf("first update: %v", err)
	}
	if err := cache.UpdateCustomTitle("session-abc", "Second Title"); err != nil {
		t.Fatalf("second update: %v", err)
	}

	if got := cache.GetCustomTitle("session-abc"); got != "Second Title" {
		t.Errorf("expected %q, got %q", "Second Title", got)
	}
}

func TestCache_UpdateCustomTitle_ClearTitle(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-abc", ts)

	cache := NewCache(db, logger)
	cache.List()

	if err := cache.UpdateCustomTitle("session-abc", "Some Title"); err != nil {
		t.Fatalf("UpdateCustomTitle: %v", err)
	}
	// Clear by setting empty string.
	if err := cache.UpdateCustomTitle("session-abc", ""); err != nil {
		t.Fatalf("clear title: %v", err)
	}
	if got := cache.GetCustomTitle("session-abc"); got != "" {
		t.Errorf("expected empty after clear, got %q", got)
	}
}

// TestIncrementalScan_PreservesCustomTitle is the most critical invariant:
// a rescan of an unchanged or modified file must NOT overwrite the user-defined
// custom_title stored in SQLite.
func TestIncrementalScan_PreservesCustomTitle(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-abc", ts)

	// Initial scan — populates cache row.
	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("first IncrementalScan: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions))
	}

	// Set a custom title directly on the DB (simulates the PATCH handler).
	cache := NewCache(db, logger)
	if err := cache.UpdateCustomTitle("session-abc", "Preserved Title"); err != nil {
		t.Fatalf("UpdateCustomTitle: %v", err)
	}

	// Trigger a second scan on an unchanged file — the upsert must not overwrite custom_title.
	sessions, err = IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second IncrementalScan: %v", err)
	}

	// Find the session in results.
	var found *ClaudeSessionSummary
	for i := range sessions {
		if sessions[i].SessionID == "session-abc" {
			found = &sessions[i]
			break
		}
	}
	if found == nil {
		t.Fatal("session-abc not found in second scan results")
	}
	if found.CustomTitle != "Preserved Title" {
		t.Errorf("custom_title was overwritten: expected %q, got %q", "Preserved Title", found.CustomTitle)
	}
}

// TestIncrementalScan_PreservesCustomTitle_OnModifiedFile checks that even when
// the underlying JSONL is modified and the row is re-upserted, the custom_title survives.
func TestIncrementalScan_PreservesCustomTitle_OnModifiedFile(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-abc", ts)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	cache := NewCache(db, logger)
	if err := cache.UpdateCustomTitle("session-abc", "Still Here"); err != nil {
		t.Fatalf("UpdateCustomTitle: %v", err)
	}

	// Modify the JSONL file (new mtime forces re-parse and upsert).
	time.Sleep(10 * time.Millisecond)
	writeJSONL(t, projectDir, "session-abc", ts.Add(2*time.Hour))

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}

	var found *ClaudeSessionSummary
	for i := range sessions {
		if sessions[i].SessionID == "session-abc" {
			found = &sessions[i]
			break
		}
	}
	if found == nil {
		t.Fatal("session-abc not found after file modification")
	}
	if found.CustomTitle != "Still Here" {
		t.Errorf("custom_title lost on file modification: expected %q, got %q", "Still Here", found.CustomTitle)
	}
}

// TestCache_UpdateAndGetFavorite verifies the basic round-trip for is_favorite.
func TestCache_UpdateAndGetFavorite(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-fav", ts)

	cache := NewCache(db, logger)
	cache.List() // populate

	// Initially not favorited.
	if got := cache.GetFavorite("session-fav"); got {
		t.Error("expected is_favorite=false initially")
	}

	// Favorite the session.
	if err := cache.UpdateFavorite("session-fav", true); err != nil {
		t.Fatalf("UpdateFavorite: %v", err)
	}
	if got := cache.GetFavorite("session-fav"); !got {
		t.Error("expected is_favorite=true after update")
	}

	// Unfavorite it.
	if err := cache.UpdateFavorite("session-fav", false); err != nil {
		t.Fatalf("UpdateFavorite(false): %v", err)
	}
	if got := cache.GetFavorite("session-fav"); got {
		t.Error("expected is_favorite=false after clearing")
	}
}

// TestIncrementalScan_PreservesFavorite verifies that a rescan of an unchanged
// file does NOT overwrite the user-set is_favorite flag.
func TestIncrementalScan_PreservesFavorite(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-fav", ts)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	cache := NewCache(db, logger)
	if err := cache.UpdateFavorite("session-fav", true); err != nil {
		t.Fatalf("UpdateFavorite: %v", err)
	}

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}

	var found *ClaudeSessionSummary
	for i := range sessions {
		if sessions[i].SessionID == "session-fav" {
			found = &sessions[i]
			break
		}
	}
	if found == nil {
		t.Fatal("session-fav not found in second scan")
	}
	if !found.IsFavorite {
		t.Error("is_favorite was overwritten by rescan")
	}
}

// TestIncrementalScan_PreservesFavorite_OnModifiedFile checks that is_favorite
// survives even when the JSONL file is modified and the row is re-upserted.
func TestIncrementalScan_PreservesFavorite_OnModifiedFile(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-fav", ts)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	cache := NewCache(db, logger)
	if err := cache.UpdateFavorite("session-fav", true); err != nil {
		t.Fatalf("UpdateFavorite: %v", err)
	}

	// Modify the file to force a re-upsert.
	time.Sleep(10 * time.Millisecond)
	writeJSONL(t, projectDir, "session-fav", ts.Add(2*time.Hour))

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan after modification: %v", err)
	}

	var found *ClaudeSessionSummary
	for i := range sessions {
		if sessions[i].SessionID == "session-fav" {
			found = &sessions[i]
			break
		}
	}
	if found == nil {
		t.Fatal("session-fav not found after file modification")
	}
	if !found.IsFavorite {
		t.Error("is_favorite lost after file modification and re-upsert")
	}
}

// TestCache_List_IncludesCustomTitle ensures that Cache.List() returns the
// stored custom_title in the session summaries.
func TestCache_List_IncludesCustomTitle(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-abc", ts)

	cache := NewCache(db, logger)
	cache.List() // initial scan

	if err := cache.UpdateCustomTitle("session-abc", "List Title"); err != nil {
		t.Fatalf("UpdateCustomTitle: %v", err)
	}

	// Second List() reads from cache (within TTL) — must include custom_title.
	sessions := cache.List()
	if len(sessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(sessions))
	}
	if sessions[0].CustomTitle != "List Title" {
		t.Errorf("List() did not include custom_title: expected %q, got %q", "List Title", sessions[0].CustomTitle)
	}
}

package claudesessions

import (
	"log/slog"
	"os"
	"path/filepath"
	"sync"
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

// setupPopulatedCache returns a cache with exactly one scanned session.
func setupPopulatedCache(t *testing.T) *Cache {
	t.Helper()
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	writeJSONL(t, filepath.Join(home, ".claude", "projects", "test-project"),
		"session-bg", time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	cache := NewCache(db, logger)
	if got := len(cache.List()); got != 1 {
		t.Fatalf("cold List returned %d sessions, want 1", got)
	}
	// The cold List returns as soon as there are rows to serve, which can be
	// before the scan goroutine has run its cleanup — leaving a real scan in
	// flight. A test that then fakes one with markScanning gets its flag
	// cleared out from under it when the real one finishes. Wait the scan out
	// so every caller starts from a quiescent cache.
	<-cache.EnsureScan()
	return cache
}

// markScanning fakes an in-flight scan without running one, so the read path
// can be tested deterministically. Returns the func that finishes it.
func markScanning(t *testing.T, c *Cache) func() {
	t.Helper()
	done := make(chan struct{})
	c.mu.Lock()
	c.scanning = true
	c.scanDone = done
	c.mu.Unlock()
	return func() {
		c.mu.Lock()
		c.scanning = false
		c.mu.Unlock()
		close(done)
	}
}

// TestCache_ListDoesNotBlockOnAnInFlightScan is the load-bearing criterion of
// #208. A stale cache must serve its existing rows immediately rather than
// waiting for the rescan — on a real corpus that wait is ~18s, and #189 made
// the trigger (saving a rate) a routine UI action.
func TestCache_ListDoesNotBlockOnAnInFlightScan(t *testing.T) {
	cache := setupPopulatedCache(t)
	finish := markScanning(t, cache)
	defer finish()

	// Force the freshness check to fail, so List takes the stale path.
	cache.Invalidate()

	start := time.Now()
	sessions := cache.List()
	elapsed := time.Since(start)

	if len(sessions) != 1 {
		t.Errorf("List returned %d sessions, want the 1 cached row", len(sessions))
	}
	if elapsed > 5*time.Second {
		t.Errorf("List took %s while a scan was in flight; it must return cached rows at once", elapsed)
	}
	if !cache.ScanInProgress() {
		t.Error("ScanInProgress() = false during an in-flight scan")
	}
}

// TestCache_EnsureScanAdmitsOnlyOneScan covers the at-most-one-scan criterion:
// two rate saves in quick succession must not queue a second full re-read.
func TestCache_EnsureScanAdmitsOnlyOneScan(t *testing.T) {
	cache := setupPopulatedCache(t)

	t.Run("joins an in-flight scan", func(t *testing.T) {
		finish := markScanning(t, cache)
		defer finish()

		cache.mu.Lock()
		inFlight := cache.scanDone
		cache.mu.Unlock()

		if got := cache.EnsureScan(); got != (<-chan struct{})(inFlight) {
			t.Error("EnsureScan started a second scan while one was already running")
		}
	})

	t.Run("concurrent callers never start more than one at a time", func(t *testing.T) {
		const callers = 8
		var wg sync.WaitGroup
		chans := make([]<-chan struct{}, callers)
		for i := range callers {
			wg.Add(1)
			go func() {
				defer wg.Done()
				chans[i] = cache.EnsureScan()
			}()
		}
		wg.Wait()

		// Callers either join the same scan or start a fresh one after the
		// previous finished — never overlap. A distinct channel is therefore
		// only legitimate if the one before it has already closed.
		for _, ch := range chans {
			if ch == nil {
				t.Fatal("EnsureScan returned a nil channel")
			}
			<-ch // must complete; a leaked guard would hang here
		}
		if cache.ScanInProgress() {
			t.Error("a scan is still marked in progress after every scan finished")
		}
	})
}

// TestCache_ColdCacheStillReturnsRows guards the carve-out. Everything else in
// #208 is about not blocking, but a first run has nothing to serve and an empty
// list reads as "no sessions" rather than "not scanned yet".
func TestCache_ColdCacheStillReturnsRows(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	writeJSONL(t, filepath.Join(home, ".claude", "projects", "test-project"),
		"session-cold", time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	// No prior scan at all — the very first request must not return [].
	if got := len(NewCache(db, logger).List()); got != 1 {
		t.Errorf("cold-cache List returned %d sessions, want 1", got)
	}
}

// TestCache_ScanGuardClearsAfterScan pins the leak the issue flags as a risk: a
// guard left set would mark the cache permanently "scanning" and it would never
// re-cost again.
func TestCache_ScanGuardClearsAfterScan(t *testing.T) {
	cache := setupPopulatedCache(t)

	<-cache.EnsureScan()

	if cache.ScanInProgress() {
		t.Error("ScanInProgress() = true after the scan completed; the guard leaked")
	}
	// And a later trigger is still admitted.
	select {
	case <-cache.EnsureScan():
	case <-time.After(30 * time.Second):
		t.Error("a scan after the first never completed; admission is stuck")
	}
}

// TestCacheGetSummary covers the read the detail endpoint depends on: cost is
// stored by the scanner and cannot be recovered from a re-read of the
// transcript, so a detail view that could not read this row would report $0.00
// for a session the list prices correctly.
func TestCacheGetSummary(t *testing.T) {
	db := setupTestDB(t)
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")
	writeJSONL(t, projectDir, "session-abc", time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	listed, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	if len(listed) != 1 {
		t.Fatalf("expected 1 session, got %d", len(listed))
	}

	cache := NewCache(db, logger)
	got := cache.GetSummary("session-abc")
	if got == nil {
		t.Fatal("GetSummary returned nil for a scanned session")
	}
	if got.SessionID != listed[0].SessionID {
		t.Errorf("session id: got %q, want %q", got.SessionID, listed[0].SessionID)
	}
	// The whole point of the row: the same figures the list serves.
	if got.Cost != listed[0].Cost {
		t.Errorf("cost: got %+v, want %+v", got.Cost, listed[0].Cost)
	}
	if got.SubagentCost != listed[0].SubagentCost {
		t.Errorf("subagent cost: got %+v, want %+v", got.SubagentCost, listed[0].SubagentCost)
	}
	if got.Usage != listed[0].Usage {
		t.Errorf("usage: got %+v, want %+v", got.Usage, listed[0].Usage)
	}

	if missing := cache.GetSummary("no-such-session"); missing != nil {
		t.Errorf("GetSummary for an unknown session: got %+v, want nil", missing)
	}
}

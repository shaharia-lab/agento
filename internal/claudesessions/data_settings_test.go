package claudesessions

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

// withDataSettings installs Data & Analytics preferences for one test and
// restores the previous snapshot afterwards. The snapshot is process-wide, so
// a test that leaked its settings would silently change what every later test
// in the binary measures.
func withDataSettings(t *testing.T, idleGapMinutes int, hidden []string) {
	t.Helper()
	dataSettings.RLock()
	prevGap, prevHidden := dataSettings.idleGap, dataSettings.hidden
	dataSettings.RUnlock()

	t.Cleanup(func() {
		dataSettings.Lock()
		dataSettings.idleGap, dataSettings.hidden = prevGap, prevHidden
		dataSettings.Unlock()
	})

	ApplyDataSettings(idleGapMinutes, hidden)
}

// writeGappedJSONL writes a two-event transcript separated by a 30-minute gap:
// long enough to be capped by the default threshold and not by a raised one,
// which is what makes the two scans in this file disagree on purpose.
func writeGappedJSONL(t *testing.T, dir, sessionID string, ts time.Time) {
	t.Helper()
	if err := os.MkdirAll(dir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

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
		Timestamp: ts.Add(30 * time.Minute),
		Message: &rawMessage{
			Role:    "assistant",
			Model:   "claude-sonnet-4-6",
			Content: json.RawMessage(`[{"type":"text","text":"hi"}]`),
			Usage:   &rawUsage{InputTokens: 10, OutputTokens: 20},
		},
	})

	data := append(append(append(userMsg, '\n'), assistantMsg...), '\n')
	if err := os.WriteFile(filepath.Join(dir, sessionID+jsonlExt), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}
}

func TestApplyDataSettings_IdleGap(t *testing.T) {
	cases := []struct {
		name    string
		minutes int
		want    time.Duration
	}{
		{"a chosen value is used as given", 25, 25 * time.Minute},
		{"the lower bound is valid", config.MinIdleGapThresholdMinutes, 1 * time.Minute},
		{"the upper bound is valid", config.MaxIdleGapThresholdMinutes, 240 * time.Minute},
		{"unset falls back to the default", 0, config.DefaultIdleGapThresholdMinutes * time.Minute},
		{"negative falls back to the default", -5, config.DefaultIdleGapThresholdMinutes * time.Minute},
		{"above the bound falls back to the default", 10_000, config.DefaultIdleGapThresholdMinutes * time.Minute},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			withDataSettings(t, tc.minutes, nil)
			if got := IdleGapThreshold(); got != tc.want {
				t.Errorf("IdleGapThreshold() = %v, want %v", got, tc.want)
			}
		})
	}
}

// TestActiveDuration_UsesConfiguredThreshold is the point of making the
// threshold configurable: the same events must produce a different active
// duration under a different definition of "still working".
func TestActiveDuration_UsesConfiguredThreshold(t *testing.T) {
	base := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	// Two gaps: 2 minutes, then 30 minutes.
	stamps := []time.Time{base, base.Add(2 * time.Minute), base.Add(32 * time.Minute)}

	measure := func() int64 {
		var tracker activeTimeTracker
		for _, ts := range stamps {
			tracker.observe(ts, false)
		}
		active, _ := tracker.durations()
		return active
	}

	withDataSettings(t, 10, nil)
	// 2 minutes + the 30-minute gap capped at 10.
	if got, want := measure(), (12 * time.Minute).Milliseconds(); got != want {
		t.Errorf("active duration at a 10-minute threshold = %d ms, want %d", got, want)
	}

	withDataSettings(t, 45, nil)
	// Nothing is capped now: the whole 32 minutes is one sitting.
	if got, want := measure(), (32 * time.Minute).Milliseconds(); got != want {
		t.Errorf("active duration at a 45-minute threshold = %d ms, want %d", got, want)
	}
}

func TestVisibleSessions_DropsHiddenProjects(t *testing.T) {
	sessions := []ClaudeSessionSummary{
		{SessionID: "a", ProjectPath: "/home/me/work"},
		{SessionID: "b", ProjectPath: "/home/me/scratch"},
		{SessionID: "c", ProjectPath: "/home/me/work"},
	}

	withDataSettings(t, 0, nil)
	if got := len(VisibleSessions(sessions)); got != 3 {
		t.Errorf("with nothing hidden, got %d sessions, want 3", got)
	}

	withDataSettings(t, 0, []string{"/home/me/scratch"})
	visible := VisibleSessions(sessions)
	if len(visible) != 2 {
		t.Fatalf("got %d visible sessions, want 2", len(visible))
	}
	for _, s := range visible {
		if s.ProjectPath == "/home/me/scratch" {
			t.Errorf("session %s belongs to a hidden project", s.SessionID)
		}
	}
	if !IsProjectHidden("/home/me/scratch") {
		t.Error("IsProjectHidden = false for a hidden project")
	}
	if IsProjectHidden("/home/me/work") {
		t.Error("IsProjectHidden = true for a visible project")
	}
	if got := HiddenProjects(); len(got) != 1 || got[0] != "/home/me/scratch" {
		t.Errorf("HiddenProjects() = %v, want [/home/me/scratch]", got)
	}
}

// TestCacheList_ExcludesHiddenProjects checks the filter where it actually
// matters: Cache.List is the single read every consumer starts from — the
// sessions list, the analytics endpoint and the insights summary — so a
// project hidden there is hidden everywhere.
func TestCacheList_ExcludesHiddenProjects(t *testing.T) {
	db := setupTestDB(t)
	home := t.TempDir()
	t.Setenv("HOME", home)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	kept := filepath.Join(home, ".claude", "projects", "-home-me-kept")
	dropped := filepath.Join(home, ".claude", "projects", "-home-me-dropped")
	writeJSONL(t, kept, "session-kept", ts)
	writeJSONL(t, dropped, "session-dropped", ts.Add(time.Hour))

	cache := NewCache(db, testLogger)
	withDataSettings(t, 0, nil)
	if got := len(cache.List()); got != 2 {
		t.Fatalf("with nothing hidden, List returned %d sessions, want 2", got)
	}

	withDataSettings(t, 0, []string{DecodeProjectPath("-home-me-dropped")})
	sessions := cache.List()
	if len(sessions) != 1 {
		t.Fatalf("List returned %d sessions, want 1", len(sessions))
	}
	if sessions[0].SessionID != "session-kept" {
		t.Errorf("List returned %q, want session-kept", sessions[0].SessionID)
	}

	// Hidden is not deleted: the row is still cached, so unhiding is immediate
	// and costs no re-read.
	withDataSettings(t, 0, nil)
	if got := len(cache.List()); got != 2 {
		t.Errorf("after unhiding, List returned %d sessions, want 2", got)
	}
}

// TestIncrementalScan_IdleThresholdChangeForcesReread covers the invalidation
// the stored durations depend on. No transcript mtime changes because a user
// moved a slider, so without this the cached active_duration_ms would keep
// answering under the old definition forever.
func TestIncrementalScan_IdleThresholdChangeForcesReread(t *testing.T) {
	db := setupTestDB(t)
	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeGappedJSONL(t, projectDir, "session-gap", ts)

	withDataSettings(t, 10, nil)
	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("first scan: %v", err)
	}
	first := findSession(t, sessions, "session-gap").ActiveDurationMs
	if want := (10 * time.Minute).Milliseconds(); first != want {
		t.Fatalf("active duration at a 10-minute threshold = %d ms, want %d", first, want)
	}

	// An insight row that must be queued for reprocessing by the same change.
	ctx := context.Background()
	if _, err := db.ExecContext(ctx,
		`INSERT INTO session_insights (session_id, processor_version, scanned_at)
		 VALUES ('session-gap', ?, ?)`,
		CurrentProcessorVersion, ts.Format(time.RFC3339)); err != nil {
		t.Fatalf("seed insight row: %v", err)
	}

	withDataSettings(t, 45, nil)
	sessions, err = IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}
	second := findSession(t, sessions, "session-gap").ActiveDurationMs
	if want := (30 * time.Minute).Milliseconds(); second != want {
		t.Errorf("active duration at a 45-minute threshold = %d ms, want %d", second, want)
	}

	var version int
	if err := db.QueryRowContext(ctx,
		`SELECT processor_version FROM session_insights WHERE session_id = 'session-gap'`,
	).Scan(&version); err != nil {
		t.Fatalf("reading processor version: %v", err)
	}
	if version != 0 {
		t.Errorf("processor_version = %d, want 0 so the insight worker reprocesses it", version)
	}

	// The new threshold is recorded, so a third scan finds nothing stale.
	var storedMs int64
	if err := db.QueryRowContext(ctx,
		`SELECT idle_threshold_ms FROM claude_cache_metadata WHERE id = 1`,
	).Scan(&storedMs); err != nil {
		t.Fatalf("reading stored threshold: %v", err)
	}
	if want := (45 * time.Minute).Milliseconds(); storedMs != want {
		t.Errorf("stored idle_threshold_ms = %d, want %d", storedMs, want)
	}
	if _, stale := idleThresholdStaleness(db); stale {
		t.Error("threshold still reported as stale after being recorded")
	}
}

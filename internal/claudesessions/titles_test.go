package claudesessions

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// writeTitledJSONL writes a session transcript with a real user/assistant pair
// plus the given title events, appended in order. Title events carry no
// timestamp — matching what Claude Code actually writes.
func writeTitledJSONL(t *testing.T, dir, sessionID string, ts time.Time, customTitles, aiTitles []string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	var data []byte
	appendEvent := func(ev rawEvent) {
		b, err := json.Marshal(ev)
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		data = append(data, b...)
		data = append(data, '\n')
	}

	appendEvent(rawEvent{
		Type: "user", SessionID: sessionID, Timestamp: ts, CWD: "/tmp",
		Message: &rawMessage{Role: "user", Content: json.RawMessage(`"first user message"`)},
	})
	appendEvent(rawEvent{
		Type: "assistant", SessionID: sessionID, Timestamp: ts.Add(time.Minute),
		Message: &rawMessage{
			Role: "assistant", Model: "claude-opus-4-8",
			Content: json.RawMessage(`[{"type":"text","text":"ok"}]`),
			Usage:   &rawUsage{InputTokens: 1, OutputTokens: 1},
		},
	})
	// Title events are re-appended on every resume; later lines must win.
	for _, title := range customTitles {
		appendEvent(rawEvent{Type: "custom-title", SessionID: sessionID, CustomTitle: title})
	}
	for _, title := range aiTitles {
		appendEvent(rawEvent{Type: "ai-title", SessionID: sessionID, AITitle: title})
	}

	if err := os.WriteFile(filepath.Join(dir, sessionID+".jsonl"), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}
}

func titleProjectDir(t *testing.T) string {
	t.Helper()
	home := t.TempDir()
	t.Setenv("HOME", home)
	return filepath.Join(home, ".claude", "projects", "test-project")
}

// TestResolveDisplayTitle_Precedence walks all 16 combinations of the four
// sources being present or absent.
func TestResolveDisplayTitle_Precedence(t *testing.T) {
	const (
		custom  = "agento-rename"
		native  = "native-rename"
		ai      = "ai-generated"
		preview = "first message"
	)
	for i := range 16 {
		s := ClaudeSessionSummary{}
		if i&1 != 0 {
			s.CustomTitle = custom
		}
		if i&2 != 0 {
			s.NativeTitle = native
		}
		if i&4 != 0 {
			s.AITitle = ai
		}
		if i&8 != 0 {
			s.Preview = preview
		}

		want := ""
		switch {
		case s.CustomTitle != "":
			want = custom
		case s.NativeTitle != "":
			want = native
		case s.AITitle != "":
			want = ai
		case s.Preview != "":
			want = preview
		}

		if got := s.ResolveDisplayTitle(); got != want {
			t.Errorf("combination %04b: got %q, want %q", i, got, want)
		}
	}
}

// TestIncrementalScan_TitleEvents_LastWins covers the core parse: both event
// types are re-appended on resume, so the final occurrence is the title.
func TestIncrementalScan_TitleEvents_LastWins(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeTitledJSONL(t, projectDir, "session-titles", ts,
		[]string{"first rename", "second rename", "final rename"},
		[]string{"first ai title", "final ai title"},
	)

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	s := findSession(t, sessions, "session-titles")

	if s.NativeTitle != "final rename" {
		t.Errorf("native_title = %q, want the last custom-title line", s.NativeTitle)
	}
	if s.AITitle != "final ai title" {
		t.Errorf("ai_title = %q, want the last ai-title line", s.AITitle)
	}
	// No Agento rename, so the native title is what the UI should show.
	if s.DisplayTitle != "final rename" {
		t.Errorf("display_title = %q, want the native title", s.DisplayTitle)
	}
}

// TestIncrementalScan_TitleEventsDoNotAffectTimeRange is the trap the issue
// calls out: title events carry no timestamp, so counting them would drag
// start_time to the zero value.
func TestIncrementalScan_TitleEventsDoNotAffectTimeRange(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeTitledJSONL(t, projectDir, "session-times", ts,
		[]string{"a rename"}, []string{"an ai title"})

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	s := findSession(t, sessions, "session-times")

	if !s.StartTime.Equal(ts) {
		t.Errorf("start_time = %v, want the first user event at %v", s.StartTime, ts)
	}
	if !s.LastActivity.Equal(ts.Add(time.Minute)) {
		t.Errorf("last_activity = %v, want the assistant event", s.LastActivity)
	}
	// Also asserted at the unit level, so a future timestamp on a title event
	// cannot silently start counting.
	for _, title := range []string{"custom-title", "ai-title"} {
		if boundsSessionTimeRange(title) {
			t.Errorf("%q must not bound the session time range", title)
		}
	}
	// Everything else still bounds it. These all carry timestamps in real
	// transcripts, and an allowlist that omitted them would silently shrink
	// last_activity for existing sessions.
	for _, other := range []string{
		"user", "assistant", "system", "attachment",
		"pr-link", "queue-operation", "file-history-delta",
	} {
		if !boundsSessionTimeRange(other) {
			t.Errorf("%q should bound the time range", other)
		}
	}
}

// TestIncrementalScan_AITitleBeatsUselessPreview is the motivating case: the
// first user message is frequently an injected prompt, so the AI title wins.
func TestIncrementalScan_AITitleBeatsUselessPreview(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeTitledJSONL(t, projectDir, "session-ai", ts, nil, []string{"Add datetime filter to sessions list"})

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	s := findSession(t, sessions, "session-ai")

	if s.DisplayTitle != "Add datetime filter to sessions list" {
		t.Errorf("display_title = %q, want the ai title", s.DisplayTitle)
	}
	if s.Preview == "" {
		t.Error("preview should still be captured, just not preferred")
	}
}

// TestIncrementalScan_NoTitleEventsFallsBackToPreview keeps the pre-change
// behavior for transcripts Claude Code wrote before title events existed.
func TestIncrementalScan_NoTitleEventsFallsBackToPreview(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-plain", ts)

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	s := findSession(t, sessions, "session-plain")

	if s.NativeTitle != "" || s.AITitle != "" {
		t.Errorf("expected no titles, got native=%q ai=%q", s.NativeTitle, s.AITitle)
	}
	if s.DisplayTitle != s.Preview {
		t.Errorf("display_title = %q, want the preview %q", s.DisplayTitle, s.Preview)
	}
}

// TestIncrementalScan_AgentoTitleWinsButNativeStillRefreshes is the invariant
// the whole two-column design exists for: an Agento rename is never touched by
// a scan, while the native/AI columns underneath it keep tracking the file.
func TestIncrementalScan_AgentoTitleWinsButNativeStillRefreshes(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeTitledJSONL(t, projectDir, "session-both", ts, []string{"native v1"}, []string{"ai v1"})

	if _, err := IncrementalScan(db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	cache := NewCache(db, testLogger)
	if err := cache.UpdateCustomTitle("session-both", "Agento Override"); err != nil {
		t.Fatalf("UpdateCustomTitle: %v", err)
	}

	// The native and AI titles change in the file; the Agento rename must not.
	time.Sleep(10 * time.Millisecond)
	writeTitledJSONL(t, projectDir, "session-both", ts, []string{"native v2"}, []string{"ai v2"})

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}
	s := findSession(t, sessions, "session-both")

	if s.CustomTitle != "Agento Override" {
		t.Errorf("custom_title was overwritten by the scan: %q", s.CustomTitle)
	}
	if s.NativeTitle != "native v2" {
		t.Errorf("native_title was not refreshed: %q", s.NativeTitle)
	}
	if s.AITitle != "ai v2" {
		t.Errorf("ai_title was not refreshed: %q", s.AITitle)
	}
	if s.DisplayTitle != "Agento Override" {
		t.Errorf("display_title = %q, want the Agento override to win", s.DisplayTitle)
	}

	// Clearing the Agento title falls through to the native title.
	if err := cache.UpdateCustomTitle("session-both", ""); err != nil {
		t.Fatalf("clear title: %v", err)
	}
	cleared := cache.List()
	s = findSession(t, cleared, "session-both")
	if s.DisplayTitle != "native v2" {
		t.Errorf("after clearing the override, display_title = %q, want the native title", s.DisplayTitle)
	}
}

// TestIncrementalScan_ScannerVersionBackfillsTitles covers the upgrade path:
// rows written by an older reader have blank title columns and their files
// never changed, so only a scanner-version bump can backfill them.
func TestIncrementalScan_ScannerVersionBackfillsTitles(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeTitledJSONL(t, projectDir, "session-backfill", ts, nil, []string{"a recovered title"})

	if _, err := IncrementalScan(db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	// Simulate a pre-v2 row: titles blank, file untouched, version rewound.
	ctx := context.Background()
	if _, err := db.ExecContext(ctx,
		`UPDATE claude_session_cache SET native_title = '', ai_title = ''`); err != nil {
		t.Fatalf("blank titles: %v", err)
	}
	if _, err := db.ExecContext(ctx,
		`UPDATE claude_cache_metadata SET scanner_version = 0 WHERE id = 1`); err != nil {
		t.Fatalf("rewind version: %v", err)
	}

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("backfill scan: %v", err)
	}
	s := findSession(t, sessions, "session-backfill")

	if s.AITitle != "a recovered title" {
		t.Errorf("ai_title was not backfilled: %q", s.AITitle)
	}
	if s.DisplayTitle != "a recovered title" {
		t.Errorf("display_title = %q, want the backfilled ai title", s.DisplayTitle)
	}
}

// TestCache_GetTitles feeds the detail endpoint, which builds its summary from
// the JSONL message tree and so cannot collect the titles itself.
func TestCache_GetTitles(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeTitledJSONL(t, projectDir, "session-detail", ts, []string{"the rename"}, []string{"the ai title"})

	if _, err := IncrementalScan(db, testLogger); err != nil {
		t.Fatalf("scan: %v", err)
	}

	cache := NewCache(db, testLogger)
	native, ai := cache.GetTitles("session-detail")
	if native != "the rename" || ai != "the ai title" {
		t.Errorf("GetTitles = (%q, %q)", native, ai)
	}

	if n, a := cache.GetTitles("does-not-exist"); n != "" || a != "" {
		t.Errorf("GetTitles for an unknown session = (%q, %q), want empty", n, a)
	}
}

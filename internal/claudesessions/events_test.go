package claudesessions

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// eventsSessionID is the single session these fixtures build.
const eventsSessionID = "session-events"

// eventsFixture writes a transcript exercising every newly-read event type.
// The metadata events are emitted twice with different values, because Claude
// Code re-appends them on every resume and the last one must win.
func eventsFixture(t *testing.T, dir string, ts time.Time) {
	t.Helper()
	const sessionID = eventsSessionID
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
		Message: &rawMessage{Role: "user", Content: json.RawMessage(`"build the thing"`)},
	})
	appendEvent(rawEvent{
		Type: "assistant", SessionID: sessionID, Timestamp: ts.Add(time.Minute),
		Message: &rawMessage{
			Role: "assistant", Model: "claude-opus-4-8",
			Content: json.RawMessage(`[{"type":"text","text":"done"}]`),
			Usage:   &rawUsage{InputTokens: 5, OutputTokens: 5},
		},
	})

	// First pass of metadata events.
	appendEvent(rawEvent{Type: "agent-name", SessionID: sessionID, AgentName: "first-agent"})
	appendEvent(rawEvent{Type: "permission-mode", SessionID: sessionID, PermissionMode: "default"})
	appendEvent(rawEvent{Type: "mode", SessionID: sessionID, Mode: "plan"})
	appendEvent(rawEvent{Type: "relocated", SessionID: sessionID, RelocatedCWD: "/old/path"})
	appendEvent(rawEvent{Type: "worktree-state", SessionID: sessionID, WorktreeSession: &rawWorktreeSession{
		WorktreeName: "first-tree", WorktreeBranch: "wt-first", OriginalBranch: "develop",
	}})

	// Two compactions. cumulativeDroppedTokens is a running total, so the
	// session's figure is the larger value, not the sum.
	appendEvent(rawEvent{
		Type: "system", SessionID: sessionID, Subtype: "compact_boundary",
		Timestamp:       ts.Add(2 * time.Minute),
		CompactMetadata: &rawCompactMetadata{Trigger: "auto", PreTokens: 900, PostTokens: 100, CumulativeDroppedTokens: 800},
	})
	appendEvent(rawEvent{
		Type: "system", SessionID: sessionID, Subtype: "compact_boundary",
		Timestamp:       ts.Add(3 * time.Minute),
		CompactMetadata: &rawCompactMetadata{Trigger: "auto", PreTokens: 950, PostTokens: 120, CumulativeDroppedTokens: 1600},
	})
	// A system subtype that carries no compaction metadata must be ignored.
	appendEvent(rawEvent{
		Type: "system", SessionID: sessionID, Subtype: "away_summary", Timestamp: ts.Add(4 * time.Minute),
	})

	// The same PR linked twice, plus a second distinct PR.
	appendEvent(rawEvent{
		Type: "pr-link", SessionID: sessionID, Timestamp: ts.Add(5 * time.Minute),
		PRNumber: 171, PRUrl: "https://github.com/shaharia-lab/agento/pull/171",
		PRRepository: "shaharia-lab/agento",
	})
	appendEvent(rawEvent{
		Type: "pr-link", SessionID: sessionID, Timestamp: ts.Add(6 * time.Minute),
		PRNumber: 171, PRUrl: "https://github.com/shaharia-lab/agento/pull/171",
		PRRepository: "shaharia-lab/agento",
	})
	appendEvent(rawEvent{
		Type: "pr-link", SessionID: sessionID, Timestamp: ts.Add(7 * time.Minute),
		PRNumber: 172, PRUrl: "https://github.com/shaharia-lab/agento/pull/172",
		PRRepository: "shaharia-lab/agento",
	})

	// Second pass of metadata events — these are the values that must survive.
	appendEvent(rawEvent{Type: "agent-name", SessionID: sessionID, AgentName: "final-agent"})
	appendEvent(rawEvent{Type: "permission-mode", SessionID: sessionID, PermissionMode: "bypassPermissions"})
	appendEvent(rawEvent{Type: "mode", SessionID: sessionID, Mode: "acceptEdits"})
	appendEvent(rawEvent{Type: "relocated", SessionID: sessionID, RelocatedCWD: "/new/path"})
	appendEvent(rawEvent{Type: "worktree-state", SessionID: sessionID, WorktreeSession: &rawWorktreeSession{
		WorktreeName: "final-tree", WorktreeBranch: "wt-final", OriginalBranch: "main",
	}})

	if err := os.WriteFile(filepath.Join(dir, sessionID+".jsonl"), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}
}

// TestScan_PRLinksDeduplicated covers the first acceptance criterion: a PR
// linked repeatedly in one file yields exactly one row.
func TestScan_PRLinksDeduplicated(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	eventsFixture(t, projectDir, time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, eventsSessionID)

	if len(s.PRs) != 2 {
		t.Fatalf("expected 2 distinct PRs, got %d: %+v", len(s.PRs), s.PRs)
	}
	first := s.PRs[0]
	if first.PRNumber != 171 {
		t.Errorf("pr_number = %d, want 171", first.PRNumber)
	}
	if first.PRURL != "https://github.com/shaharia-lab/agento/pull/171" {
		t.Errorf("pr_url = %q", first.PRURL)
	}
	if first.PRRepository != "shaharia-lab/agento" {
		t.Errorf("pr_repository = %q, want shaharia-lab/agento", first.PRRepository)
	}

	// Assert the in-memory dedupe too: the unique index on claude_session_pr
	// would collapse duplicates on its own, so reading only through the DB
	// would not prove addSummaryPRLink does its job.
	raw, _, err := readSessionSummary(
		eventsSessionID, projectDir,
		filepath.Join(projectDir, eventsSessionID+".jsonl"), testLogger,
	)
	if err != nil {
		t.Fatalf("read summary: %v", err)
	}
	if len(raw.PRs) != 2 {
		t.Errorf("scanner produced %d PRs before persistence, want 2 — dedupe is not happening in Go", len(raw.PRs))
	}
}

// TestScan_CompactionCounters covers the second acceptance criterion.
func TestScan_CompactionCounters(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	eventsFixture(t, projectDir, time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, eventsSessionID)

	if s.CompactionCount != 2 {
		t.Errorf("compaction_count = %d, want 2 (the away_summary system event must not count)", s.CompactionCount)
	}
	if s.DroppedTokens != 1600 {
		t.Errorf("dropped_tokens = %d, want 1600 — the highest cumulative total, not the sum", s.DroppedTokens)
	}
}

// TestScan_MetadataEventsLastWins covers the third acceptance criterion: every
// metadata event is re-appended on resume, so the final occurrence is the one
// that describes the session.
func TestScan_MetadataEventsLastWins(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	eventsFixture(t, projectDir, time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, eventsSessionID)

	for _, tc := range []struct{ field, got, want string }{
		{"agent_name", s.AgentName, "final-agent"},
		{"permission_mode", s.PermissionMode, "bypassPermissions"},
		{"mode", s.Mode, "acceptEdits"},
		{"relocated_cwd", s.RelocatedCWD, "/new/path"},
		{"worktree_name", s.WorktreeName, "final-tree"},
		{"worktree_branch", s.WorktreeBranch, "wt-final"},
		{"original_branch", s.OriginalBranch, "main"},
	} {
		if tc.got != tc.want {
			t.Errorf("%s = %q, want %q (last occurrence must win)", tc.field, tc.got, tc.want)
		}
	}
}

// TestScan_PRLinkDoesNotExtendActivity covers the fourth acceptance criterion.
// The fixture's pr-link events are deliberately timestamped after every
// conversation event, so a regression here is immediately visible.
func TestScan_PRLinkDoesNotExtendActivity(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	eventsFixture(t, projectDir, ts)

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, eventsSessionID)

	// The last event that may bound the range is the away_summary system event
	// at ts+4m; the pr-link events at ts+5m..+7m must not count.
	want := ts.Add(4 * time.Minute)
	if !s.LastActivity.Equal(want) {
		t.Errorf("last_activity = %s, want %s — pr-link must not extend the activity range",
			s.LastActivity, want)
	}
	if !s.StartTime.Equal(ts) {
		t.Errorf("start_time = %s, want %s", s.StartTime, ts)
	}

	// The detail path must agree with the summary, since it applies the same denylist.
	detail, err := readSessionDetail(
		eventsSessionID, projectDir,
		filepath.Join(projectDir, eventsSessionID+".jsonl"), testLogger,
	)
	if err != nil {
		t.Fatalf("detail: %v", err)
	}
	if !detail.LastActivity.Equal(s.LastActivity) {
		t.Errorf("detail last_activity = %s, summary = %s — the two paths disagree",
			detail.LastActivity, s.LastActivity)
	}
}

// TestIncrementalScan_ScannerVersionBackfillsEvents covers the last acceptance
// criterion: existing rows predate these columns and their files never change,
// so only the scanner-version bump can populate them.
func TestIncrementalScan_ScannerVersionBackfillsEvents(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	eventsFixture(t, projectDir, time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	if _, err := IncrementalScan(db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	// Simulate a pre-v4 row: new columns blank, PR rows absent, file untouched.
	ctx := context.Background()
	if _, err := db.ExecContext(ctx, `
		UPDATE claude_session_cache SET
			agent_name = '', permission_mode = '', mode = '', relocated_cwd = '',
			worktree_name = '', worktree_branch = '', original_branch = '',
			compaction_count = 0, dropped_tokens = 0`); err != nil {
		t.Fatalf("blank columns: %v", err)
	}
	if _, err := db.ExecContext(ctx, `DELETE FROM claude_session_pr`); err != nil {
		t.Fatalf("delete prs: %v", err)
	}
	if _, err := db.ExecContext(ctx,
		`UPDATE claude_cache_metadata SET scanner_version = 3 WHERE id = 1`); err != nil {
		t.Fatalf("rewind version: %v", err)
	}

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("backfill scan: %v", err)
	}
	s := findSession(t, sessions, eventsSessionID)

	if s.PermissionMode != "bypassPermissions" {
		t.Errorf("permission_mode was not backfilled: %q", s.PermissionMode)
	}
	if s.CompactionCount != 2 {
		t.Errorf("compaction_count was not backfilled: %d", s.CompactionCount)
	}
	if len(s.PRs) != 2 {
		t.Errorf("linked PRs were not backfilled: %d rows", len(s.PRs))
	}
}

// TestScan_RescanPreservesUserFieldsAndRefreshesEvents guards the split between
// user-owned columns (kept) and transcript-derived ones (refreshed).
func TestScan_RescanPreservesUserFieldsAndRefreshesEvents(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	eventsFixture(t, projectDir, ts)

	if _, err := IncrementalScan(db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	cache := NewCache(db, testLogger)
	if err := cache.UpdateCustomTitle(eventsSessionID, "my label"); err != nil {
		t.Fatalf("set title: %v", err)
	}
	if err := cache.UpdateFavorite(eventsSessionID, true); err != nil {
		t.Fatalf("set favorite: %v", err)
	}

	// Force a re-read the way a scanner-version bump would.
	if _, err := db.ExecContext(context.Background(),
		`UPDATE claude_cache_metadata SET scanner_version = 0 WHERE id = 1`); err != nil {
		t.Fatalf("rewind version: %v", err)
	}
	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("rescan: %v", err)
	}
	s := findSession(t, sessions, eventsSessionID)

	if s.CustomTitle != "my label" {
		t.Errorf("custom_title = %q, want it preserved across the rescan", s.CustomTitle)
	}
	if !s.IsFavorite {
		t.Error("is_favorite was lost across the rescan")
	}
	if s.PermissionMode != "bypassPermissions" {
		t.Errorf("permission_mode = %q, want it refreshed from the transcript", s.PermissionMode)
	}
	if len(s.PRs) != 2 {
		t.Errorf("linked PRs = %d, want 2 after the rescan", len(s.PRs))
	}
}

// TestDetail_CollectsSessionMetadata is the regression guard for the detail
// view: it builds a message tree rather than a summary, so it has to collect
// these fields itself. Without this the detail page's worktree, permission-mode
// and compaction badges render for no session at all.
func TestDetail_CollectsSessionMetadata(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	eventsFixture(t, projectDir, time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	summary := findSession(t, sessions, eventsSessionID)

	detail, err := readSessionDetail(
		eventsSessionID, projectDir,
		filepath.Join(projectDir, eventsSessionID+".jsonl"), testLogger,
	)
	if err != nil {
		t.Fatalf("detail: %v", err)
	}

	for _, tc := range []struct{ field, got, want string }{
		{"agent_name", detail.AgentName, summary.AgentName},
		{"permission_mode", detail.PermissionMode, summary.PermissionMode},
		{"mode", detail.Mode, summary.Mode},
		{"relocated_cwd", detail.RelocatedCWD, summary.RelocatedCWD},
		{"worktree_name", detail.WorktreeName, summary.WorktreeName},
		{"worktree_branch", detail.WorktreeBranch, summary.WorktreeBranch},
		{"original_branch", detail.OriginalBranch, summary.OriginalBranch},
	} {
		if tc.got != tc.want {
			t.Errorf("detail %s = %q, summary = %q — the two paths disagree", tc.field, tc.got, tc.want)
		}
		if tc.got == "" {
			t.Errorf("detail %s is empty — the detail reader is not collecting it", tc.field)
		}
	}

	if detail.CompactionCount != summary.CompactionCount {
		t.Errorf("detail compaction_count = %d, summary = %d", detail.CompactionCount, summary.CompactionCount)
	}
	if detail.DroppedTokens != summary.DroppedTokens {
		t.Errorf("detail dropped_tokens = %d, summary = %d", detail.DroppedTokens, summary.DroppedTokens)
	}
	if len(detail.PRs) != len(summary.PRs) {
		t.Errorf("detail has %d PRs, summary has %d", len(detail.PRs), len(summary.PRs))
	}
}

// TestScan_CompactionWithoutCumulativeTotal covers the older Claude Code
// releases that report preTokens/postTokens but no cumulativeDroppedTokens.
// Reporting zero there would state a plainly wrong number in the UI.
func TestScan_CompactionWithoutCumulativeTotal(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	if err := os.MkdirAll(projectDir, 0750); err != nil {
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
		Type: "user", SessionID: "session-nocum", Timestamp: ts, CWD: "/tmp",
		Message: &rawMessage{Role: "user", Content: json.RawMessage(`"go"`)},
	})
	appendEvent(rawEvent{
		Type: "system", SessionID: "session-nocum", Subtype: "compact_boundary",
		Timestamp: ts.Add(time.Minute),
		// No CumulativeDroppedTokens, exactly as Claude Code 2.1.186 writes it.
		CompactMetadata: &rawCompactMetadata{Trigger: "auto", PreTokens: 1000563, PostTokens: 26087},
	})
	if err := os.WriteFile(filepath.Join(projectDir, "session-nocum.jsonl"), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, "session-nocum")

	if s.CompactionCount != 1 {
		t.Errorf("compaction_count = %d, want 1", s.CompactionCount)
	}
	if want := 1000563 - 26087; s.DroppedTokens != want {
		t.Errorf("dropped_tokens = %d, want %d derived from pre/post", s.DroppedTokens, want)
	}
}

// TestScan_DeletedSessionRemovesPRRows guards the cleanup path: claude_session_pr
// has no foreign key onto the session, and attachPRs reads the whole table on
// every list, so orphans would accumulate forever and follow a recycled ID.
func TestScan_DeletedSessionRemovesPRRows(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	eventsFixture(t, projectDir, time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))
	if _, err := IncrementalScan(db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	countPRs := func() int {
		t.Helper()
		var n int
		if err := db.QueryRowContext(context.Background(),
			`SELECT COUNT(*) FROM claude_session_pr`).Scan(&n); err != nil {
			t.Fatalf("count prs: %v", err)
		}
		return n
	}
	if got := countPRs(); got != 2 {
		t.Fatalf("expected 2 PR rows after the first scan, got %d", got)
	}

	// The transcript disappears — the session and everything hanging off it go.
	if err := os.Remove(filepath.Join(projectDir, eventsSessionID+".jsonl")); err != nil {
		t.Fatalf("remove transcript: %v", err)
	}
	if _, err := IncrementalScan(db, testLogger); err != nil {
		t.Fatalf("rescan: %v", err)
	}

	if got := countPRs(); got != 0 {
		t.Errorf("%d PR rows survived the session's deletion, want 0", got)
	}
}

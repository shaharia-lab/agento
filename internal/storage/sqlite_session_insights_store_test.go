package storage_test

import (
	"context"
	"fmt"
	"log/slog"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/storage"
)

func setupInsightsTestDB(t *testing.T) *storage.SQLiteSessionInsightsStore {
	t.Helper()
	dbPath := filepath.Join(t.TempDir(), "test.db")
	db, _, err := storage.NewSQLiteDB(dbPath, slog.Default())
	if err != nil {
		t.Fatalf("failed to create test db: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return storage.NewSQLiteSessionInsightsStore(db)
}

func sampleRecord(sessionID string) storage.InsightRecord {
	return storage.InsightRecord{
		SessionID:               sessionID,
		ProcessorVersion:        1,
		ScannedAt:               time.Now().UTC().Truncate(time.Second),
		TurnCount:               3,
		StepsPerTurnAvg:         5.0,
		AutonomyScore:           72.5,
		ToolCallsTotal:          15,
		ToolBreakdown:           map[string]int{"bash": 10, "read": 5},
		ToolErrorRate:           0.1,
		TotalDurationMs:         60000,
		ThinkingTimeMs:          5000,
		CacheHitRate:            0.8,
		TokensPerTurnAvg:        250.0,
		CostEstimateUSD:         0.0045,
		ToolErrorCount:          2,
		HasErrors:               true,
		MaxConsecutiveToolCalls: 5,
		LongestAutonomousChain:  12,
		AvgUserResponseTimeMs:   3000.0,
		AvgClaudeResponseTimeMs: 500.0,
		SessionType:             "",
	}
}

func TestSQLiteSessionInsightsStore_UpsertAndGet(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	r := sampleRecord("session-1")
	if err := store.Upsert(ctx, r); err != nil {
		t.Fatalf("Upsert failed: %v", err)
	}

	got, err := store.Get(ctx, "session-1")
	if err != nil {
		t.Fatalf("Get failed: %v", err)
	}
	if got == nil {
		t.Fatal("expected non-nil record")
	}
	if got.SessionID != r.SessionID {
		t.Errorf("session_id mismatch: got %q, want %q", got.SessionID, r.SessionID)
	}
	if got.TurnCount != r.TurnCount {
		t.Errorf("turn_count mismatch: got %d, want %d", got.TurnCount, r.TurnCount)
	}
	if got.AutonomyScore != r.AutonomyScore {
		t.Errorf("autonomy_score mismatch: got %f, want %f", got.AutonomyScore, r.AutonomyScore)
	}
	if got.ToolBreakdown["bash"] != 10 {
		t.Errorf("tool_breakdown bash mismatch: got %d, want 10", got.ToolBreakdown["bash"])
	}
	if !got.HasErrors {
		t.Error("expected has_errors=true")
	}
	if got.ProcessorVersion != 1 {
		t.Errorf("processor_version mismatch: got %d, want 1", got.ProcessorVersion)
	}
}

func TestSQLiteSessionInsightsStore_GetNotFound(t *testing.T) {
	store := setupInsightsTestDB(t)
	got, err := store.Get(context.Background(), "nonexistent")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got != nil {
		t.Errorf("expected nil for missing session, got %+v", got)
	}
}

func TestSQLiteSessionInsightsStore_UpsertUpdatesExisting(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	r := sampleRecord("session-update")
	if err := store.Upsert(ctx, r); err != nil {
		t.Fatal(err)
	}

	// Update the record.
	r.TurnCount = 99
	r.AutonomyScore = 42.0
	if err := store.Upsert(ctx, r); err != nil {
		t.Fatal(err)
	}

	got, err := store.Get(ctx, "session-update")
	if err != nil {
		t.Fatal(err)
	}
	if got.TurnCount != 99 {
		t.Errorf("expected updated turn_count=99, got %d", got.TurnCount)
	}
	if got.AutonomyScore != 42.0 {
		t.Errorf("expected updated autonomy_score=42.0, got %f", got.AutonomyScore)
	}
}

func TestSQLiteSessionInsightsStore_GetMany(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	for _, id := range []string{"s1", "s2", "s3"} {
		r := sampleRecord(id)
		if err := store.Upsert(ctx, r); err != nil {
			t.Fatalf("Upsert %s: %v", id, err)
		}
	}

	results, err := store.GetMany(ctx, []string{"s1", "s3"})
	if err != nil {
		t.Fatal(err)
	}
	if len(results) != 2 {
		t.Errorf("expected 2 results, got %d", len(results))
	}
}

// TestGetMany_LargeIDSet covers the binding GetMany now shares with
// GetAggregateSummary: one placeholder per ID would exceed SQLite's variable
// limit at this size.
func TestGetMany_LargeIDSet(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	const n = 2500
	ids := make([]string, 0, n)
	for i := range n {
		id := fmt.Sprintf("many-%04d", i)
		ids = append(ids, id)
		if err := store.Upsert(ctx, sampleRecord(id)); err != nil {
			t.Fatal(err)
		}
	}

	results, err := store.GetMany(ctx, ids)
	if err != nil {
		t.Fatalf("fetching %d records: %v", n, err)
	}
	if len(results) != n {
		t.Errorf("got %d records, want %d", len(results), n)
	}
}

func TestSQLiteSessionInsightsStore_GetManyEmpty(t *testing.T) {
	store := setupInsightsTestDB(t)
	results, err := store.GetMany(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(results) != 0 {
		t.Errorf("expected 0 results for empty IDs, got %d", len(results))
	}
}

func TestSQLiteSessionInsightsStore_GetAggregateSummary(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	for _, id := range []string{"a1", "a2"} {
		if err := store.Upsert(ctx, sampleRecord(id)); err != nil {
			t.Fatal(err)
		}
	}

	summary, err := store.GetAggregateSummary(ctx, []string{"a1", "a2"})
	if err != nil {
		t.Fatal(err)
	}
	if summary.TotalSessions != 2 {
		t.Errorf("expected TotalSessions=2, got %d", summary.TotalSessions)
	}

	// Filtered to one session.
	filtered, err := store.GetAggregateSummary(ctx, []string{"a1"})
	if err != nil {
		t.Fatal(err)
	}
	if filtered.TotalSessions != 1 {
		t.Errorf("expected TotalSessions=1 when filtering, got %d", filtered.TotalSessions)
	}
}

// TestGetAggregateSummary_EmptySetIsEmptyNotEverything pins the contract that
// makes windowing safe to do outside SQL: the ID set is complete, so a window
// that matched nothing must aggregate nothing. Treating empty as "no filter"
// would turn an empty range into a full-corpus total — the most misleading
// possible answer, and silently plausible.
func TestGetAggregateSummary_EmptySetIsEmptyNotEverything(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	for _, id := range []string{"a1", "a2"} {
		if err := store.Upsert(ctx, sampleRecord(id)); err != nil {
			t.Fatal(err)
		}
	}

	for name, ids := range map[string][]string{"nil": nil, "empty": {}} {
		summary, err := store.GetAggregateSummary(ctx, ids)
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if summary.TotalSessions != 0 {
			t.Errorf("%s id set: TotalSessions = %d, want 0", name, summary.TotalSessions)
		}
	}
}

// TestGetAggregateSummary_LargeIDSet covers the corpora this feature exists for.
// The set travels as one JSON parameter precisely so a few thousand sessions in
// a window do not exceed SQLite's bound-variable limit, which a placeholder per
// ID would.
func TestGetAggregateSummary_LargeIDSet(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	const n = 2500
	ids := make([]string, 0, n)
	for i := range n {
		id := fmt.Sprintf("session-%04d", i)
		ids = append(ids, id)
		if err := store.Upsert(ctx, sampleRecord(id)); err != nil {
			t.Fatal(err)
		}
	}

	summary, err := store.GetAggregateSummary(ctx, ids)
	if err != nil {
		t.Fatalf("aggregating %d sessions: %v", n, err)
	}
	if summary.TotalSessions != n {
		t.Errorf("TotalSessions = %d, want %d", summary.TotalSessions, n)
	}
}

func TestSQLiteSessionInsightsStore_NeedsProcessing(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "test.db")
	db, _, err := storage.NewSQLiteDB(dbPath, slog.Default())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = db.Close() })

	// Insert a cache entry directly.
	_, err = db.ExecContext(context.Background(), `
		INSERT INTO claude_session_cache (
			session_id, project_path, file_path, file_mtime,
			start_time, last_activity
		) VALUES ('cached-session', '/proj', '/proj/cached-session.jsonl', ?, ?, ?)`,
		time.Now(), time.Now(), time.Now(),
	)
	if err != nil {
		t.Fatalf("inserting cache row: %v", err)
	}

	store := storage.NewSQLiteSessionInsightsStore(db)
	ctx := context.Background()

	// Without any insight, it should need processing.
	ids, err := store.NeedsProcessing(ctx, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(ids) != 1 || ids[0].SessionID != "cached-session" {
		t.Errorf("expected ['cached-session'], got %v", ids)
	}

	// Insert an insight with version 1.
	r := sampleRecord("cached-session")
	r.ProcessorVersion = 1
	if err := store.Upsert(ctx, r); err != nil {
		t.Fatal(err)
	}

	// Now it should NOT need processing at version 1.
	ids, err = store.NeedsProcessing(ctx, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(ids) != 0 {
		t.Errorf("expected empty, got %v", ids)
	}

	// But it should need processing at version 2.
	ids, err = store.NeedsProcessing(ctx, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(ids) != 1 {
		t.Errorf("expected 1 session needing version-2 processing, got %v", ids)
	}
}

// TestInsightRecord_AttributionRoundTrip pins the new breakdown columns through
// a real upsert and read-back. They are positional in both the INSERT and the
// Scan, so a misalignment would be silent.
func TestInsightRecord_AttributionRoundTrip(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	r := sampleRecord("attr-session")
	r.SkillBreakdown = map[string]int{"lab-workflow:review-pr": 10, "vibexp:prime": 3}
	r.PluginBreakdown = map[string]int{"lab-workflow": 10}
	r.McpServerBreakdown = map[string]int{"vibexp_io_vibexp_team": 4}
	r.McpToolBreakdown = map[string]int{"vibexp_io_post_to_feed": 4}
	r.EffortBreakdown = map[string]int{"high": 13}
	r.AgentBreakdown = map[string]int{"Explore": 6, "general-purpose": 2}
	r.UnattributedCalls = 7

	if err := store.Upsert(ctx, r); err != nil {
		t.Fatalf("upsert: %v", err)
	}
	got, err := store.Get(ctx, "attr-session")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got == nil {
		t.Fatal("expected a record")
	}

	if got.SkillBreakdown["lab-workflow:review-pr"] != 10 || got.SkillBreakdown["vibexp:prime"] != 3 {
		t.Errorf("skill_breakdown round-trip failed: %+v", got.SkillBreakdown)
	}
	if got.PluginBreakdown["lab-workflow"] != 10 {
		t.Errorf("plugin_breakdown round-trip failed: %+v", got.PluginBreakdown)
	}
	if got.McpServerBreakdown["vibexp_io_vibexp_team"] != 4 {
		t.Errorf("mcp_server_breakdown round-trip failed: %+v", got.McpServerBreakdown)
	}
	if got.McpToolBreakdown["vibexp_io_post_to_feed"] != 4 {
		t.Errorf("mcp_tool_breakdown round-trip failed: %+v", got.McpToolBreakdown)
	}
	if got.EffortBreakdown["high"] != 13 {
		t.Errorf("effort_breakdown round-trip failed: %+v", got.EffortBreakdown)
	}
	if got.AgentBreakdown["Explore"] != 6 || got.AgentBreakdown["general-purpose"] != 2 {
		t.Errorf("agent_breakdown round-trip failed: %+v", got.AgentBreakdown)
	}
	if got.UnattributedCalls != 7 {
		t.Errorf("unattributed_calls = %d, want 7", got.UnattributedCalls)
	}
	// The pre-existing columns must still land in the right places.
	if got.ToolCallsTotal != r.ToolCallsTotal || got.ToolBreakdown["bash"] != 10 {
		t.Errorf("existing columns shifted: tool_calls_total=%d breakdown=%+v",
			got.ToolCallsTotal, got.ToolBreakdown)
	}
}

// TestInsightRecord_NilBreakdownsStoreAsEmpty covers a record written before
// the attribution processor ran: the columns default to '{}' and must read back
// without error.
func TestInsightRecord_NilBreakdownsStoreAsEmpty(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	r := sampleRecord("nil-attr-session") // leaves every attribution map nil
	if err := store.Upsert(ctx, r); err != nil {
		t.Fatalf("upsert: %v", err)
	}
	got, err := store.Get(ctx, "nil-attr-session")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if len(got.SkillBreakdown) != 0 || len(got.EffortBreakdown) != 0 {
		t.Errorf("expected empty breakdowns, got %+v / %+v", got.SkillBreakdown, got.EffortBreakdown)
	}
	if got.UnattributedCalls != 0 {
		t.Errorf("unattributed_calls = %d, want 0", got.UnattributedCalls)
	}
}

// TestGetAggregateSummary_BreakdownColumnsNotCrossWired guards the column→field
// table in GetAggregateSummary. The four breakdowns are fetched by name into
// separate slices, so a swapped pair would permanently label the "Top Skills"
// panel with plugin names — with a fully green CI, since nothing else reads
// these slices.
func TestGetAggregateSummary_BreakdownColumnsNotCrossWired(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	// Deliberately disjoint key sets, so any cross-wiring is unambiguous.
	r := sampleRecord("wiring")
	r.ToolBreakdown = map[string]int{"TOOL_KEY": 1}
	r.SkillBreakdown = map[string]int{"SKILL_KEY": 2}
	r.PluginBreakdown = map[string]int{"PLUGIN_KEY": 3}
	r.McpServerBreakdown = map[string]int{"SERVER_KEY": 4}
	if err := store.Upsert(ctx, r); err != nil {
		t.Fatal(err)
	}

	summary, err := store.GetAggregateSummary(ctx, []string{"wiring"})
	if err != nil {
		t.Fatal(err)
	}

	for _, tc := range []struct {
		field string
		blobs []string
		want  string
	}{
		{"ToolBreakdowns", summary.ToolBreakdowns, "TOOL_KEY"},
		{"SkillBreakdowns", summary.SkillBreakdowns, "SKILL_KEY"},
		{"PluginBreakdowns", summary.PluginBreakdowns, "PLUGIN_KEY"},
		{"McpServerBreakdowns", summary.McpServerBreakdowns, "SERVER_KEY"},
	} {
		if len(tc.blobs) != 1 {
			t.Errorf("%s: got %d blobs, want 1", tc.field, len(tc.blobs))
			continue
		}
		if !strings.Contains(tc.blobs[0], tc.want) {
			t.Errorf("%s = %s, want it to contain %q — the columns are cross-wired",
				tc.field, tc.blobs[0], tc.want)
		}
	}
}

// TestGetAggregateSummary_BreakdownsFilteredPerColumn pins the one behavior a
// single-scan rewrite can silently break: the four columns are filtered
// independently, so a row with tools but no skills must contribute to
// ToolBreakdowns alone. Discarding such a row wholesale would drop real data
// from the merged totals with every other test still green.
func TestGetAggregateSummary_BreakdownsFilteredPerColumn(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	// Three rows, each empty in a different place; "empty" reaches SQLite as the
	// column's '{}' default because an empty map marshals to "{}".
	toolsOnly := sampleRecord("tools-only")
	toolsOnly.ToolBreakdown = map[string]int{"TOOL_KEY": 1}
	toolsOnly.SkillBreakdown = nil
	toolsOnly.PluginBreakdown = nil
	toolsOnly.McpServerBreakdown = nil
	toolsOnly.McpToolBreakdown = nil
	toolsOnly.EffortBreakdown = nil
	toolsOnly.AgentBreakdown = nil

	skillsAndServers := sampleRecord("skills-and-servers")
	skillsAndServers.ToolBreakdown = nil
	skillsAndServers.SkillBreakdown = map[string]int{"SKILL_KEY": 2}
	skillsAndServers.PluginBreakdown = map[string]int{}
	skillsAndServers.McpServerBreakdown = map[string]int{"SERVER_KEY": 4}
	skillsAndServers.McpToolBreakdown = map[string]int{"MCPTOOL_KEY": 5}
	skillsAndServers.EffortBreakdown = nil
	skillsAndServers.AgentBreakdown = map[string]int{"AGENT_KEY": 6}

	allEmpty := sampleRecord("all-empty")
	allEmpty.ToolBreakdown = nil
	allEmpty.SkillBreakdown = nil
	allEmpty.PluginBreakdown = nil
	allEmpty.McpServerBreakdown = nil
	allEmpty.McpToolBreakdown = nil
	allEmpty.EffortBreakdown = nil
	allEmpty.AgentBreakdown = nil

	for _, r := range []storage.InsightRecord{toolsOnly, skillsAndServers, allEmpty} {
		if err := store.Upsert(ctx, r); err != nil {
			t.Fatal(err)
		}
	}

	summary, err := store.GetAggregateSummary(ctx, []string{"tools-only", "skills-and-servers", "all-empty"})
	if err != nil {
		t.Fatal(err)
	}
	if summary.TotalSessions != 3 {
		t.Fatalf("TotalSessions = %d, want 3", summary.TotalSessions)
	}

	for _, tc := range []struct {
		field string
		blobs []string
		want  []string
	}{
		{"ToolBreakdowns", summary.ToolBreakdowns, []string{"TOOL_KEY"}},
		{"SkillBreakdowns", summary.SkillBreakdowns, []string{"SKILL_KEY"}},
		{"PluginBreakdowns", summary.PluginBreakdowns, nil},
		{"McpServerBreakdowns", summary.McpServerBreakdowns, []string{"SERVER_KEY"}},
		{"McpToolBreakdowns", summary.McpToolBreakdowns, []string{"MCPTOOL_KEY"}},
		{"EffortBreakdowns", summary.EffortBreakdowns, nil},
		{"AgentBreakdowns", summary.AgentBreakdowns, []string{"AGENT_KEY"}},
	} {
		if len(tc.blobs) != len(tc.want) {
			t.Errorf("%s: got %d blobs %v, want %d — empty columns must be skipped per column, not per row",
				tc.field, len(tc.blobs), tc.blobs, len(tc.want))
			continue
		}
		for i, want := range tc.want {
			if !strings.Contains(tc.blobs[i], want) {
				t.Errorf("%s[%d] = %s, want it to contain %q", tc.field, i, tc.blobs[i], want)
			}
		}
	}
}

// TestGetAggregateSummary_BreakdownColumnsScanAsPlainString pins the schema
// guarantee the single-scan query relies on: the breakdown columns are
// NOT NULL DEFAULT '{}', so a row written without them still scans into a
// plain string. Were any of them nullable, Scan would fail at runtime and only
// on real, pre-migration data.
func TestGetAggregateSummary_BreakdownColumnsScanAsPlainString(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "test.db")
	db, _, err := storage.NewSQLiteDB(dbPath, slog.Default())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = db.Close() })
	store := storage.NewSQLiteSessionInsightsStore(db)
	ctx := context.Background()

	// Insert bypassing Upsert so the breakdown columns fall to their defaults.
	if _, err := db.ExecContext(ctx, `
INSERT INTO session_insights (session_id, processor_version, scanned_at)
VALUES ('defaults-only', 1, '2026-01-01T00:00:00Z')`); err != nil {
		t.Fatal(err)
	}

	summary, err := store.GetAggregateSummary(ctx, []string{"defaults-only"})
	if err != nil {
		t.Fatalf("GetAggregateSummary: %v — breakdown columns must scan into string", err)
	}
	if summary.TotalSessions != 1 {
		t.Fatalf("TotalSessions = %d, want 1", summary.TotalSessions)
	}
	if len(summary.ToolBreakdowns)+len(summary.SkillBreakdowns)+
		len(summary.PluginBreakdowns)+len(summary.McpServerBreakdowns)+
		len(summary.McpToolBreakdowns)+len(summary.EffortBreakdowns)+
		len(summary.AgentBreakdowns) != 0 {
		t.Errorf("default '{}' columns must contribute nothing, got %+v", summary)
	}
}

// TestGetAggregateSummary_ToolCallTotals covers the denominator the skills
// panel needs: a breakdown without the unattributed share overstates how much
// of the work skills account for.
func TestGetAggregateSummary_ToolCallTotals(t *testing.T) {
	store := setupInsightsTestDB(t)
	ctx := context.Background()

	for i, id := range []string{"t1", "t2"} {
		r := sampleRecord(id)
		r.ToolCallsTotal = 10 * (i + 1) // 10, 20
		r.UnattributedCalls = 4 * (i + 1)
		if err := store.Upsert(ctx, r); err != nil {
			t.Fatal(err)
		}
	}

	summary, err := store.GetAggregateSummary(ctx, []string{"t1", "t2"})
	if err != nil {
		t.Fatal(err)
	}
	if summary.TotalToolCalls != 30 {
		t.Errorf("TotalToolCalls = %d, want 30", summary.TotalToolCalls)
	}
	if summary.UnattributedCalls != 12 {
		t.Errorf("UnattributedCalls = %d, want 12", summary.UnattributedCalls)
	}
}

// TestNeedsProcessing_ReturnsRowsBelowCurrentVersion ties the processor-version
// bump to reprocessing: a row one version behind must come back, which is what
// makes an upgrade backfill the new columns.
func TestNeedsProcessing_ReturnsRowsBelowCurrentVersion(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "test.db")
	db, _, err := storage.NewSQLiteDB(dbPath, slog.Default())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = db.Close() })
	store := storage.NewSQLiteSessionInsightsStore(db)
	ctx := context.Background()

	if _, err := db.ExecContext(ctx, `
		INSERT INTO claude_session_cache
			(session_id, project_path, file_path, file_mtime, start_time, last_activity)
		VALUES ('stale', '/p', '/p/stale.jsonl', '2025-01-01', '2025-01-01', '2025-01-01')`,
	); err != nil {
		t.Fatal(err)
	}

	const current = 4 // CurrentProcessorVersion at the time of writing
	r := sampleRecord("stale")
	r.ProcessorVersion = current - 1
	if err := store.Upsert(ctx, r); err != nil {
		t.Fatal(err)
	}

	pending, err := store.NeedsProcessing(ctx, current)
	if err != nil {
		t.Fatal(err)
	}
	if len(pending) != 1 || pending[0].SessionID != "stale" {
		t.Fatalf("expected the stale session to need processing, got %+v", pending)
	}

	// Once reprocessed at the current version it must drop out.
	r.ProcessorVersion = current
	if err := store.Upsert(ctx, r); err != nil {
		t.Fatal(err)
	}
	pending, err = store.NeedsProcessing(ctx, current)
	if err != nil {
		t.Fatal(err)
	}
	if len(pending) != 0 {
		t.Errorf("expected no pending sessions after reprocessing, got %+v", pending)
	}
}

// TestMigration20_AppliesToExistingDatabaseWithRows covers the upgrade path for
// agent_breakdown (#202) rather than just the fresh-schema path: an existing
// database with insight rows must gain the column with its '{}' default, so a
// pre-v6 row reads as "nothing attributed" until the processor-version bump
// reprocesses it. A nullable column here would fail Scan on real data only.
//
// The version-19 fixture is built forwards, by replaying the migration list up
// to 19, rather than by undoing everything above 19 on a current database. The
// backwards version needed an inverse statement per later migration and had to
// be hand-extended by every new one; worse, a forgotten inverse failed silently
// — MAX(version) stayed high, migration 20 was skipped, and the test passed
// asserting nothing. storage.ApplyMigrationsUpTo makes upgrade-path tests for
// any other migration cheap to add the same way.
func TestMigration20_AppliesToExistingDatabaseWithRows(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "test.db")
	db, err := storage.ApplyMigrationsUpTo(dbPath, 19)
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()

	// Written as raw SQL against the version-19 column list, not through
	// NewSQLiteSessionInsightsStore.Upsert: the store writes agent_breakdown,
	// which does not exist yet. Knowing the old schema here is inherent to
	// testing an upgrade — a row has to predate the column to prove it is
	// backfilled. Only the NOT NULL columns without a default are required;
	// the rest are left to their defaults exactly as a real old row would be.
	if _, err := db.ExecContext(ctx, `
		INSERT INTO session_insights
			(session_id, processor_version, scanned_at, turn_count, tool_calls_total, tool_breakdown, session_type)
		VALUES (?, ?, ?, ?, ?, ?, ?)`,
		"pre-migration", 1, time.Now().UTC().Format(time.RFC3339), 3, 4, `{"Read":4}`, "coding",
	); err != nil {
		t.Fatalf("inserting a row through the version-19 schema: %v", err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}

	// Re-open: migration 20 must re-apply over the existing row.
	db2, fresh, err := storage.NewSQLiteDB(dbPath, slog.Default())
	if err != nil {
		t.Fatalf("re-opening a version-19 database: %v", err)
	}
	t.Cleanup(func() { _ = db2.Close() })
	if fresh {
		t.Error("an existing database was reported as fresh")
	}

	// Compared against a freshly created database rather than a literal: what
	// this asserts is that the upgrade replayed all the way up to head, not
	// that head is any particular number. Pinning the number here made every
	// new migration fail this test for a reason unrelated to what it covers.
	var version, head int
	if err := db2.QueryRowContext(ctx,
		"SELECT MAX(version) FROM schema_migrations").Scan(&version); err != nil {
		t.Fatal(err)
	}
	freshDB, _, err := storage.NewSQLiteDB(filepath.Join(t.TempDir(), "head.db"), slog.Default())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = freshDB.Close() })
	if err := freshDB.QueryRowContext(ctx,
		"SELECT MAX(version) FROM schema_migrations").Scan(&head); err != nil {
		t.Fatal(err)
	}
	if version != head {
		t.Fatalf("schema version = %d, want %d — every migration from 20 up must re-apply", version, head)
	}

	got, err := storage.NewSQLiteSessionInsightsStore(db2).Get(ctx, "pre-migration")
	if err != nil {
		t.Fatalf("reading the pre-migration row: %v — agent_breakdown must scan as a string", err)
	}
	if got == nil {
		t.Fatal("the pre-migration row went missing")
	}
	// Empty, not nil: the column defaulted, and unmarshalCounts keeps the
	// empty convention so callers never special-case a backfilled row.
	if got.AgentBreakdown == nil {
		t.Error("AgentBreakdown is nil, want an empty map")
	}
	if len(got.AgentBreakdown) != 0 {
		t.Errorf("AgentBreakdown = %v, want empty — the column was dropped and re-added", got.AgentBreakdown)
	}
}

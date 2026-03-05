package storage_test

import (
	"context"
	"log/slog"
	"path/filepath"
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

	// All sessions (empty filter).
	summary, err := store.GetAggregateSummary(ctx, nil)
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

package claudesessions

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// costTestLogger keeps scan output quiet unless something actually breaks.
func costTestLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelWarn}))
}

// scanOneCostedSession writes a fixture session, scans it, and returns the
// summary as it comes back through the cache — i.e. after a full
// persist-and-reload round trip, which is the path the UI actually reads.
func scanOneCostedSession(
	t *testing.T, sessionID string,
	turns []struct {
		model string
		ts    time.Time
		usage rawUsage
	},
) ClaudeSessionSummary {
	t.Helper()
	db := setupTestDB(t)
	logger := costTestLogger()
	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")
	writeJSONLPricedTurns(t, projectDir, sessionID, turns)

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	return findSession(t, sessions, sessionID)
}

// TestSessionCost_PersistedBreakdownRoundTrips is the storage half of #188: the
// cost the scanner computes must survive the write to claude_session_cache and
// come back intact through the shared SELECT. A column added to the INSERT but
// forgotten in the SELECT (or scanned in the wrong position) fails here.
func TestSessionCost_PersistedBreakdownRoundTrips(t *testing.T) {
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	got := scanOneCostedSession(t, "session-cost", []struct {
		model string
		ts    time.Time
		usage rawUsage
	}{
		// 1M input @ $5 + 1M output @ $25 on Opus 4.8.
		{"claude-opus-4-8", ts, rawUsage{InputTokens: 1_000_000, OutputTokens: 1_000_000}},
	})

	assertUSD(t, "input", got.Cost.InputUSD, 5.00)
	assertUSD(t, "output", got.Cost.OutputUSD, 25.00)
	assertUSD(t, "total", got.Cost.TotalUSD, 30.00)

	// The total must equal the sum of its parts, or the badge and its tooltip
	// would disagree with each other.
	parts := got.Cost.InputUSD + got.Cost.OutputUSD + got.Cost.CacheReadUSD + got.Cost.CacheWriteUSD
	assertUSD(t, "parts sum to total", parts, got.Cost.TotalUSD)

	if len(got.UnpricedModels) != 0 {
		t.Errorf("unpriced models = %v, want none", got.UnpricedModels)
	}
}

// TestSessionCost_MultiModelPricedPerMessage is why the cost is stored rather
// than re-derived downstream: pricing this session's aggregate tokens at any
// single model gives the wrong answer, and only the scan sees each message's
// own model.
func TestSessionCost_MultiModelPricedPerMessage(t *testing.T) {
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	got := scanOneCostedSession(t, "session-mixed", []struct {
		model string
		ts    time.Time
		usage rawUsage
	}{
		{"claude-opus-5", ts, rawUsage{InputTokens: 1_000_000}},                     // $5
		{"claude-haiku-4-5", ts.Add(time.Minute), rawUsage{InputTokens: 1_000_000}}, // $1
	})

	assertUSD(t, "mixed-model total", got.Cost.TotalUSD, 6.00)
}

// TestSessionCost_UnpricedModelsRecorded covers the honest-reporting criterion:
// a session that used a model with no known rate stores the model name and its
// token count, so the UI can mark the total a floor instead of presenting an
// understated figure as complete.
func TestSessionCost_UnpricedModelsRecorded(t *testing.T) {
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	got := scanOneCostedSession(t, "session-unpriced", []struct {
		model string
		ts    time.Time
		usage rawUsage
	}{
		{"claude-opus-5", ts, rawUsage{InputTokens: 1_000_000}}, // $5, priced
		{"any", ts.Add(time.Minute), rawUsage{InputTokens: 400, OutputTokens: 100}},
	})

	assertUSD(t, "priced portion only", got.Cost.TotalUSD, 5.00)
	if len(got.UnpricedModels) != 1 || got.UnpricedModels[0] != "any" {
		t.Errorf("unpriced models = %v, want [any]", got.UnpricedModels)
	}
	if got.UnpricedTokens != 500 {
		t.Errorf("unpriced tokens = %d, want 500", got.UnpricedTokens)
	}
}

// TestSessionCost_NonBillableIsNotAGap separates the two zeros one more time,
// this time through storage: a synthetic-only session costs $0.00 and reports
// no unpriced models, so the UI shows a confident zero rather than a caveat.
func TestSessionCost_NonBillableIsNotAGap(t *testing.T) {
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	got := scanOneCostedSession(t, "session-synthetic", []struct {
		model string
		ts    time.Time
		usage rawUsage
	}{
		{syntheticModel, ts, rawUsage{InputTokens: 1_000}},
	})

	assertUSD(t, "synthetic total", got.Cost.TotalUSD, 0)
	if len(got.UnpricedModels) != 0 {
		t.Errorf("unpriced models = %v, want none — <synthetic> is priced at zero, not unpriced",
			got.UnpricedModels)
	}
}

// TestSessionCost_SubagentCostRolledUpSeparately mirrors how sub-agent tokens
// are reported: main-thread cost stays in Cost, delegated cost lands in
// SubagentCost, and TotalCost sums them. Folding delegated spend into Cost
// would silently change what the existing per-session number means.
func TestSessionCost_SubagentCostRolledUpSeparately(t *testing.T) {
	db := setupTestDB(t)
	logger := costTestLogger()
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "parent-cost", ts)

	writeSubagentJSONL(t, projectDir, "parent-cost", "agent-1", ts, 1_000_000, 0)
	writeSubagentMeta(t, projectDir, "parent-cost", "agent-1", "Explore", "map the code", "tool-1")

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	got := findSession(t, sessions, "parent-cost")

	if got.SubagentCost.TotalUSD <= 0 {
		t.Fatalf("sub-agent cost = %v, want > 0 — delegated spend must be costed",
			got.SubagentCost.TotalUSD)
	}
	assertUSD(t, "total = main + delegated",
		got.TotalCost().TotalUSD, got.Cost.TotalUSD+got.SubagentCost.TotalUSD)
}

// TestSessionCost_SubagentUnpricedModelSurfaces guards a pairing that is easy
// to half-implement: unpriced tokens and unpriced model names are written
// together and must be read together. Rolling up only the tokens leaves a
// session reporting excluded tokens attributed to no model, and — because the
// UI keys its disclosure off the model list — rendering a confident total for a
// session that is only partly priced.
func TestSessionCost_SubagentUnpricedModelSurfaces(t *testing.T) {
	db := setupTestDB(t)
	logger := costTestLogger()
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	projectDir := setupSubagentProject(t, "parent-unpriced", ts)

	// The parent is fully priced; only the delegated transcript is not.
	writeJSONLPricedTurns(t, filepath.Join(projectDir, "parent-unpriced", "subagents"),
		"agent-1", []struct {
			model string
			ts    time.Time
			usage rawUsage
		}{
			{"any", ts, rawUsage{InputTokens: 300, OutputTokens: 200}},
		})
	writeSubagentMeta(t, projectDir, "parent-unpriced", "agent-1", "Explore", "look around", "tool-1")

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	got := findSession(t, sessions, "parent-unpriced")

	if got.UnpricedTokens != 500 {
		t.Errorf("unpriced tokens = %d, want 500 from the sub-agent", got.UnpricedTokens)
	}
	if len(got.UnpricedModels) != 1 || got.UnpricedModels[0] != "any" {
		t.Errorf("unpriced models = %v, want [any] — a delegated unpriced model must "+
			"reach the disclosure, not just its token count", got.UnpricedModels)
	}
}

// TestMergeUnpricedModels_DedupesAcrossSubagents covers the GROUP_CONCAT shape
// directly: several sub-agents on the same unpriced model must name it once.
func TestMergeUnpricedModels_DedupesAcrossSubagents(t *testing.T) {
	got := mergeUnpricedModels("any", "any\nzeta\nany")
	want := []string{"any", "zeta"}
	if len(got) != len(want) {
		t.Fatalf("merged = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("merged = %v, want %v (sorted, deduped)", got, want)
		}
	}
	if mergeUnpricedModels("", "") != nil {
		t.Error("a fully-priced session must yield nil so the JSON key is omitted")
	}
}

// TestPricingRevision_ChangeForcesRecost is the mechanism that makes storing
// cost safe. Cost used to be recomputed on every read, so a rate edit took
// effect immediately; now that it is persisted, a changed catalog has to
// re-read the transcripts or the cached figures silently rot. #189 lets users
// edit rates from the UI, which is exactly when this fires.
func TestPricingRevision_ChangeForcesRecost(t *testing.T) {
	db := setupTestDB(t)
	logger := costTestLogger()
	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-rev", ts)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	live := currentPricingRevision()
	if live == pricingRevUnknown {
		t.Skip("no pricing wired in this test binary")
	}
	if stored := storedPricingRevision(db); stored != live {
		t.Fatalf("stored pricing revision = %d, want the live %d after a scan", stored, live)
	}

	// Simulate a rate edit: the catalog fingerprint moves on.
	recordPricingRevision(db, logger, live+1)

	cache := NewCache(db, logger)
	if !cache.pricingChanged() {
		t.Fatal("a drifted pricing revision must be detected as changed")
	}
	// Listing re-scans and re-records the live revision, even though the cache
	// is inside its freshness window and no file mtime moved.
	cache.List()
	if stored := storedPricingRevision(db); stored != live {
		t.Errorf("stored pricing revision = %d, want %d re-recorded after the re-cost", stored, live)
	}
}

// TestPricingRevision_UnwiredPricingNeverForcesRescan guards the degenerate
// case: a process with no pricing store must not decide on every single List
// that the catalog changed and re-read every transcript forever.
func TestPricingRevision_UnwiredPricingNeverForcesRescan(t *testing.T) {
	db := setupTestDB(t)
	cache := NewCache(db, costTestLogger())

	packagePricing.Lock()
	saved := packagePricing.revision
	packagePricing.revision = pricingRevUnknown
	packagePricing.Unlock()
	t.Cleanup(func() {
		packagePricing.Lock()
		packagePricing.revision = saved
		packagePricing.Unlock()
	})

	if cache.pricingChanged() {
		t.Error("unknown pricing revision must not count as a change")
	}
}

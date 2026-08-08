package claudesessions

import (
	"context"
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"testing"
	"time"
)

const floatTolerance = 1e-9

func assertUSD(t *testing.T, label string, got, want float64) {
	t.Helper()
	if math.Abs(got-want) > floatTolerance {
		t.Errorf("%s: got $%.8f, want $%.8f", label, got, want)
	}
}

// TestCostForUsage_CacheWriteTiers is the acceptance criterion from #180:
// 1000 1-hour cache-creation tokens on an Opus model cost $0.010 (the 2× input
// rate), not the $0.00625 the single-rate table produced.
func TestCostForUsage_CacheWriteTiers(t *testing.T) {
	tests := []struct {
		name       string
		model      string
		usage      TokenUsage
		wantWrite  float64
		wantPriced bool
	}{
		{
			name:       "opus 1h cache write bills at 2x input",
			model:      "claude-opus-4-8",
			usage:      TokenUsage{CacheCreationTokens: 1000, CacheCreation1hTokens: 1000},
			wantWrite:  0.010,
			wantPriced: true,
		},
		{
			name:       "opus 5m cache write bills at 1.25x input",
			model:      "claude-opus-4-8",
			usage:      TokenUsage{CacheCreationTokens: 1000, CacheCreation5mTokens: 1000},
			wantWrite:  0.00625,
			wantPriced: true,
		},
		{
			name:  "mixed tiers bill independently",
			model: "claude-opus-4-8",
			usage: TokenUsage{
				CacheCreationTokens:   2000,
				CacheCreation5mTokens: 1000,
				CacheCreation1hTokens: 1000,
			},
			wantWrite:  0.01625,
			wantPriced: true,
		},
		{
			name:       "sonnet 1h cache write",
			model:      "claude-sonnet-5",
			usage:      TokenUsage{CacheCreationTokens: 1000, CacheCreation1hTokens: 1000},
			wantWrite:  0.006,
			wantPriced: true,
		},
		{
			name:       "haiku 1h cache write",
			model:      "claude-haiku-4-5",
			usage:      TokenUsage{CacheCreationTokens: 1000, CacheCreation1hTokens: 1000},
			wantWrite:  0.002,
			wantPriced: true,
		},
		{
			name:       "fable 1h cache write",
			model:      "claude-fable-5",
			usage:      TokenUsage{CacheCreationTokens: 1000, CacheCreation1hTokens: 1000},
			wantWrite:  0.020,
			wantPriced: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			c, priced := costForUsage(tc.model, tc.usage)
			if priced != tc.wantPriced {
				t.Fatalf("priced = %v, want %v", priced, tc.wantPriced)
			}
			assertUSD(t, "cache write", c.CacheWriteCostUSD, tc.wantWrite)
			assertUSD(t, "total", c.TotalCostUSD, tc.wantWrite)
		})
	}
}

// TestPricingForModel_UnknownModelsAreNotGuessed pins the second half of #180:
// models with no published rates must not be silently billed at Sonnet rates.
func TestPricingForModel_UnknownModelsAreNotGuessed(t *testing.T) {
	priced := []struct {
		model  string
		family string
	}{
		{"claude-opus-4-8", "opus"},
		{"claude-opus-5", "opus"},
		{"opus", "opus"},
		{"claude-sonnet-4-6", "sonnet"},
		{"claude-haiku-4-5", "haiku"},
		{"claude-fable-5", "fable"},
		{"CLAUDE-OPUS-5", "opus"},
	}
	for _, tc := range priced {
		p, ok := pricingForModel(tc.model)
		if !ok {
			t.Errorf("%q: expected known pricing", tc.model)
			continue
		}
		if p != pricingTable[tc.family] {
			t.Errorf("%q: resolved to the wrong family (want %s)", tc.model, tc.family)
		}
	}

	// Every one of these appears in the reference corpus and previously priced
	// as Sonnet.
	for _, model := range []string{
		"k3", "glm-5.2", "mixedbread-ai/mxbai-embed-large-v1", syntheticModel, "",
		"some-opus-lookalike", "gpt-sonnet-clone",
	} {
		if _, ok := pricingForModel(model); ok {
			t.Errorf("%q: expected unknown pricing, got a rate", model)
		}
	}
}

func TestCostForUsage_UnknownModelCostsNothing(t *testing.T) {
	for _, model := range []string{"k3", syntheticModel} {
		c, priced := costForUsage(model, TokenUsage{
			InputTokens: 1_000_000, OutputTokens: 1_000_000,
			CacheCreationTokens: 1_000_000, CacheCreation1hTokens: 1_000_000,
		})
		if priced {
			t.Errorf("%q: expected unpriced", model)
		}
		if c.TotalCostUSD != 0 {
			t.Errorf("%q: expected $0, got $%.4f", model, c.TotalCostUSD)
		}
	}
}

// TestSplitCacheCreation covers the fallback rule that keeps pre-split
// transcripts costing exactly what they did before this change.
func TestSplitCacheCreation(t *testing.T) {
	tests := []struct {
		name                 string
		usage                rawUsage
		want5m, want1h       int
		wantSumEqualsCreated bool
	}{
		{
			name:                 "no nested object attributes everything to 5m",
			usage:                rawUsage{CacheCreationInputTokens: 500},
			want5m:               500,
			want1h:               0,
			wantSumEqualsCreated: true,
		},
		{
			name: "all 1h",
			usage: rawUsage{
				CacheCreationInputTokens: 1835,
				CacheCreation:            &rawCacheCreation{Ephemeral1hInputTokens: 1835},
			},
			want5m: 0, want1h: 1835, wantSumEqualsCreated: true,
		},
		{
			name: "both tiers present",
			usage: rawUsage{
				CacheCreationInputTokens: 300,
				CacheCreation: &rawCacheCreation{
					Ephemeral5mInputTokens: 100, Ephemeral1hInputTokens: 200,
				},
			},
			want5m: 100, want1h: 200, wantSumEqualsCreated: true,
		},
		{
			name: "zeroed nested object",
			usage: rawUsage{
				CacheCreationInputTokens: 0,
				CacheCreation:            &rawCacheCreation{},
			},
			want5m: 0, want1h: 0, wantSumEqualsCreated: true,
		},
		{
			name: "unattributed remainder falls to 5m so buckets sum to the total",
			usage: rawUsage{
				CacheCreationInputTokens: 1000,
				CacheCreation:            &rawCacheCreation{Ephemeral1hInputTokens: 600},
			},
			want5m: 400, want1h: 600, wantSumEqualsCreated: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got5m, got1h := splitCacheCreation(&tc.usage)
			if got5m != tc.want5m || got1h != tc.want1h {
				t.Errorf("got 5m=%d 1h=%d, want 5m=%d 1h=%d", got5m, got1h, tc.want5m, tc.want1h)
			}
			if tc.wantSumEqualsCreated && got5m+got1h != tc.usage.CacheCreationInputTokens {
				t.Errorf("buckets sum to %d, want %d", got5m+got1h, tc.usage.CacheCreationInputTokens)
			}
		})
	}
}

// TestEventUsageSplit_MatchesScannerSplit guards the two decoders against drift
// — the insight pipeline and the scanner must attribute tiers identically.
func TestEventUsageSplit_MatchesScannerSplit(t *testing.T) {
	raw := `{"input_tokens":1,"cache_creation_input_tokens":1000,
	         "cache_creation":{"ephemeral_5m_input_tokens":250,"ephemeral_1h_input_tokens":750}}`

	var scannerUsage rawUsage
	if err := json.Unmarshal([]byte(raw), &scannerUsage); err != nil {
		t.Fatalf("scanner decode: %v", err)
	}
	var eventUsage EventUsage
	if err := json.Unmarshal([]byte(raw), &eventUsage); err != nil {
		t.Fatalf("event decode: %v", err)
	}

	s5, s1 := splitCacheCreation(&scannerUsage)
	e5, e1 := eventUsage.Split()
	if s5 != e5 || s1 != e1 {
		t.Errorf("decoders disagree: scanner 5m=%d 1h=%d, event 5m=%d 1h=%d", s5, s1, e5, e1)
	}
	if s5 != 250 || s1 != 750 {
		t.Errorf("got 5m=%d 1h=%d, want 250/750", s5, s1)
	}
}

// writeJSONLWithCacheTTL writes a session whose assistant turn carries a nested
// cache_creation split.
func writeJSONLWithCacheTTL(t *testing.T, dir, sessionID string, ts time.Time, c5m, c1h int) {
	t.Helper()
	if err := os.MkdirAll(dir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	user, _ := json.Marshal(rawEvent{
		Type: "user", SessionID: sessionID, Timestamp: ts, CWD: "/tmp",
		Message: &rawMessage{Role: "user", Content: json.RawMessage(`"hi"`)},
	})
	assistant, _ := json.Marshal(rawEvent{
		Type: "assistant", SessionID: sessionID, Timestamp: ts.Add(time.Second),
		Message: &rawMessage{
			Role: "assistant", Model: "claude-opus-4-8",
			Content: json.RawMessage(`[{"type":"text","text":"ok"}]`),
			Usage: &rawUsage{
				InputTokens:              10,
				OutputTokens:             20,
				CacheCreationInputTokens: c5m + c1h,
				CacheCreation: &rawCacheCreation{
					Ephemeral5mInputTokens: c5m, Ephemeral1hInputTokens: c1h,
				},
			},
		},
	})
	data := append(append(user, '\n'), append(assistant, '\n')...)
	if err := os.WriteFile(filepath.Join(dir, sessionID+".jsonl"), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}
}

// TestIncrementalScan_PersistsCacheTTLSplit walks the whole path: JSONL → cache
// → summary, and asserts the split survives and still sums to the flat total.
func TestIncrementalScan_PersistsCacheTTLSplit(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONLWithCacheTTL(t, projectDir, "session-ttl", ts, 40, 960)

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	s := findSession(t, sessions, "session-ttl")

	if s.Usage.CacheCreation5mTokens != 40 || s.Usage.CacheCreation1hTokens != 960 {
		t.Errorf("split not persisted: got 5m=%d 1h=%d",
			s.Usage.CacheCreation5mTokens, s.Usage.CacheCreation1hTokens)
	}
	if got := s.Usage.CacheCreation5mTokens + s.Usage.CacheCreation1hTokens; got != s.Usage.CacheCreationTokens {
		t.Errorf("buckets sum to %d, want the flat total %d", got, s.Usage.CacheCreationTokens)
	}

	// 40 × $6.25/MTok + 960 × $10/MTok = $0.00025 + $0.0096
	c, priced := costForUsage(s.Model, s.TotalUsage())
	if !priced {
		t.Fatal("expected opus to be priced")
	}
	assertUSD(t, "cache write", c.CacheWriteCostUSD, 40.0/1e6*6.25+960.0/1e6*10.00)
}

// TestIncrementalScan_NoNestedSplitCostsAsBefore is the regression guarantee:
// a transcript without the nested object produces the pre-change cost exactly.
func TestIncrementalScan_NoNestedSplitCostsAsBefore(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	// writeJSONL emits usage with no nested cache_creation object.
	writeJSONL(t, projectDir, "session-flat", ts)

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("IncrementalScan: %v", err)
	}
	s := findSession(t, sessions, "session-flat")

	if s.Usage.CacheCreation1hTokens != 0 {
		t.Errorf("expected no 1h tokens without a nested split, got %d", s.Usage.CacheCreation1hTokens)
	}
	if s.Usage.CacheCreation5mTokens != s.Usage.CacheCreationTokens {
		t.Errorf("expected the flat total in the 5m bucket: got %d, want %d",
			s.Usage.CacheCreation5mTokens, s.Usage.CacheCreationTokens)
	}
}

// TestBuildSummary_UnknownModelsExcludedFromCost asserts unknown-model tokens
// are counted and surfaced but contribute no invented cost.
func TestBuildSummary_UnknownModelsExcludedFromCost(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		{
			SessionID: "opus", Model: "claude-opus-4-8", StartTime: ts, LastActivity: ts,
			Usage: TokenUsage{InputTokens: 1_000_000},
		},
		{
			SessionID: "k3", Model: "k3", StartTime: ts, LastActivity: ts,
			Usage: TokenUsage{InputTokens: 500_000, OutputTokens: 100_000},
		},
		{
			SessionID: "syn", Model: syntheticModel, StartTime: ts, LastActivity: ts,
			Usage: TokenUsage{InputTokens: 1_000},
		},
	}

	summary, cost := buildSummary(sessions)

	// Only the Opus session contributes: 1M input × $5/MTok.
	assertUSD(t, "total cost", cost.TotalCostUSD, 5.00)
	assertUSD(t, "summary cost", summary.EstimatedCostUSD, 5.00)

	if summary.UnknownPricingTokens != 500_000+100_000+1_000 {
		t.Errorf("unknown token count = %d", summary.UnknownPricingTokens)
	}
	want := []string{"<synthetic>", "k3"}
	if len(summary.UnknownPricingModels) != len(want) {
		t.Fatalf("unknown models = %v, want %v", summary.UnknownPricingModels, want)
	}
	for i, m := range want {
		if summary.UnknownPricingModels[i] != m {
			t.Errorf("unknown models = %v, want %v (sorted)", summary.UnknownPricingModels, want)
		}
	}

	// Token totals still include every session — only cost is withheld.
	if summary.TotalInputTokens != 1_501_000 {
		t.Errorf("total input = %d, want 1501000", summary.TotalInputTokens)
	}
}

// TestBuildModelBreakdown_ExcludesSynthetic keeps the placeholder out of the
// per-model charts.
func TestBuildModelBreakdown_ExcludesSynthetic(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		{Model: "claude-opus-4-8", StartTime: ts, LastActivity: ts, Usage: TokenUsage{InputTokens: 100}},
		{Model: syntheticModel, StartTime: ts, LastActivity: ts, Usage: TokenUsage{InputTokens: 900}},
	}

	for _, stat := range buildModelBreakdown(sessions) {
		if stat.Model == syntheticModel {
			t.Error("<synthetic> leaked into model_breakdown")
		}
	}
	for _, stat := range buildSessionsPerModel(sessions) {
		if stat.Model == syntheticModel {
			t.Error("<synthetic> leaked into sessions_per_model")
		}
	}

	// With the synthetic session excluded, Opus is 100% of the remaining tokens.
	breakdown := buildModelBreakdown(sessions)
	if len(breakdown) != 1 || breakdown[0].Percentage != 100 {
		t.Errorf("expected opus at 100%%, got %+v", breakdown)
	}
}

// TestIncrementalScan_ScannerVersionForcesReread covers the re-cost path: a
// stale scanner_version must re-read transcripts whose mtimes never changed.
func TestIncrementalScan_ScannerVersionForcesReread(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONLWithCacheTTL(t, projectDir, "session-rescan", ts, 0, 1000)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}
	if got := storedScannerVersion(db); got != CurrentScannerVersion {
		t.Fatalf("scanner version not recorded: got %d, want %d", got, CurrentScannerVersion)
	}

	// Simulate rows written by an older reader: blank the split and rewind the
	// version, leaving file mtimes untouched.
	if _, err := db.ExecContext(context.Background(),
		`UPDATE claude_session_cache SET cache_creation_5m_tokens = 0, cache_creation_1h_tokens = 0`,
	); err != nil {
		t.Fatalf("blank split: %v", err)
	}
	if _, err := db.ExecContext(context.Background(), `UPDATE claude_cache_metadata SET scanner_version = 0 WHERE id = 1`); err != nil {
		t.Fatalf("rewind version: %v", err)
	}

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}

	s := findSession(t, sessions, "session-rescan")
	if s.Usage.CacheCreation1hTokens != 1000 {
		t.Errorf("stale rows were not re-read: 1h tokens = %d, want 1000", s.Usage.CacheCreation1hTokens)
	}
	if got := storedScannerVersion(db); got != CurrentScannerVersion {
		t.Errorf("scanner version not updated after re-read: got %d", got)
	}
}

// TestIncrementalScan_ScannerVersionCurrentSkipsReread confirms the forced
// re-read is one-time: an up-to-date version leaves unchanged files alone.
func TestIncrementalScan_ScannerVersionCurrentSkipsReread(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONLWithCacheTTL(t, projectDir, "session-stable", ts, 0, 1000)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	// Blank the split WITHOUT rewinding the version. The file is unchanged and
	// the reader is current, so the scan must not re-read it.
	if _, err := db.ExecContext(context.Background(),
		`UPDATE claude_session_cache SET cache_creation_1h_tokens = 0`,
	); err != nil {
		t.Fatalf("blank split: %v", err)
	}

	sessions, err := IncrementalScan(db, logger)
	if err != nil {
		t.Fatalf("second scan: %v", err)
	}
	s := findSession(t, sessions, "session-stable")
	if s.Usage.CacheCreation1hTokens != 0 {
		t.Errorf("unchanged file was re-read despite a current scanner version")
	}
}

// TestSplitCacheTiers_BucketsAlwaysSumToTotal is the acceptance criterion that
// the two buckets sum to the flat total for every session — including when the
// nested object disagrees with it, where naively adding both nested fields
// would exceed the total and overcharge.
func TestSplitCacheTiers_BucketsAlwaysSumToTotal(t *testing.T) {
	cases := []struct {
		total, nested1h int
		want5m, want1h  int
	}{
		{total: 0, nested1h: 0, want5m: 0, want1h: 0},
		{total: 500, nested1h: 0, want5m: 500, want1h: 0},
		{total: 1000, nested1h: 1000, want5m: 0, want1h: 1000},
		{total: 300, nested1h: 200, want5m: 100, want1h: 200},
		// Nested claims more 1h than the total: clamp rather than overcharge.
		{total: 500, nested1h: 600, want5m: 0, want1h: 500},
		// Defensive: a negative count must not invert the split.
		{total: 500, nested1h: -10, want5m: 500, want1h: 0},
	}
	for _, tc := range cases {
		got5m, got1h := splitCacheTiers(tc.total, tc.nested1h)
		if got5m != tc.want5m || got1h != tc.want1h {
			t.Errorf("splitCacheTiers(%d, %d) = (%d, %d), want (%d, %d)",
				tc.total, tc.nested1h, got5m, got1h, tc.want5m, tc.want1h)
		}
		if got5m+got1h != tc.total {
			t.Errorf("splitCacheTiers(%d, %d): buckets sum to %d, want %d",
				tc.total, tc.nested1h, got5m+got1h, tc.total)
		}
	}
}

// TestIncrementalScan_ForcedRescanReportsUpdatesNotDiscoveries guards the
// forced re-read: the files are unchanged, so their sessions must be notified
// as updated, and a transcript deleted meanwhile must still be reaped.
func TestIncrementalScan_ForcedRescanReportsUpdatesNotDiscoveries(t *testing.T) {
	db := setupTestDB(t)
	logger := testLogger

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONLWithCacheTTL(t, projectDir, "session-keep", ts, 0, 1000)
	writeJSONLWithCacheTTL(t, projectDir, "session-gone", ts, 0, 1000)

	if _, err := IncrementalScan(db, logger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	// Rewind the reader version and delete one transcript.
	if _, err := db.ExecContext(context.Background(),
		`UPDATE claude_cache_metadata SET scanner_version = 0 WHERE id = 1`); err != nil {
		t.Fatalf("rewind version: %v", err)
	}
	if err := os.Remove(filepath.Join(projectDir, "session-gone.jsonl")); err != nil {
		t.Fatalf("remove transcript: %v", err)
	}

	var newIDs, updatedIDs []string
	sessions, err := IncrementalScanWithNotify(db, logger,
		func(sessionID, _ string, isNew bool) {
			if isNew {
				newIDs = append(newIDs, sessionID)
			} else {
				updatedIDs = append(updatedIDs, sessionID)
			}
		})
	if err != nil {
		t.Fatalf("forced rescan: %v", err)
	}

	if len(newIDs) != 0 {
		t.Errorf("existing sessions reported as newly discovered: %v", newIDs)
	}
	if len(updatedIDs) != 1 || updatedIDs[0] != "session-keep" {
		t.Errorf("expected one update for session-keep, got %v", updatedIDs)
	}
	// The deleted transcript must not survive the forced rescan.
	for _, s := range sessions {
		if s.SessionID == "session-gone" {
			t.Error("deleted session was not reaped during the forced rescan")
		}
	}
}

// TestBuildSummary_SyntheticNeverTopModel keeps the placeholder out of the
// headline KPI, consistent with its exclusion from the breakdowns.
func TestBuildSummary_SyntheticNeverTopModel(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		{Model: syntheticModel, StartTime: ts, LastActivity: ts},
		{Model: syntheticModel, StartTime: ts, LastActivity: ts},
		{Model: syntheticModel, StartTime: ts, LastActivity: ts},
		{Model: "claude-opus-4-8", StartTime: ts, LastActivity: ts},
	}
	summary, _ := buildSummary(sessions)
	if summary.MostUsedModel == syntheticModel {
		t.Error("<synthetic> reported as the most used model")
	}
	if summary.MostUsedModel != "claude-opus-4-8" {
		t.Errorf("most used model = %q, want claude-opus-4-8", summary.MostUsedModel)
	}
}

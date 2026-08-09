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
			// Looked up at time.Now() (2026-08), the Sonnet 5 introductory rate
			// applies: $2/$10 through 2026-08-31, so 1h cache write bills at
			// 2 × $2/MTok — the flat-table $0.006 was the mispricing this issue
			// removes.
			name:       "sonnet 5 1h cache write bills at the introductory rate",
			model:      "claude-sonnet-5",
			usage:      TokenUsage{CacheCreationTokens: 1000, CacheCreation1hTokens: 1000},
			wantWrite:  0.004,
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
			c, priced := costForUsage(tc.model, tc.usage, time.Now())
			if priced != tc.wantPriced {
				t.Fatalf("priced = %v, want %v", priced, tc.wantPriced)
			}
			assertUSD(t, "cache write", c.CacheWriteCostUSD, tc.wantWrite)
			assertUSD(t, "total", c.TotalCostUSD, tc.wantWrite)
		})
	}
}

// TestResolver_UnknownModelsAreNotGuessed pins the second half of #180 through
// the pricing catalog: models with no published rates must not be silently
// billed at another model's rates. Catalog semantics are covered exhaustively
// in internal/pricing; this guards the wiring the analytics paths depend on.
func TestResolver_UnknownModelsAreNotGuessed(t *testing.T) {
	resolver := defaultPricingResolver()
	if resolver == nil {
		t.Skip("no pricing resolver wired in this test binary")
	}
	at := time.Now()

	for _, model := range []string{
		"claude-opus-4-8", "claude-opus-5", "opus",
		"claude-sonnet-4-6", "claude-haiku-4-5", "claude-fable-5", "CLAUDE-OPUS-5",
		// Third-party backends, priced from their providers' published rates
		// as of #187 — every one of these appears in the reference corpus.
		"k3", "glm-5.2", "mixedbread-ai/mxbai-embed-large-v1", syntheticModel,
	} {
		if _, ok := resolver.Resolve(model, at); !ok {
			t.Errorf("%q: expected known pricing", model)
		}
	}

	// A model ID that names no real model must still resolve to nothing rather
	// than being billed at a lookalike's rate — anchored matching is what makes
	// "some-opus-lookalike" miss the "opus" alias.
	for _, model := range []string{
		"", "any", "some-opus-lookalike", "gpt-sonnet-clone",
	} {
		if _, ok := resolver.Resolve(model, at); ok {
			t.Errorf("%q: expected unknown pricing, got a rate", model)
		}
	}
}

// TestResolver_NonBillableModelsCostNothingWithoutBeingUnknown separates the
// two ways a model can price at $0.00: a deliberate zero (the synthetic
// placeholder, embeddings) resolves and contributes nothing, while an unpriced
// model does not resolve at all and is reported as a gap. Conflating them is
// how a $0 row becomes an invisible bug.
func TestResolver_NonBillableModelsCostNothingWithoutBeingUnknown(t *testing.T) {
	resolver := defaultPricingResolver()
	if resolver == nil {
		t.Skip("no pricing resolver wired in this test binary")
	}
	usage := TokenUsage{InputTokens: 1_000_000, OutputTokens: 1_000_000, CacheReadTokens: 1_000_000}

	for _, model := range []string{syntheticModel, "mixedbread-ai/mxbai-embed-large-v1"} {
		res, ok := resolver.Resolve(model, time.Now())
		if !ok {
			t.Fatalf("%q: expected a catalog row, got unknown", model)
		}
		if res.Rate.Billable {
			t.Errorf("%q: expected non-billable", model)
		}
		if got := res.Rate.Price(usage.eventUsage()).TotalCostUSD; got != 0 {
			t.Errorf("%q: expected $0, got $%.4f", model, got)
		}
	}
}

// TestResolver_ThirdPartyRatesMatchPublished pins the seeded provider rates to
// the figures verified against each provider's pricing page, so a careless
// catalog edit shows up here rather than in a user's cost total.
func TestResolver_ThirdPartyRatesMatchPublished(t *testing.T) {
	resolver := defaultPricingResolver()
	if resolver == nil {
		t.Skip("no pricing resolver wired in this test binary")
	}
	tests := []struct {
		name                      string
		model                     string
		wantIn, wantOut, wantRead float64
		wantWrite5m, wantWrite1h  float64
	}{
		{"kimi k3 bare id from transcripts", "k3", 3.00, 15.00, 0.30, 3.00, 3.00},
		{"kimi k3 canonical id", "kimi-k3-20260801", 3.00, 15.00, 0.30, 3.00, 3.00},
		{"glm 5.2", "glm-5.2", 1.40, 4.40, 0.26, 1.40, 1.40},
		{"qwen3.5 397b", "qwen3.5-397b-a17b", 0.60, 3.60, 0.06, 0.75, 0.75},
		{"qwen3.5 plus", "qwen3.5-plus", 0.40, 2.40, 0.04, 0.50, 0.50},
		{"qwen3.5 flash", "qwen3.5-flash", 0.10, 0.40, 0.01, 0.125, 0.125},
		{"qwen3 max", "qwen3-max", 1.20, 6.00, 0.12, 1.50, 1.50},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			res, ok := resolver.Resolve(tt.model, time.Now())
			if !ok {
				t.Fatalf("%q: expected a rate", tt.model)
			}
			r := res.Rate
			assertUSD(t, "input", r.InputPerMTok, tt.wantIn)
			assertUSD(t, "output", r.OutputPerMTok, tt.wantOut)
			assertUSD(t, "cache read", r.CacheReadPerMTok, tt.wantRead)
			assertUSD(t, "cache write 5m", r.CacheWrite5mPerMTok, tt.wantWrite5m)
			assertUSD(t, "cache write 1h", r.CacheWrite1hPerMTok, tt.wantWrite1h)
			if !r.Billable {
				t.Error("expected billable")
			}
		})
	}
}

// TestResolver_AliasesAndContextVariants covers the model IDs that name no
// concrete model: the bare family aliases price at that tier's flagship and say
// so via Estimated, "any" names nothing at all and stays unknown, and a 1M
// context variant rides its family's prefix rather than falling off the catalog.
func TestResolver_AliasesAndContextVariants(t *testing.T) {
	resolver := defaultPricingResolver()
	if resolver == nil {
		t.Skip("no pricing resolver wired in this test binary")
	}
	at := time.Now()

	res, ok := resolver.Resolve("opus", at)
	if !ok {
		t.Fatal(`"opus": expected the generic Opus alias to resolve`)
	}
	if !res.Estimated {
		t.Error(`"opus": expected Estimated — the bare alias names no concrete model`)
	}

	if _, ok := resolver.Resolve("any", at); ok {
		t.Error(`"any": expected unknown — it names no family, so any price is invented`)
	}

	base, ok := resolver.Resolve("claude-opus-4-7", at)
	if !ok {
		t.Fatal(`"claude-opus-4-7": expected a rate`)
	}
	variant, ok := resolver.Resolve("claude-opus-4-7[1m]", at)
	if !ok {
		t.Fatal(`"claude-opus-4-7[1m]": expected the family prefix to match`)
	}
	assertUSD(t, "1m variant input", variant.Rate.InputPerMTok, base.Rate.InputPerMTok)
	assertUSD(t, "1m variant output", variant.Rate.OutputPerMTok, base.Rate.OutputPerMTok)
}

func TestCostForUsage_UnknownModelCostsNothing(t *testing.T) {
	for _, model := range []string{"any", "gpt-sonnet-clone"} {
		c, priced := costForUsage(model, TokenUsage{
			InputTokens: 1_000_000, OutputTokens: 1_000_000,
			CacheCreationTokens: 1_000_000, CacheCreation1hTokens: 1_000_000,
		}, time.Now())
		if priced {
			t.Errorf("%q: expected unpriced", model)
		}
		if c.TotalCostUSD != 0 {
			t.Errorf("%q: expected $0, got $%.4f", model, c.TotalCostUSD)
		}
	}
}

// TestCostForUsage_ThirdPartyModelIsPriced is the headline of #187: the
// third-most-used model in the reference corpus must report real spend rather
// than the $0.00 an absent catalog row produced.
func TestCostForUsage_ThirdPartyModelIsPriced(t *testing.T) {
	c, priced := costForUsage("k3", TokenUsage{
		InputTokens: 1_000_000, OutputTokens: 1_000_000, CacheReadTokens: 1_000_000,
	}, time.Now())
	if !priced {
		t.Fatal(`"k3": expected priced`)
	}
	// 1 MTok each at Moonshot's published $3.00 / $15.00 / $0.30.
	assertUSD(t, "input", c.InputCostUSD, 3.00)
	assertUSD(t, "output", c.OutputCostUSD, 15.00)
	assertUSD(t, "cache read", c.CacheReadCostUSD, 0.30)
	assertUSD(t, "total", c.TotalCostUSD, 18.30)
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
	c, priced := costForUsage(s.Model, s.TotalUsage(), s.LastActivity)
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
// are counted and surfaced but contribute no invented cost, while the two other
// kinds of $0.00 stay out of that bucket: a priced third-party model now adds
// real spend, and a non-billable one adds nothing without being a gap.
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
		{
			SessionID: "alias", Model: "any", StartTime: ts, LastActivity: ts,
			Usage: TokenUsage{InputTokens: 7_000, OutputTokens: 3_000},
		},
	}

	summary, cost := buildSummary(sessions)

	// Opus 1M input × $5/MTok, plus K3's 0.5M × $3 + 0.1M × $15 = $3.00.
	assertUSD(t, "total cost", cost.TotalCostUSD, 8.00)
	assertUSD(t, "summary cost", summary.EstimatedCostUSD, 8.00)

	// Only "any" is a genuine gap — the synthetic placeholder resolves to a
	// non-billable row and K3 is priced.
	if summary.UnknownPricingTokens != 7_000+3_000 {
		t.Errorf("unknown token count = %d, want 10000", summary.UnknownPricingTokens)
	}
	want := []string{"any"}
	if len(summary.UnknownPricingModels) != len(want) {
		t.Fatalf("unknown models = %v, want %v", summary.UnknownPricingModels, want)
	}
	for i, m := range want {
		if summary.UnknownPricingModels[i] != m {
			t.Errorf("unknown models = %v, want %v (sorted)", summary.UnknownPricingModels, want)
		}
	}

	// Token totals still include every session — only cost is withheld.
	if summary.TotalInputTokens != 1_508_000 {
		t.Errorf("total input = %d, want 1508000", summary.TotalInputTokens)
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

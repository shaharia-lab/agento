package pricing

import (
	"testing"
	"time"
)

func ts(s string) time.Time {
	t, err := time.Parse(time.RFC3339, s)
	if err != nil {
		panic(err)
	}
	return t
}

func rate(pattern string, mt MatchType, from string, in, out float64) Rate {
	return Rate{
		Provider:            "anthropic",
		ModelPattern:        pattern,
		MatchType:           mt,
		InputPerMTok:        in,
		OutputPerMTok:       out,
		CacheWrite5mPerMTok: in * 1.25,
		CacheWrite1hPerMTok: in * 2,
		CacheReadPerMTok:    in * 0.1,
		EffectiveFrom:       ts(from),
		IsBuiltin:           true,
	}
}

// testCatalog mirrors the embedded catalog's shape for the models under test.
func testCatalog() []Rate {
	return []Rate{
		rate("claude-sonnet-5", MatchPrefix, "2020-01-01T00:00:00Z", 2, 10),
		rate("claude-sonnet-5", MatchPrefix, "2026-09-01T00:00:00Z", 3, 15),
		rate("claude-haiku-4-5", MatchPrefix, "2020-01-01T00:00:00Z", 1, 5),
		rate("claude-opus-4-7", MatchPrefix, "2020-01-01T00:00:00Z", 5, 25),
		rate("claude-opus-4", MatchPrefix, "2020-01-01T00:00:00Z", 15, 75),
		rate("claude-opus-4-1", MatchPrefix, "2020-01-01T00:00:00Z", 15, 75),
		rate("opus", MatchExact, "2020-01-01T00:00:00Z", 5, 25),
		rate("claude-fable-5", MatchPrefix, "2020-01-01T00:00:00Z", 10, 50),
	}
}

func TestResolve(t *testing.T) {
	r := NewResolver(testCatalog())
	tests := []struct {
		name        string
		model       string
		at          string
		wantIn      float64
		wantOut     float64
		wantEst     bool
		wantMatched bool
	}{
		{
			name:  "sonnet 5 introductory rate before the boundary",
			model: "claude-sonnet-5", at: "2026-08-15T00:00:00Z",
			wantIn: 2, wantOut: 10, wantMatched: true,
		},
		{
			name:  "sonnet 5 list rate after the boundary",
			model: "claude-sonnet-5", at: "2026-09-15T00:00:00Z",
			wantIn: 3, wantOut: 15, wantMatched: true,
		},
		{
			name:  "dated haiku snapshot resolves via prefix",
			model: "claude-haiku-4-5-20251001", at: "2026-03-01T00:00:00Z",
			wantIn: 1, wantOut: 5, wantMatched: true,
		},
		{
			name:  "opus 4.7 context variant resolves via prefix, not the opus-4 row",
			model: "claude-opus-4-7[1m]", at: "2026-06-01T00:00:00Z",
			wantIn: 5, wantOut: 25, wantMatched: true,
		},
		{
			name:  "longest prefix wins: opus-4-1 beats opus-4",
			model: "claude-opus-4-1-20250805", at: "2025-10-01T00:00:00Z",
			wantIn: 15, wantOut: 75, wantMatched: true,
		},
		{
			name:  "exact generic alias",
			model: "opus", at: "2026-06-01T00:00:00Z",
			wantIn: 5, wantOut: 25, wantMatched: true,
		},
		{
			name:  "exact match is anchored: 'some-opus-thing' does not match 'opus'",
			model: "some-opus-thing", at: "2026-06-01T00:00:00Z",
			wantMatched: false,
		},
		{
			name:  "unknown third-party model",
			model: "k3", at: "2026-06-01T00:00:00Z",
			wantMatched: false,
		},
		{
			name:  "empty model",
			model: "", at: "2026-06-01T00:00:00Z",
			wantMatched: false,
		},
		{
			name:  "synthetic placeholder is never priced",
			model: "<synthetic>", at: "2026-06-01T00:00:00Z",
			wantMatched: false,
		},
		{
			name:  "usage predating every row falls back to the earliest, marked estimated",
			model: "claude-sonnet-5", at: "2019-06-01T00:00:00Z",
			wantIn: 2, wantOut: 10, wantEst: true, wantMatched: true,
		},
		{
			name:  "matching is case-insensitive",
			model: "Claude-Opus-4-7", at: "2026-06-01T00:00:00Z",
			wantIn: 5, wantOut: 25, wantMatched: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, ok := r.Resolve(tt.model, ts(tt.at))
			if ok != tt.wantMatched {
				t.Fatalf("matched = %v, want %v", ok, tt.wantMatched)
			}
			if !tt.wantMatched {
				return
			}
			if got.Rate.InputPerMTok != tt.wantIn || got.Rate.OutputPerMTok != tt.wantOut {
				t.Errorf("rate = %v/%v, want %v/%v",
					got.Rate.InputPerMTok, got.Rate.OutputPerMTok, tt.wantIn, tt.wantOut)
			}
			if got.Estimated != tt.wantEst {
				t.Errorf("estimated = %v, want %v", got.Estimated, tt.wantEst)
			}
		})
	}
}

func TestResolve_ExactBeatsPrefix(t *testing.T) {
	// A hypothetical exact row for "claude-opus-4-7" must win over the prefix
	// row even though the prefix pattern is equally long.
	r := NewResolver([]Rate{
		rate("claude-opus-4-7", MatchPrefix, "2020-01-01T00:00:00Z", 5, 25),
		rate("claude-opus-4-7", MatchExact, "2020-01-01T00:00:00Z", 6, 30),
	})
	got, ok := r.Resolve("claude-opus-4-7", ts("2026-01-01T00:00:00Z"))
	if !ok {
		t.Fatal("no match")
	}
	if got.Rate.MatchType != MatchExact || got.Rate.InputPerMTok != 6 {
		t.Errorf("got %s %v, want exact 6", got.Rate.MatchType, got.Rate.InputPerMTok)
	}
}

func TestRatePrice_CacheTiers(t *testing.T) {
	rt := rate("claude-opus-5", MatchPrefix, "2020-01-01T00:00:00Z", 5, 25)
	c := rt.Price(Usage{
		InputTokens:           1_000_000,
		OutputTokens:          100_000,
		CacheCreation5mTokens: 1_000_000,
		CacheCreation1hTokens: 1_000_000,
		CacheReadTokens:       10_000_000,
	})
	// 1M in @5 + 0.1M out @25 + 1M 5m @6.25 + 1M 1h @10 + 10M read @0.5
	want := 5 + 2.5 + 6.25 + 10 + 5
	if c.TotalCostUSD != want {
		t.Errorf("total = %v, want %v", c.TotalCostUSD, want)
	}
}

func TestBuiltinCatalog_Parses(t *testing.T) {
	entries := BuiltinCatalog()
	if len(entries) == 0 {
		t.Fatal("embedded catalog is empty")
	}
	seen := map[string]int{}
	for _, e := range entries {
		rates, err := e.rates()
		if err != nil {
			t.Fatalf("entry %q: %v", e.ModelPattern, err)
		}
		seen[e.ModelPattern] = len(rates)
		if e.Source == "" {
			t.Errorf("entry %q has no source", e.ModelPattern)
		}
	}
	// The acceptance-critical rows must be in the seed.
	if seen["claude-sonnet-5"] != 2 {
		t.Errorf("claude-sonnet-5 rows = %d, want 2 (intro + list)", seen["claude-sonnet-5"])
	}
	for _, p := range []string{"claude-haiku-4-5", "claude-opus-4-7", "claude-fable-5", "claude-opus-5"} {
		if seen[p] == 0 {
			t.Errorf("missing seed row for %q", p)
		}
	}
	// The third-party providers #187 seeded.
	for _, p := range []string{
		"k3", "kimi-k3", "glm-5.2",
		"qwen3.5-397b-a17b", "qwen3.5-plus", "qwen3.5-flash", "qwen3-max",
		"<synthetic>", "mixedbread-ai/",
	} {
		if seen[p] == 0 {
			t.Errorf("missing seed row for %q", p)
		}
	}
}

// TestBuiltinCatalog_BillableMatchesZeroRates enforces the invariant that keeps
// a $0.00 row meaningful: an entry is non-billable exactly when every one of
// its rates is zero. A half-filled entry — a real model with a forgotten output
// rate, or a placeholder still carrying a price — fails here rather than
// silently mis-reporting a user's spend.
func TestBuiltinCatalog_BillableMatchesZeroRates(t *testing.T) {
	for _, e := range BuiltinCatalog() {
		rates, err := e.rates()
		if err != nil {
			t.Fatalf("entry %q: %v", e.ModelPattern, err)
		}
		for _, r := range rates {
			allZero := r.InputPerMTok == 0 && r.OutputPerMTok == 0 &&
				r.CacheWrite5mPerMTok == 0 && r.CacheWrite1hPerMTok == 0 && r.CacheReadPerMTok == 0
			if r.Billable == allZero {
				t.Errorf("entry %q: billable=%v with allZero=%v — a deliberate zero must be "+
					"marked non-billable, and a billable model must price every category",
					e.ModelPattern, r.Billable, allZero)
			}
		}
	}
}

// TestBuiltinCatalog_RejectsHalfFilledEntry proves the validation above is load
// bearing rather than incidentally satisfied by the current data.
func TestBuiltinCatalog_RejectsHalfFilledEntry(t *testing.T) {
	no := false
	tests := []struct {
		name  string
		entry builtinEntry
	}{
		{"billable entry missing its output rate", builtinEntry{
			ModelPattern: "half-filled",
			Rates:        []builtinPrice{{InputPerMTok: 1, OutputPerMTok: 0}},
		}},
		{"non-billable entry still carrying a price", builtinEntry{
			ModelPattern: "not-really-free",
			Billable:     &no,
			Rates:        []builtinPrice{{InputPerMTok: 1, OutputPerMTok: 2}},
		}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if _, err := tt.entry.rates(); err == nil {
				t.Error("expected an error, got nil")
			}
		})
	}
}

// TestBuiltinCatalog_CacheOverridesRespected checks the seed format change
// #187 needed: third-party providers publish their own cached-input price, so
// Anthropic's 1.25×/2×/0.1× multipliers must be overridable per rate.
func TestBuiltinCatalog_CacheOverridesRespected(t *testing.T) {
	read, write := 0.26, 1.40
	e := builtinEntry{
		ModelPattern: "glm-test",
		Rates: []builtinPrice{{
			InputPerMTok: 1.40, OutputPerMTok: 4.40,
			CacheRead: &read, CacheWrite5m: &write, CacheWrite1h: &write,
		}},
	}
	rates, err := e.rates()
	if err != nil {
		t.Fatalf("rates: %v", err)
	}
	r := rates[0]
	// Derived would be 0.14 / 1.75 / 2.80 — the published figures must win.
	if r.CacheReadPerMTok != read || r.CacheWrite5mPerMTok != write || r.CacheWrite1hPerMTok != write {
		t.Errorf("overrides ignored: read=%v write5m=%v write1h=%v",
			r.CacheReadPerMTok, r.CacheWrite5mPerMTok, r.CacheWrite1hPerMTok)
	}

	// An entry that omits them still derives from input.
	derived, err := builtinEntry{
		ModelPattern: "anthropic-shaped",
		Rates:        []builtinPrice{{InputPerMTok: 4, OutputPerMTok: 20}},
	}.rates()
	if err != nil {
		t.Fatalf("rates: %v", err)
	}
	d := derived[0]
	if d.CacheWrite5mPerMTok != 5 || d.CacheWrite1hPerMTok != 8 || d.CacheReadPerMTok != 0.4 {
		t.Errorf("derivation broken: write5m=%v write1h=%v read=%v",
			d.CacheWrite5mPerMTok, d.CacheWrite1hPerMTok, d.CacheReadPerMTok)
	}
}

func TestBuiltinCatalog_SeedRatesMatchAcceptanceCases(t *testing.T) {
	var all []Rate
	for _, e := range BuiltinCatalog() {
		rates, err := e.rates()
		if err != nil {
			t.Fatalf("entry %q: %v", e.ModelPattern, err)
		}
		all = append(all, rates...)
	}
	r := NewResolver(all)

	before, ok := r.Resolve("claude-sonnet-5", ts("2026-08-15T00:00:00Z"))
	if !ok || before.Rate.InputPerMTok != 2 || before.Rate.OutputPerMTok != 10 {
		t.Errorf("sonnet-5 @2026-08-15 = %+v ok=%v, want $2/$10", before, ok)
	}
	after, ok := r.Resolve("claude-sonnet-5", ts("2026-09-15T00:00:00Z"))
	if !ok || after.Rate.InputPerMTok != 3 || after.Rate.OutputPerMTok != 15 {
		t.Errorf("sonnet-5 @2026-09-15 = %+v ok=%v, want $3/$15", after, ok)
	}
	if _, ok := r.Resolve("claude-haiku-4-5-20251001", ts("2026-03-01T00:00:00Z")); !ok {
		t.Error("claude-haiku-4-5-20251001 landed in the unknown bucket")
	}
	if _, ok := r.Resolve("claude-opus-4-7[1m]", ts("2026-03-01T00:00:00Z")); !ok {
		t.Error("claude-opus-4-7[1m] landed in the unknown bucket")
	}
}

package pricing

import (
	"fmt"
	"math"
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

// TestBuiltinCatalog_Qwen37Generation covers the rows #206 seeded. The risk in
// a data-only change is not a crash but a silent mis-resolution: a pattern that
// shadows a neighbor prices one model at another's rate, and every test still
// passes.
func TestBuiltinCatalog_Qwen37Generation(t *testing.T) {
	r := NewResolver(allBuiltinRates(t))
	at := ts("2026-08-09T12:00:00Z")

	for _, tc := range []struct {
		modelID       string
		input, output float64
	}{
		// Newly seeded, verified against the Model Studio pricing page.
		{"qwen3.7-max", 2.5, 7.5},
		{"qwen3.7-plus", 0.4, 1.6},
		{"qwen3.6-flash", 0.25, 1.5},
		// Pre-existing rows must be unaffected by the new prefixes. qwen3-max is
		// the one that could plausibly capture qwen3.7-max, and qwen3.5-flash
		// the one that could be captured by qwen3.6-flash.
		{"qwen3-max", 1.2, 6.0},
		{"qwen3.5-flash", 0.1, 0.4},
		{"qwen3.5-plus", 0.4, 2.4},
	} {
		got, ok := r.Resolve(tc.modelID, at)
		if !ok {
			t.Errorf("%s landed in the unknown-pricing bucket", tc.modelID)
			continue
		}
		if got.Rate.InputPerMTok != tc.input || got.Rate.OutputPerMTok != tc.output {
			t.Errorf("%s = $%v/$%v, want $%v/$%v — check for a shadowing pattern",
				tc.modelID, got.Rate.InputPerMTok, got.Rate.OutputPerMTok, tc.input, tc.output)
		}
		// These are concrete published prices, not family aliases.
		if got.Estimated {
			t.Errorf("%s resolved as estimated; it has a published rate", tc.modelID)
		}
	}

	// A dated or suffixed ID must still land on its prefix rather than falling
	// through to a coarser row.
	suffixed, ok := r.Resolve("qwen3.7-max-2026-08-01", at)
	if !ok || suffixed.Rate.InputPerMTok != 2.5 {
		t.Errorf("qwen3.7-max-2026-08-01 = %+v ok=%v, want the qwen3.7-max rate", suffixed, ok)
	}
}

// TestBuiltinCatalog_QwenPromoRatesHaveNoGuessedBoundary pins the decision
// #206 made. qwen3.7-max and qwen3.7-plus are both limited-time discounts for
// which Alibaba publishes no end date, so they are seeded as a single rate: a
// guessed expiry would misprice a whole date range with nothing to signal it.
// If a boundary is ever added it must come from a published date, and this test
// is the reminder to check that the date is real.
func TestBuiltinCatalog_QwenPromoRatesHaveNoGuessedBoundary(t *testing.T) {
	for _, pattern := range []string{"qwen3.7-max", "qwen3.7-plus", "qwen3.6-flash"} {
		var found bool
		for _, e := range BuiltinCatalog() {
			if e.ModelPattern != pattern {
				continue
			}
			found = true
			if len(e.Rates) != 1 {
				t.Errorf("%s has %d rate rows; a boundary must come from a published expiry date",
					pattern, len(e.Rates))
			}
		}
		if !found {
			t.Errorf("%s is not in the built-in catalog", pattern)
		}
	}

	// The effective-dated mechanism itself is exercised by claude-sonnet-5 in
	// TestBuiltinCatalog_SeedRatesMatchAcceptanceCases — the only entry with a
	// real published boundary.
	r := NewResolver(allBuiltinRates(t))
	early, ok1 := r.Resolve("qwen3.7-max", ts("2026-08-09T00:00:00Z"))
	late, ok2 := r.Resolve("qwen3.7-max", ts("2027-08-09T00:00:00Z"))
	if !ok1 || !ok2 || early.Rate.InputPerMTok != late.Rate.InputPerMTok {
		t.Errorf("qwen3.7-max changed rate over time (%+v -> %+v) without a published boundary",
			early.Rate, late.Rate)
	}
}

// TestBuiltinCatalog_QwenCachePercentageRule checks the arithmetic behind the
// cache columns. Alibaba publishes cache pricing as a percentage of input
// rather than an absolute figure, so these are derived numbers and a slip is
// invisible — nothing else in the catalog would disagree with it.
func TestBuiltinCatalog_QwenCachePercentageRule(t *testing.T) {
	const epsilon = 1e-9
	for _, e := range BuiltinCatalog() {
		if e.Provider != "alibaba" {
			continue
		}
		rates, err := e.rates()
		if err != nil {
			t.Fatalf("entry %q: %v", e.ModelPattern, err)
		}
		type check struct {
			name string
			got  float64
			want float64
		}
		// cacheChecks states the rule once, for any set of input/cache columns
		// — the base row and each of its context-length bands are the same
		// shape and owe the same arithmetic.
		cacheChecks := func(label string, in, read, write5m, write1h float64) []check {
			return []check{
				{label + "cache_read (10% of input)", read, in * 0.10},
				{label + "cache_write_5m (125% of input)", write5m, in * 1.25},
				{label + "cache_write_1h (125% of input)", write1h, in * 1.25},
			}
		}
		for _, rate := range rates {
			checks := cacheChecks("", rate.InputPerMTok,
				rate.CacheReadPerMTok, rate.CacheWrite5mPerMTok, rate.CacheWrite1hPerMTok)
			// The rule is a property of the provider, not of the base row, so
			// every band owes it too — and a band's cache columns are derived
			// exactly like the flat ones, so a slip there is just as invisible.
			for i, tier := range rate.Tiers {
				checks = append(checks, cacheChecks(fmt.Sprintf("tier %d ", i), tier.InputPerMTok,
					tier.CacheReadPerMTok, tier.CacheWrite5mPerMTok, tier.CacheWrite1hPerMTok)...)
			}
			for _, c := range checks {
				if math.Abs(c.got-c.want) > epsilon {
					t.Errorf("%s: %s = %v, want %v (input $%v)",
						e.ModelPattern, c.name, c.got, c.want, rate.InputPerMTok)
				}
			}
		}
	}
}

// allBuiltinRates flattens the embedded catalog into resolver input.
func allBuiltinRates(t *testing.T) []Rate {
	t.Helper()
	var all []Rate
	for _, e := range BuiltinCatalog() {
		rates, err := e.rates()
		if err != nil {
			t.Fatalf("entry %q: %v", e.ModelPattern, err)
		}
		all = append(all, rates...)
	}
	return all
}

// TestBuiltinCatalog_QwenContextTiers pins the seeded bands to what Alibaba
// publishes (#218). These are the numbers that decide whether a long-context
// session is priced right, and nothing else in the catalog contradicts them,
// so an edit slip would be silent.
func TestBuiltinCatalog_QwenContextTiers(t *testing.T) {
	want := map[string][]TierRate{
		"qwen3.7-plus": {
			{MaxInputTokens: 256_000, InputPerMTok: 0.4, OutputPerMTok: 1.6},
			{MaxInputTokens: 1_000_000, InputPerMTok: 1.2, OutputPerMTok: 4.8},
		},
		"qwen3.6-flash": {
			{MaxInputTokens: 256_000, InputPerMTok: 0.25, OutputPerMTok: 1.5},
			{MaxInputTokens: 1_000_000, InputPerMTok: 1.0, OutputPerMTok: 4.0},
		},
		"qwen3.5-plus": {
			{MaxInputTokens: 256_000, InputPerMTok: 0.4, OutputPerMTok: 2.4},
			{MaxInputTokens: 1_000_000, InputPerMTok: 0.5, OutputPerMTok: 3.0},
		},
		"qwen3-max": {
			{MaxInputTokens: 32_000, InputPerMTok: 1.2, OutputPerMTok: 6.0},
			{MaxInputTokens: 128_000, InputPerMTok: 2.4, OutputPerMTok: 12.0},
			{MaxInputTokens: 256_000, InputPerMTok: 3.0, OutputPerMTok: 15.0},
		},
	}

	seen := map[string]bool{}
	for _, e := range BuiltinCatalog() {
		rates, err := e.rates()
		if err != nil {
			t.Fatalf("entry %q: %v", e.ModelPattern, err)
		}
		exp, tiered := want[e.ModelPattern]
		for _, r := range rates {
			if !tiered {
				// Everything else in the catalog stays flat. qwen3.7-max in
				// particular is flat 0-1M and must not acquire bands.
				if len(r.Tiers) != 0 {
					t.Errorf("%s is not a tiered model but declares %d bands", e.ModelPattern, len(r.Tiers))
				}
				continue
			}
			seen[e.ModelPattern] = true
			if len(r.Tiers) != len(exp) {
				t.Errorf("%s: %d bands, want %d", e.ModelPattern, len(r.Tiers), len(exp))
				continue
			}
			for i, w := range exp {
				got := r.Tiers[i]
				if got.MaxInputTokens != w.MaxInputTokens ||
					got.InputPerMTok != w.InputPerMTok || got.OutputPerMTok != w.OutputPerMTok {
					t.Errorf("%s band %d = {%d, $%v/$%v}, want {%d, $%v/$%v}",
						e.ModelPattern, i,
						got.MaxInputTokens, got.InputPerMTok, got.OutputPerMTok,
						w.MaxInputTokens, w.InputPerMTok, w.OutputPerMTok)
				}
			}
			// The flat columns are the lowest band, so a consumer that ignores
			// Tiers still reports the base price rather than a random one.
			if r.InputPerMTok != exp[0].InputPerMTok || r.OutputPerMTok != exp[0].OutputPerMTok {
				t.Errorf("%s flat columns $%v/$%v do not match the lowest band $%v/$%v",
					e.ModelPattern, r.InputPerMTok, r.OutputPerMTok, exp[0].InputPerMTok, exp[0].OutputPerMTok)
			}
		}
	}
	for pattern := range want {
		if !seen[pattern] {
			t.Errorf("%s is not in the built-in catalog", pattern)
		}
	}
}

// TestValidateTiers_RejectsAuthoringErrors covers the build-time guard. A band
// list is only usable if it ascends, so these are the mistakes that would
// otherwise silently pick the wrong price.
func TestValidateTiers_RejectsAuthoringErrors(t *testing.T) {
	base := func(tiers []TierRate) Rate {
		return Rate{
			InputPerMTok: 1, OutputPerMTok: 2, Billable: true, Tiers: tiers,
		}
	}
	cases := []struct {
		name string
		rate Rate
	}{
		{"descending bounds", base([]TierRate{
			{MaxInputTokens: 100, InputPerMTok: 1, OutputPerMTok: 2},
			{MaxInputTokens: 50, InputPerMTok: 3, OutputPerMTok: 4},
		})},
		{"duplicate bounds", base([]TierRate{
			{MaxInputTokens: 100, InputPerMTok: 1, OutputPerMTok: 2},
			{MaxInputTokens: 100, InputPerMTok: 3, OutputPerMTok: 4},
		})},
		{"zero bound", base([]TierRate{{MaxInputTokens: 0, InputPerMTok: 1, OutputPerMTok: 2}})},
		{"zero price in a band", base([]TierRate{
			{MaxInputTokens: 100, InputPerMTok: 1, OutputPerMTok: 2},
			{MaxInputTokens: 200, InputPerMTok: 3, OutputPerMTok: 0},
		})},
		{"negative cache price in a band", base([]TierRate{
			{MaxInputTokens: 100, InputPerMTok: 1, OutputPerMTok: 2, CacheReadPerMTok: -1},
		})},
		{"flat columns disagree with the lowest band", base([]TierRate{
			{MaxInputTokens: 100, InputPerMTok: 9, OutputPerMTok: 2},
		})},
		{"non-billable rate with bands", Rate{
			Billable: false,
			Tiers:    []TierRate{{MaxInputTokens: 100, InputPerMTok: 1, OutputPerMTok: 2}},
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if err := validateRate("test-model", tc.rate); err == nil {
				t.Error("expected a validation error, got nil")
			}
		})
	}

	// And the well-formed case passes.
	ok := base([]TierRate{
		{MaxInputTokens: 100, InputPerMTok: 1, OutputPerMTok: 2},
		{MaxInputTokens: 200, InputPerMTok: 3, OutputPerMTok: 4},
	})
	if err := validateRate("test-model", ok); err != nil {
		t.Errorf("a well-formed tiered rate was rejected: %v", err)
	}
}

package pricing

import (
	"math"
	"reflect"
	"testing"
)

// flashTiers mirrors the seeded qwen3.6-flash bands: the model with the
// widest gap between its base and upper rate, and therefore the one the
// under-reporting figure in #218 was measured on.
func flashTiers() []TierRate {
	return []TierRate{
		{MaxInputTokens: 256_000, InputPerMTok: 0.25, OutputPerMTok: 1.5,
			CacheWrite5mPerMTok: 0.3125, CacheWrite1hPerMTok: 0.3125, CacheReadPerMTok: 0.025},
		{MaxInputTokens: 1_000_000, InputPerMTok: 1.0, OutputPerMTok: 4.0,
			CacheWrite5mPerMTok: 1.25, CacheWrite1hPerMTok: 1.25, CacheReadPerMTok: 0.1},
	}
}

func tieredRate() Rate {
	t := flashTiers()
	return Rate{
		ModelPattern:        "qwen3.6-flash",
		InputPerMTok:        t[0].InputPerMTok,
		OutputPerMTok:       t[0].OutputPerMTok,
		CacheWrite5mPerMTok: t[0].CacheWrite5mPerMTok,
		CacheWrite1hPerMTok: t[0].CacheWrite1hPerMTok,
		CacheReadPerMTok:    t[0].CacheReadPerMTok,
		Billable:            true,
		Tiers:               t,
	}
}

func TestTierFor_BandSelection(t *testing.T) {
	r := tieredRate()

	cases := []struct {
		name      string
		input     int
		wantInput float64
	}{
		{"below the first bound", 1_000, 0.25},
		{"zero tokens stays in the first band", 0, 0.25},
		{"exactly on the first bound is inclusive", 256_000, 0.25},
		{"one token past the bound crosses", 256_001, 1.0},
		{"between bounds", 300_000, 1.0},
		{"exactly on the last bound", 1_000_000, 1.0},
		// Falling through to the highest band rather than to flat or to zero
		// is the difference between over- and under-reporting an outlier.
		{"above every declared bound uses the highest band", 5_000_000, 1.0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := r.tierFor(tc.input).InputPerMTok; got != tc.wantInput {
				t.Errorf("tierFor(%d).InputPerMTok = %g, want %g", tc.input, got, tc.wantInput)
			}
		})
	}
}

func TestTierFor_SubstitutesEveryPriceColumn(t *testing.T) {
	got := tieredRate().tierFor(300_000)
	want := flashTiers()[1]
	if got.InputPerMTok != want.InputPerMTok || got.OutputPerMTok != want.OutputPerMTok ||
		got.CacheWrite5mPerMTok != want.CacheWrite5mPerMTok ||
		got.CacheWrite1hPerMTok != want.CacheWrite1hPerMTok ||
		got.CacheReadPerMTok != want.CacheReadPerMTok {
		t.Errorf("tierFor left a price column on the base band: %+v", got)
	}
	// Everything that is not a price must survive the substitution — a band
	// changes what a request costs, not which model it was.
	if got.ModelPattern != "qwen3.6-flash" || !got.Billable {
		t.Errorf("tierFor mutated non-price fields: %+v", got)
	}
}

// A flat rate is the overwhelmingly common case (every Anthropic model), so
// its behavior must be provably untouched by the tier machinery.
func TestTierFor_FlatRateIsReturnedUnchanged(t *testing.T) {
	flat := Rate{
		ModelPattern: "claude-opus-5", InputPerMTok: 5, OutputPerMTok: 25,
		CacheWrite5mPerMTok: 6.25, CacheWrite1hPerMTok: 10, CacheReadPerMTok: 0.5,
		Billable: true,
	}
	for _, n := range []int{0, 1_000, 5_000_000} {
		if got := flat.tierFor(n); !reflect.DeepEqual(got, flat) {
			t.Errorf("tierFor(%d) changed a flat rate: %+v", n, got)
		}
	}
}

func TestTierFor_SingleBand(t *testing.T) {
	r := Rate{
		InputPerMTok: 2, OutputPerMTok: 8, Billable: true,
		Tiers: []TierRate{{MaxInputTokens: 100, InputPerMTok: 2, OutputPerMTok: 8}},
	}
	// Below, on, and above the only bound all resolve to it.
	for _, n := range []int{1, 100, 10_000} {
		if got := r.tierFor(n).InputPerMTok; got != 2 {
			t.Errorf("tierFor(%d).InputPerMTok = %g, want 2", n, got)
		}
	}
}

// The acceptance case from #218: 300K input tokens on qwen3.6-flash must
// price at the 256K-1M band, and every token must price there — Alibaba bills
// the whole request at the selected tier, not just the excess above the bound.
func TestPrice_WholeRequestBillsAtTheSelectedBand(t *testing.T) {
	u := Usage{InputTokens: 300_000, OutputTokens: 10_000}
	got := tieredRate().Price(u)

	wantInput := 300_000.0 / 1_000_000 * 1.0  // $0.30, all of it at the upper band
	wantOutput := 10_000.0 / 1_000_000 * 4.0  // $0.04
	baseInput := 300_000.0 / 1_000_000 * 0.25 // what the pre-#218 flat rate charged

	if !almostEqual(got.InputCostUSD, wantInput) {
		t.Errorf("input cost = %g, want %g", got.InputCostUSD, wantInput)
	}
	if !almostEqual(got.OutputCostUSD, wantOutput) {
		t.Errorf("output cost = %g, want %g", got.OutputCostUSD, wantOutput)
	}
	// Guard against a marginal/progressive reading sneaking in: that would
	// charge the first 256K at the base rate and only the remainder at the
	// upper one, landing strictly between the two whole-request figures.
	if got.InputCostUSD <= baseInput {
		t.Errorf("input cost %g did not exceed the base-band figure %g", got.InputCostUSD, baseInput)
	}
	marginal := baseInput + 44_000.0/1_000_000*1.0
	if almostEqual(got.InputCostUSD, marginal) {
		t.Errorf("input cost %g looks like marginal-bracket billing", got.InputCostUSD)
	}
}

// The tier boundary is the request's total input, so cached tokens push a
// request into a higher band even when fresh input alone would not. This is
// the reading documented on Usage.tierInputTokens.
func TestPrice_CachedTokensCountTowardTheBoundary(t *testing.T) {
	r := tieredRate()
	// 200K fresh + 100K cache-read = 300K of input for the boundary.
	u := Usage{InputTokens: 200_000, CacheReadTokens: 100_000}
	got := r.Price(u)

	wantInput := 200_000.0 / 1_000_000 * 1.0
	wantRead := 100_000.0 / 1_000_000 * 0.1
	if !almostEqual(got.InputCostUSD, wantInput) || !almostEqual(got.CacheReadCostUSD, wantRead) {
		t.Errorf("cached tokens did not lift the request into the upper band: %+v", got)
	}
	// Same fresh input with no cache stays in the base band.
	if base := r.Price(Usage{InputTokens: 200_000}); !almostEqual(base.InputCostUSD, 200_000.0/1_000_000*0.25) {
		t.Errorf("a 200K request without cache should stay in the base band: %+v", base)
	}
}

func TestUsageTierInputTokens_CountsEveryInputCategory(t *testing.T) {
	u := Usage{
		InputTokens: 1, OutputTokens: 100_000,
		CacheCreation5mTokens: 2, CacheCreation1hTokens: 4, CacheReadTokens: 8,
	}
	// Output is excluded: the provider defines the boundary on input alone.
	if got := u.tierInputTokens(); got != 15 {
		t.Errorf("tierInputTokens() = %d, want 15", got)
	}
}

// The strongest regression guard in the change: an untiered rate must price
// exactly as it did before tiers existed, for every token category.
func TestPrice_UntieredRateMatchesFlatArithmetic(t *testing.T) {
	flat := Rate{
		InputPerMTok: 5, OutputPerMTok: 25,
		CacheWrite5mPerMTok: 6.25, CacheWrite1hPerMTok: 10, CacheReadPerMTok: 0.5,
		Billable: true,
	}
	u := Usage{
		InputTokens: 12_345, OutputTokens: 6_789,
		CacheCreation5mTokens: 1_111, CacheCreation1hTokens: 2_222, CacheReadTokens: 3_333,
	}
	if got, want := flat.Price(u), flat.priceFlat(u); got != want {
		t.Errorf("Price = %+v, priceFlat = %+v — the flat path moved", got, want)
	}
}

func almostEqual(a, b float64) bool { return math.Abs(a-b) < 1e-12 }

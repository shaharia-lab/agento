package claudesessions

import (
	"math"
	"testing"
)

// TestCacheHitRate covers the definition itself: the read share of every
// input-side token.
func TestCacheHitRate(t *testing.T) {
	cases := []struct {
		name                          string
		input, cacheRead, cacheCreate int
		want                          float64
	}{
		{"no tokens at all", 0, 0, 0, 0},
		{"everything read from cache", 0, 100, 0, 1},
		{
			// The case the old analytics formula could not express: a backend
			// with no prompt caching re-bills its whole context as fresh input
			// every turn. Excluding it from its own denominator was what let a
			// dashboard show ~100% for a corpus dominated by an uncached model.
			"model that never caches", 1000, 0, 0, 0,
		},
		{"mixed", 200, 700, 100, 0.7},
		{
			// Cache writes belong in the denominator: they are input-side tokens
			// that were paid for and not served from cache. Omitting them — the
			// old insights formula omitted fresh input instead — inflates the
			// rate for a session that keeps rewriting its cache.
			"cache writes count against the rate", 0, 50, 50, 0.5,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := CacheHitRate(tc.input, tc.cacheRead, tc.cacheCreate)
			if math.Abs(got-tc.want) > 1e-9 {
				t.Errorf("CacheHitRate(%d,%d,%d) = %v, want %v",
					tc.input, tc.cacheRead, tc.cacheCreate, got, tc.want)
			}
		})
	}
}

// TestCacheHitRate_AnalyticsAndInsightsAgree is the regression guard for the
// defect this function exists to prevent: two dashboards computing a metric of
// the same name from the same tokens and printing different numbers (~100% on
// Token Usage, ~74% on Insights).
//
// It drives both real code paths — buildCacheEfficiency for the analytics
// series and TokenProfileProcessor.Finalize for the stored insight — over one
// set of token counts, and requires the same answer.
func TestCacheHitRate_AnalyticsAndInsightsAgree(t *testing.T) {
	const (
		input       = 1_200
		output      = 400
		cacheRead   = 18_000
		cacheCreate = 2_800
	)

	analytics := buildCacheEfficiency([]TimeSeriesPoint{{
		Date:             "2026-08-01",
		InputTokens:      input,
		OutputTokens:     output,
		CacheReadTokens:  cacheRead,
		CacheWriteTokens: cacheCreate,
	}})
	if len(analytics) != 1 {
		t.Fatalf("expected one efficiency point, got %d", len(analytics))
	}

	p := &TokenProfileProcessor{
		inputTokens:   input,
		outputTokens:  output,
		cacheRead:     cacheRead,
		cacheCreation: cacheCreate,
	}
	var insight SessionInsight
	p.Finalize(&insight)

	// The analytics series reports percent rounded to two decimals for the
	// chart axis; the insight keeps the raw 0–1 fraction. Anything beyond that
	// rounding is two different formulas again.
	if diff := math.Abs(analytics[0].CacheHitRate - insight.CacheHitRate*100); diff > 0.005 {
		t.Errorf("analytics reports %.4f%% but the insight reports %.4f%% for the same tokens",
			analytics[0].CacheHitRate, insight.CacheHitRate*100)
	}

	if want := CacheHitRate(input, cacheRead, cacheCreate); math.Abs(insight.CacheHitRate-want) > 1e-9 {
		t.Errorf("insight rate = %v, want the shared definition's %v", insight.CacheHitRate, want)
	}
}

// TestCacheEfficiencyDenominatorIsAllInputSideTokens guards the reported
// denominator, which is what makes the percentage auditable: a reader must be
// able to divide CachedTokens by TotalInputTokens and get CacheHitRate back.
func TestCacheEfficiencyDenominatorIsAllInputSideTokens(t *testing.T) {
	pts := buildCacheEfficiency([]TimeSeriesPoint{{
		InputTokens: 100, CacheReadTokens: 300, CacheWriteTokens: 100,
	}})

	got := pts[0]
	if got.TotalInputTokens != 500 {
		t.Errorf("TotalInputTokens = %d, want 500 (fresh + read + write)", got.TotalInputTokens)
	}
	if want := 60.0; math.Abs(got.CacheHitRate-want) > 1e-9 {
		t.Errorf("CacheHitRate = %v, want %v", got.CacheHitRate, want)
	}
}

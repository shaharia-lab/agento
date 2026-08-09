package claudesessions

import (
	"math"
	"testing"
	"time"
)

// costSession builds a session whose per-model breakdown is consistent with its
// stored totals, the way a scan produces one.
func costSession(id, model string, at time.Time, main map[string]SessionCost, delegated map[string]SessionCost) ClaudeSessionSummary {
	s := ClaudeSessionSummary{
		SessionID:    id,
		ProjectPath:  "/proj",
		Model:        model,
		StartTime:    at,
		LastActivity: at,
		CostByModel:  main,
	}
	for _, c := range main {
		s.Cost.Add(c)
	}
	if delegated != nil {
		s.SubagentCostByModel = delegated
		s.SubagentCount = len(delegated)
		for _, c := range delegated {
			s.SubagentCost.Add(c)
		}
	}
	return s
}

func cost(input, output, cacheRead, cacheWrite float64) SessionCost {
	return SessionCost{
		InputUSD:      input,
		OutputUSD:     output,
		CacheReadUSD:  cacheRead,
		CacheWriteUSD: cacheWrite,
		TotalUSD:      input + output + cacheRead + cacheWrite,
	}
}

// TestTotalCostByModel_ReKeysWithoutChangingTheTotal is the invariant the whole
// feature rests on. The breakdown must be the same money as TotalCost(), keyed
// differently — if the two can drift, the cost chart and the cost total on the
// same page start disagreeing, which is precisely the class of defect this work
// exists to remove.
func TestTotalCostByModel_ReKeysWithoutChangingTheTotal(t *testing.T) {
	at := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	s := costSession("s1", "claude-opus-4-8", at,
		map[string]SessionCost{"claude-opus-4-8": cost(1, 2, 3, 4)},
		map[string]SessionCost{
			"claude-fable-5": cost(0.5, 1, 0, 0.25),
			"k3":             cost(2, 0.5, 0, 0),
		},
	)

	var summed float64
	for _, c := range s.TotalCostByModel() {
		summed += c.TotalUSD
	}
	if want := s.TotalCost().TotalUSD; math.Abs(summed-want) > 1e-9 {
		t.Errorf("per-model costs sum to %v, but TotalCost() is %v", summed, want)
	}

	// Delegated spend belongs to the sub-agent's model, not the parent's — the
	// point of the breakdown is to reveal whether delegation routes work to
	// cheaper models, which crediting the parent would hide.
	byModel := s.TotalCostByModel()
	if got := byModel["k3"].TotalUSD; math.Abs(got-2.5) > 1e-9 {
		t.Errorf("delegated k3 cost = %v, want 2.5 attributed to k3 rather than the parent", got)
	}
	if got := byModel["claude-opus-4-8"].TotalUSD; math.Abs(got-10) > 1e-9 {
		t.Errorf("parent model cost = %v, want its own 10 only", got)
	}
}

// TestBuildCostByModel_MatchesCostSummary checks the aggregate: the chart's
// slices must add up to the cost total rendered beside them.
func TestBuildCostByModel_MatchesCostSummary(t *testing.T) {
	at := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		costSession("s1", "claude-opus-4-8", at,
			map[string]SessionCost{"claude-opus-4-8": cost(1, 2, 3, 4)},
			map[string]SessionCost{"k3": cost(2, 0.5, 0, 0)}),
		costSession("s2", "k3", at.Add(time.Hour),
			map[string]SessionCost{"k3": cost(5, 1, 0, 0)}, nil),
		costSession("s3", "claude-fable-5", at.Add(2*time.Hour),
			map[string]SessionCost{"claude-fable-5": cost(0.25, 0.75, 0.1, 0)}, nil),
	}

	report := AggregateAnalytics(sessions, AnalyticsParams{
		From: at.Add(-time.Hour), To: at.Add(24 * time.Hour), Loc: time.UTC,
	})

	var charted float64
	for _, m := range report.CostByModel {
		charted += m.Cost.TotalUSD
	}
	if want := report.CostSummary.TotalCostUSD; math.Abs(charted-want) > 1e-9 {
		t.Errorf("cost-by-model sums to %v but the cost summary says %v", charted, want)
	}

	// Ordered by spend, so the model a reader is looking for is first:
	// opus-4-8 spent 10, k3 spent 8.5 across two sessions, fable-5 spent 1.1.
	if len(report.CostByModel) != 3 || report.CostByModel[0].Model != "claude-opus-4-8" {
		t.Fatalf("expected claude-opus-4-8 (10) first by spend, got %+v", report.CostByModel)
	}
	if p := report.CostByModel[0].Provider; p != "Anthropic" {
		t.Errorf("claude-opus-4-8 provider = %q, want Anthropic", p)
	}

	k3 := report.CostByModel[1]
	if k3.Model != "k3" || math.Abs(k3.Cost.TotalUSD-8.5) > 1e-9 {
		t.Errorf("second by spend = %+v, want k3 at 8.5", k3)
	}
	if k3.Provider != "Moonshot" {
		t.Errorf("k3 provider = %q, want Moonshot", k3.Provider)
	}
	if k3.Sessions != 2 {
		t.Errorf("k3 sessions = %d, want 2 (its own, plus the one it was delegated in)", k3.Sessions)
	}

	var pct float64
	for _, m := range report.CostByModel {
		pct += m.Percentage
	}
	if math.Abs(pct-100) > 0.3 { // rounded to one decimal per slice
		t.Errorf("percentages sum to %v, want ~100", pct)
	}
}

// TestBuildCostOverTimeByModel_MatchesPlainSeries pins the two cost series
// against each other: the stacked chart is a decomposition of the plain one, so
// bucket by bucket they must agree.
func TestBuildCostOverTimeByModel_MatchesPlainSeries(t *testing.T) {
	day1 := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	day3 := time.Date(2026, 8, 3, 9, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		costSession("s1", "claude-opus-4-8", day1,
			map[string]SessionCost{"claude-opus-4-8": cost(1, 2, 0, 0)},
			map[string]SessionCost{"k3": cost(1, 0, 0, 0)}),
		costSession("s2", "k3", day3, map[string]SessionCost{"k3": cost(4, 1, 0, 0)}, nil),
	}

	report := AggregateAnalytics(sessions, AnalyticsParams{
		From: day1.Add(-24 * time.Hour), To: day3.Add(24 * time.Hour), Loc: time.UTC,
	})

	if len(report.CostOverTime) != len(report.CostOverTimeByModel) {
		t.Fatalf("series lengths differ: %d plain vs %d stacked",
			len(report.CostOverTime), len(report.CostOverTimeByModel))
	}
	for i, plain := range report.CostOverTime {
		stacked := report.CostOverTimeByModel[i]
		if plain.Date != stacked.Date {
			t.Errorf("bucket %d: dates differ (%q vs %q)", i, plain.Date, stacked.Date)
		}
		var summed float64
		for _, v := range stacked.CostByModel {
			summed += v
		}
		if math.Abs(summed-plain.EstimatedCostUSD) > 1e-9 {
			t.Errorf("bucket %s: stacked sums to %v, plain says %v",
				plain.Date, summed, plain.EstimatedCostUSD)
		}
	}
}

// TestBuildCostByModel_SkipsSynthetic keeps the placeholder Claude Code records
// for locally generated events out of the spend chart, matching what the token
// breakdown does — it is billed at zero and is not a model anyone ran.
func TestBuildCostByModel_SkipsSynthetic(t *testing.T) {
	at := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	stats := buildCostByModel([]ClaudeSessionSummary{
		costSession("s1", syntheticModel, at, map[string]SessionCost{
			syntheticModel:  cost(0, 0, 0, 0),
			"claude-opus-5": cost(1, 1, 0, 0),
		}, nil),
	})

	for _, s := range stats {
		if s.Model == syntheticModel {
			t.Errorf("synthetic placeholder must not appear as a model: %+v", stats)
		}
	}
	if len(stats) != 1 {
		t.Errorf("expected the one real model, got %+v", stats)
	}
}

// TestProviderFor covers the grouping, including the single-letter Moonshot
// prefix that must not swallow other vendors' identifiers.
func TestProviderFor(t *testing.T) {
	for model, want := range map[string]string{
		"claude-opus-4-8":    "Anthropic",
		"claude-fable-5":     "Anthropic",
		"k3":                 "Moonshot",
		"kimi-k2":            "Moonshot",
		"glm-5.2":            "Z.ai",
		"qwen3-max":          "Alibaba",
		"unknown":            "Other",
		"mixedbread/embed-1": "Other",
	} {
		if got := providerFor(model); got != want {
			t.Errorf("providerFor(%q) = %q, want %q", model, got, want)
		}
	}
}

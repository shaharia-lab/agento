package claudesessions

import (
	"math"
	"testing"
	"time"
)

// cardOf returns the card of a given kind, or false when it was not emitted.
func cardOf(cards []InsightCard, kind InsightCardKind) (InsightCard, bool) {
	for _, c := range cards {
		if c.Kind == kind {
			return c, true
		}
	}
	return InsightCard{}, false
}

// TestLowCacheCard names the costliest model served almost nothing from cache
// — the fact behind the token/cost inversion, stated rather than left to be
// inferred from two charts disagreeing.
func TestLowCacheCard(t *testing.T) {
	at := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)

	caching := costSession("s1", "claude-opus-5", at,
		map[string]SessionCost{"claude-opus-5": cost(10, 5, 20, 5)}, nil)
	caching.Usage = TokenUsage{InputTokens: 1_000, CacheReadTokens: 500_000}

	uncached := costSession("s2", "k3", at,
		map[string]SessionCost{"k3": cost(30, 5, 0, 0)}, nil)
	uncached.Usage = TokenUsage{InputTokens: 2_000_000}

	cards := buildInsightCards(
		[]ClaudeSessionSummary{caching, uncached},
		buildCostByModel([]ClaudeSessionSummary{caching, uncached}),
	)

	card, ok := cardOf(cards, CardModelLowCache)
	if !ok {
		t.Fatalf("expected a low-cache card, got %+v", cards)
	}
	if card.Model != "k3" {
		t.Errorf("card names %q, want the uncached model k3", card.Model)
	}
	if card.Tokens != 2_000_000 {
		t.Errorf("card tokens = %d, want the re-billed input total", card.Tokens)
	}
	if card.Percent != 0 {
		t.Errorf("cache share = %v%%, want 0 for a model with no cache reads", card.Percent)
	}
}

// TestLowCacheCard_SkipsTrivialSpend keeps the page free of trivia: a model
// that spent pennies does not warrant a card about its caching behavior.
func TestLowCacheCard_SkipsTrivialSpend(t *testing.T) {
	at := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	tiny := costSession("s1", "glm-5.2", at,
		map[string]SessionCost{"glm-5.2": cost(0.05, 0.02, 0, 0)}, nil)
	tiny.Usage = TokenUsage{InputTokens: 900}

	sessions := []ClaudeSessionSummary{tiny}
	if _, ok := cardOf(buildInsightCards(sessions, buildCostByModel(sessions)), CardModelLowCache); ok {
		t.Error("a model with cents of spend must not produce a card")
	}
}

// TestDelegationCard reports delegated spend as money and share, which is what
// makes "is delegation routing work to cheaper models" answerable.
func TestDelegationCard(t *testing.T) {
	at := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		costSession("s1", "claude-opus-4-8", at,
			map[string]SessionCost{"claude-opus-4-8": cost(30, 10, 0, 0)},
			map[string]SessionCost{"k3": cost(8, 2, 0, 0), "claude-fable-5": cost(4, 1, 0, 0)}),
		costSession("s2", "claude-opus-5", at, map[string]SessionCost{"claude-opus-5": cost(40, 5, 0, 0)}, nil),
	}

	card, ok := cardOf(buildInsightCards(sessions, buildCostByModel(sessions)), CardDelegationMix)
	if !ok {
		t.Fatal("expected a delegation card")
	}
	if math.Abs(card.AmountUSD-15) > 1e-9 {
		t.Errorf("delegated spend = %v, want 15", card.AmountUSD)
	}
	// 15 of 100 total.
	if math.Abs(card.Percent-15) > 0.05 {
		t.Errorf("delegated share = %v%%, want 15", card.Percent)
	}
	if card.Model != "k3" {
		t.Errorf("top delegated model = %q, want k3 (10 vs 5)", card.Model)
	}
	if card.Count != 1 {
		t.Errorf("delegating sessions = %d, want 1", card.Count)
	}
}

// TestExpensiveSessionsCard concentrates attention where a habit change pays.
func TestExpensiveSessionsCard(t *testing.T) {
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)
	sessions := make([]ClaudeSessionSummary, 0, 10)
	// Five big sessions of two hours each, plus five trivial ones.
	for i := range 5 {
		s := costSession("big"+itoa(i), "claude-opus-5", at,
			map[string]SessionCost{"claude-opus-5": cost(100, 0, 0, 0)}, nil)
		s.StartTime = at
		s.LastActivity = at.Add(2 * time.Hour)
		sessions = append(sessions, s)
	}
	for i := range 5 {
		sessions = append(sessions, costSession("small"+itoa(i), "claude-opus-5", at,
			map[string]SessionCost{"claude-opus-5": cost(20, 0, 0, 0)}, nil))
	}

	card, ok := cardOf(buildInsightCards(sessions, buildCostByModel(sessions)), CardExpensiveSessions)
	if !ok {
		t.Fatal("expected an expensive-sessions card")
	}
	if math.Abs(card.AmountUSD-500) > 1e-9 {
		t.Errorf("top-5 spend = %v, want 500", card.AmountUSD)
	}
	// 500 of 600.
	if math.Abs(card.Percent-83.3) > 0.1 {
		t.Errorf("share = %v%%, want ~83.3", card.Percent)
	}
	if card.AvgDurationMs != (2 * time.Hour).Milliseconds() {
		t.Errorf("avg duration = %dms, want two hours", card.AvgDurationMs)
	}
}

// TestExpensiveSessionsCard_NeedsEnoughSessions avoids a card claiming "your 5
// most expensive sessions" when there are three.
func TestExpensiveSessionsCard_NeedsEnoughSessions(t *testing.T) {
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		costSession("a", "claude-opus-5", at, map[string]SessionCost{"claude-opus-5": cost(100, 0, 0, 0)}, nil),
	}
	if _, ok := cardOf(buildInsightCards(sessions, buildCostByModel(sessions)), CardExpensiveSessions); ok {
		t.Error("a one-session window must not produce a top-5 card")
	}
}

// TestTotalUsageByModel is the per-model token attribution the caching cards
// rest on: delegated tokens belong to the sub-agent's model.
func TestTotalUsageByModel(t *testing.T) {
	s := ClaudeSessionSummary{
		Model:         "claude-opus-5",
		Usage:         TokenUsage{InputTokens: 100, CacheReadTokens: 900},
		SubagentCount: 1,
		SubagentUsage: TokenUsage{InputTokens: 50},
		SubagentUsageByModel: map[string]TokenUsage{
			"k3": {InputTokens: 50},
		},
	}

	byModel := s.TotalUsageByModel()
	if byModel["claude-opus-5"].CacheReadTokens != 900 {
		t.Errorf("parent cache reads = %d, want 900", byModel["claude-opus-5"].CacheReadTokens)
	}
	if byModel["k3"].InputTokens != 50 {
		t.Errorf("delegated input = %d, want 50 under k3", byModel["k3"].InputTokens)
	}
	if byModel["claude-opus-5"].InputTokens != 100 {
		t.Errorf("parent input = %d, want its own 100 only", byModel["claude-opus-5"].InputTokens)
	}
}

// TestTotalUsageByModel_FallsBackWhenBreakdownMissing keeps delegated tokens
// counted when the per-model breakdown was not loaded, matching
// buildModelBreakdown rather than silently dropping them.
func TestTotalUsageByModel_FallsBackWhenBreakdownMissing(t *testing.T) {
	s := ClaudeSessionSummary{
		Model:         "claude-opus-5",
		Usage:         TokenUsage{InputTokens: 100},
		SubagentCount: 1,
		SubagentUsage: TokenUsage{InputTokens: 40},
	}
	if got := s.TotalUsageByModel()["claude-opus-5"].InputTokens; got != 140 {
		t.Errorf("input = %d, want 140 — delegated tokens must not be dropped", got)
	}
}

package claudesessions

import (
	"math"
	"sort"
)

// Insight cards: a specific fact with a number attached, replacing a 0–100
// composite grade.
//
// The grade it replaces was `0.5·avg(autonomy) + 0.3·avg(cache-hit)·100 +
// 0.2·error-free%`, over three unweighted per-session averages, with arbitrary
// weights and one broken component. A user reading "58/100 Moderate" learns
// nothing they can act on. "Cache reads saved you $X against uncached input"
// and "this model is served almost nothing from cache" name something specific
// and something to do about it.
//
// Every card is computed from data already stored — no new scanning — and the
// backend produces the numbers while the frontend does the phrasing, so the
// arithmetic is testable and the copy stays where copy belongs.

// InsightCardKind identifies which fact a card carries. The frontend switches
// on it to phrase the card.
type InsightCardKind string

const (
	// CardCacheSavings is what cache reads saved against paying input rates for
	// the same tokens.
	CardCacheSavings InsightCardKind = "cache_savings"
	// CardModelLowCache names a model with real spend that is served almost
	// nothing from cache, so its context is re-billed as fresh input every turn.
	CardModelLowCache InsightCardKind = "model_low_cache"
	// CardDelegationMix is how much of the window's spend was delegated, and to
	// which model most of it went.
	CardDelegationMix InsightCardKind = "delegation_mix"
	// CardExpensiveSessions is what the priciest handful of sessions cost
	// together, and how long they ran.
	CardExpensiveSessions InsightCardKind = "expensive_sessions"
)

// InsightCard is one fact. Fields not relevant to a Kind are left zero; the
// frontend reads only the ones its phrasing for that Kind uses.
type InsightCard struct {
	Kind InsightCardKind `json:"kind"`
	// AmountUSD is the card's money figure: savings, spend, or delegated cost.
	AmountUSD float64 `json:"amount_usd,omitempty"`
	// Percent is its share figure, 0–100.
	Percent float64 `json:"percent,omitempty"`
	// Count is a session, model or token count depending on Kind.
	Count int `json:"count,omitempty"`
	// Model names the model a card is about.
	Model string `json:"model,omitempty"`
	// Tokens is the token figure behind AmountUSD, when there is one.
	Tokens int `json:"tokens,omitempty"`
	// AvgDurationMs is the mean active duration of the sessions a card covers —
	// idle gaps above IdleGapThreshold excluded, delegated work included.
	AvgDurationMs int64 `json:"avg_duration_ms,omitempty"`
	// ComparisonUSD is the figure AmountUSD should be read against — for cache
	// savings, what the window actually cost. A saving of $102k means nothing
	// until you know the bill was $20k.
	ComparisonUSD float64 `json:"comparison_usd,omitempty"`
	// Estimated marks a figure derived from list rates rather than read from a
	// stored total, so the UI can say "about" and mean it.
	Estimated bool `json:"estimated,omitempty"`
}

// lowCacheShare is the read share below which a model is worth a card.
//
// Not zero, and not a near-zero epsilon: on the reference corpus the
// non-caching backend still shows 2.8% cache reads, because a handful of its
// sessions routed elsewhere, while every caching model sits above 99%. The gap
// between those two populations is enormous, so any threshold in the middle
// separates them — and a card that says "served only 2.8% from cache" is both
// true and more useful than one that would have to claim "never".
const lowCacheShare = 0.5

// expensiveSessionSample is how many top sessions the expensive-sessions card
// characterizes. Small enough that "these specific runs" is actionable.
const expensiveSessionSample = 5

// minCardCostUSD keeps trivia off the page: a model that spent a few cents is
// not worth a card about its caching behavior.
const minCardCostUSD = 1.0

// buildInsightCards derives the cards from the filtered window.
//
// A card is emitted only when its fact is true and material; there is no
// placeholder or "nothing to report" card, because a page of empty cards is
// what a composite score already was.
func buildInsightCards(sessions []ClaudeSessionSummary, costByModel []ModelCostStat) []InsightCard {
	cards := make([]InsightCard, 0, 4)

	if card, ok := cacheSavingsCard(sessions); ok {
		cards = append(cards, card)
	}
	if card, ok := lowCacheCard(sessions, costByModel); ok {
		cards = append(cards, card)
	}
	if card, ok := delegationCard(sessions); ok {
		cards = append(cards, card)
	}
	if card, ok := expensiveSessionsCard(sessions); ok {
		cards = append(cards, card)
	}
	return cards
}

// cacheSavingsCard estimates what cache reads saved against paying the input
// rate for the same tokens.
//
// This is a savings *estimate* and is marked as one: unlike every other cost
// figure on the dashboards, it prices a counterfactual — tokens that were never
// billed at the input rate — so it cannot come from a stored total. It is still
// computed the same way real cost is, per session at that session's own model
// and instant, rather than by picking one rate for the whole window.
func cacheSavingsCard(sessions []ClaudeSessionSummary) (InsightCard, bool) {
	resolver := defaultPricingResolver()
	if resolver == nil {
		return InsightCard{}, false
	}

	savings, tokens, actual := 0.0, 0, 0.0
	for _, s := range sessions {
		actual += s.TotalCost().TotalUSD
		for model, u := range s.TotalUsageByModel() {
			if u.CacheReadTokens == 0 {
				continue
			}
			res, ok := resolver.Resolve(model, s.LastActivity)
			if !ok || !res.Rate.Billable {
				continue
			}
			perToken := (res.Rate.InputPerMTok - res.Rate.CacheReadPerMTok) / 1_000_000
			if perToken <= 0 {
				continue // a provider whose cached reads cost no less than input
			}
			savings += float64(u.CacheReadTokens) * perToken
			tokens += u.CacheReadTokens
		}
	}

	if savings < minCardCostUSD {
		return InsightCard{}, false
	}
	return InsightCard{
		Kind:          CardCacheSavings,
		AmountUSD:     savings,
		ComparisonUSD: actual,
		Tokens:        tokens,
		Estimated:     true,
	}, true
}

// lowCacheCard names the costliest model that is served almost nothing from
// cache, so its context is re-billed as fresh input on every turn.
//
// This is the fact behind the token/cost inversion on the model charts, stated
// directly instead of left for a reader to infer from two charts disagreeing:
// the model with the most tokens can be a small share of the bill precisely
// because those tokens are cheap cache reads, and vice versa.
func lowCacheCard(sessions []ClaudeSessionSummary, costByModel []ModelCostStat) (InsightCard, bool) {
	cacheReads := map[string]int{}
	inputTokens := map[string]int{}
	for _, s := range sessions {
		for model, u := range s.TotalUsageByModel() {
			cacheReads[model] += u.CacheReadTokens
			inputTokens[model] += u.InputTokens
		}
	}

	// costByModel is already ordered by spend, so the first match is the one
	// worth naming.
	for _, m := range costByModel {
		if m.Cost.TotalUSD < minCardCostUSD {
			break
		}
		inputSide := cacheReads[m.Model] + inputTokens[m.Model]
		if inputSide == 0 {
			continue
		}
		share := float64(cacheReads[m.Model]) / float64(inputSide)
		if share < lowCacheShare {
			return InsightCard{
				Kind:  CardModelLowCache,
				Model: m.Model,
				// AmountUSD is what the model spent; Percent is how much of its
				// input side came from cache, which is the number the card is
				// about. Its share of total spend is on the cost chart already.
				AmountUSD: m.Cost.TotalUSD,
				Percent:   math.Round(share*1000) / 10,
				Tokens:    inputTokens[m.Model],
			}, true
		}
	}
	return InsightCard{}, false
}

// delegationCard reports how much of the window's spend was delegated to
// sub-agents, and which model took most of it.
//
// "Is delegation routing work to cheaper models" is the question the per-model
// attribution exists to answer; this states the answer in dollars rather than
// leaving it to be read off a chart.
func delegationCard(sessions []ClaudeSessionSummary) (InsightCard, bool) {
	delegated, total := 0.0, 0.0
	byModel := map[string]float64{}
	delegatingSessions := 0

	for _, s := range sessions {
		total += s.TotalCost().TotalUSD
		if s.SubagentCount == 0 {
			continue
		}
		delegatingSessions++
		delegated += s.SubagentCost.TotalUSD
		for model, c := range s.SubagentCostByModel {
			byModel[model] += c.TotalUSD
		}
	}

	if delegated < minCardCostUSD || total <= 0 {
		return InsightCard{}, false
	}

	topModel, topCost := "", 0.0
	for model, cost := range byModel {
		if cost > topCost {
			topModel, topCost = model, cost
		}
	}

	return InsightCard{
		Kind:      CardDelegationMix,
		AmountUSD: delegated,
		Percent:   math.Round(delegated/total*1000) / 10,
		Count:     delegatingSessions,
		Model:     topModel,
	}, true
}

// expensiveSessionsCard characterizes the priciest handful of sessions: what
// they cost together, what share of the window that is, and how long they ran.
//
// A concentrated share is the actionable part — if five sessions are a third of
// the bill, they are where a change of habit pays.
func expensiveSessionsCard(sessions []ClaudeSessionSummary) (InsightCard, bool) {
	if len(sessions) < expensiveSessionSample {
		return InsightCard{}, false
	}

	costs := make([]ClaudeSessionSummary, len(sessions))
	copy(costs, sessions)
	sort.Slice(costs, func(i, j int) bool {
		return costs[i].TotalCost().TotalUSD > costs[j].TotalCost().TotalUSD
	})

	total := 0.0
	for _, s := range sessions {
		total += s.TotalCost().TotalUSD
	}

	top, durationMs := 0.0, int64(0)
	for _, s := range costs[:expensiveSessionSample] {
		top += s.TotalCost().TotalUSD
		// Active time, not the start/last span: expensive sessions are exactly
		// the long-lived ones people resume, and a span that includes the idle
		// week between sittings would make this card's "ran Xh on average"
		// meaningless.
		durationMs += s.TotalActiveDurationMs()
	}

	if top < minCardCostUSD || total <= 0 {
		return InsightCard{}, false
	}
	return InsightCard{
		Kind:          CardExpensiveSessions,
		AmountUSD:     top,
		Percent:       math.Round(top/total*1000) / 10,
		Count:         expensiveSessionSample,
		AvgDurationMs: durationMs / expensiveSessionSample,
	}, true
}

package claudesessions

// TokenProfileProcessor accumulates token usage across all assistant messages
// and derives cache efficiency and cost estimates.
//
// Cost is accumulated per assistant message against the pricing catalog, using
// each message's own model and timestamp — the same resolver the session
// scanner uses, so a session's insight cost and its analytics cost cannot
// diverge over first-seen-model or price-boundary differences.
type TokenProfileProcessor struct {
	inputTokens     int
	outputTokens    int
	cacheCreation   int
	cacheCreation5m int
	cacheCreation1h int
	cacheRead       int
	model           string
	costs           *costAccumulator
}

// Name returns the processor identifier.
func (p *TokenProfileProcessor) Name() string { return "token_profile" }

// Process collects usage from assistant events.
func (p *TokenProfileProcessor) Process(ev ProcessableEvent) {
	if ev.Message == nil || ev.Message.Role != "assistant" {
		return
	}
	if p.model == "" && ev.Message.Model != "" {
		p.model = ev.Message.Model
	}
	if ev.Message.Usage == nil {
		return
	}
	u := ev.Message.Usage
	fiveMin, oneHour := u.Split()
	p.inputTokens += u.InputTokens
	p.outputTokens += u.OutputTokens
	p.cacheCreation += u.CacheCreationInputTokens
	p.cacheCreation5m += fiveMin
	p.cacheCreation1h += oneHour
	p.cacheRead += u.CacheReadInputTokens
	if p.costs == nil {
		p.costs = newCostAccumulator(defaultPricingResolver())
	}
	p.costs.addAssistantMessage(ev.Message.Model, TokenUsage{
		InputTokens:           u.InputTokens,
		OutputTokens:          u.OutputTokens,
		CacheCreationTokens:   u.CacheCreationInputTokens,
		CacheCreation5mTokens: fiveMin,
		CacheCreation1hTokens: oneHour,
		CacheReadTokens:       u.CacheReadInputTokens,
	}, ev.Timestamp)
}

// Finalize writes CacheHitRate, TokensPerTurnAvg, and CostEstimateUSD into the insight.
func (p *TokenProfileProcessor) Finalize(insight *SessionInsight) {
	// Via the shared definition, so this and the analytics dashboard cannot
	// report two different numbers under the same name again.
	insight.CacheHitRate = CacheHitRate(p.inputTokens, p.cacheRead, p.cacheCreation)

	totalTokens := p.inputTokens + p.outputTokens
	if insight.TurnCount > 0 {
		insight.TokensPerTurnAvg = float64(totalTokens) / float64(insight.TurnCount)
	}

	// Sessions with no usage-bearing messages — or run without a pricing
	// resolver — leave the estimate at zero, matching the pre-#186 semantics
	// where an unpriced model contributes no cost.
	if p.costs != nil && p.costs.pricedMessages > 0 {
		insight.CostEstimateUSD = p.costs.cost.TotalCostUSD
	}
}

// Reset clears all internal state.
func (p *TokenProfileProcessor) Reset() {
	p.inputTokens = 0
	p.outputTokens = 0
	p.cacheCreation = 0
	p.cacheCreation5m = 0
	p.cacheCreation1h = 0
	p.cacheRead = 0
	p.model = ""
	p.costs = nil
}

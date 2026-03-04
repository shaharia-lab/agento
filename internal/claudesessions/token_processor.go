package claudesessions

// TokenProfileProcessor accumulates token usage across all assistant messages
// and derives cache efficiency and cost estimates.
//
// Pricing tiers (per 1M tokens) are shared with analytics.go via pricingTable.
type TokenProfileProcessor struct {
	inputTokens   int
	outputTokens  int
	cacheCreation int
	cacheRead     int
	model         string
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
	p.inputTokens += u.InputTokens
	p.outputTokens += u.OutputTokens
	p.cacheCreation += u.CacheCreationInputTokens
	p.cacheRead += u.CacheReadInputTokens
}

// Finalize writes CacheHitRate, TokensPerTurnAvg, and CostEstimateUSD into the insight.
func (p *TokenProfileProcessor) Finalize(insight *SessionInsight) {
	cacheTotal := p.cacheCreation + p.cacheRead
	if cacheTotal > 0 {
		insight.CacheHitRate = float64(p.cacheRead) / float64(cacheTotal)
	}

	totalTokens := p.inputTokens + p.outputTokens
	if insight.TurnCount > 0 {
		insight.TokensPerTurnAvg = float64(totalTokens) / float64(insight.TurnCount)
	}

	pr := pricingForModel(p.model)
	insight.CostEstimateUSD = (float64(p.inputTokens)*pr.InputPerMTok +
		float64(p.outputTokens)*pr.OutputPerMTok) / 1_000_000
}

// Reset clears all internal state.
func (p *TokenProfileProcessor) Reset() {
	p.inputTokens = 0
	p.outputTokens = 0
	p.cacheCreation = 0
	p.cacheRead = 0
	p.model = ""
}

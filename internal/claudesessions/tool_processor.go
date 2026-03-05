package claudesessions

// ToolUsageProcessor tracks how many tool calls were made and which tools
// were used.
//
// - tool_use blocks in assistant messages → per-tool counter increments
//
// Error tracking is intentionally left to ErrorRateProcessor to avoid
// duplicate logic and ensure a single source of truth for error metrics.
type ToolUsageProcessor struct {
	toolBreakdown map[string]int
}

// Name returns the processor identifier.
func (p *ToolUsageProcessor) Name() string { return "tool_usage" }

// Process inspects content blocks in assistant messages (tool_use) to build
// the tool usage profile.
func (p *ToolUsageProcessor) Process(ev ProcessableEvent) {
	if ev.Message == nil {
		return
	}
	if ev.Message.Role != "assistant" {
		return
	}
	if p.toolBreakdown == nil {
		p.toolBreakdown = make(map[string]int)
	}
	for _, b := range parseContentBlocks(ev.Message.Content) {
		if b.Type == "tool_use" && b.Name != "" {
			p.toolBreakdown[b.Name]++
		}
	}
}

// Finalize writes ToolCallsTotal and ToolBreakdown into the insight.
func (p *ToolUsageProcessor) Finalize(insight *SessionInsight) {
	total := 0
	for _, count := range p.toolBreakdown {
		total += count
	}
	insight.ToolCallsTotal = total

	if insight.ToolBreakdown == nil {
		insight.ToolBreakdown = make(map[string]int)
	}
	for k, v := range p.toolBreakdown {
		insight.ToolBreakdown[k] = v
	}
}

// Reset clears all internal state.
func (p *ToolUsageProcessor) Reset() {
	p.toolBreakdown = make(map[string]int)
}

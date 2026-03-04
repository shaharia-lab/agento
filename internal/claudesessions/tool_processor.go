package claudesessions

// ToolUsageProcessor tracks how many tool calls were made and which tools
// were used, as well as the overall tool error rate.
//
// - tool_use blocks in assistant messages → per-tool counter increments
// - tool_result blocks in user messages with is_error:true → error counter
type ToolUsageProcessor struct {
	toolBreakdown    map[string]int
	totalToolResults int
	toolErrors       int
}

// Name returns the processor identifier.
func (p *ToolUsageProcessor) Name() string { return "tool_usage" }

// Process inspects content blocks in assistant messages (tool_use) and user
// messages (tool_result) to build the tool usage profile.
func (p *ToolUsageProcessor) Process(ev ProcessableEvent) {
	if ev.Message == nil {
		return
	}
	blocks := parseContentBlocks(ev.Message.Content)
	switch ev.Message.Role {
	case "assistant":
		p.processAssistantBlocks(blocks)
	case "user":
		p.processUserBlocks(blocks)
	}
}

func (p *ToolUsageProcessor) processAssistantBlocks(blocks []contentBlock) {
	for _, b := range blocks {
		if b.Type == "tool_use" && b.Name != "" {
			p.toolBreakdown[b.Name]++
		}
	}
}

func (p *ToolUsageProcessor) processUserBlocks(blocks []contentBlock) {
	for _, b := range blocks {
		if b.Type == "tool_result" {
			p.totalToolResults++
			if b.IsError {
				p.toolErrors++
			}
		}
	}
}

// Finalize writes ToolCallsTotal, ToolBreakdown, and ToolErrorRate into the insight.
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

	if p.totalToolResults > 0 {
		insight.ToolErrorRate = float64(p.toolErrors) / float64(p.totalToolResults)
	}
}

// Reset clears all internal state.
func (p *ToolUsageProcessor) Reset() {
	p.toolBreakdown = make(map[string]int)
	p.totalToolResults = 0
	p.toolErrors = 0
}

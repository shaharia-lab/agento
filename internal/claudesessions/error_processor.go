package claudesessions

// ErrorRateProcessor counts tool errors and derives the overall error rate.
// It is intentionally separate from ToolUsageProcessor for clarity.
//
// - Counts tool_result blocks in user messages where is_error:true.
// - tool_error_rate = error_count / total_tool_results
// - has_errors is true if any tool call returned an error.
type ErrorRateProcessor struct {
	errorCount       int
	totalToolResults int
}

// Name returns the processor identifier.
func (p *ErrorRateProcessor) Name() string { return "error_rate" }

// Process inspects user message content for tool_result blocks.
func (p *ErrorRateProcessor) Process(ev ProcessableEvent) {
	if ev.Message == nil || ev.Message.Role != "user" {
		return
	}
	for _, b := range parseContentBlocks(ev.Message.Content) {
		if b.Type == "tool_result" {
			p.totalToolResults++
			if b.IsError {
				p.errorCount++
			}
		}
	}
}

// Finalize writes ToolErrorCount, ToolErrorRate, and HasErrors into the insight.
func (p *ErrorRateProcessor) Finalize(insight *SessionInsight) {
	insight.ToolErrorCount = p.errorCount
	insight.HasErrors = p.errorCount > 0
	if p.totalToolResults > 0 {
		insight.ToolErrorRate = float64(p.errorCount) / float64(p.totalToolResults)
	}
}

// Reset clears all internal state.
func (p *ErrorRateProcessor) Reset() {
	p.errorCount = 0
	p.totalToolResults = 0
}

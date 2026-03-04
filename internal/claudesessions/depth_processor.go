package claudesessions

// ConversationDepthProcessor measures how deeply Claude goes without user
// intervention.
//
//   - MaxConsecutiveToolCalls: longest unbroken run of tool_use blocks within a
//     single assistant message; a non-tool_use block (e.g. text) resets the run.
//   - LongestAutonomousChain: longest unbroken sequence of tool calls across the
//     entire session without a genuine user message in between.
type ConversationDepthProcessor struct {
	maxConsecutive int // within a single assistant message
	longestChain   int // across session
	currentChain   int // running tally of autonomous tool calls
}

// Name returns the processor identifier.
func (p *ConversationDepthProcessor) Name() string { return "conversation_depth" }

// Process tracks tool_use sequences inside assistant messages and resets the
// autonomous chain counter when genuine user input arrives.
func (p *ConversationDepthProcessor) Process(ev ProcessableEvent) {
	switch ev.Type {
	case "assistant":
		if ev.Message != nil {
			p.processAssistantMessage(parseContentBlocks(ev.Message.Content))
		}
	case "user":
		if isTurnStart(ev) {
			// Genuine user input breaks the autonomous chain.
			p.currentChain = 0
		}
	}
}

// processAssistantMessage scans content blocks counting consecutive tool_use runs.
// A non-tool_use block (e.g. text) resets the per-message consecutive counter.
func (p *ConversationDepthProcessor) processAssistantMessage(blocks []contentBlock) {
	consecutive := 0
	for _, b := range blocks {
		if b.Type == "tool_use" {
			consecutive++
			p.currentChain++
		} else {
			if consecutive > p.maxConsecutive {
				p.maxConsecutive = consecutive
			}
			consecutive = 0
		}
	}
	if consecutive > p.maxConsecutive {
		p.maxConsecutive = consecutive
	}
	if p.currentChain > p.longestChain {
		p.longestChain = p.currentChain
	}
}

// Finalize writes MaxConsecutiveToolCalls and LongestAutonomousChain into the insight.
func (p *ConversationDepthProcessor) Finalize(insight *SessionInsight) {
	insight.MaxConsecutiveToolCalls = p.maxConsecutive
	insight.LongestAutonomousChain = p.longestChain
}

// Reset clears all internal state.
func (p *ConversationDepthProcessor) Reset() {
	p.maxConsecutive = 0
	p.longestChain = 0
	p.currentChain = 0
}

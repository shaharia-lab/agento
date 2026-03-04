package claudesessions

// ConversationDepthProcessor measures how deeply Claude goes without user
// intervention.
//
//   - MaxConsecutiveToolCalls: longest unbroken run of tool_use blocks within a
//     single assistant message (how deep Claude goes in one shot).
//   - LongestAutonomousChain: longest unbroken sequence of tool calls across the
//     entire session without a genuine user message in between.
type ConversationDepthProcessor struct {
	maxConsecutive                int // within a single assistant message
	longestChain                  int // across session
	currentChain                  int // running tally of autonomous tool calls
	sawGenuineUserSinceChainStart bool
}

// Name returns the processor identifier.
func (p *ConversationDepthProcessor) Name() string { return "conversation_depth" }

// Process tracks tool_use sequences inside assistant messages and resets the
// autonomous chain counter when genuine user input arrives.
func (p *ConversationDepthProcessor) Process(ev ProcessableEvent) {
	switch ev.Type {
	case "assistant":
		if ev.Message == nil {
			return
		}
		blocks := parseContentBlocks(ev.Message.Content)
		consecutive := 0
		for _, b := range blocks {
			if b.Type == "tool_use" {
				consecutive++
				p.currentChain++
			}
		}
		if consecutive > p.maxConsecutive {
			p.maxConsecutive = consecutive
		}
		if p.currentChain > p.longestChain {
			p.longestChain = p.currentChain
		}
	case "user":
		if isTurnStart(ev) {
			// Genuine user input breaks the autonomous chain.
			p.currentChain = 0
		}
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
	p.sawGenuineUserSinceChainStart = false
}

package claudesessions

// TurnCountProcessor counts genuine user turns and the average steps per turn.
//
// A "turn" begins each time a non-sidechain user message is received that is
// NOT a programmatic tool_result reply. A "step" is any non-skipped event.
// StepsPerTurnAvg = totalEvents / turnCount.
type TurnCountProcessor struct {
	turnCount   int
	totalEvents int
}

// Name returns the processor identifier.
func (p *TurnCountProcessor) Name() string { return "turn_count" }

// Process increments the event and turn counters for each event.
func (p *TurnCountProcessor) Process(ev ProcessableEvent) {
	p.totalEvents++
	if isTurnStart(ev) {
		p.turnCount++
	}
}

// Finalize writes TurnCount and StepsPerTurnAvg into the insight.
func (p *TurnCountProcessor) Finalize(insight *SessionInsight) {
	insight.TurnCount = p.turnCount
	if p.turnCount > 0 {
		insight.StepsPerTurnAvg = float64(p.totalEvents) / float64(p.turnCount)
	}
}

// Reset clears all internal state.
func (p *TurnCountProcessor) Reset() {
	p.turnCount = 0
	p.totalEvents = 0
}

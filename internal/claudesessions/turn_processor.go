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
//
// A zero-turn session divides by one, not by zero. Since #226 excluded the
// injected wrappers, a session driven entirely by one skill invocation has no
// genuine user event at all — the user's argument is embedded inside the
// preamble block — so turnCount is legitimately 0 for real, often very long
// sessions. Leaving StepsPerTurnAvg at 0 for those would report the work as
// not having happened, and AutonomyScore reads this value: it would score the
// most autonomous sessions in the corpus 0, the wrong extreme. One unattended
// run of n steps is n steps per turn, which is what turnCount == 1 already
// means to every consumer.
func (p *TurnCountProcessor) Finalize(insight *SessionInsight) {
	insight.TurnCount = p.turnCount
	insight.StepsPerTurnAvg = float64(p.totalEvents) / float64(max(1, p.turnCount))
}

// Reset clears all internal state.
func (p *TurnCountProcessor) Reset() {
	p.turnCount = 0
	p.totalEvents = 0
}

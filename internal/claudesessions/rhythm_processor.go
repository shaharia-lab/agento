package claudesessions

import "time"

// SessionRhythmProcessor measures the pacing of the conversation by tracking
// how long users and Claude each take to respond.
//
//   - AvgUserResponseTimeMs: mean gap between the last assistant event and the
//     next genuine user input event (how quickly the user reacts).
//   - AvgClaudeResponseTimeMs: mean gap between a genuine user input and the
//     first assistant event that follows it (how quickly Claude starts responding).
//
// Gaps above IdleGapThreshold are excluded from both averages rather than
// capped. A message sent after lunch, overnight, or on resuming the session
// days later is the start of a new sitting, not a reply — the corpus contained
// a 226-hour "user response time" from exactly that — and capping would only
// make every resumed session's average converge on the cap. On the Claude
// side a gap that large is a queued-message artifact (the event carries the
// typed-at timestamp, delivery came after the running turn finished), not a
// response Claude took hours to start.
type SessionRhythmProcessor struct {
	lastAssistantTS   time.Time
	lastGenuineUserTS time.Time

	userResponseGaps   []int64 // milliseconds
	claudeResponseGaps []int64 // milliseconds

	// idleGapMs caches the configurable threshold for this processor's
	// lifetime, which is one session: a settings save landing mid-pass must
	// not judge two gaps of the same conversation by different rules.
	idleGapMs int64
}

// maxGapMs is the largest gap this pass still counts as a reply, resolved on
// first use. Zero is never a valid threshold, so it doubles as "unresolved".
func (p *SessionRhythmProcessor) maxGapMs() int64 {
	if p.idleGapMs == 0 {
		p.idleGapMs = IdleGapThreshold().Milliseconds()
	}
	return p.idleGapMs
}

// Name returns the processor identifier.
func (p *SessionRhythmProcessor) Name() string { return "session_rhythm" }

// Process records timing gaps between user and assistant events.
func (p *SessionRhythmProcessor) Process(ev ProcessableEvent) {
	if ev.Timestamp.IsZero() {
		return
	}

	switch ev.Type {
	case "user":
		p.processUser(ev)
	case "assistant":
		p.processAssistant(ev)
	}
}

// processUser measures how long the user took to respond after Claude finished.
func (p *SessionRhythmProcessor) processUser(ev ProcessableEvent) {
	if !isTurnStart(ev) {
		return
	}
	if !p.lastAssistantTS.IsZero() {
		gap := ev.Timestamp.Sub(p.lastAssistantTS).Milliseconds()
		if gap >= 0 && gap <= p.maxGapMs() {
			p.userResponseGaps = append(p.userResponseGaps, gap)
		}
	}
	p.lastGenuineUserTS = ev.Timestamp
}

// processAssistant measures how quickly Claude started responding after the user.
func (p *SessionRhythmProcessor) processAssistant(ev ProcessableEvent) {
	if !p.lastGenuineUserTS.IsZero() {
		gap := ev.Timestamp.Sub(p.lastGenuineUserTS).Milliseconds()
		if gap >= 0 {
			if gap <= p.maxGapMs() {
				p.claudeResponseGaps = append(p.claudeResponseGaps, gap)
			}
			// Consume the pair even when the gap was an artifact, so a later
			// assistant event is not measured against it too.
			p.lastGenuineUserTS = time.Time{}
		}
	}
	p.lastAssistantTS = ev.Timestamp
}

// Finalize writes AvgUserResponseTimeMs and AvgClaudeResponseTimeMs into the insight.
func (p *SessionRhythmProcessor) Finalize(insight *SessionInsight) {
	insight.AvgUserResponseTimeMs = avg(p.userResponseGaps)
	insight.AvgClaudeResponseTimeMs = avg(p.claudeResponseGaps)
}

// Reset clears all internal state.
func (p *SessionRhythmProcessor) Reset() {
	p.lastAssistantTS = time.Time{}
	p.lastGenuineUserTS = time.Time{}
	p.userResponseGaps = nil
	p.claudeResponseGaps = nil
}

// avg returns the mean of a slice, or 0 if empty.
func avg(vals []int64) float64 {
	if len(vals) == 0 {
		return 0
	}
	var sum int64
	for _, v := range vals {
		sum += v
	}
	return float64(sum) / float64(len(vals))
}

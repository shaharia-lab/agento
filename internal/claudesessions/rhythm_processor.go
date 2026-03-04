package claudesessions

import "time"

// SessionRhythmProcessor measures the pacing of the conversation by tracking
// how long users and Claude each take to respond.
//
//   - AvgUserResponseTimeMs: mean gap between the last assistant event and the
//     next genuine user input event (how quickly the user reacts).
//   - AvgClaudeResponseTimeMs: mean gap between a genuine user input and the
//     first assistant event that follows it (how quickly Claude starts responding).
type SessionRhythmProcessor struct {
	lastAssistantTS   time.Time
	lastGenuineUserTS time.Time

	userResponseGaps   []int64 // milliseconds
	claudeResponseGaps []int64 // milliseconds
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
		if !isTurnStart(ev) {
			return
		}
		// How long did the user take to respond after Claude finished?
		if !p.lastAssistantTS.IsZero() {
			gap := ev.Timestamp.Sub(p.lastAssistantTS).Milliseconds()
			if gap >= 0 {
				p.userResponseGaps = append(p.userResponseGaps, gap)
			}
		}
		p.lastGenuineUserTS = ev.Timestamp

	case "assistant":
		// How quickly did Claude start responding after the user?
		if !p.lastGenuineUserTS.IsZero() {
			gap := ev.Timestamp.Sub(p.lastGenuineUserTS).Milliseconds()
			if gap >= 0 {
				p.claudeResponseGaps = append(p.claudeResponseGaps, gap)
				// Only record once per user→assistant pair.
				p.lastGenuineUserTS = time.Time{}
			}
		}
		p.lastAssistantTS = ev.Timestamp
	}
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
	p.userResponseGaps = p.userResponseGaps[:0]
	p.claudeResponseGaps = p.claudeResponseGaps[:0]
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

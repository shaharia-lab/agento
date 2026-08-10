package claudesessions

import (
	"time"
)

// TimeProfileProcessor computes the session's time figures:
//
//   - TotalDurationMs = last_event_timestamp − first_event_timestamp: the raw
//     wall-clock span, honest as "first seen → last touched" but nothing else.
//     A resumed session's span contains every idle day in between.
//   - ActiveDurationMs = inter-event gaps capped at IdleGapThreshold: time the
//     session was actually being worked, the figure dashboards should average.
//   - ClaudeWorkingTimeMs = the subset of active time spent producing
//     assistant output, measured from event timestamps.
//
// ClaudeWorkingTimeMs replaces the old ThinkingTimeMs, which was built on
// `system` events with subtype "turn_duration" that modern Claude Code never
// emits (the fallback guessed 0.5ms per thinking character; the maximum it
// ever produced across a 1,071-session corpus was 26 seconds). Measured gap
// attribution needs no fallback and covers delegated work, because sub-agent
// transcripts feed the same processor instance.
type TimeProfileProcessor struct {
	firstTS time.Time
	lastTS  time.Time
	tracker activeTimeTracker
}

// Name returns the processor identifier.
func (p *TimeProfileProcessor) Name() string { return "time_profile" }

// Process tracks first/last timestamps and feeds the active-time tracker.
func (p *TimeProfileProcessor) Process(ev ProcessableEvent) {
	if ev.Timestamp.IsZero() {
		return
	}

	if p.firstTS.IsZero() || ev.Timestamp.Before(p.firstTS) {
		p.firstTS = ev.Timestamp
	}
	if ev.Timestamp.After(p.lastTS) {
		p.lastTS = ev.Timestamp
	}

	p.tracker.observe(ev.Timestamp, ev.Type == "assistant")
}

// Finalize writes TotalDurationMs, ActiveDurationMs and ClaudeWorkingTimeMs
// into the insight.
func (p *TimeProfileProcessor) Finalize(insight *SessionInsight) {
	if !p.firstTS.IsZero() && !p.lastTS.IsZero() {
		insight.TotalDurationMs = p.lastTS.Sub(p.firstTS).Milliseconds()
	}
	insight.ActiveDurationMs, insight.ClaudeWorkingTimeMs = p.tracker.durations()
}

// Reset clears all internal state.
func (p *TimeProfileProcessor) Reset() {
	p.firstTS = time.Time{}
	p.lastTS = time.Time{}
	p.tracker.reset()
}

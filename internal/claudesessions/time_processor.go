package claudesessions

import (
	"encoding/json"
	"time"
)

// timeSystemEvent is used to decode system events that carry duration data.
type timeSystemEvent struct {
	Subtype    string `json:"subtype"`
	DurationMs int64  `json:"duration_ms"`
	// Some variants use a plain "duration" key in seconds.
	Duration float64 `json:"duration"`
}

// TimeProfileProcessor computes the total wall-clock duration of the session
// and an estimate of Claude's thinking time.
//
//   - TotalDurationMs = last_event_timestamp − first_event_timestamp
//   - ThinkingTimeMs  = sum of system{"subtype":"turn_duration"} durationMs values;
//     falls back to summing (len(thinking_text) × 0.5) across all thinking blocks.
type TimeProfileProcessor struct {
	firstTS           time.Time
	lastTS            time.Time
	thinkingTimeMs    int64
	hasSystemDuration bool
}

// Name returns the processor identifier.
func (p *TimeProfileProcessor) Name() string { return "time_profile" }

// Process tracks first/last timestamps and accumulates thinking time.
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

	switch ev.Type {
	case "system":
		p.processSystemEvent(ev.Raw)
	case "assistant":
		if !p.hasSystemDuration && ev.Message != nil {
			p.accumulateThinkingBlocks(ev.Message.Content)
		}
	}
}

func (p *TimeProfileProcessor) processSystemEvent(raw json.RawMessage) {
	var sys timeSystemEvent
	if err := json.Unmarshal(raw, &sys); err != nil {
		return
	}
	if sys.Subtype != "turn_duration" {
		return
	}
	p.hasSystemDuration = true
	if sys.DurationMs > 0 {
		p.thinkingTimeMs += sys.DurationMs
	} else if sys.Duration > 0 {
		// Convert seconds to milliseconds.
		p.thinkingTimeMs += int64(sys.Duration * 1000)
	}
}

func (p *TimeProfileProcessor) accumulateThinkingBlocks(raw json.RawMessage) {
	for _, b := range parseContentBlocks(raw) {
		if b.Type == "thinking" {
			// Fallback: estimate 0.5 ms per character.
			p.thinkingTimeMs += int64(float64(len(b.Thinking)) * 0.5)
		}
	}
}

// Finalize writes TotalDurationMs and ThinkingTimeMs into the insight.
func (p *TimeProfileProcessor) Finalize(insight *SessionInsight) {
	if !p.firstTS.IsZero() && !p.lastTS.IsZero() {
		insight.TotalDurationMs = p.lastTS.Sub(p.firstTS).Milliseconds()
	}
	insight.ThinkingTimeMs = p.thinkingTimeMs
}

// Reset clears all internal state.
func (p *TimeProfileProcessor) Reset() {
	p.firstTS = time.Time{}
	p.lastTS = time.Time{}
	p.thinkingTimeMs = 0
	p.hasSystemDuration = false
}

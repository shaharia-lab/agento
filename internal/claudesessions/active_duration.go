package claudesessions

import (
	"sort"
	"time"
)

// IdleGapThreshold is the largest gap between two consecutive transcript
// events that still counts as continuous work. Claude Code sessions are
// resumable: a transcript's wall-clock span routinely contains lunch breaks,
// nights, or a resume weeks later, and on the reference corpus a single
// resumed-after-28-days session carried 82% of the dashboard's "Avg Duration"
// on its own. Ten minutes keeps reading a long reply or manually testing a
// change inside a sitting while excluding everything a person would not call
// working time.
//
// Every consumer of "how long did this actually run" shares this constant —
// the scanner, the insight processors, and the journey builder — so no two
// pages can disagree about what active time means.
const IdleGapThreshold = 10 * time.Minute

// activeTimeTracker accumulates event timestamps and derives active duration:
// the sum of gaps between consecutive events, each capped at IdleGapThreshold.
//
// Timestamps are collected and sorted at read time rather than walked in
// arrival order, because callers feed transcripts whose order is not globally
// chronological — the insight pipeline processes the parent transcript first
// and each sub-agent transcript after it, with timestamps interleaved into the
// parent's range. Sorting also makes delegated work fill the parent's Task
// wait gaps, so a session that spent 40 minutes in sub-agents is credited
// those 40 minutes instead of one capped gap.
type activeTimeTracker struct {
	stamps []activeStamp
}

type activeStamp struct {
	ts        time.Time
	assistant bool
}

// observe records one event timestamp. assistant marks events produced by the
// model, which attributes the gap leading into them to Claude working time.
func (t *activeTimeTracker) observe(ts time.Time, assistant bool) {
	if ts.IsZero() {
		return
	}
	t.stamps = append(t.stamps, activeStamp{ts: ts, assistant: assistant})
}

// durations returns the tracker's two figures in milliseconds:
//
//   - active: every inter-event gap capped at IdleGapThreshold — wall-clock
//     time someone or something was actually doing work in this session.
//   - assistant: the subset of those capped gaps that end at an assistant
//     event — time spent producing model output (thinking plus generation,
//     measured from timestamps rather than estimated).
func (t *activeTimeTracker) durations() (active, assistant int64) {
	if len(t.stamps) < 2 {
		return 0, 0
	}
	sorted := make([]activeStamp, len(t.stamps))
	copy(sorted, t.stamps)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].ts.Before(sorted[j].ts) })

	capMs := IdleGapThreshold.Milliseconds()
	for i := 1; i < len(sorted); i++ {
		gap := sorted[i].ts.Sub(sorted[i-1].ts).Milliseconds()
		if gap > capMs {
			gap = capMs
		}
		active += gap
		if sorted[i].assistant {
			assistant += gap
		}
	}
	return active, assistant
}

// reset clears the tracker for reuse.
func (t *activeTimeTracker) reset() {
	t.stamps = nil
}

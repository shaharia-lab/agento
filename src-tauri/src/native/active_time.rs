//! Active duration, ported from `internal/claudesessions/active_duration.go`.
//!
//! **Duration means active duration everywhere a user reads one.** Sessions are
//! resumable, so the raw start→last span counts every idle day between
//! sittings — one resumed-after-28-days session carried 82% of the dashboard's
//! "Avg Duration" (476 min shown against a 17 min median). The fix is to cap
//! each inter-event gap at the idle threshold and sum those.
//!
//! This lives outside both callers because **the scanner and the insight
//! pipeline must agree**. The scanner stores `active_duration_ms` on the cache
//! row; the pipeline stores its own on `session_insights`; the journey computes
//! one on read. Three implementations of one rule is how they would drift, and
//! the threshold is user-configurable, so the drift would be invisible until
//! someone changed it.

use chrono::{DateTime, Utc};

/// Accumulates the timestamps of one session's events and reports the capped
/// active spans over them.
#[derive(Debug, Clone, Default)]
pub struct ActiveTimeTracker {
    idle_gap_ms: i64,
    stamps: Vec<(DateTime<Utc>, bool)>,
}

impl ActiveTimeTracker {
    pub fn new(idle_gap_ms: i64) -> Self {
        ActiveTimeTracker {
            idle_gap_ms,
            stamps: Vec::new(),
        }
    }

    /// Records one event. `assistant` marks an assistant event, which is what
    /// separates Claude's working time from the session's active time.
    pub fn observe(&mut self, ts: DateTime<Utc>, assistant: bool) {
        self.stamps.push((ts, assistant));
    }

    /// Take on another tracker's stamps, so its events count towards this
    /// session's active time.
    ///
    /// The journey builder is the caller: a delegated sub-agent's timestamps
    /// merge into its parent's before `durations()` is read, which is what
    /// credits a 40-minute delegated run instead of collapsing the parent's
    /// `Task` wait to one capped gap. `durations()` sorts, so the order the
    /// stamps arrive in does not matter — only that they arrive before it runs.
    ///
    /// **This produces a union, not the sum the sessions list shows.** The
    /// scanner caps each transcript on its own and the list adds the parent's
    /// figure to `SUM(active_duration_ms)` over the sub-agent rows, so a
    /// wall-clock minute in which the parent waits and its agent works is
    /// counted twice there and once here. Neither is derivable from the other,
    /// and `sessions/journey.rs`'s header carries the whole argument — read it
    /// before changing which stamps reach a tracker.
    ///
    /// The threshold is this tracker's; a sub-builder is constructed with the
    /// same one, so there is no window in which two gaps of one session are
    /// capped differently.
    pub fn absorb(&mut self, other: &ActiveTimeTracker) {
        self.stamps.extend_from_slice(&other.stamps);
    }

    /// The session's active time, in milliseconds.
    pub fn active_ms(&self) -> i64 {
        self.durations().0
    }

    /// `(active, claude_working)` — the capped inter-event gaps, and the subset
    /// of them that end at an assistant event.
    ///
    /// Sorted first because callers feed parent-then-sub-agent transcripts
    /// whose timestamps interleave — which is also what credits a 40-minute
    /// delegated run instead of collapsing the parent's `Task` wait to one
    /// capped gap.
    ///
    /// A **stable** sort where Go uses `sort.Slice`: with two events sharing a
    /// timestamp but differing in whether they are assistant events, the gap
    /// *into* the pair is attributed to whichever sorts first, so Go's result
    /// is order-dependent there. Stable keeps file order, which is the order
    /// the events were written in.
    ///
    /// The threshold is read once per call, so a settings save landing mid-walk
    /// cannot cap two gaps of the same session differently.
    pub fn durations(&self) -> (i64, i64) {
        if self.stamps.len() < 2 {
            return (0, 0);
        }
        let mut sorted = self.stamps.clone();
        sorted.sort_by_key(|(ts, _)| *ts);

        let cap = self.idle_gap_ms;
        let (mut active, mut assistant) = (0i64, 0i64);
        for pair in sorted.windows(2) {
            let gap = (pair[1].0 - pair[0].0).num_milliseconds().min(cap);
            active += gap;
            if pair[1].1 {
                assistant += gap;
            }
        }
        (active, assistant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-03-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::minutes(minutes)
    }

    #[test]
    fn fewer_than_two_events_have_no_duration() {
        let mut tracker = ActiveTimeTracker::new(600_000);
        assert_eq!(tracker.durations(), (0, 0));
        tracker.observe(t(0), false);
        assert_eq!(tracker.durations(), (0, 0));
    }

    #[test]
    fn a_long_gap_is_capped_at_the_threshold() {
        // The resumed-after-days case: without the cap this reads as hours.
        let mut tracker = ActiveTimeTracker::new(600_000); // 10 minutes
        tracker.observe(t(0), false);
        tracker.observe(t(60 * 24), false);
        assert_eq!(tracker.active_ms(), 600_000);
    }

    #[test]
    fn assistant_time_is_the_subset_of_gaps_ending_at_an_assistant() {
        let mut tracker = ActiveTimeTracker::new(600_000);
        tracker.observe(t(0), false);
        tracker.observe(t(1), true); // 1 min, credited to both
        tracker.observe(t(3), false); // 2 min, active only
        let (active, assistant) = tracker.durations();
        assert_eq!(active, 3 * 60_000);
        assert_eq!(assistant, 60_000);
    }

    #[test]
    fn out_of_order_stamps_are_sorted_before_the_walk() {
        // Parent-then-sub-agent feeding is not chronological.
        let mut tracker = ActiveTimeTracker::new(600_000);
        tracker.observe(t(4), false);
        tracker.observe(t(0), false);
        tracker.observe(t(2), false);
        assert_eq!(tracker.active_ms(), 4 * 60_000);
    }
}

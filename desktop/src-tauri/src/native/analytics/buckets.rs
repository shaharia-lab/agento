//! Go's calendar arithmetic, in the request's timezone.
//!
//! Mirrors the "Time bucket helpers" section of
//! `internal/claudesessions/analytics.go` plus the parts of Go's `time` package
//! those helpers lean on. Storage and transport stay UTC; only aggregation and
//! labelling move, because a day, an hour or a weekday is meaningless until you
//! say whose it is.
//!
//! Three rules decide whether a bucketed series is right, and all three are
//! easy to get subtly wrong:
//!
//! 1. **`bucket_start` is the one definition**, shared by [`bucket_key`] and
//!    [`walk_buckets`]. When Go had two, the walk started at the raw window edge
//!    and a weekly series emitted keys no session could land in.
//! 2. **Steps advance the calendar unit, not a fixed duration.** A local day is
//!    23 or 25 hours across a DST transition, so `+24h` drifts off the wall
//!    clock — duplicating one key and skipping another. Months are not a fixed
//!    length at all.
//! 3. **`time.Date` normalizes, it does not fail.** Given a wall-clock time
//!    that a DST gap skipped, Go still returns a real instant; `chrono`'s
//!    `LocalResult` would hand back `None`. [`go_date`] reproduces Go's rule
//!    rather than inventing one.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Offset, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

/// The bucket widths a time series can be reported at, coarsest last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Granularity {
    /// The wire name, which travels with the report because a bucket key alone
    /// no longer says how wide its bucket is.
    pub fn as_str(self) -> &'static str {
        match self {
            Granularity::Hourly => "hourly",
            Granularity::Daily => "daily",
            Granularity::Weekly => "weekly",
            Granularity::Monthly => "monthly",
            Granularity::Yearly => "yearly",
        }
    }
}

/// `time.Date(year, month, day, hour, min, sec, nsec, loc)`.
///
/// Go resolves the zone in two lookups rather than one, and the second is what
/// makes a nonexistent or ambiguous wall clock resolve to *something* instead of
/// failing: it takes the offset in force at the naive time read as UTC, uses it
/// to guess the real instant, and then re-reads the offset at that guess. Both
/// lookups are reproduced here.
///
/// Out-of-range fields normalize as Go's `norm` does — hour 24 rolls into the
/// next day, month 13 into the next year, day 0 into the previous month — which
/// is what lets callers write `hour + 1` and `day - 30` directly.
#[allow(clippy::too_many_arguments)] // it is `time.Date`'s signature, verbatim
pub fn go_date(
    loc: Tz,
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
    second: i32,
    nanosecond: u32,
) -> DateTime<Utc> {
    let (year, month0) = norm(year, month - 1, 12);
    let (hour, minute) = norm(hour, minute, 60);
    let (day, hour) = norm(day, hour, 24);

    // Day may still sit outside the month's length; Go counts days from the
    // first of the month rather than validating, and so does this.
    let first = NaiveDate::from_ymd_opt(year, (month0 + 1) as u32, 1)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch is a valid date"));
    let date = first + Duration::days(i64::from(day) - 1);
    let naive = date
        .and_hms_nano_opt(hour as u32, minute as u32, second as u32, nanosecond)
        .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).expect("midnight is a valid time"));

    // The naive wall clock read as if it were UTC — Go's starting point too.
    let as_utc = naive.and_utc();
    let guess = offset_seconds(loc, as_utc);
    let probe = as_utc - Duration::seconds(guess);
    let offset = offset_seconds(loc, probe);
    as_utc - Duration::seconds(offset)
}

/// Go's `norm(hi, lo, base)`: carry `lo` into `hi` until `lo` is in `[0, base)`.
fn norm(hi: i32, lo: i32, base: i32) -> (i32, i32) {
    let mut hi = hi;
    let mut lo = lo;
    if lo < 0 {
        let n = (-lo - 1) / base + 1;
        hi -= n;
        lo += n * base;
    }
    if lo >= base {
        let n = lo / base;
        hi += n;
        lo -= n * base;
    }
    (hi, lo)
}

/// The zone's offset from UTC, in seconds, at a given instant.
fn offset_seconds(loc: Tz, at: DateTime<Utc>) -> i64 {
    i64::from(
        loc.offset_from_utc_datetime(&at.naive_utc())
            .fix()
            .local_minus_utc(),
    )
}

/// `t.AddDate(years, months, days)`: adjust the calendar fields of the local
/// wall clock, then resolve through [`go_date`]. Not a duration addition — that
/// is the whole point.
pub fn add_date(t: DateTime<Utc>, loc: Tz, years: i32, months: i32, days: i32) -> DateTime<Utc> {
    let local = t.with_timezone(&loc);
    go_date(
        loc,
        local.year() + years,
        local.month() as i32 + months,
        local.day() as i32 + days,
        local.hour() as i32,
        local.minute() as i32,
        local.second() as i32,
        local.nanosecond(),
    )
}

/// Truncate an instant to the start of the bucket containing it, in `loc`.
///
/// Weeks start on Monday, matching ISO-8601 and every other weekday-aware
/// figure on the dashboard.
pub fn bucket_start(t: DateTime<Utc>, granularity: Granularity, loc: Tz) -> DateTime<Utc> {
    let l = t.with_timezone(&loc);
    let (y, m, d) = (l.year(), l.month() as i32, l.day() as i32);
    match granularity {
        Granularity::Hourly => go_date(loc, y, m, d, l.hour() as i32, 0, 0, 0),
        Granularity::Weekly => {
            // Sunday is 0 in Go's numbering; shift so Monday leads the week.
            let offset = (l.weekday().num_days_from_sunday() as i32 + 6) % 7;
            let day = go_date(loc, y, m, d, 0, 0, 0, 0);
            add_date(day, loc, 0, 0, -offset)
        }
        Granularity::Monthly => go_date(loc, y, m, 1, 0, 0, 0, 0),
        Granularity::Yearly => go_date(loc, y, 1, 1, 0, 0, 0, 0),
        Granularity::Daily => go_date(loc, y, m, d, 0, 0, 0, 0),
    }
}

/// A bucket's key, which doubles as its label.
///
/// Weekly and monthly buckets are keyed by their first day rather than by a
/// `2026-W32` or `2026-08` form, so every key still parses as the `YYYY-MM-DD`
/// (optionally `T`-suffixed with an hour) that `analyticsMetrics.ts` splits.
///
/// Formatted by hand rather than through `strftime`: Go's `2006` pads a
/// year to four digits and `chrono`'s `%Y` does not, which would diverge on a
/// zero timestamp.
pub fn bucket_key(t: DateTime<Utc>, granularity: Granularity, loc: Tz) -> String {
    let start = bucket_start(t, granularity, loc).with_timezone(&loc);
    let date = format!(
        "{:04}-{:02}-{:02}",
        start.year(),
        start.month(),
        start.day()
    );
    if granularity == Granularity::Hourly {
        return format!("{date}T{:02}", start.hour());
    }
    date
}

/// Call `f` once per bucket from `from` to `to` inclusive, stepping in `loc`.
///
/// The walk starts at the bucket *containing* `from`, not at the raw window
/// edge: a window beginning mid-week must still emit the week its first
/// sessions key into.
pub fn walk_buckets<F>(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    granularity: Granularity,
    loc: Tz,
    mut f: F,
) where
    F: FnMut(&str, DateTime<Utc>),
{
    let mut cur = bucket_start(from, granularity, loc);
    while cur <= to {
        f(&bucket_key(cur, granularity, loc), cur);
        let next = match granularity {
            Granularity::Hourly => cur + Duration::hours(1),
            Granularity::Weekly => add_date(cur, loc, 0, 0, 7),
            Granularity::Monthly => add_date(cur, loc, 0, 1, 0),
            Granularity::Yearly => add_date(cur, loc, 1, 0, 0),
            Granularity::Daily => add_date(cur, loc, 0, 0, 1),
        };
        if next <= cur {
            // Unreachable for a real zone, but a walk that cannot advance would
            // hang the request rather than answer it.
            break;
        }
        cur = next;
    }
}

/// How many hour cells one session may occupy.
///
/// Two weeks of continuous activity is already far outside anything real; a
/// span longer than that is a broken time range, and letting it paint thousands
/// of cells would swamp the chart with one bad row.
const MAX_SESSION_HOUR_CELLS: i64 = 24 * 14;

/// Call `f` once for every local hour a session was active, with that hour's
/// share of the session's span.
///
/// Bucketing a session at a single instant is what made "Activity by Hour of
/// Day" answer a different question than its title: an 8h51m session put all of
/// its weight on the hour it *ended*. Spreading it across its span also makes
/// the chart agree with its own drill-down, which has always selected sessions
/// whose activity window overlaps the clicked hour.
pub fn walk_session_hours<F>(start: DateTime<Utc>, end: DateTime<Utc>, loc: Tz, mut f: F)
where
    F: FnMut(DateTime<Utc>, f64),
{
    if end <= start {
        // No measurable duration: the single hour it happened in, full weight.
        f(start, 1.0);
        return;
    }
    if end - start > Duration::hours(MAX_SESSION_HOUR_CELLS) {
        f(end, 1.0);
        return;
    }

    let total = (end - start).num_nanoseconds().unwrap_or(1) as f64;
    let mut cur = start;
    while cur < end {
        let mut next = next_local_hour(cur, loc);
        if next > end {
            next = end;
        }
        let share = (next - cur).num_nanoseconds().unwrap_or(0) as f64 / total;
        f(cur, share);
        cur = next;
    }
}

/// The start of the hour after `t`, on `t`'s own wall clock.
///
/// Built through [`go_date`] rather than by truncating to a UTC hour: in a zone
/// offset by a half or quarter hour (Asia/Kolkata, Asia/Kathmandu) truncation
/// puts the cell boundary at :30 or :45 local, splitting one local hour into two
/// cells and counting the session in it twice.
fn next_local_hour(t: DateTime<Utc>, loc: Tz) -> DateTime<Utc> {
    let l = t.with_timezone(&loc);
    let next = go_date(
        loc,
        l.year(),
        l.month() as i32,
        l.day() as i32,
        l.hour() as i32 + 1,
        0,
        0,
        0,
    );
    if next <= t {
        // Only reachable if a zone transition maps the next wall-clock hour to
        // an instant at or before t; stepping an hour keeps the walk moving.
        return t + Duration::hours(1);
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn norm_carries_both_ways_like_gos() {
        assert_eq!(norm(2026, 12, 12), (2027, 0));
        assert_eq!(norm(2026, -1, 12), (2025, 11));
        assert_eq!(norm(10, 24, 24), (11, 0));
        assert_eq!(norm(10, -1, 24), (9, 23));
    }

    #[test]
    fn a_local_midnight_is_the_zones_midnight_not_utcs() {
        let berlin: Tz = "Europe/Berlin".parse().expect("zone");
        // 2026-08-13 00:30 UTC is already the 13th in Berlin (02:30 CEST).
        let start = bucket_start(utc("2026-08-13T00:30:00Z"), Granularity::Daily, berlin);
        assert_eq!(start, utc("2026-08-12T22:00:00Z"));
        assert_eq!(
            bucket_key(utc("2026-08-13T00:30:00Z"), Granularity::Daily, berlin),
            "2026-08-13"
        );
        // …and 22:30 UTC on the 13th is already the 14th there.
        assert_eq!(
            bucket_key(utc("2026-08-13T22:30:00Z"), Granularity::Daily, berlin),
            "2026-08-14"
        );
    }

    #[test]
    fn weeks_start_on_monday() {
        let utc_tz = Tz::UTC;
        // 2026-08-13 is a Thursday.
        assert_eq!(
            bucket_key(utc("2026-08-13T12:00:00Z"), Granularity::Weekly, utc_tz),
            "2026-08-10"
        );
        // A Monday is its own week's start.
        assert_eq!(
            bucket_key(utc("2026-08-10T00:00:00Z"), Granularity::Weekly, utc_tz),
            "2026-08-10"
        );
        // A Sunday belongs to the week that began six days earlier.
        assert_eq!(
            bucket_key(utc("2026-08-16T23:59:59Z"), Granularity::Weekly, utc_tz),
            "2026-08-10"
        );
    }

    /// A fixed 24-hour step would emit one key twice and skip another across a
    /// DST transition. This is the case that rule exists for.
    #[test]
    fn a_daily_walk_crossing_dst_emits_each_local_day_once() {
        let ny: Tz = "America/New_York".parse().expect("zone");
        let mut keys = Vec::new();
        walk_buckets(
            utc("2026-03-06T12:00:00Z"),
            utc("2026-03-11T12:00:00Z"),
            Granularity::Daily,
            ny,
            |key, _| keys.push(key.to_string()),
        );
        assert_eq!(
            keys,
            vec![
                "2026-03-06",
                "2026-03-07",
                "2026-03-08", // the 23-hour day
                "2026-03-09",
                "2026-03-10",
                "2026-03-11",
            ]
        );
    }

    /// Go's `time.Date` resolves a wall clock the spring-forward gap removed;
    /// `chrono`'s own local resolution would refuse to.
    #[test]
    fn a_nonexistent_wall_clock_resolves_the_way_go_resolves_it() {
        let ny: Tz = "America/New_York".parse().expect("zone");
        // 02:30 on 2026-03-08 never happens; Go lands on 06:30Z (01:30 EST).
        assert_eq!(
            go_date(ny, 2026, 3, 8, 2, 30, 0, 0),
            utc("2026-03-08T06:30:00Z")
        );
        assert!(ny.with_ymd_and_hms(2026, 3, 8, 2, 30, 0).single().is_none());
    }

    #[test]
    fn hour_cells_follow_the_local_clock_in_a_half_hour_zone() {
        let kolkata: Tz = "Asia/Kolkata".parse().expect("zone");
        let mut cells = Vec::new();
        walk_session_hours(
            utc("2026-08-13T06:00:00Z"), // 11:30 IST
            utc("2026-08-13T08:00:00Z"), // 13:30 IST
            kolkata,
            |at, share| cells.push((at.with_timezone(&kolkata).hour(), share)),
        );
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].0, 11);
        assert_eq!(cells[1].0, 12);
        assert_eq!(cells[2].0, 13);
        let total: f64 = cells.iter().map(|c| c.1).sum();
        assert!(
            (total - 1.0).abs() < 1e-12,
            "shares sum to one, got {total}"
        );
    }

    #[test]
    fn a_session_with_no_duration_occupies_one_hour_at_full_weight() {
        let mut cells = Vec::new();
        let at = utc("2026-08-13T06:00:00Z");
        walk_session_hours(at, at, Tz::UTC, |t, share| cells.push((t, share)));
        assert_eq!(cells, vec![(at, 1.0)]);
    }

    #[test]
    fn an_absurd_span_collapses_to_the_hour_it_ended_in() {
        let mut cells = Vec::new();
        walk_session_hours(
            utc("2026-01-01T00:00:00Z"),
            utc("2026-06-01T00:00:00Z"),
            Tz::UTC,
            |t, share| cells.push((t, share)),
        );
        assert_eq!(cells, vec![(utc("2026-06-01T00:00:00Z"), 1.0)]);
    }

    #[test]
    fn monthly_and_yearly_walks_step_the_calendar() {
        let mut keys = Vec::new();
        walk_buckets(
            utc("2026-01-15T00:00:00Z"),
            utc("2026-04-02T00:00:00Z"),
            Granularity::Monthly,
            Tz::UTC,
            |key, _| keys.push(key.to_string()),
        );
        assert_eq!(
            keys,
            vec!["2026-01-01", "2026-02-01", "2026-03-01", "2026-04-01"]
        );

        keys.clear();
        walk_buckets(
            utc("2020-06-01T00:00:00Z"),
            utc("2023-01-01T00:00:00Z"),
            Granularity::Yearly,
            Tz::UTC,
            |key, _| keys.push(key.to_string()),
        );
        assert_eq!(
            keys,
            vec!["2020-01-01", "2021-01-01", "2022-01-01", "2023-01-01"]
        );
    }
}

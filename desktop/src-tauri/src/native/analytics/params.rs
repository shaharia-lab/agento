//! The window, project and timezone every analytics-shaped endpoint accepts.
//!
//! Mirrors `parseAnalyticsParams`, `parseAnalyticsDate`, `parseRangeEnd` and
//! `parseTimezone` in `internal/api/claude_analytics.go`, plus
//! `AnalyticsParams.Granularity` in `internal/claudesessions/analytics.go`.
//!
//! Two rules from the Go side that are easy to lose:
//!
//! - **A bare `YYYY-MM-DD` is a *local* day**, not a UTC one, and a bare `to`
//!   names the whole day — its final second — so "to: today" does not exclude
//!   everything that happened today. An RFC 3339 value states its own instant
//!   and is taken at its word.
//! - **Nothing here errors.** An unparseable date falls back to the default
//!   window and a bad timezone to UTC, because a read-only dashboard is more
//!   useful rendered over a default range than refused.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;

use super::buckets::{go_date, Granularity};

/// Filtering and bucketing for one analytics request.
#[derive(Debug, Clone)]
pub struct AnalyticsParams {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Empty means every project. Matched against `project_path` exactly.
    pub project: String,
    /// The timezone day, hour and weekday buckets are derived in. Storage and
    /// transport stay UTC; only aggregation and labelling move.
    pub loc: Tz,
}

impl AnalyticsParams {
    /// Read the parameters out of a raw query string.
    ///
    /// `Err` means "let Go answer this one". The only case is a timezone this
    /// build's tzdata does not know: Go would fall back to UTC, but *its*
    /// tzdata may well know the zone, and answering in UTC when Go would answer
    /// in Asia/Kathmandu is a wrong answer rather than a missing one. Forwarding
    /// gets whichever answer Go would have given, which is the bar.
    pub fn parse(query: &str) -> Result<Self, String> {
        let params = first_values(query);
        let get = |key: &str| params.get(key).cloned().unwrap_or_default();

        let loc = parse_timezone(&get("tz"))?;

        // A "day" is only meaningful in a timezone, so the default window is
        // anchored in the requested one rather than the server's.
        let now = Utc::now();
        let mut from = add_days(now, loc, -30);
        let mut to = now;

        let raw_from = get("from");
        if !raw_from.is_empty() {
            if let Some(t) = parse_analytics_date(&raw_from, loc) {
                from = t;
            }
        }
        let raw_to = get("to");
        if !raw_to.is_empty() {
            if let Some(t) = parse_range_end(&raw_to, loc) {
                to = t;
            }
        }

        Ok(AnalyticsParams {
            from,
            to,
            project: get("project"),
            loc,
        })
    }

    /// The bucket width the window's length picks.
    ///
    /// Coarsening rather than truncating: a reader asking for six years wants
    /// six years, at whatever resolution six years can be read at. The
    /// thresholds are chosen so no series exceeds ~200 buckets — 7 days of
    /// hours is 169, 120 days is 121, 3 years of weeks is 157, 12 years of
    /// months is 145.
    pub fn granularity(&self) -> Granularity {
        let span = self.to - self.from;
        let days = |n: i64| chrono::Duration::days(n);
        if span <= days(7) {
            Granularity::Hourly
        } else if span <= days(120) {
            Granularity::Daily
        } else if span <= days(3 * 365) {
            Granularity::Weekly
        } else if span <= days(12 * 365) {
            Granularity::Monthly
        } else {
            Granularity::Yearly
        }
    }
}

/// One query parameter, with `url.Values.Get` semantics.
///
/// Shared with the insights summary, which reads `ids` alongside the window
/// this module parses.
pub fn query_value(query: &str, key: &str) -> String {
    first_values(query).remove(key).unwrap_or_default()
}

/// `url.Values.Get` semantics: the first occurrence of each key wins.
fn first_values(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        out.entry(key.into_owned())
            .or_insert_with(|| value.into_owned());
    }
    out
}

/// `t.AddDate(0, 0, days)` in `loc` — a calendar step, so a window across a DST
/// transition still lands on the same wall clock.
fn add_days(t: DateTime<Utc>, loc: Tz, days: i32) -> DateTime<Utc> {
    let l = t.with_timezone(&loc);
    go_date(
        loc,
        l.year(),
        l.month() as i32,
        l.day() as i32 + days,
        l.hour() as i32,
        l.minute() as i32,
        l.second() as i32,
        l.nanosecond(),
    )
}

/// RFC 3339 first, then `YYYY-MM-DD` interpreted in `loc`.
///
/// A bare date carries no offset, so it has to be read in the requesting
/// timezone — parsing it as UTC is what shifted every range edge by the user's
/// own offset.
fn parse_analytics_date(raw: &str, loc: Tz) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Some(t.with_timezone(&Utc));
    }
    let (y, m, d) = parse_ymd(raw)?;
    Some(go_date(loc, y, m, d, 0, 0, 0, 0))
}

/// An inclusive window end. A bare `YYYY-MM-DD` names a whole local day, so it
/// resolves to that day's final second rather than its first.
fn parse_range_end(raw: &str, loc: Tz) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Some(t.with_timezone(&Utc));
    }
    let (y, m, d) = parse_ymd(raw)?;
    Some(go_date(loc, y, m, d, 23, 59, 59, 0))
}

/// `time.ParseInLocation("2006-01-02", …)`: exactly four, two and two digits,
/// and nothing else in the string.
fn parse_ymd(raw: &str) -> Option<(i32, i32, i32)> {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i32 = raw.get(0..4)?.parse().ok()?;
    let month: i32 = raw.get(5..7)?.parse().ok()?;
    let day: i32 = raw.get(8..10)?.parse().ok()?;
    // Go's layout parser range-checks month and day before normalizing.
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

/// Resolve an IANA timezone name. Empty is UTC, matching `parseTimezone`;
/// anything this build's tzdata cannot resolve is an error, so the request
/// forwards to Go rather than being answered in the wrong zone.
fn parse_timezone(name: &str) -> Result<Tz, String> {
    if name.is_empty() {
        return Ok(Tz::UTC);
    }
    name.parse::<Tz>()
        .map_err(|_| format!("unknown timezone {name:?}; letting Go resolve it"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(query: &str) -> AnalyticsParams {
        AnalyticsParams::parse(query).expect("parse")
    }

    #[test]
    fn a_bare_date_is_a_local_day_not_a_utc_one() {
        let p = params("from=2026-08-13&to=2026-08-13&tz=Europe/Berlin");
        // Midnight in Berlin is 22:00 UTC the day before.
        assert_eq!(p.from.to_rfc3339(), "2026-08-12T22:00:00+00:00");
        // …and the inclusive end is that local day's final second.
        assert_eq!(p.to.to_rfc3339(), "2026-08-13T21:59:59+00:00");
    }

    #[test]
    fn an_rfc3339_bound_is_taken_at_its_word() {
        let p = params("from=2026-08-13T06:30:00Z&to=2026-08-13T18:00:00%2B02:00&tz=Europe/Berlin");
        assert_eq!(p.from.to_rfc3339(), "2026-08-13T06:30:00+00:00");
        assert_eq!(p.to.to_rfc3339(), "2026-08-13T16:00:00+00:00");
    }

    #[test]
    fn an_unparseable_bound_falls_back_to_the_default_window() {
        let p = params("from=yesterday&to=2026-08-13");
        let default_from = params("to=2026-08-13").from;
        // Both defaulted to "30 days ago", within the test's own runtime.
        assert!((p.from - default_from).num_seconds().abs() <= 1);
    }

    #[test]
    fn an_unknown_timezone_forwards_rather_than_answering_in_utc() {
        assert!(AnalyticsParams::parse("tz=Mars/Olympus").is_err());
        assert_eq!(params("").loc, Tz::UTC);
        assert_eq!(params("tz=UTC").loc, Tz::UTC);
    }

    #[test]
    fn granularity_follows_the_window() {
        let g = |from: &str, to: &str| params(&format!("from={from}&to={to}")).granularity();
        assert_eq!(
            g("2026-08-01T00:00:00Z", "2026-08-08T00:00:00Z"),
            Granularity::Hourly
        );
        assert_eq!(
            g("2026-08-01T00:00:00Z", "2026-08-08T00:00:01Z"),
            Granularity::Daily
        );
        assert_eq!(
            g("2026-01-01T00:00:00Z", "2026-05-01T00:00:00Z"),
            Granularity::Daily
        );
        assert_eq!(
            g("2024-01-01T00:00:00Z", "2026-08-01T00:00:00Z"),
            Granularity::Weekly
        );
        assert_eq!(
            g("2016-01-01T00:00:00Z", "2026-08-01T00:00:00Z"),
            Granularity::Monthly
        );
        assert_eq!(
            g("2000-01-01T00:00:00Z", "2026-08-01T00:00:00Z"),
            Granularity::Yearly
        );
    }

    #[test]
    fn a_repeated_parameter_takes_the_first_as_url_values_get_does() {
        assert_eq!(params("project=a&project=b").project, "a");
    }

    #[test]
    fn a_malformed_bare_date_is_rejected_rather_than_normalized() {
        assert!(parse_ymd("2026-8-13").is_none());
        assert!(parse_ymd("2026-13-01").is_none());
        assert!(parse_ymd("2026-08-13T00").is_none());
        assert_eq!(parse_ymd("2026-08-13"), Some((2026, 8, 13)));
    }
}

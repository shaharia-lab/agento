//! `robfig/cron` v3's dialect, which is the one an Agento cron task is written
//! in — whether or not anyone chose it.
//!
//! `gocron.CronJob(expr, false)` does not implement cron itself. It prepends
//! `CRON_TZ=<scheduler location>` to the string and hands it to
//! `cron.ParseStandard`, so the accepted syntax, the field semantics and the
//! next-fire-time search are all `robfig/cron`'s. Four consequences that a
//! "standard crontab" reading of the expression would get wrong:
//!
//! - **Descriptors are accepted.** `ParseStandard` enables them, so `@daily`,
//!   `@midnight`, `@hourly`, `@weekly`, `@monthly`, `@yearly`/`@annually` and
//!   `@every <duration>` all parse. `@every` is not a spec at all: it is Go's
//!   `time.ParseDuration` behind a constant-delay schedule, floored at one
//!   second and truncated to whole seconds.
//! - **Exactly five fields.** Agento passes `withSeconds = false`, so a
//!   six-field seconds spec is rejected rather than interpreted.
//! - **`N/step` means `N-max/step`**, and `?` is a synonym for `*` — both are
//!   robfig extensions, and `?` sets the same star bit `*` does, which is what
//!   decides whether day-of-month and day-of-week are ANDed or ORed.
//! - **The search walks absolute hours.** `Next` steps the hour field with
//!   `t.Add(1h)`, so a wall-clock time a spring-forward gap removed is not
//!   reached at all and the job silently skips that day. It is `time.Date` —
//!   [`go_date`], normalizing rather than failing — that resets the lower
//!   fields, which is why this reuses the analytics port's calendar arithmetic
//!   rather than `chrono`'s.
//!
//! One divergence, deliberate and unreachable from Agento: a bare
//! `"CRON_TZ=UTC"` with no space after it makes robfig's `strings.Index(spec,
//! " ")` return -1 and panic on the slice. Agento always prepends a location
//! (which supplies the space) unless the string already carries a `TZ=` prefix,
//! so reaching it needs an expression that is *only* a prefix. This returns a
//! parse error there; there is no vector for it, because pinning a panic is
//! pinning a bug.

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

use crate::native::analytics::buckets::{add_date, go_date};

/// Set when the field was written as `*` or `?`. `dayMatches` reads it, so it
/// is part of the schedule rather than a parser detail.
const STAR_BIT: u64 = 1 << 63;

/// Why a crontab did not parse. The text is not reproduced — Go's messages are
/// not a contract and the vectors classify rather than quote — but the
/// *distinction* between "did not parse" and "parsed and never fires" is,
/// because gocron reports them as two different errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub &'static str);

/// What a parsed crontab is: a bit-set spec, or `@every`'s constant delay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    Spec(Box<SpecSchedule>),
    /// `ConstantDelaySchedule`. The delay is always a whole number of seconds.
    ConstantDelay(Duration),
}

/// `cron.SpecSchedule`: one bit per permitted value in each field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecSchedule {
    second: u64,
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    dow: u64,
    /// The zone the spec is evaluated in. Always set here, because gocron
    /// prepends the scheduler's location when the expression carries none.
    loc: Tz,
}

/// Field bounds, with the name table where robfig has one.
struct Bounds {
    min: u32,
    max: u32,
    names: &'static [(&'static str, u32)],
}

const SECONDS: Bounds = Bounds {
    min: 0,
    max: 59,
    names: &[],
};
const MINUTES: Bounds = Bounds {
    min: 0,
    max: 59,
    names: &[],
};
const HOURS: Bounds = Bounds {
    min: 0,
    max: 23,
    names: &[],
};
const DOM: Bounds = Bounds {
    min: 1,
    max: 31,
    names: &[],
};
const MONTHS: Bounds = Bounds {
    min: 1,
    max: 12,
    names: &[
        ("jan", 1),
        ("feb", 2),
        ("mar", 3),
        ("apr", 4),
        ("may", 5),
        ("jun", 6),
        ("jul", 7),
        ("aug", 8),
        ("sep", 9),
        ("oct", 10),
        ("nov", 11),
        ("dec", 12),
    ],
};
const DOW: Bounds = Bounds {
    min: 0,
    max: 6,
    names: &[
        ("sun", 0),
        ("mon", 1),
        ("tue", 2),
        ("wed", 3),
        ("thu", 4),
        ("fri", 5),
        ("sat", 6),
    ],
};

/// Go's `<<` on a `uint64`, which yields 0 for a shift of 64 or more where
/// Rust's would panic. Reachable only through `getBits`' `max + 1`.
fn shl(v: u64, n: u32) -> u64 {
    v.checked_shl(n).unwrap_or(0)
}

/// `getBits(min, max, step)`.
fn get_bits(min: u32, max: u32, step: u32) -> u64 {
    if step == 1 {
        return !shl(u64::MAX, max + 1) & shl(u64::MAX, min);
    }
    let mut bits = 0u64;
    let mut i = min;
    while i <= max {
        bits |= shl(1, i);
        i += step;
    }
    bits
}

fn all(r: &Bounds) -> u64 {
    get_bits(r.min, r.max, 1) | STAR_BIT
}

/// `mustParseInt`: Go's `strconv.Atoi` plus a non-negative check.
fn must_parse_int(expr: &str) -> Result<u32, ParseError> {
    let n: i64 = expr
        .parse()
        .map_err(|_| ParseError("failed to parse int from field"))?;
    if n < 0 {
        return Err(ParseError("negative number not allowed"));
    }
    u32::try_from(n).map_err(|_| ParseError("number out of range"))
}

/// `parseIntOrName`: the name table wins, case-insensitively.
fn parse_int_or_name(expr: &str, r: &Bounds) -> Result<u32, ParseError> {
    let lower = expr.to_ascii_lowercase();
    for (name, value) in r.names {
        if *name == lower {
            return Ok(*value);
        }
    }
    must_parse_int(expr)
}

/// `getRange`: `number | number "-" number [ "/" number ]`, plus `*` and `?`.
fn get_range(expr: &str, r: &Bounds) -> Result<u64, ParseError> {
    let range_and_step: Vec<&str> = expr.split('/').collect();
    let low_and_high: Vec<&str> = range_and_step[0].split('-').collect();
    let single_digit = low_and_high.len() == 1;

    let (start, mut end, mut extra) = if low_and_high[0] == "*" || low_and_high[0] == "?" {
        (r.min, r.max, STAR_BIT)
    } else {
        let start = parse_int_or_name(low_and_high[0], r)?;
        let end = match low_and_high.len() {
            1 => start,
            2 => parse_int_or_name(low_and_high[1], r)?,
            _ => return Err(ParseError("too many hyphens")),
        };
        (start, end, 0u64)
    };

    let step = match range_and_step.len() {
        1 => 1,
        2 => {
            let step = must_parse_int(range_and_step[1])?;
            // "N/step" means "N-max/step" — robfig's own extension.
            if single_digit {
                end = r.max;
            }
            if step > 1 {
                extra = 0;
            }
            step
        }
        _ => return Err(ParseError("too many slashes")),
    };

    if start < r.min {
        return Err(ParseError("beginning of range below minimum"));
    }
    if end > r.max {
        return Err(ParseError("end of range above maximum"));
    }
    if start > end {
        return Err(ParseError("beginning of range beyond end of range"));
    }
    if step == 0 {
        return Err(ParseError("step of range should be a positive number"));
    }

    Ok(get_bits(start, end, step) | extra)
}

/// `getField`: a comma-separated list of ranges.
///
/// `strings.FieldsFunc` **drops empty pieces**, so `"1,,2"` is `1,2` and a lone
/// `","` is an empty field that matches nothing — with no error. Reproduced
/// rather than tightened.
fn get_field(field: &str, r: &Bounds) -> Result<u64, ParseError> {
    let mut bits = 0u64;
    for expr in field.split(',').filter(|s| !s.is_empty()) {
        bits |= get_range(expr, r)?;
    }
    Ok(bits)
}

/// Go's `time.ParseDuration`, for `@every`.
///
/// Only reachable through a descriptor, but reachable: `@every 1h30m` is a
/// perfectly ordinary thing to type into the task form, and Rust's own duration
/// parsing is not this grammar.
fn parse_go_duration(s: &str) -> Result<Duration, ParseError> {
    let bad = ParseError("invalid duration");
    let mut rest = s;
    let mut neg = false;
    if let Some(stripped) = rest.strip_prefix('-') {
        neg = true;
        rest = stripped;
    } else if let Some(stripped) = rest.strip_prefix('+') {
        rest = stripped;
    }
    // "0" on its own is the one bare number Go accepts.
    if rest == "0" {
        return Ok(Duration::zero());
    }
    if rest.is_empty() {
        return Err(bad);
    }

    let mut nanos: i128 = 0;
    while !rest.is_empty() {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        rest = &rest[digits.len()..];
        let frac: String = if let Some(stripped) = rest.strip_prefix('.') {
            let f: String = stripped
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            rest = &stripped[f.len()..];
            f
        } else {
            String::new()
        };
        if digits.is_empty() && frac.is_empty() {
            return Err(bad);
        }
        let unit_len = rest
            .char_indices()
            .find(|(_, c)| c.is_ascii_digit() || *c == '.')
            .map_or(rest.len(), |(i, _)| i);
        let unit = &rest[..unit_len];
        rest = &rest[unit_len..];
        let scale: i128 = match unit {
            "ns" => 1,
            "us" | "µs" | "\u{03bc}s" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return Err(bad),
        };
        let whole: i128 = if digits.is_empty() {
            0
        } else {
            digits.parse().map_err(|_| bad.clone())?
        };
        nanos += whole * scale;
        if !frac.is_empty() {
            let value: i128 = frac.parse().map_err(|_| bad.clone())?;
            let divisor = 10i128.pow(u32::try_from(frac.len()).map_err(|_| bad.clone())?);
            nanos += value * scale / divisor;
        }
    }
    if neg {
        nanos = -nanos;
    }
    Ok(Duration::nanoseconds(
        i64::try_from(nanos).map_err(|_| bad.clone())?,
    ))
}

/// `Every(duration)`: floored at one second, then truncated to whole seconds.
fn every(d: Duration) -> Duration {
    let d = if d < Duration::seconds(1) {
        Duration::seconds(1)
    } else {
        d
    };
    Duration::seconds(d.num_seconds())
}

/// `parseDescriptor`.
fn parse_descriptor(descriptor: &str, loc: Tz) -> Result<Schedule, ParseError> {
    let spec = |second, minute, hour, dom, month, dow| {
        Ok(Schedule::Spec(Box::new(SpecSchedule {
            second,
            minute,
            hour,
            dom,
            month,
            dow,
            loc,
        })))
    };
    match descriptor {
        "@yearly" | "@annually" => spec(
            shl(1, SECONDS.min),
            shl(1, MINUTES.min),
            shl(1, HOURS.min),
            shl(1, DOM.min),
            shl(1, MONTHS.min),
            all(&DOW),
        ),
        "@monthly" => spec(
            shl(1, SECONDS.min),
            shl(1, MINUTES.min),
            shl(1, HOURS.min),
            shl(1, DOM.min),
            all(&MONTHS),
            all(&DOW),
        ),
        "@weekly" => spec(
            shl(1, SECONDS.min),
            shl(1, MINUTES.min),
            shl(1, HOURS.min),
            all(&DOM),
            all(&MONTHS),
            shl(1, DOW.min),
        ),
        "@daily" | "@midnight" => spec(
            shl(1, SECONDS.min),
            shl(1, MINUTES.min),
            shl(1, HOURS.min),
            all(&DOM),
            all(&MONTHS),
            all(&DOW),
        ),
        "@hourly" => spec(
            shl(1, SECONDS.min),
            shl(1, MINUTES.min),
            all(&HOURS),
            all(&DOM),
            all(&MONTHS),
            all(&DOW),
        ),
        _ => {
            if let Some(rest) = descriptor.strip_prefix("@every ") {
                return Ok(Schedule::ConstantDelay(every(parse_go_duration(rest)?)));
            }
            Err(ParseError("unrecognized descriptor"))
        }
    }
}

/// `defaultCron.IsValid` plus `cron.ParseStandard`, as gocron chains them.
///
/// `loc` is the scheduler's location, prepended as `CRON_TZ=` unless the
/// expression already names one — which is what makes a `TZ=`/`CRON_TZ=` prefix
/// win over the scheduler.
pub fn parse(crontab: &str, loc: Tz) -> Result<Schedule, ParseError> {
    let with_location = if crontab.starts_with("TZ=") || crontab.starts_with("CRON_TZ=") {
        crontab.to_string()
    } else {
        format!("CRON_TZ={} {}", loc.name(), crontab)
    };
    parse_standard(&with_location)
}

fn parse_standard(spec: &str) -> Result<Schedule, ParseError> {
    if spec.is_empty() {
        return Err(ParseError("empty spec string"));
    }

    let mut spec = spec;
    let mut loc = Tz::UTC;
    if spec.starts_with("TZ=") || spec.starts_with("CRON_TZ=") {
        // Go slices between the '=' and the first space. With no space it
        // panics; see the module comment for why that is not reproduced.
        let (Some(i), Some(eq)) = (spec.find(' '), spec.find('=')) else {
            return Err(ParseError("timezone prefix with no expression after it"));
        };
        loc = spec[eq + 1..i]
            .parse()
            .map_err(|_| ParseError("provided bad location"))?;
        spec = spec[i..].trim();
    }

    if spec.starts_with('@') {
        return parse_descriptor(spec, loc);
    }

    let fields: Vec<&str> = spec.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(ParseError("expected exactly 5 fields"));
    }

    Ok(Schedule::Spec(Box::new(SpecSchedule {
        // The seconds field is not in a standard spec; its default is "0".
        second: get_field("0", &SECONDS)?,
        minute: get_field(fields[0], &MINUTES)?,
        hour: get_field(fields[1], &HOURS)?,
        dom: get_field(fields[2], &DOM)?,
        month: get_field(fields[3], &MONTHS)?,
        dow: get_field(fields[4], &DOW)?,
        loc,
    })))
}

impl Schedule {
    /// `Schedule.Next`. `None` is Go's zero time — no run within five years.
    ///
    /// The answer is an instant. Go returns it `.In(origLocation)`, which is
    /// only how it is spelled; the caller owns that, because the zone a fire
    /// time renders in differs per job type.
    pub fn next(&self, last_run: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::ConstantDelay(delay) => {
                // `t.Add(delay - t.Nanosecond())`.
                let nanos = i64::from(last_run.timestamp_subsec_nanos());
                Some(last_run + *delay - Duration::nanoseconds(nanos))
            }
            Schedule::Spec(spec) => spec.next(last_run),
        }
    }
}

impl SpecSchedule {
    /// `SpecSchedule.Next`, transcribed loop for loop.
    ///
    /// The gotos are labelled `continue`s: Go's `goto WRAP` re-verifies every
    /// field from the month down, because incrementing one field invalidates
    /// the ones above it.
    fn next(&self, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let loc = self.loc;
        // Start at the earliest possible time (the upcoming second).
        let mut t = from + Duration::seconds(1)
            - Duration::nanoseconds(i64::from(from.timestamp_subsec_nanos()));

        let mut added = false;
        let year_limit = t.with_timezone(&loc).year() + 5;

        'wrap: loop {
            if t.with_timezone(&loc).year() > year_limit {
                return None;
            }

            // Month.
            while shl(1, local(t, loc).month()) & self.month == 0 {
                if !added {
                    added = true;
                    let l = local(t, loc);
                    t = go_date(loc, l.year(), l.month() as i32, 1, 0, 0, 0, 0);
                }
                t = add_date(t, loc, 0, 1, 0);
                if local(t, loc).month() == 1 {
                    continue 'wrap;
                }
            }

            // Day. Adding a day can land off midnight where a DST transition
            // removed it; robfig nudges back to midnight by absolute hours.
            while !self.day_matches(t, loc) {
                if !added {
                    added = true;
                    let l = local(t, loc);
                    t = go_date(loc, l.year(), l.month() as i32, l.day() as i32, 0, 0, 0, 0);
                }
                t = add_date(t, loc, 0, 0, 1);
                let hour = local(t, loc).hour();
                if hour != 0 {
                    if hour > 12 {
                        t += Duration::hours(i64::from(24 - hour));
                    } else {
                        t -= Duration::hours(i64::from(hour));
                    }
                }
                if local(t, loc).day() == 1 {
                    continue 'wrap;
                }
            }

            // Hour. `t.Add(1h)` is absolute, which is what makes a spring-
            // forward gap skip the day rather than shift the run.
            while shl(1, local(t, loc).hour()) & self.hour == 0 {
                if !added {
                    added = true;
                    let l = local(t, loc);
                    t = go_date(
                        loc,
                        l.year(),
                        l.month() as i32,
                        l.day() as i32,
                        l.hour() as i32,
                        0,
                        0,
                        0,
                    );
                }
                t += Duration::hours(1);
                if local(t, loc).hour() == 0 {
                    continue 'wrap;
                }
            }

            // Minute.
            while shl(1, local(t, loc).minute()) & self.minute == 0 {
                if !added {
                    added = true;
                    t = truncate(t, 60);
                }
                t += Duration::minutes(1);
                if local(t, loc).minute() == 0 {
                    continue 'wrap;
                }
            }

            // Second.
            while shl(1, local(t, loc).second()) & self.second == 0 {
                if !added {
                    added = true;
                    t = truncate(t, 1);
                }
                t += Duration::seconds(1);
                if local(t, loc).second() == 0 {
                    continue 'wrap;
                }
            }

            return Some(t);
        }
    }

    /// `dayMatches`: AND when either field was starred, OR when neither was.
    fn day_matches(&self, t: DateTime<Utc>, loc: Tz) -> bool {
        let l = local(t, loc);
        let dom_match = shl(1, l.day()) & self.dom > 0;
        let dow_match = shl(1, l.weekday().num_days_from_sunday()) & self.dow > 0;
        if self.dom & STAR_BIT > 0 || self.dow & STAR_BIT > 0 {
            return dom_match && dow_match;
        }
        dom_match || dow_match
    }
}

/// The wall clock in `loc`, which is what every field read in `Next` is.
fn local(t: DateTime<Utc>, loc: Tz) -> DateTime<Tz> {
    t.with_timezone(&loc)
}

/// Go's `Time.Truncate`, which rounds down on the **absolute** time since the
/// zero instant — not on the wall clock. Equivalent for the whole-minute
/// offsets every modern zone has, and this is the definition either way.
fn truncate(t: DateTime<Utc>, secs: i64) -> DateTime<Utc> {
    let s = t.timestamp();
    Utc.timestamp_opt(s - s.rem_euclid(secs), 0)
        .single()
        .unwrap_or(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(loc: Tz, y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        go_date(loc, y, m as i32, d as i32, h as i32, mi as i32, 0, 0)
    }

    #[test]
    fn five_fields_are_required() {
        assert!(parse("* * * * *", Tz::UTC).is_ok());
        assert!(parse("* * * * * *", Tz::UTC).is_err());
        assert!(parse("* * * *", Tz::UTC).is_err());
    }

    #[test]
    fn descriptors_parse_and_at_is_not_one() {
        assert!(parse("@daily", Tz::UTC).is_ok());
        assert!(parse("@every 90s", Tz::UTC).is_ok());
        assert!(parse("@fortnightly", Tz::UTC).is_err());
    }

    /// `?` must set the star bit, or a `0 0 ? * *` spec silently ORs the two
    /// day fields and fires on every day-of-week as well.
    #[test]
    fn question_mark_sets_the_star_bit() {
        let Schedule::Spec(spec) = parse("0 0 ? * *", Tz::UTC).expect("parses") else {
            panic!("expected a spec");
        };
        assert!(spec.dom & STAR_BIT > 0);
    }

    /// robfig's own extension, and the one a POSIX reading gets wrong.
    #[test]
    fn a_step_from_a_value_runs_to_the_field_maximum() {
        let Schedule::Spec(spec) = parse("0 9/4 * * *", Tz::UTC).expect("parses") else {
            panic!("expected a spec");
        };
        // 9, 13, 17, 21 — and nothing before 9.
        assert_eq!(spec.hour, (1 << 9) | (1 << 13) | (1 << 17) | (1 << 21));
    }

    #[test]
    fn go_durations_parse_the_way_go_parses_them() {
        assert_eq!(parse_go_duration("1h30m"), Ok(Duration::minutes(90)));
        assert_eq!(parse_go_duration("100ms"), Ok(Duration::milliseconds(100)));
        assert_eq!(parse_go_duration("1.5s"), Ok(Duration::milliseconds(1500)));
        assert_eq!(parse_go_duration("0"), Ok(Duration::zero()));
        assert!(parse_go_duration("soon").is_err());
        assert!(parse_go_duration("").is_err());
        assert!(parse_go_duration("5").is_err());
    }

    /// `Every` floors at a second, so a sub-second `@every` is not a busy loop.
    #[test]
    fn every_floors_at_one_second() {
        assert_eq!(every(Duration::milliseconds(100)), Duration::seconds(1));
        assert_eq!(every(Duration::milliseconds(1500)), Duration::seconds(1));
    }

    /// The behaviour the module comment leads with: an absolute hour step means
    /// a daily 02:00 job does not run at all on the spring-forward day.
    #[test]
    fn a_spring_forward_gap_skips_the_day_entirely() {
        let berlin: Tz = "Europe/Berlin".parse().expect("known zone");
        let sched = parse("0 2 * * *", berlin).expect("parses");
        let from = at(berlin, 2026, 3, 28, 12, 0);
        let next = sched.next(from).expect("has a next run");
        assert_eq!(
            next.with_timezone(&berlin)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            "2026-03-30 02:00",
            "02:00 does not exist on 2026-03-29 and must not be reached"
        );
    }

    /// Five years with no match is Go's zero time, which gocron turns into
    /// `ErrCronJobInvalid` rather than scheduling a job that never fires.
    #[test]
    fn a_spec_that_never_matches_answers_none() {
        let sched = parse("0 0 30 2 *", Tz::UTC).expect("February 30th parses");
        assert_eq!(sched.next(at(Tz::UTC, 2026, 2, 10, 9, 15)), None);
    }
}

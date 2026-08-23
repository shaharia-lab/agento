//! When a scheduled task fires — `internal/scheduler`'s `buildJobDefinition`
//! and the `gocron/v2` semantics behind it.
//!
//! # What this is, and what it deliberately is not
//!
//! It is the schedule computation: given a stored `ScheduledTask`'s type and
//! config, which of gocron's four job types Agento builds, and what the
//! resulting job's fire times are. It is **not** the scheduler and not the
//! executor. Nothing calls it yet: two processes sharing `~/.agento` share one
//! `scheduled_tasks` table, so a task scheduled by both fires twice, and
//! `cmd/web.go` starts the sidecar's scheduler on every boot. Ownership moves
//! in one commit that stops the sidecar scheduling — the #289 model — and that
//! commit is blocked on `chat/runner.rs::build_options`, which still refuses an
//! agent whose tools come from an integration this build cannot host. Since #313
//! that is `whatsapp` alone — the local in-process server (#310), github (#311),
//! confluence (#317), jira (#316), slack (#315), telegram (#314) and google
//! (#313) are all hosted here — so the remaining blockers are that one type and
//! the two non-integration refusals. A chat answers a 500 on that refusal; a
//! scheduled task records a failed `job_history` row, because a run that simply
//! never happened would be indistinguishable from one that was not due. See
//! `super::executor`.
//!
//! Porting this half now is worth it because it is the half that is
//! *verifiable* before ownership moves, and the half most likely to be subtly
//! wrong. `desktop/parity/scheduler_vectors.json` is generated from a real
//! `gocron.Scheduler` driven by a fake clock and asserted by both languages,
//! the way `gopath_vectors.json` pins `filepath.Clean`.
//!
//! # The three things that are silent when wrong
//!
//! **`run_immediately` is a one-time job at `now + 2s`, not "now".** gocron
//! drops one-time start times that are not strictly in the future and then
//! refuses the job outright, so "now" would never run at all. The two seconds
//! are the whole mechanism.
//!
//! **`every_days` with an `at_time` is a `DailyJob`, not a 24-hour
//! `DurationJob`.** A daily job holds the wall clock across a DST transition; a
//! duration job adds 24 absolute hours and walks off it — a 12:00 run becomes
//! 13:00 the morning the clocks go forward, and stays there.
//!
//! **A malformed `at_time` falls back to that 24-hour duration job with no
//! error.** `buildIntervalJob` calls `buildDailyAtTimeJob` and discards its
//! error, so `9am`, `09:00:00`, `25:00` and `09:60` all schedule *something* —
//! just not what was asked for. Nothing is logged and nothing is returned.
//!
//! # Two more that the vectors pin
//!
//! A cron expression's semantics are `robfig/cron`'s, not POSIX's — see
//! [`cron`]. And gocron guards against a fall-back hour firing a cron job
//! twice: when the next run has the same wall clock as the last, it skips to
//! the one after. That guard is why a daily `0 2 * * *` runs once on the
//! October Sunday when 02:00 happens twice, while `*/30 * * * *` runs through
//! the repeated hour exactly as it reads.
//!
//! # Known divergences, all of them past the far edge of a real schedule
//!
//! **Beyond ~2038 the offsets can differ by an hour.** `chrono_tz`'s transition
//! table is finite and falls back to standard time once it runs out, while Go's
//! zoneinfo carries a POSIX TZ footer and keeps applying the last rule
//! indefinitely. Europe/Berlin on 23 May is CEST here for 2026, 2037 and 2038
//! and CET for 2100, 2200 and 2318. The *instant* still agrees — a differential
//! fuzz surfaced it as `@every 2562047h` rendering `+02:00` in Go and `+01:00`
//! here — but a daily job's wall-clock arithmetic then walks an hour off. The
//! vectors deliberately stop short of it, because pinning a date that far out
//! would pin `chrono_tz`'s table rather than gocron's behaviour.
//!
//! **A daily job whose interval overflows the calendar answers "never".**
//! `every_days` is unbounded (`validateScheduleConfig` imposes no ceiling), and
//! `every_days = 2^31` with an `at_time` puts Go's next run in the year
//! 5,881,636 — outside `chrono`'s ±262,143. [`next_daily`] returns the zero
//! time rather than panicking. See its comment.

pub mod cron;

use chrono::{DateTime, Datelike, Duration, FixedOffset, Offset, Timelike, Utc};
use chrono_tz::Tz;

use crate::native::analytics::buckets::go_date;

/// The task's stored schedule, decoded by the module that already reads the
/// column. Two decoders of one JSON shape is precisely how the two would drift.
pub use super::tasks::ScheduleConfig;

/// Which gocron job type `buildJobDefinition` chose, with its parameters.
///
/// The *choice* is the interesting part: it is where the silent `at_time`
/// fallback shows up, and a duration job and a daily job at the same nominal
/// interval answer differently the moment a DST transition is involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobDefinition {
    /// `gocron.OneTimeJob`. Carries the offset it was written with, because a
    /// one-off's `run_at` keeps its own zone rather than the scheduler's.
    OneTime(DateTime<FixedOffset>),
    /// `gocron.DurationJob`.
    Duration(Duration),
    /// `gocron.DailyJob` with a single at-time.
    Daily {
        interval: i64,
        hour: u32,
        minute: u32,
        second: u32,
    },
    /// `gocron.CronJob(expr, false)`.
    Cron(String),
}

impl JobDefinition {
    /// The name the vectors record, and the only observable evidence of which
    /// branch was taken.
    pub fn kind(&self) -> &'static str {
        match self {
            JobDefinition::OneTime(_) => "one_time",
            JobDefinition::Duration(_) => "duration",
            JobDefinition::Daily { .. } => "daily",
            JobDefinition::Cron(_) => "cron",
        }
    }
}

/// A failure of Agento's own builder, before gocron ever sees the definition.
///
/// Each schedule type has exactly one, which is why the vectors can classify
/// them without quoting Go's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// `one_off` whose `run_at` is not RFC 3339.
    RunAt,
    /// `interval` with no positive `every_*`.
    InvalidInterval,
    /// A `schedule_type` the switch does not know.
    UnknownType,
}

impl BuildError {
    pub fn class(self) -> &'static str {
        match self {
            BuildError::RunAt => "build:run_at",
            BuildError::InvalidInterval => "build:invalid_interval",
            BuildError::UnknownType => "build:unknown_type",
        }
    }
}

/// A failure of `gocron.Scheduler.NewJob` on a definition that built fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    OneTimePast,
    CronParse,
    CronInvalid,
    DurationZero,
    DurationNegative,
    DailyZeroInterval,
    DailyHours,
    DailyMinutesSeconds,
}

impl ScheduleError {
    pub fn class(self) -> &'static str {
        match self {
            ScheduleError::OneTimePast => "schedule:one_time_past",
            ScheduleError::CronParse => "schedule:cron_parse",
            ScheduleError::CronInvalid => "schedule:cron_invalid",
            ScheduleError::DurationZero => "schedule:duration_zero",
            ScheduleError::DurationNegative => "schedule:duration_negative",
            ScheduleError::DailyZeroInterval => "schedule:daily_zero_interval",
            ScheduleError::DailyHours => "schedule:daily_hours",
            ScheduleError::DailyMinutesSeconds => "schedule:daily_minutes_seconds",
        }
    }
}

/// `buildJobDefinition`.
///
/// `now` is what `run_immediately` is measured from — Go reads `time.Now()`
/// inside the function; taking it as a parameter is the one shape change here,
/// and it is what makes the rule testable.
pub fn build_job_definition(
    schedule_type: &str,
    cfg: &ScheduleConfig,
    now: DateTime<FixedOffset>,
) -> Result<JobDefinition, BuildError> {
    match schedule_type {
        "run_immediately" => Ok(JobDefinition::OneTime(now + Duration::seconds(2))),
        "one_off" => parse_go_rfc3339(&cfg.run_at)
            .map(JobDefinition::OneTime)
            .ok_or(BuildError::RunAt),
        "interval" => build_interval_job(cfg),
        "cron" => Ok(JobDefinition::Cron(cfg.expression.clone())),
        _ => Err(BuildError::UnknownType),
    }
}

/// `time.Parse(time.RFC3339, s)`, which is not `chrono`'s RFC 3339.
///
/// They disagree in **five** ways, in both directions, and `run_at` is free-form
/// client input, so every one of them is reachable from `POST /api/tasks`. The
/// table and the transcription live in [`crate::native::gotime`], which is the
/// single implementation: #275 found three of the five by a differential run of
/// 636 shapes through a real `gocron.Scheduler`, and #313 needed the same parse
/// for a Google token's `expiry` and found two more — a one-digit hour and an
/// offset hour past 23, both of which Go accepts and this used to refuse to
/// schedule.
fn parse_go_rfc3339(s: &str) -> Option<DateTime<FixedOffset>> {
    crate::native::gotime::parse_rfc3339(s)
}

/// Go's `time.Duration` arithmetic: `int64` nanoseconds that **wrap** rather
/// than trap or saturate.
///
/// `validateScheduleConfig` puts no ceiling on `every_minutes`/`every_hours`/
/// `every_days`, so `POST /api/tasks` can store any `int64` and
/// `buildIntervalJob` multiplies it out unchecked. Go answers *something* for
/// every one of them — `every_hours = 2^31` wraps to a real 2081 fire time,
/// `every_days = math.MaxInt64` wraps negative and gocron rejects it as
/// `ErrDurationJobIntervalNegative`. `chrono::TimeDelta::minutes` and
/// `::hours` panic out of bounds instead, and a panic is the one answer gocron
/// never gives. `TimeDelta::nanoseconds` is total over `i64`, so routing
/// through it reproduces the wrap exactly.
fn go_duration(n: i64, unit_nanos: i64) -> Duration {
    Duration::nanoseconds(n.wrapping_mul(unit_nanos))
}

const NANOS_PER_MINUTE: i64 = 60_000_000_000;
const NANOS_PER_HOUR: i64 = 3_600_000_000_000;

/// `buildIntervalJob`. The guards are `> 0`, in that order — so a negative
/// `every_minutes` is not rejected, it is simply never chosen, and a config
/// carrying only a negative value has no interval at all.
fn build_interval_job(cfg: &ScheduleConfig) -> Result<JobDefinition, BuildError> {
    if cfg.every_minutes > 0 {
        return Ok(JobDefinition::Duration(go_duration(
            cfg.every_minutes,
            NANOS_PER_MINUTE,
        )));
    }
    if cfg.every_hours > 0 {
        return Ok(JobDefinition::Duration(go_duration(
            cfg.every_hours,
            NANOS_PER_HOUR,
        )));
    }
    if cfg.every_days > 0 {
        if !cfg.at_time.is_empty() {
            if let Some(daily) = build_daily_at_time_job(cfg) {
                return Ok(daily);
            }
            // THE SILENT FALLBACK. Go discards the error here; so does this.
        }
        // `time.Duration(cfg.EveryDays) * 24 * time.Hour`, left to right — so
        // the `* 24` is its own wrapping int64 multiply, not part of one.
        return Ok(JobDefinition::Duration(go_duration(
            cfg.every_days.wrapping_mul(24),
            NANOS_PER_HOUR,
        )));
    }
    Err(BuildError::InvalidInterval)
}

/// `buildDailyAtTimeJob`. `None` is every one of Go's five error returns, which
/// the caller cannot distinguish either.
///
/// `Atoi` does not require zero padding, so `7:5` is 07:05 — and it accepts a
/// sign, so `-1:00` is caught by the range check rather than the parse.
fn build_daily_at_time_job(cfg: &ScheduleConfig) -> Option<JobDefinition> {
    let parts: Vec<&str> = cfg.at_time.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let hour: i64 = parts[0].parse().ok()?;
    let minute: i64 = parts[1].parse().ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) {
        return None;
    }
    if cfg.every_days < 0 {
        return None;
    }
    Some(JobDefinition::Daily {
        interval: cfg.every_days,
        hour: hour as u32,
        minute: minute as u32,
        second: 0,
    })
}

/// One fire time: the instant, plus the offset gocron renders it at.
///
/// Go's `time.Time` carries its location, and the two halves matter separately
/// here — comparisons and arithmetic use the instant, while the RFC 3339 the
/// vectors record uses the offset. They diverge for exactly one job type: a
/// one-off keeps the offset its `run_at` was written with, while every other
/// type renders in the scheduler's location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fire {
    pub instant: DateTime<Utc>,
    pub offset: FixedOffset,
}

impl Fire {
    fn zoned(t: DateTime<Tz>) -> Self {
        Fire {
            instant: t.with_timezone(&Utc),
            offset: t.offset().fix(),
        }
    }

    fn fixed(t: DateTime<FixedOffset>) -> Self {
        Fire {
            instant: t.with_timezone(&Utc),
            offset: *t.offset(),
        }
    }

    /// Go's `time.Time{}` — the zero value gocron returns for "no next run".
    fn zero() -> Self {
        Fire {
            instant: DateTime::<Utc>::from_timestamp(-62_135_596_800, 0)
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
            offset: FixedOffset::east_opt(0).expect("UTC is a valid offset"),
        }
    }

    pub fn is_zero(self) -> bool {
        self == Fire::zero()
    }

    /// Go's `time.RFC3339Nano`: no fractional part when there is none, and
    /// trailing zeros trimmed when there is.
    pub fn rfc3339(self) -> String {
        let t = self.instant.with_timezone(&self.offset);
        let mut out = t.format("%Y-%m-%dT%H:%M:%S").to_string();
        let nanos = t.nanosecond();
        if nanos != 0 {
            let mut frac = format!("{nanos:09}");
            while frac.ends_with('0') {
                frac.pop();
            }
            out.push('.');
            out.push_str(&frac);
        }
        if self.offset.local_minus_utc() == 0 {
            out.push('Z');
        } else {
            out.push_str(&t.format("%:z").to_string());
        }
        out
    }
}

/// A definition gocron has accepted: the `internalJob.jobSchedule` it installed.
#[derive(Debug, Clone)]
pub enum JobSchedule {
    /// Sorted, deduplicated and filtered to the future, as `setup` leaves it.
    /// These keep their own offsets; every other variant renders in `loc`.
    OneTime(Vec<DateTime<FixedOffset>>),
    /// `lastRun.Add(d)` keeps `lastRun`'s *location*, not its offset — which is
    /// exactly how a 24h "daily" interval walks off the wall clock across a DST
    /// transition, so `loc` is part of the schedule rather than a formatting
    /// detail.
    Duration {
        every: Duration,
        loc: Tz,
    },
    Daily {
        interval: i64,
        at_times: Vec<(u32, u32, u32)>,
        loc: Tz,
    },
    Cron {
        schedule: cron::Schedule,
        loc: Tz,
    },
}

/// `JobDefinition.setup` for the four types Agento builds, plus the parse
/// gocron's `defaultCron.IsValid` performs while validating.
///
/// `now` is the scheduler's clock at `NewJob` time, which is what the one-time
/// job's past filter is measured against.
pub fn setup(
    def: &JobDefinition,
    loc: Tz,
    now: DateTime<Utc>,
) -> Result<JobSchedule, ScheduleError> {
    match def {
        JobDefinition::OneTime(start) => {
            // One element, so the sort and the dedupe are no-ops; the filter is
            // not. `BinarySearchFunc` reports an exact hit and `setup` then
            // skips past it, so a run_at of exactly now is already too late.
            let mut times = vec![*start];
            times.retain(|t| t.with_timezone(&Utc) > now);
            if times.is_empty() {
                return Err(ScheduleError::OneTimePast);
            }
            Ok(JobSchedule::OneTime(times))
        }
        JobDefinition::Duration(d) => {
            if d.is_zero() {
                return Err(ScheduleError::DurationZero);
            }
            if *d < Duration::zero() {
                return Err(ScheduleError::DurationNegative);
            }
            Ok(JobSchedule::Duration { every: *d, loc })
        }
        JobDefinition::Daily {
            interval,
            hour,
            minute,
            second,
        } => {
            // `convertAtTimesToDateTime` validates before the interval check.
            if *hour > 23 {
                return Err(ScheduleError::DailyHours);
            }
            if *minute > 59 || *second > 59 {
                return Err(ScheduleError::DailyMinutesSeconds);
            }
            if *interval == 0 {
                return Err(ScheduleError::DailyZeroInterval);
            }
            Ok(JobSchedule::Daily {
                interval: *interval,
                at_times: vec![(*hour, *minute, *second)],
                loc,
            })
        }
        JobDefinition::Cron(expr) => {
            let schedule = cron::parse(expr, loc).map_err(|_| ScheduleError::CronParse)?;
            // "Parses but never fires" is a *different* gocron error, and the
            // five-year search is what tells them apart.
            if schedule.next(now).is_none() {
                return Err(ScheduleError::CronInvalid);
            }
            Ok(JobSchedule::Cron { schedule, loc })
        }
    }
}

impl JobSchedule {
    /// `jobSchedule.next(lastRun)`. The zero `Fire` in and out is Go's zero
    /// time, which is how an exhausted schedule answers — and feeding it back
    /// is what makes a one-time job's `NextRuns` oscillate rather than stop.
    pub fn next(&self, last_run: Fire) -> Fire {
        match self {
            JobSchedule::OneTime(times) => next_one_time(times, last_run),
            JobSchedule::Duration { every, loc } => {
                Fire::zoned((last_run.instant + *every).with_timezone(loc))
            }
            JobSchedule::Daily {
                interval,
                at_times,
                loc,
            } => next_daily(*interval, at_times, *loc, last_run),
            JobSchedule::Cron { schedule, loc } => next_cron(schedule, *loc, last_run),
        }
    }
}

/// `oneTimeJob.next`: a binary search over the remaining times.
fn next_one_time(times: &[DateTime<FixedOffset>], last_run: Fire) -> Fire {
    let key = last_run.instant;
    let mut idx = times.partition_point(|t| t.with_timezone(&Utc) < key);
    if idx < times.len() && times[idx].with_timezone(&Utc) == key {
        idx += 1;
    }
    times.get(idx).copied().map_or(Fire::zero(), Fire::fixed)
}

/// `durationJob`'s sibling: the daily job's two-pass day search.
///
/// The scheduler runs with the default DST policy, so neither the skip arm nor
/// the run-after-transition arm applies: a wall-clock time a gap removed is
/// taken as whatever `time.Date` normalizes it to, and a time that occurs twice
/// is taken once.
fn next_daily(interval: i64, at_times: &[(u32, u32, u32)], loc: Tz, last_run: Fire) -> Fire {
    if let Some(next) = next_day(at_times, loc, last_run.instant, true) {
        return Fire::zoned(next.with_timezone(&loc));
    }
    let l = last_run.instant.with_timezone(&loc);
    // `lastRun.Day() + int(d.interval)`, in Go's width and with Go's wrap.
    // `interval` is `every_days`, which nothing bounds, so this is the only
    // place a stored `int64` reaches the calendar unscaled.
    let Some(day) = calendar_day(i64::from(l.day()).wrapping_add(interval)) else {
        return Fire::zero();
    };
    let start_next_day = go_date(loc, l.year(), l.month() as i32, day, 0, 0, 0, 0);
    match next_day(at_times, loc, start_next_day, false) {
        Some(next) => Fire::zoned(next.with_timezone(&loc)),
        None => Fire::zero(),
    }
}

/// Days `go_date` can still turn into a date, as a day-of-month offset.
///
/// `time.Date` counts days from the first of the month and normalizes without
/// bound; `chrono`'s calendar stops at ±262,143 years, and `NaiveDate + Duration`
/// **panics** past it rather than saturating. ±9,000,000 days is roughly ±24,600
/// years — beyond every fire time a person will ever configure and comfortably
/// inside what `chrono` can hold.
const MAX_CALENDAR_DAY: i64 = 9_000_000;

/// A day-of-month `go_date` can be handed, or `None` where Go would answer a
/// date `chrono` cannot represent.
///
/// The caller turns `None` into gocron's zero time — "no next run" — which is a
/// divergence, and the deliberate one: `every_days = 2^31` with an `at_time`
/// fires in the year 5,881,636 for Go. There is no `chrono` value for that, so
/// the choice is between the wrong answer and a panic on a request the HTTP API
/// accepts today. See the module comment.
fn calendar_day(day: i64) -> Option<i32> {
    if !(-MAX_CALENDAR_DAY..=MAX_CALENDAR_DAY).contains(&day) {
        return None;
    }
    i32::try_from(day).ok()
}

/// `dailyJob.nextDay`.
fn next_day(
    at_times: &[(u32, u32, u32)],
    loc: Tz,
    last_run: DateTime<Utc>,
    first_pass: bool,
) -> Option<DateTime<Utc>> {
    let l = last_run.with_timezone(&loc);
    for (h, m, s) in at_times {
        let at_date = go_date(
            loc,
            l.year(),
            l.month() as i32,
            l.day() as i32,
            *h as i32,
            *m as i32,
            *s as i32,
            0,
        );
        if first_pass && at_date > last_run {
            return Some(at_date);
        }
        if !first_pass && at_date >= last_run {
            return Some(at_date);
        }
    }
    None
}

/// `cronJob.next`, including the fall-back guard.
///
/// `cron.Next` always advances at least one second in absolute time, so an
/// identical wall clock can only mean the hour repeated — which would otherwise
/// run a daily cron job twice on the same calendar day.
fn next_cron(schedule: &cron::Schedule, loc: Tz, last_run: Fire) -> Fire {
    let Some(next) = schedule.next(last_run.instant) else {
        return Fire::zero();
    };
    let last_local = last_run.instant.with_timezone(&loc);
    let next_local = next.with_timezone(&loc);
    if same_wall_clock(last_local, next_local) {
        return match schedule.next(next) {
            Some(after) => Fire::zoned(after.with_timezone(&loc)),
            None => Fire::zero(),
        };
    }
    Fire::zoned(next_local)
}

fn same_wall_clock(a: DateTime<Tz>, b: DateTime<Tz>) -> bool {
    a.year() == b.year()
        && a.month() == b.month()
        && a.day() == b.day()
        && a.hour() == b.hour()
        && a.minute() == b.minute()
        && a.second() == b.second()
}

/// `scheduler.advancePastNow`. `None` means the schedule is exhausted or has
/// stopped making progress, which the caller treats as "remove the job".
pub fn advance_past_now(sched: &JobSchedule, mut next: Fire, now: DateTime<Utc>) -> Option<Fire> {
    while next.instant < now {
        let n = sched.next(next);
        if n.is_zero() || n.instant <= next.instant {
            return None;
        }
        next = n;
    }
    Some(next)
}

/// `Job.NextRuns(count)` for a job installed by `selectNewJob`.
///
/// The first element is the scheduler's own initial `nextRun` — `next(now)`
/// advanced past now — and the rest are successive `next` applications, exactly
/// as gocron computes them. `None` is the zero time.
pub fn next_runs(
    sched: &JobSchedule,
    loc: Tz,
    now: DateTime<Utc>,
    count: usize,
) -> Vec<Option<Fire>> {
    let start = Fire::zoned(now.with_timezone(&loc));
    // Go calls `advancePastNow` only when the first run is already past, but
    // the helper's own loop makes the guard redundant — and calling it
    // unconditionally is what keeps the zero-time case (a one-time job whose
    // single run gocron then searches back to) on the same path Go takes.
    let Some(first) = advance_past_now(sched, sched.next(start), now) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(count);
    out.push(Some(first));
    let mut prev = first;
    for _ in 1..count {
        let next = sched.next(prev);
        prev = next;
        out.push(if next.is_zero() { None } else { Some(next) });
    }
    out
}

/// Everything one (schedule, location, now) triple produces — which is exactly
/// what one vector records.
///
/// `definition` survives a scheduling failure on purpose: a one-off in the past
/// *built* fine as a one-time job and was then refused, and telling those two
/// apart is half of what the vectors are for.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub definition: Option<JobDefinition>,
    pub error: Option<&'static str>,
    pub next_runs: Vec<Option<Fire>>,
}

/// Build, install and run a schedule in one call.
pub fn fire_times(
    schedule_type: &str,
    cfg: &ScheduleConfig,
    loc: Tz,
    now: DateTime<Utc>,
    count: usize,
) -> Outcome {
    let now_local = now.with_timezone(&loc);
    let def = match build_job_definition(
        schedule_type,
        cfg,
        now_local.with_timezone(&now_local.offset().fix()),
    ) {
        Ok(def) => def,
        Err(err) => {
            return Outcome {
                definition: None,
                error: Some(err.class()),
                next_runs: Vec::new(),
            }
        }
    };
    match setup(&def, loc, now) {
        Ok(sched) => Outcome {
            next_runs: next_runs(&sched, loc, now, count),
            definition: Some(def),
            error: None,
        },
        Err(err) => Outcome {
            definition: Some(def),
            error: Some(err.class()),
            next_runs: Vec::new(),
        },
    }
}

pub mod executor;
pub mod runtime;

#[cfg(test)]
mod tests_vectors;

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("literal parses")
            .with_timezone(&Utc)
    }

    /// The unbounded `every_*` values. `validateScheduleConfig` has no ceiling,
    /// so every one of these is reachable from `POST /api/tasks`, and Go answers
    /// all of them by wrapping its int64 nanoseconds. The vectors pin the
    /// answers; this pins that *nothing panics*, which is the part a wrong
    /// answer would not tell you apart from.
    #[test]
    fn an_unbounded_interval_answers_rather_than_panicking() {
        let now = utc("2026-02-10T09:15:30Z");
        for cfg in [
            ScheduleConfig {
                every_minutes: i64::MAX,
                ..Default::default()
            },
            ScheduleConfig {
                every_hours: i64::MAX,
                ..Default::default()
            },
            ScheduleConfig {
                every_days: i64::MAX,
                ..Default::default()
            },
            ScheduleConfig {
                every_hours: 1 << 31,
                ..Default::default()
            },
            ScheduleConfig {
                every_minutes: 1 << 40,
                ..Default::default()
            },
            // The daily branch, whose interval reaches the calendar unscaled.
            ScheduleConfig {
                every_days: i64::MAX,
                at_time: "09:00".into(),
                ..Default::default()
            },
            ScheduleConfig {
                every_days: 1 << 31,
                at_time: "09:00".into(),
                ..Default::default()
            },
        ] {
            let out = fire_times("interval", &cfg, Tz::UTC, now, 4);
            assert!(
                out.definition.is_some(),
                "{cfg:?} built nothing, but the builder's only failure is an absent interval"
            );
        }
    }

    /// Go's arithmetic, not a bounds check: `2^31` hours is 7.7e21 nanoseconds
    /// and wraps to an ordinary positive interval, while `MaxInt64` days wraps
    /// negative and gocron then refuses the job.
    #[test]
    fn go_durations_wrap_where_chrono_would_panic() {
        assert_eq!(
            go_duration(1 << 31, NANOS_PER_HOUR),
            Duration::nanoseconds((1i64 << 31).wrapping_mul(NANOS_PER_HOUR))
        );
        assert!(go_duration(i64::MAX.wrapping_mul(24), NANOS_PER_HOUR) < Duration::zero());
        assert_eq!(go_duration(5, NANOS_PER_MINUTE), Duration::minutes(5));
    }

    /// A daily interval Go answers past the end of `chrono`'s calendar. The
    /// answer diverges — gocron's zero time rather than the year 5,881,636 —
    /// and the point of the test is that it is an answer at all.
    #[test]
    fn a_daily_interval_past_the_calendar_is_the_zero_time() {
        assert_eq!(calendar_day(10), Some(10));
        assert_eq!(calendar_day(i64::from(u32::MAX)), None);
        assert_eq!(calendar_day(i64::MIN), None);

        let sched = JobSchedule::Daily {
            interval: 1 << 31,
            at_times: vec![(9, 0, 0)],
            loc: Tz::UTC,
        };
        // 09:00 is behind 12:00, so the first pass finds nothing and the second
        // one has to walk 2^31 days forward.
        let fired = sched.next(Fire {
            instant: utc("2026-02-10T12:00:00Z"),
            offset: FixedOffset::east_opt(0).expect("UTC"),
        });
        assert!(fired.is_zero());
    }

    /// Go's RFC 3339, which is neither RFC 3339 nor chrono's reading of it.
    /// `run_at` is free-form client input, so this is the most reachable
    /// divergence in the module.
    #[test]
    fn run_at_is_parsed_the_way_go_parses_it() {
        assert!(
            parse_go_rfc3339("2026-06-01t12:00:00z").is_none(),
            "lowercase designators are legal per §5.6 and rejected by Go"
        );
        assert!(parse_go_rfc3339("2026-06-01T12:00:00z").is_none());
        assert!(parse_go_rfc3339("2026-06-01t12:00:00Z").is_none());
        assert!(
            parse_go_rfc3339("2026-06-01 12:00:00Z").is_none(),
            "a space is not the layout's literal T either"
        );

        let comma = parse_go_rfc3339("2026-06-01T12:00:00,5Z").expect("Go accepts a comma");
        assert_eq!(
            comma,
            parse_go_rfc3339("2026-06-01T12:00:00.5Z").expect("and a period")
        );

        assert!(parse_go_rfc3339("2026-06-01T12:00:00+02:00").is_some());
        assert!(parse_go_rfc3339("2026-06-01").is_none());
        assert!(parse_go_rfc3339("tomorrow").is_none());
        assert!(parse_go_rfc3339("").is_none());
    }
}

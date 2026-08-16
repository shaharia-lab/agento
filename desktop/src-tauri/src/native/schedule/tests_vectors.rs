//! The Rust half of `desktop/parity/scheduler_vectors.json`.
//!
//! The file is generated from a real `gocron.Scheduler` on a fake clock
//! (`go test ./internal/scheduler/ -update-scheduler-vectors`) and asserted
//! against Go's own behaviour by `internal/scheduler/schedule_vectors_test.go`.
//! This half asserts the port answers the same — so a divergence fails one
//! language against the other's *real output* rather than against a belief
//! about it, which is the whole reason `gopath_vectors.json` exists in this
//! shape.

use super::*;

const VECTORS: &str = include_str!("../../../../parity/scheduler_vectors.json");

#[derive(serde::Deserialize)]
struct VectorFile {
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    why: String,
    location: String,
    now: String,
    schedule_type: String,
    schedule_config: ScheduleConfig,
    definition: String,
    error: String,
    next_runs: Vec<Option<String>>,
    #[serde(default)]
    one_time_offset_ms: Option<i64>,
}

fn vectors() -> Vec<Case> {
    serde_json::from_str::<VectorFile>(VECTORS)
        .expect("scheduler_vectors.json is embedded and must parse")
        .cases
}

/// The whole point of the file.
#[test]
fn every_vector_matches_what_go_produced() {
    let cases = vectors();
    assert!(cases.len() >= 75, "vectors look truncated");

    for case in cases {
        let loc: Tz = case
            .location
            .parse()
            .unwrap_or_else(|_| panic!("{}: unknown zone {}", case.name, case.location));
        let now = DateTime::parse_from_rfc3339(&case.now)
            .unwrap_or_else(|e| panic!("{}: bad now {}: {e}", case.name, case.now))
            .with_timezone(&Utc);

        // `run_immediately` has no absolute instant to freeze; the vector
        // records the offset from the build instant instead.
        if let Some(want_ms) = case.one_time_offset_ms {
            let def = build_job_definition(
                &case.schedule_type,
                &case.schedule_config,
                now.with_timezone(&loc).fixed_offset(),
            )
            .unwrap_or_else(|e| panic!("{}: build failed with {}", case.name, e.class()));
            let JobDefinition::OneTime(at) = def else {
                panic!("{}: definition is {}, want one_time", case.name, def.kind());
            };
            let got_ms = (at.with_timezone(&Utc) - now).num_milliseconds();
            assert_eq!(got_ms, want_ms, "{} — {}", case.name, case.why);
            continue;
        }

        let out = fire_times(
            &case.schedule_type,
            &case.schedule_config,
            loc,
            now,
            case.next_runs.len().max(4),
        );

        let got_definition = out.definition.as_ref().map_or("", JobDefinition::kind);
        assert_eq!(
            got_definition, case.definition,
            "{} — {}: wrong gocron job type",
            case.name, case.why
        );
        assert_eq!(
            out.error.unwrap_or(""),
            case.error,
            "{} — {}: wrong failure",
            case.name,
            case.why
        );

        let got: Vec<Option<String>> = out
            .next_runs
            .iter()
            .take(case.next_runs.len())
            .map(|f| f.map(Fire::rfc3339))
            .collect();
        assert_eq!(got, case.next_runs, "{} — {}", case.name, case.why);
    }
}

/// The vector table is only as good as its coverage, and three behaviours are
/// the reason this port exists. Assert they are actually in the file, so
/// deleting a case fails rather than quietly shrinking the guarantee.
#[test]
fn the_three_silent_behaviours_are_covered() {
    let cases = vectors();
    let named = |n: &str| cases.iter().find(|c| c.name == n);

    let immediate = named("run_immediately/plain").expect("run_immediately vector");
    assert_eq!(
        immediate.one_time_offset_ms,
        Some(2000),
        "run_immediately must be a one-time job two seconds out"
    );

    let fallback = named("interval/at_time_hour_out_of_range").expect("at_time fallback vector");
    assert_eq!(
        fallback.definition, "duration",
        "a malformed at_time must fall back to a duration job, silently"
    );
    assert_eq!(
        fallback.error, "",
        "and the fallback must not surface an error"
    );

    let daily = named("interval/daily_at_time_later_today").expect("daily vector");
    assert_eq!(
        daily.definition, "daily",
        "every_days with a valid at_time must be a DailyJob, not 24 hours"
    );
}

/// Three more the file has to keep, for the same reason: each one is a case
/// where the port used to panic or answer differently, and a vector is the only
/// thing standing between that and a silent regression.
#[test]
fn the_reachable_edges_are_covered() {
    let cases = vectors();
    let named = |n: &str| {
        cases
            .iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("missing vector {n}"))
    };

    // Production's own location, which every other vector names explicitly.
    let local = named("cron/scheduler_location_is_time_local");
    assert!(
        local.next_runs[0]
            .as_deref()
            .is_some_and(|t| t.ends_with("+11:00")),
        "generated through time.Local under a pinned TZ, so the offset is the zone's own"
    );

    // An `every_*` that no validation bounds, which Go answers by wrapping.
    assert_eq!(
        named("interval/every_days_overflows_negative").error,
        "schedule:duration_negative",
        "MaxInt64 days wraps negative and gocron refuses it — it does not panic"
    );
    assert_eq!(
        named("interval/every_hours_overflows_into_the_future").definition,
        "duration",
        "and 2^31 hours wraps to an interval gocron happily schedules"
    );

    // A step the field's width cannot hold.
    assert_eq!(
        named("cron/step_wider_than_the_field").error,
        "",
        "Go schedules it; the port must not error, and must not overflow computing the bits"
    );

    // Go's RFC 3339, in both directions.
    assert_eq!(
        named("one_off/lowercase_designators").error,
        "build:run_at",
        "legal per RFC 3339 §5.6 and refused by Go"
    );
    assert_eq!(
        named("one_off/comma_fraction").error,
        "",
        "and a comma decimal, which chrono refuses and Go accepts"
    );
}

/// The two job types answer differently across a spring-forward, which is the
/// entire reason the distinction above matters. Read straight off the vectors
/// so the claim cannot drift from the file.
#[test]
fn a_duration_job_walks_off_the_wall_clock_where_a_daily_job_does_not() {
    let cases = vectors();
    let find = |n: &str| {
        cases
            .iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("missing vector {n}"))
    };

    // Both start on 2026-03-28 in Europe/Berlin, the day before the transition.
    let duration = find("interval/every_days_no_at_time");
    let daily = find("interval/daily_midnight");

    assert!(
        duration.next_runs[0].as_deref() == Some("2026-03-29T13:00:00+02:00"),
        "a 24h duration job from 12:00 lands on 13:00 the morning the clocks move"
    );
    assert!(
        daily.next_runs[0].as_deref() == Some("2026-03-29T00:00:00+01:00")
            && daily.next_runs[1].as_deref() == Some("2026-03-30T00:00:00+02:00"),
        "a daily job holds midnight and lets the offset change under it"
    );
}

/// A one-time job's `NextRuns` does not stop at the zero time — `next(zero)`
/// binary-searches straight back to the single run, so the sequence
/// oscillates. Reproduced rather than tidied, because the port's job is to
/// agree with gocron.
#[test]
fn an_exhausted_one_time_job_oscillates() {
    let loc = Tz::UTC;
    let now = DateTime::parse_from_rfc3339("2026-02-10T09:15:30Z")
        .expect("literal parses")
        .with_timezone(&Utc);
    let out = fire_times(
        "one_off",
        &ScheduleConfig {
            run_at: "2026-06-01T12:00:00Z".into(),
            ..Default::default()
        },
        loc,
        now,
        4,
    );
    let spelled: Vec<Option<String>> = out.next_runs.iter().map(|f| f.map(Fire::rfc3339)).collect();
    assert_eq!(
        spelled,
        vec![
            Some("2026-06-01T12:00:00Z".to_string()),
            None,
            Some("2026-06-01T12:00:00Z".to_string()),
            None,
        ]
    );
}

/// `run_at` keeps the offset it was written with; every other job type renders
/// in the scheduler's location. Getting this wrong is invisible in UTC.
#[test]
fn a_one_off_keeps_its_own_offset() {
    let now = DateTime::parse_from_rfc3339("2026-02-10T09:15:30Z")
        .expect("literal parses")
        .with_timezone(&Utc);
    let out = fire_times(
        "one_off",
        &ScheduleConfig {
            run_at: "2026-06-01T12:00:00+02:00".into(),
            ..Default::default()
        },
        Tz::UTC,
        now,
        1,
    );
    assert_eq!(
        out.next_runs[0].map(Fire::rfc3339).as_deref(),
        Some("2026-06-01T12:00:00+02:00")
    );
}

//! Analytics over sessions whose transcript expired (#708).
//!
//! History outlives the transcript: an expired row stays in every total, and
//! the report only *adds* what the UI needs to say so — a count on the summary
//! and a flag on each leaderboard row. Driven through `analytics` over a
//! migrated database rather than through `aggregate` over hand-built
//! summaries, so the flag is proven to arrive from the column.

use rusqlite::Connection;

use super::analytics;
use super::report::AnalyticsReport;
use crate::native::gojson;
use crate::native::migrate;
use crate::native::settings::DataSettings;

const QUERY: &str = "from=2026-08-01&to=2026-08-02&tz=UTC";

fn corpus() -> Connection {
    let mut conn = Connection::open_in_memory().expect("in-memory database");
    migrate::apply(&mut conn).expect("apply migrations");
    conn.execute_batch(
        "INSERT INTO claude_session_cache
             (session_id, project_path, file_path, file_mtime, start_time, last_activity,
              model, input_tokens, output_tokens, input_cost_usd, output_cost_usd,
              total_cost_usd, active_duration_ms)
         VALUES ('live', '/work/alpha', '/work/alpha/live.jsonl', 0,
                 '2026-08-01 10:00:00 +0000 UTC', '2026-08-01 12:00:00 +0000 UTC',
                 'claude-opus-5', 1000, 200, 1.0, 0.5, 1.5, 60000),
                ('gone', '/work/beta', '/work/beta/gone.jsonl', 0,
                 '2026-08-01 09:00:00 +0000 UTC', '2026-08-01 11:00:00 +0000 UTC',
                 'claude-opus-5', 4000, 800, 3.0, 1.25, 4.25, 120000);",
    )
    .expect("seed sessions");
    conn
}

fn report(conn: &Connection) -> AnalyticsReport {
    analytics(conn, &DataSettings::default(), QUERY).expect("report")
}

fn expire(conn: &Connection) {
    conn.execute(
        "UPDATE claude_session_cache
            SET transcript_expired_at = '2026-09-01 08:30:00 +0000 UTC', preview = ''
          WHERE session_id = 'gone'",
        [],
    )
    .expect("expire");
}

#[test]
fn the_totals_are_the_same_before_and_after_a_session_expires() {
    let conn = corpus();
    let before = report(&conn);
    expire(&conn);
    let after = report(&conn);

    assert_eq!(before.summary.total_sessions, 2);
    assert_eq!(before.summary.total_tokens, 6000);
    assert_eq!(before.summary.estimated_cost_usd, 5.75);

    assert_eq!(after.summary.total_sessions, before.summary.total_sessions);
    assert_eq!(after.summary.total_tokens, before.summary.total_tokens);
    assert_eq!(
        after.summary.estimated_cost_usd,
        before.summary.estimated_cost_usd
    );

    assert_eq!(before.summary.expired_sessions, 0);
    assert_eq!(after.summary.expired_sessions, 1);
}

#[test]
fn an_expired_session_is_flagged_on_every_leaderboard_it_is_on() {
    let conn = corpus();
    expire(&conn);
    let top = report(&conn).top_sessions;

    for (name, board) in [
        ("by_cost", &top.by_cost),
        ("by_duration", &top.by_duration),
        ("by_tokens", &top.by_tokens),
    ] {
        let flags: Vec<(&str, bool)> = board
            .iter()
            .map(|r| (r.session_id.as_str(), r.transcript_expired))
            .collect();
        assert_eq!(flags, vec![("gone", true), ("live", false)], "{name}");
    }
}

/// The omit-when-empty half, which is what keeps
/// `parity/claude_analytics_golden.json` byte-identical.
#[test]
fn a_window_with_no_expired_session_spells_neither_key() {
    let conn = corpus();
    let json = String::from_utf8(gojson::to_vec(&report(&conn)).expect("encode")).expect("utf-8");
    assert!(!json.contains("expired"), "{json}");

    expire(&conn);
    let json = String::from_utf8(gojson::to_vec(&report(&conn)).expect("encode")).expect("utf-8");
    assert!(json.contains("\"expired_sessions\":1}"), "{json}");
    assert!(
        json.contains("\"transcript_expired\":true}"),
        "the flag is the ranking's last key: {json}"
    );
}

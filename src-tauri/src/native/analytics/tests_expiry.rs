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
    assert!(!json.contains("_since"), "{json}");

    expire(&conn);
    let json = String::from_utf8(gojson::to_vec(&report(&conn)).expect("encode")).expect("utf-8");
    // #716's two dates follow the count, in that order, and close the summary.
    assert!(
        json.contains(
            "\"expired_sessions\":1,\"history_since\":\"2026-08-01T09:00:00Z\",\
             \"transcripts_since\":\"2026-08-01T10:00:00Z\"}"
        ),
        "{json}"
    );
    assert!(
        json.contains("\"transcript_expired\":true}"),
        "the flag is the ranking's last key: {json}"
    );
}

// ─── How far back history and transcripts reach (#716) ───────────────────────

fn report_for(conn: &Connection, query: &str) -> AnalyticsReport {
    analytics(conn, &DataSettings::default(), query).expect("report")
}

fn since(r: &AnalyticsReport) -> (Option<String>, Option<String>) {
    let text = |t: Option<crate::native::gotime::GoTime>| t.map(|t| t.rfc3339_nano_utc());
    (
        text(r.summary.history_since),
        text(r.summary.transcripts_since),
    )
}

fn at(text: &str) -> Option<String> {
    Some(text.to_string())
}

#[test]
fn an_expired_session_older_than_every_live_one_separates_the_two_dates() {
    let conn = corpus();
    assert_eq!(since(&report(&conn)), (None, None), "nothing expired yet");

    expire(&conn);
    assert_eq!(
        since(&report(&conn)),
        (at("2026-08-01T09:00:00Z"), at("2026-08-01T10:00:00Z"))
    );
}

/// The dates describe the corpus, not the window: a window holding neither
/// session, and one holding no session at all, answer the same pair — the
/// second through `empty_report`.
#[test]
fn the_two_dates_ignore_the_window() {
    let conn = corpus();
    expire(&conn);
    let expected = (at("2026-08-01T09:00:00Z"), at("2026-08-01T10:00:00Z"));

    for query in [
        QUERY,
        "from=2026-09-01&to=2026-09-30&tz=UTC",
        "from=2020-01-01&to=2020-01-02&tz=UTC",
    ] {
        assert_eq!(since(&report_for(&conn, query)), expected, "{query}");
    }

    let empty = report_for(&conn, "from=2026-09-01&to=2026-09-30&tz=UTC");
    assert_eq!(empty.summary.total_sessions, 0, "the empty-report path");
    assert_eq!(empty.summary.expired_sessions, 0);
}

#[test]
fn a_project_filter_narrows_both_dates() {
    let conn = corpus();
    expire(&conn);

    // `/work/alpha` holds only the live session: nothing of its own expired,
    // so it says nothing, whatever happened in `/work/beta`.
    let alpha = report_for(&conn, &format!("{QUERY}&project=/work/alpha"));
    assert_eq!(since(&alpha), (None, None));

    // `/work/beta` holds only the expired one.
    let beta = report_for(&conn, &format!("{QUERY}&project=/work/beta"));
    assert_eq!(since(&beta), (at("2026-08-01T09:00:00Z"), None));
}

#[test]
fn a_corpus_with_every_transcript_expired_has_no_transcripts_date() {
    let conn = corpus();
    conn.execute(
        "UPDATE claude_session_cache
            SET transcript_expired_at = '2026-09-01 08:30:00 +0000 UTC'",
        [],
    )
    .expect("expire all");

    let r = report(&conn);
    assert_eq!(since(&r), (at("2026-08-01T09:00:00Z"), None));

    let json = String::from_utf8(gojson::to_vec(&r).expect("encode")).expect("utf-8");
    assert!(
        json.contains("\"expired_sessions\":2,\"history_since\":\"2026-08-01T09:00:00Z\"}"),
        "{json}"
    );
    assert!(!json.contains("transcripts_since"), "{json}");
}

/// An expired session that is *newer* than a live one moves neither date off
/// the live minimum: history and transcripts then start on the same day.
#[test]
fn an_expired_session_newer_than_a_live_one_leaves_the_dates_equal() {
    let conn = corpus();
    conn.execute(
        "UPDATE claude_session_cache
            SET transcript_expired_at = '2026-09-01 08:30:00 +0000 UTC'
          WHERE session_id = 'live'",
        [],
    )
    .expect("expire the newer one");

    assert_eq!(
        since(&report(&conn)),
        (at("2026-08-01T09:00:00Z"), at("2026-08-01T09:00:00Z"))
    );
}

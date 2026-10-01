//! Transcript expiry on the sessions list (#708).
//!
//! Since #705 the scanner stamps `transcript_expired_at` on a row whose
//! transcript vanished rather than deleting it. These tests pin what the list
//! does with that stamp: the two trailing fields, the `transcript` filter, the
//! facet count, and the relevance key's column index, which moved when the
//! projection grew.
//!
//! Built on `migrate::apply` like `tests_search.rs`, so the column is migration
//! 46's own rather than a second spelling of it.

use rusqlite::{params, Connection};

use super::page::{facets, list_page};
use super::query::SessionQuery;
use crate::native::migrate;
use crate::native::search::{self, SearchDoc};
use crate::native::settings::DataSettings;

/// The stamp the scanner writes: `gotime::now_go_text()`'s shape.
const EXPIRED_AT: &str = "2026-09-01 08:30:00 +0000 UTC";

fn migrated() -> Connection {
    let mut conn = Connection::open_in_memory().expect("in-memory database");
    migrate::apply(&mut conn).expect("apply migrations");
    conn
}

/// One cache row. `hour` is its `last_activity`, so every row sorts to exactly
/// one place under the default order.
fn session(conn: &Connection, id: &str, project: &str, hour: u32, tokens: i64, cost: f64) {
    conn.execute(
        "INSERT INTO claude_session_cache
             (session_id, project_path, file_path, file_mtime, start_time, last_activity,
              preview, input_tokens, total_cost_usd)
         VALUES (?1, ?2, ?3, '2026-08-01 10:00:00 +0000 UTC',
                 '2026-08-01 10:00:00 +0000 UTC', ?4, ?5, ?6, ?7)",
        params![
            id,
            project,
            format!("{project}/{id}.jsonl"),
            format!("2026-08-01 {hour:02}:00:00 +0000 UTC"),
            format!("prompt of {id}"),
            tokens,
            cost,
        ],
    )
    .expect("insert session");
}

/// What the scanner does to a row whose transcript is gone (#705): stamp it
/// and blank the preview.
fn expire(conn: &Connection, id: &str) {
    let changed = conn
        .execute(
            "UPDATE claude_session_cache SET transcript_expired_at = ?1, preview = ''
              WHERE session_id = ?2",
            params![EXPIRED_AT, id],
        )
        .expect("expire");
    assert_eq!(changed, 1, "{id} is not in the fixture");
}

fn query(q: &str) -> SessionQuery {
    SessionQuery::parse(q).expect("parse query")
}

fn ids(conn: &Connection, q: &str) -> Vec<String> {
    list_page(conn, &DataSettings::default(), &query(q))
        .expect("page")
        .items
        .into_iter()
        .map(|s| s.session_id)
        .collect()
}

/// Two live rows around one expired one, with no two sharing a `last_activity`.
fn corpus() -> Connection {
    let conn = migrated();
    session(&conn, "aaa-live", "/work/alpha", 14, 100, 1.5);
    session(&conn, "bbb-expired", "/work/beta", 13, 200, 2.25);
    session(&conn, "ccc-live", "/work/gamma", 12, 300, 4.0);
    conn.execute(
        "UPDATE claude_session_cache SET custom_title = 'kept after expiry'
          WHERE session_id = 'bbb-expired'",
        [],
    )
    .expect("rename");
    expire(&conn, "bbb-expired");
    conn
}

#[test]
fn only_the_expired_row_carries_the_flag_and_its_stamp() {
    let conn = corpus();
    let page = list_page(&conn, &DataSettings::default(), &query("")).expect("page");

    let flags: Vec<(&str, bool, bool)> = page
        .items
        .iter()
        .map(|s| {
            (
                s.session_id.as_str(),
                s.transcript_expired,
                s.transcript_expired_at.is_some(),
            )
        })
        .collect();
    assert_eq!(
        flags,
        vec![
            ("aaa-live", false, false),
            ("bbb-expired", true, true),
            ("ccc-live", false, false),
        ]
    );
}

/// The reason every frozen golden survived: a response with no expired row
/// never spells either key.
#[test]
fn a_response_with_no_expired_row_carries_neither_key() {
    let conn = corpus();
    conn.execute(
        "UPDATE claude_session_cache SET transcript_expired_at = NULL",
        [],
    )
    .expect("un-expire");

    let page = list_page(&conn, &DataSettings::default(), &query("")).expect("page");
    let f = facets(&conn, &DataSettings::default(), &query("")).expect("facets");
    for json in [
        String::from_utf8(crate::native::gojson::to_vec(&page).expect("encode")).expect("utf-8"),
        String::from_utf8(crate::native::gojson::to_vec(&f).expect("encode")).expect("utf-8"),
    ] {
        assert!(!json.contains("transcript_expired"), "{json}");
        assert!(!json.contains("expired_sessions"), "{json}");
    }
}

#[test]
fn the_transcript_filter_selects_expired_available_or_both() {
    let conn = corpus();
    assert_eq!(ids(&conn, "transcript=expired"), vec!["bbb-expired"]);
    assert_eq!(
        ids(&conn, "transcript=available"),
        vec!["aaa-live", "ccc-live"]
    );
    assert_eq!(
        ids(&conn, "transcript="),
        vec!["aaa-live", "bbb-expired", "ccc-live"]
    );
    assert_eq!(ids(&conn, ""), ids(&conn, "transcript="));
}

/// The handler answers a parse error as its 400.
#[test]
fn an_unknown_transcript_filter_is_refused() {
    assert_eq!(
        SessionQuery::parse("transcript=bogus").unwrap_err(),
        "invalid transcript filter \"bogus\""
    );
}

#[test]
fn facets_agree_with_the_page_under_each_transcript_filter() {
    let conn = corpus();
    for (q, total, expired) in [
        ("", 3, 1),
        ("transcript=expired", 1, 1),
        ("transcript=available", 2, 0),
    ] {
        let page = list_page(&conn, &DataSettings::default(), &query(q)).expect("page");
        let f = facets(&conn, &DataSettings::default(), &query(q)).expect("facets");
        assert_eq!(f.total, page.items.len() as i64, "{q}: total");
        assert_eq!(f.total, total, "{q}: total");
        assert_eq!(f.expired_sessions, expired, "{q}: expired_sessions");
        assert_eq!(
            f.expired_sessions,
            page.items.iter().filter(|s| s.transcript_expired).count() as i64,
            "{q}: the count and the rows disagree"
        );
    }
}

/// An empty filtered set sums to NULL in SQL; the count must still be 0.
#[test]
fn an_empty_filtered_set_counts_no_expired_sessions() {
    let conn = corpus();
    let f = facets(
        &conn,
        &DataSettings::default(),
        &query("project=/work/nowhere"),
    )
    .expect("facets");
    assert_eq!((f.total, f.expired_sessions), (0, 0));
}

/// The projection grew by one column, which moved the relevance key from
/// index 54 to 55. A stale index reads the expiry text where a float is
/// expected, and only on this sort.
#[test]
fn a_relevance_sorted_search_reads_an_expired_row() {
    let conn = corpus();
    for id_project in [("aaa-live", "/work/alpha"), ("bbb-expired", "/work/beta")] {
        search::replace(
            &conn,
            &SearchDoc {
                session_id: id_project.0.to_string(),
                project_path: id_project.1.to_string(),
                user_text: "investigate the zephyrlock timeout".to_string(),
                ..Default::default()
            },
        )
        .expect("index");
    }

    let page = list_page(
        &conn,
        &DataSettings::default(),
        &query("q=zephyrlock&sort=relevance&limit=1"),
    )
    .expect("a relevance page over an expired row");
    assert!(page.has_more, "two rows match, so a cursor is minted");

    let mut found: Vec<(String, bool)> = page
        .items
        .iter()
        .map(|s| (s.session_id.clone(), s.transcript_expired))
        .collect();
    let rest = list_page(
        &conn,
        &DataSettings::default(),
        &query(&format!(
            "q=zephyrlock&sort=relevance&limit=1&cursor={}",
            page.next_cursor
        )),
    )
    .expect("the second relevance page");
    found.extend(
        rest.items
            .iter()
            .map(|s| (s.session_id.clone(), s.transcript_expired)),
    );
    found.sort();
    assert_eq!(
        found,
        vec![
            ("aaa-live".to_string(), false),
            ("bbb-expired".to_string(), true)
        ]
    );
}

/// The list with an expired row in it, byte for byte.
///
/// Like the search golden, this one has no Go ancestor: `transcript_expired`
/// and `transcript_expired_at` are Agento's own, so the file is hand-kept
/// beside the code. **A change here is a change to the contract.**
///
/// What it pins: both keys sit last, after every field Go declared; only
/// `bbb-expired` carries them; and the stamp is spelled as RFC 3339 like every
/// other timestamp on the wire. The fixture has no ties on `last_activity`, so
/// it can only produce one order.
#[test]
fn the_list_with_an_expired_row_matches_the_golden_bytes() {
    let conn = corpus();
    let page = list_page(&conn, &DataSettings::default(), &query("limit=10")).expect("page");
    let got =
        String::from_utf8(crate::native::gojson::to_vec(&page).expect("encode")).expect("utf-8");

    let want = include_str!("../../../../parity/claude_sessions_expired_golden.json");
    assert_eq!(got, want, "the expired list drifted from its golden");
}

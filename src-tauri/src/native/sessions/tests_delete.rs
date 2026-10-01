//! The two deletes of expired sessions (#711), through the real handlers.
//!
//! Built on `migrate::apply`, so every table is the migrations' own, and on a
//! config dir the settings row names — "the transcript is on disk" is a real
//! file under a temp dir, never a stubbed answer.

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::*;
use crate::native::sessions::serve;
use crate::native::{writes, Ctx, Request};

/// The stamp the scanner writes: `gotime::now_go_text()`'s shape.
const EXPIRED_AT: &str = "2026-09-01 08:30:00 +0000 UTC";

/// Every table the cascade touches, plus the search key table.
const TABLES: &[(&str, &str)] = &[
    ("claude_session_cache", "session_id"),
    ("claude_subagent_cache", "parent_session_id"),
    ("claude_session_pr", "session_id"),
    ("session_insights", "session_id"),
    ("session_search", "session_id"),
    ("session_search_key", "session_id"),
    ("credential_findings", "session_id"),
    ("credential_scan_state", "session_id"),
];

struct Fixture {
    _dir: tempfile::TempDir,
    ctx: Ctx,
    /// `<config dir>/projects/-home-u-proj`, where a transcript the detail
    /// read can find lives.
    project_dir: std::path::PathBuf,
    /// A directory no config dir covers.
    elsewhere: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let claude = dir.path().join(".claude");
        let project_dir = claude.join("projects").join("-home-u-proj");
        std::fs::create_dir_all(&project_dir).expect("project dir");
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere dir");

        let db = dir.path().join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO user_settings (id, claude_config_dir) VALUES (1, ?1)",
            [claude.to_string_lossy()],
        )
        .expect("settings row");

        Self {
            _dir: dir,
            ctx: Ctx { db_path: db },
            project_dir,
            elsewhere,
        }
    }

    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.ctx.db_path).expect("open")
    }

    /// A cache row whose stored path is under the config dir, with no file
    /// there. `last_activity` is `2026-08-<day> 10:00:00`.
    fn session(&self, id: &str, project: &str, day: u32, expired: bool) {
        let file = self.project_dir.join(format!("{id}.jsonl"));
        self.session_at(id, project, day, expired, &file.to_string_lossy());
    }

    fn session_at(&self, id: &str, project: &str, day: u32, expired: bool, file_path: &str) {
        self.conn()
            .execute(
                "INSERT INTO claude_session_cache
                     (session_id, project_path, file_path, file_mtime, start_time,
                      last_activity, transcript_expired_at)
                 VALUES (?1, ?2, ?3, '2026-08-01 09:00:00 +0000 UTC',
                         '2026-08-01 09:00:00 +0000 UTC', ?4, ?5)",
                params![
                    id,
                    project,
                    file_path,
                    format!("2026-08-{day:02} 10:00:00 +0000 UTC"),
                    expired.then_some(EXPIRED_AT),
                ],
            )
            .expect("insert session");
    }

    /// One row in each of the six dependent tables for the pair.
    fn dependents(&self, id: &str, project: &str) {
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO claude_subagent_cache
                 (parent_session_id, agent_id, file_path, file_mtime)
             VALUES (?1, 'agent-1', 'sub.jsonl', '2026-08-01 09:00:00 +0000 UTC')",
            [id],
        )
        .expect("sub-agent");
        conn.execute(
            "INSERT OR IGNORE INTO claude_session_pr (session_id, pr_url)
             VALUES (?1, 'https://example.test/pr/1')",
            [id],
        )
        .expect("pr link");
        conn.execute(
            "INSERT INTO session_insights (session_id, project_path, scanned_at)
             VALUES (?1, ?2, '2026-08-01 09:00:00 +0000 UTC')",
            params![id, project],
        )
        .expect("insights");
        crate::native::search::replace(
            &conn,
            &crate::native::search::SearchDoc {
                session_id: id.to_string(),
                project_path: project.to_string(),
                title: "a title".to_string(),
                user_text: "user words".to_string(),
                assistant_text: "assistant words".to_string(),
                tool_text: String::new(),
            },
        )
        .expect("search row");
        conn.execute(
            "INSERT INTO credential_findings
                 (session_id, project_path, rule_id, confidence, masked_snippet,
                  location_start, location_end, ruleset_version, detected_at)
             VALUES (?1, ?2, 'rule', 'high', '***', 0, 3, 1, '2026-08-01 09:00:00 +0000 UTC')",
            params![id, project],
        )
        .expect("finding");
        conn.execute(
            "INSERT INTO credential_scan_state (session_id, project_path, ruleset_version, scanned_at)
             VALUES (?1, ?2, 1, '2026-08-01 09:00:00 +0000 UTC')",
            params![id, project],
        )
        .expect("scan state");
    }

    /// An expired session with a row in every table.
    fn full(&self, id: &str, project: &str, day: u32) {
        self.session(id, project, day, true);
        self.dependents(id, project);
    }

    fn count(&self, table: &str, column: &str, id: &str) -> i64 {
        self.conn()
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                [id],
                |r| r.get(0),
            )
            .expect("count")
    }

    /// How many rows each table holds for `id`, in `TABLES` order.
    fn counts(&self, id: &str) -> Vec<i64> {
        TABLES
            .iter()
            .map(|(table, column)| self.count(table, column, id))
            .collect()
    }

    /// Every row of every table the cascade touches, as text — so "unchanged"
    /// is a comparison of contents, not of counts.
    fn dump(&self) -> Vec<String> {
        let conn = self.conn();
        let mut out = Vec::new();
        for (table, _) in TABLES {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
                .expect("prepare dump");
            let width = stmt.column_count();
            let mut rows = stmt.query([]).expect("dump");
            while let Some(row) = rows.next().expect("row") {
                let cells: Vec<String> = (0..width)
                    .map(|i| format!("{:?}", row.get_ref(i).expect("cell")))
                    .collect();
                out.push(format!("{table}: {}", cells.join("|")));
            }
        }
        out
    }

    fn request(&self, method: Method, path: &str, query: &str, body: &[u8]) -> Answer {
        serve(
            &self.ctx,
            &Request {
                method: &method,
                path,
                query,
                content_type: "",
                secret_token: "",
                body,
            },
        )
        .expect("answer")
    }

    fn delete(&self, id: &str) -> Answer {
        self.request(
            Method::DELETE,
            &format!("/api/claude-sessions/{id}"),
            "",
            &[],
        )
    }

    fn delete_before(&self, body: &str) -> Answer {
        self.request(Method::DELETE, "/api/claude-sessions", "", body.as_bytes())
    }

    fn ids(&self) -> Vec<String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT session_id FROM claude_session_cache ORDER BY session_id, project_path",
            )
            .expect("prepare");
        let ids = stmt
            .query_map([], |r| r.get(0))
            .expect("ids")
            .collect::<Result<_, _>>()
            .expect("id");
        ids
    }
}

fn body(answer: &Answer) -> &str {
    std::str::from_utf8(answer.body.as_deref().unwrap_or_default()).expect("utf-8 body")
}

const ALL_PRESENT: [i64; 8] = [1; 8];
const ALL_GONE: [i64; 8] = [0; 8];
/// An expired session a **bulk** delete left alone. Its search rows are gone
/// because the bulk form reconciles the index with one `search::delete_orphans`,
/// which drops every expired pair's rows exactly as each scan does (#706) —
/// the fixture's indexed-yet-expired session is a state only the gap before
/// that reconcile produces.
const EXPIRED_SURVIVOR_OF_A_SWEEP: [i64; 8] = [1, 1, 1, 1, 0, 0, 1, 1];

#[test]
fn deleting_an_expired_session_removes_its_row_from_all_seven_tables() {
    let f = Fixture::new();
    f.full("gone", "/home/u/proj", 1);
    f.full("kept", "/home/u/proj", 2);
    assert_eq!(
        f.counts("gone"),
        ALL_PRESENT,
        "the fixture seeds every table"
    );

    let answer = f.delete("gone");

    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert!(answer.body.is_none());
    assert_eq!(f.counts("gone"), ALL_GONE);
    // And nothing else: the other session keeps every row it had.
    assert_eq!(f.counts("kept"), ALL_PRESENT);
}

#[test]
fn an_unknown_id_is_404_and_deletes_nothing() {
    let f = Fixture::new();
    f.full("kept", "/home/u/proj", 1);
    let before = f.dump();

    let answer = f.delete("nobody");

    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(body(&answer), "{\"error\":\"session not found\"}\n");
    assert_eq!(f.dump(), before);
}

#[test]
fn a_session_whose_transcript_is_on_disk_is_409_and_every_table_is_unchanged() {
    let f = Fixture::new();
    // Stamped, but the file is back and no scan has cleared the stamp yet.
    f.full("back", "/home/u/proj", 1);
    std::fs::write(f.project_dir.join("back.jsonl"), "{}\n").expect("transcript");
    let before = f.dump();

    let answer = f.delete("back");

    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(
        body(&answer),
        "{\"error\":\"session transcript is not expired\"}\n"
    );
    assert_eq!(f.dump(), before);
}

#[test]
fn an_unstamped_row_is_409_even_with_its_file_missing() {
    let f = Fixture::new();
    f.session("live", "/home/u/proj", 1, false);
    f.dependents("live", "/home/u/proj");
    let before = f.dump();

    let answer = f.delete("live");

    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(f.dump(), before);
}

/// The stored path is checked as well as the config dirs: a transcript under a
/// dir that is no longer indexed is still a transcript on disk.
#[test]
fn a_stamped_row_whose_stored_file_exists_outside_the_config_dirs_is_409() {
    let f = Fixture::new();
    let file = f.elsewhere.join("away.jsonl");
    std::fs::write(&file, "{}\n").expect("transcript");
    f.session_at("away", "/home/u/proj", 1, true, &file.to_string_lossy());
    let before = f.dump();

    let answer = f.delete("away");

    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(f.dump(), before);
}

/// The half of the rule only `detail::expiry` decides: the stored path is gone,
/// but a config dir holds the transcript under another project directory — so
/// the detail read answers 200, and a session that reads is not one to delete.
#[test]
fn a_stamped_row_whose_transcript_is_under_another_project_dir_is_409() {
    let f = Fixture::new();
    f.session("moved", "/home/u/proj", 1, true);
    f.dependents("moved", "/home/u/proj");
    let other = f
        .project_dir
        .parent()
        .expect("projects dir")
        .join("-home-u-other");
    std::fs::create_dir_all(&other).expect("other project dir");
    std::fs::write(other.join("moved.jsonl"), "{}\n").expect("transcript");
    let before = f.dump();

    let answer = f.delete("moved");

    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(f.dump(), before);
}

/// All-or-nothing over the id: one live pair refuses the whole delete.
#[test]
fn an_id_with_one_expired_and_one_live_pair_is_409_and_keeps_both() {
    let f = Fixture::new();
    f.full("twin", "/home/u/a", 1);
    f.session("twin", "/home/u/b", 2, false);
    let before = f.dump();

    let answer = f.delete("twin");

    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(f.dump(), before);
}

#[test]
fn an_id_expired_under_two_projects_is_deleted_whole() {
    let f = Fixture::new();
    f.full("twin", "/home/u/a", 1);
    f.full("twin", "/home/u/b", 2);

    let answer = f.delete("twin");

    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert_eq!(f.counts("twin"), ALL_GONE);
}

/// The id is refused before it is looked up, so the row is seeded *under* the
/// bad id: with the guard gone this would be a 204 and an empty table.
#[test]
fn an_invalid_id_is_404_even_when_a_row_carries_it() {
    let f = Fixture::new();
    f.session("bad.id", "/home/u/proj", 1, true);

    let answer = f.delete("bad.id");

    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(f.ids(), ["bad.id"]);
}

#[test]
fn one_pair_of_a_twin_deleted_leaves_the_survivor_its_sub_agent_and_pr_rows() {
    let f = Fixture::new();
    f.full("twin", "/home/u/a", 1);
    f.full("twin", "/home/u/b", 2);

    let conn = f.conn();
    let pair = |project: &str| vec![("twin".to_string(), project.to_string())];
    let deleted = delete_pairs(&conn, &pair("/home/u/a"), SearchMode::Keyed).expect("cascade");

    assert_eq!(deleted, 1);
    // One pair's rows in the five pair-keyed tables are gone; the two id-keyed
    // tables still hold the survivor's.
    assert_eq!(f.counts("twin"), [1, 1, 1, 1, 1, 1, 1, 1]);
    assert_eq!(
        f.conn()
            .query_row(
                "SELECT project_path FROM claude_session_cache WHERE session_id = 'twin'",
                [],
                |r| r.get::<_, String>(0),
            )
            .expect("survivor"),
        "/home/u/b"
    );

    // And they go with the last pair.
    let deleted = delete_pairs(&conn, &pair("/home/u/b"), SearchMode::Keyed).expect("cascade");
    assert_eq!(deleted, 1);
    assert_eq!(f.counts("twin"), ALL_GONE);
}

/// The cascade's own guard: a pair that is not stamped any more — a scan
/// un-expired it after the caller selected it — is skipped whole.
#[test]
fn the_cascade_skips_a_pair_that_is_not_stamped_and_keeps_its_dependents() {
    let f = Fixture::new();
    f.session("live", "/home/u/proj", 1, false);
    f.dependents("live", "/home/u/proj");
    let before = f.dump();

    let conn = f.conn();
    for mode in [SearchMode::Keyed, SearchMode::Sweep] {
        let deleted = delete_pairs(
            &conn,
            &[("live".to_string(), "/home/u/proj".to_string())],
            mode,
        )
        .expect("cascade");
        assert_eq!(deleted, 0, "{mode:?}");
        assert_eq!(f.dump(), before, "{mode:?}");
    }
}

/// `before` is `2026-08-10T10:00:00Z`, which is exactly `at-bound`'s
/// `last_activity`.
const BEFORE: &str = r#"{"before":"2026-08-10T10:00:00Z"}"#;

/// Two old expired sessions, and one of everything that must survive.
fn bulk_corpus() -> Fixture {
    let f = Fixture::new();
    f.full("old-1", "/home/u/proj", 1);
    f.full("old-2", "/home/u/proj", 9);
    f.full("at-bound", "/home/u/proj", 10);
    f.full("newer", "/home/u/proj", 11);
    f.session("old-live", "/home/u/proj", 2, false);
    f.dependents("old-live", "/home/u/proj");
    // Old and stamped, but its file is back on disk.
    f.full("old-back", "/home/u/proj", 3);
    std::fs::write(f.project_dir.join("old-back.jsonl"), "{}\n").expect("transcript");
    f
}

#[test]
fn bulk_delete_removes_only_expired_sessions_strictly_older_than_the_bound() {
    let f = bulk_corpus();

    let answer = f.delete_before(BEFORE);

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        body(&answer),
        include_str!("../../../../parity/session_delete_golden.json")
    );
    assert_eq!(f.ids(), ["at-bound", "newer", "old-back", "old-live"]);
    for gone in ["old-1", "old-2"] {
        assert_eq!(f.counts(gone), ALL_GONE, "{gone}");
    }
    for kept in ["at-bound", "newer", "old-back"] {
        assert_eq!(f.counts(kept), EXPIRED_SURVIVOR_OF_A_SWEEP, "{kept}");
    }
    assert_eq!(f.counts("old-live"), ALL_PRESENT);
}

/// The bound is compared as the text the column holds, in UTC: the same
/// instant spelled with an offset deletes the same set.
#[test]
fn the_bound_is_an_instant_whatever_offset_it_is_spelled_in() {
    let f = bulk_corpus();

    let answer = f.delete_before(r#"{"before":"2026-08-10T12:00:00+02:00"}"#);

    assert_eq!(body(&answer), "{\"deleted\":2}\n");
    assert_eq!(f.ids(), ["at-bound", "newer", "old-back", "old-live"]);
}

#[test]
fn a_bulk_delete_that_matches_nothing_answers_zero() {
    let f = bulk_corpus();
    let before = f.dump();

    let answer = f.delete_before(r#"{"before":"2026-07-01T00:00:00Z"}"#);

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(body(&answer), "{\"deleted\":0}\n");
    assert_eq!(f.dump(), before);
}

#[test]
fn a_bulk_delete_keeps_the_sub_agent_and_pr_rows_of_a_twin_that_survives() {
    let f = Fixture::new();
    f.full("twin", "/home/u/a", 1);
    f.full("twin", "/home/u/b", 20);

    let answer = f.delete_before(BEFORE);

    assert_eq!(body(&answer), "{\"deleted\":1}\n");
    assert_eq!(f.counts("twin"), EXPIRED_SURVIVOR_OF_A_SWEEP);
}

#[test]
fn the_facets_count_under_the_same_bound_is_what_the_bulk_delete_removes() {
    let f = bulk_corpus();
    // Out of the list's sight, so out of the count — and so not deleted.
    f.full("old-hidden", "/home/u/hidden", 1);
    f.conn()
        .execute(
            "UPDATE user_settings SET hidden_projects = ?1 WHERE id = 1",
            [r#"["/home/u/hidden"]"#],
        )
        .expect("hide project");
    // `old-back` is counted until a scan clears its stamp, and is the "minus
    // rows whose file reappeared"; take it out so the two numbers are equal.
    std::fs::remove_file(f.project_dir.join("old-back.jsonl")).expect("remove transcript");

    let facets = f.request(
        Method::GET,
        "/api/claude-sessions/facets",
        "transcript=expired&ended_before=2026-08-10T10%3A00%3A00Z",
        &[],
    );
    assert_eq!(facets.status, StatusCode::OK);
    let total = serde_json::from_str::<serde_json::Value>(body(&facets)).expect("facets json")
        ["total"]
        .as_u64()
        .expect("total");
    assert_eq!(total, 3, "old-1, old-2 and old-back");

    let answer = f.delete_before(BEFORE);

    assert_eq!(body(&answer), format!("{{\"deleted\":{total}}}\n"));
    assert_eq!(f.counts("old-hidden"), EXPIRED_SURVIVOR_OF_A_SWEEP);
}

#[test]
fn a_missing_null_empty_or_unparseable_before_is_422_and_deletes_nothing() {
    let f = bulk_corpus();
    let before = f.dump();

    for (raw, message) in [
        ("{}", "before is required"),
        (r#"{"before":null}"#, "before is required"),
        (r#"{"before":""}"#, "before is required"),
        // A `null` body is Go's zero value, so it reaches the same check.
        ("null", "before is required"),
        (
            r#"{"before":"yesterday"}"#,
            "before must be an RFC 3339 timestamp",
        ),
        (
            r#"{"before":"2026-08-10"}"#,
            "before must be an RFC 3339 timestamp",
        ),
    ] {
        let answer = f.delete_before(raw);
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY, "{raw}");
        assert_eq!(
            body(&answer),
            format!("{{\"error\":\"validation error for \\\"before\\\": {message}\"}}\n"),
            "{raw}"
        );
    }
    assert_eq!(f.dump(), before);
}

#[test]
fn an_empty_or_malformed_body_is_400_and_deletes_nothing() {
    let f = bulk_corpus();
    let before = f.dump();

    for raw in ["", "[]", "{", r#"{"before":5}"#] {
        let answer = f.delete_before(raw);
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{raw:?}");
    }
    assert_eq!(f.dump(), before);
}

/// The handler functions answer through `writes::finish` exactly as `serve`
/// calls them, so a caller outside the route (#712's prune) sees the same
/// statuses.
#[test]
fn the_handlers_answer_the_same_outside_the_route() {
    let f = Fixture::new();
    f.full("gone", "/home/u/proj", 1);

    let answer = writes::finish(delete_one(&f.ctx.db_path, "gone")).expect("answer");
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    let answer = writes::finish(delete_one(&f.ctx.db_path, "gone")).expect("answer");
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

// ─── the retention prune (#712) ──────────────────────────────────────────────

/// 2027-08-10 10:00:00 UTC: 365 days after `at-bound`'s `last_activity`.
fn prune_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2027-08-10T10:00:00Z")
        .expect("now")
        .with_timezone(&Utc)
}

impl Fixture {
    fn prune(&self, days: i64, now: DateTime<Utc>) -> usize {
        prune(&mut self.conn(), days, now).expect("prune")
    }

    /// Restate when `id` ended and when its transcript was found missing.
    fn restamp(&self, id: &str, last_activity: DateTime<Utc>, expired_at: Option<DateTime<Utc>>) {
        self.conn()
            .execute(
                "UPDATE claude_session_cache
                    SET last_activity = ?2, transcript_expired_at = ?3
                  WHERE session_id = ?1",
                params![
                    id,
                    gotime::go_text(&last_activity),
                    expired_at.map(|t| gotime::go_text(&t)),
                ],
            )
            .expect("restamp");
    }
}

#[test]
fn a_retention_of_zero_prunes_nothing() {
    let f = bulk_corpus();
    let before = f.dump();

    // A century on, so every row is older than any window.
    let now = prune_now() + chrono::Duration::days(36_500);
    assert_eq!(f.prune(0, now), 0);

    assert_eq!(f.dump(), before);
}

/// The window is measured on `last_activity`, and never from
/// `transcript_expired_at`: a session that ended 13 months ago goes even though
/// its transcript vanished yesterday.
#[test]
fn the_prune_measures_age_from_the_sessions_end_not_from_its_expiry() {
    let f = Fixture::new();
    let now = prune_now();
    let yesterday = now - chrono::Duration::days(1);
    for id in [
        "ended-13-months-ago",
        "ended-11-months-ago",
        "old-but-on-record",
    ] {
        f.full(id, "/home/u/proj", 1);
    }
    f.restamp(
        "ended-13-months-ago",
        now - chrono::Duration::days(395),
        Some(yesterday),
    );
    f.restamp(
        "ended-11-months-ago",
        now - chrono::Duration::days(335),
        Some(now - chrono::Duration::days(300)),
    );
    // 13 months old with its transcript never found missing.
    f.restamp("old-but-on-record", now - chrono::Duration::days(395), None);

    assert_eq!(f.prune(365, now), 1);

    assert_eq!(f.ids(), ["ended-11-months-ago", "old-but-on-record"]);
    assert_eq!(
        f.counts("ended-13-months-ago"),
        vec![0; TABLES.len()],
        "a pruned session leaves no row in any table the cascade covers"
    );
    assert_eq!(f.counts("old-but-on-record"), vec![1; TABLES.len()]);
}

/// Strictly older than the horizon: the row ending exactly on it is kept, and
/// so are a live row and a stamped row whose file is back on disk.
#[test]
fn the_prune_keeps_the_boundary_row_live_rows_and_restored_transcripts() {
    let f = bulk_corpus();

    assert_eq!(f.prune(365, prune_now()), 2);

    assert_eq!(f.ids(), ["at-bound", "newer", "old-back", "old-live"]);
    for id in ["old-1", "old-2"] {
        assert_eq!(f.counts(id), vec![0; TABLES.len()], "{id}");
    }
    assert_eq!(f.counts("old-live"), vec![1; TABLES.len()]);

    // One second later the boundary row is past the horizon too.
    assert_eq!(f.prune(365, prune_now() + chrono::Duration::seconds(1)), 1);
    assert_eq!(f.ids(), ["newer", "old-back", "old-live"]);
}

/// The shorter window reaches further forward, and a second pass is a no-op.
#[test]
fn a_shorter_window_prunes_more_and_a_repeat_prunes_nothing() {
    let f = bulk_corpus();
    // 180 days after `newer` ended, plus a second.
    let now = prune_now() - chrono::Duration::days(184) + chrono::Duration::seconds(1);

    assert_eq!(f.prune(365, now), 0);
    assert_eq!(f.prune(180, now), 4);
    assert_eq!(f.prune(180, now), 0);

    assert_eq!(f.ids(), ["old-back", "old-live"]);
}

/// A hidden project is trimmed with the rest: retention is a property of the
/// store, not of what the sessions list shows.
#[test]
fn the_prune_reaches_a_hidden_projects_expired_sessions() {
    let f = Fixture::new();
    f.full("hidden-old", "/home/u/secret", 1);
    f.conn()
        .execute(
            "UPDATE user_settings SET hidden_projects = '[\"/home/u/secret\"]'",
            [],
        )
        .expect("hide");

    assert_eq!(f.prune(365, prune_now()), 1);
    assert!(f.ids().is_empty());
}

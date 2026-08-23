//! The `session_insights` rows: what needs recomputing, the upsert, and the
//! reconcile. Ported from `internal/storage/sqlite_session_insights_store.go`.
//!
//! ## Every statement here keys on `(session_id, project_path)`
//!
//! **The Go store keys on `session_id` alone**, in all three of its statements —
//! `ON CONFLICT(session_id)`, `LEFT JOIN … ON c.session_id = i.session_id`, and
//! `SELECT DISTINCT c.session_id, c.file_path`. That is the bug migration 29
//! (#362) exists to fix: `claude_session_cache` is keyed on the pair, so a
//! session id living under two project paths is two legitimate rows — which is
//! what copying a `~/.claude` to set up a second account produces — and the
//! single-keyed insight row was whichever transcript the pipeline reached last,
//! a mix of neither.
//!
//! So this is deliberately **not** a transcription of the Go SQL. Every place
//! the id appears, the pair appears. Three consequences, each silent if missed:
//!
//! * `ON CONFLICT(session_id)` would not even parse against the rebuilt table —
//!   that is the failure the issue reports when pointing `main`'s worker at a
//!   desktop database, and it is the *loud* one.
//! * A join on the id alone cross-produces a duplicated id's cache rows against
//!   each other's insights, so `needs_processing` under-reports: one of the two
//!   sessions is considered done because the *other* one has a current row.
//! * Dedup on the id alone drops the second transcript outright. That is
//!   `insight_worker.go`'s `inFlight` map, and it is the one the worker in
//!   `worker.rs` had to write differently rather than port.

use rusqlite::{params, Connection, Transaction};

use super::processors::SessionInsight;
use crate::native::gojson;
use crate::native::gotime;

/// One session the worker still has to compute an insight for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pending {
    /// The whole cache key, in the order that makes a `BTreeSet` of these sort
    /// the way a person would read them.
    pub session_id: String,
    pub project_path: String,
    /// The parent transcript. Sub-agent transcripts are found from it by
    /// `processors::session_files`, not stored here — `claude_session_cache`
    /// holds parents only, sub-agents living in `claude_subagent_cache`.
    pub file_path: String,
}

/// `NeedsProcessing`: every cached session with no insight row, or one computed
/// by an older processor version.
///
/// `DISTINCT` is Go's and is kept, though the pair is the cache's primary key
/// so it can no longer remove a row — it is what stopped the single-keyed join
/// returning a duplicated id twice, and leaving it costs nothing.
///
/// A row whose `file_path` is empty is skipped by the caller rather than here,
/// matching `rescanOutdated`.
pub fn needs_processing(conn: &Connection, version: i64) -> Result<Vec<Pending>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT c.session_id, c.project_path, c.file_path
             FROM claude_session_cache c
             LEFT JOIN session_insights i
                    ON c.session_id = i.session_id
                   AND c.project_path = i.project_path
             WHERE i.session_id IS NULL OR i.processor_version < ?1",
        )
        .map_err(|e| format!("preparing needs_processing: {e}"))?;

    let rows = stmt
        .query_map(params![version], |row| {
            Ok(Pending {
                session_id: row.get(0)?,
                project_path: row.get(1)?,
                file_path: row.get(2)?,
            })
        })
        .map_err(|e| format!("querying needs_processing: {e}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("reading needs_processing: {e}"))
}

/// The column list, shared by the insert and the conflict update so the two
/// cannot drift.
///
/// `session_id` and `project_path` are the key and are therefore *not* in the
/// update set; every other column is overwritten, which is what makes a
/// reprocess authoritative rather than a merge.
const WRITABLE_COLUMNS: &[&str] = &[
    "processor_version",
    "scanned_at",
    "turn_count",
    "steps_per_turn_avg",
    "autonomy_score",
    "tool_calls_total",
    "tool_breakdown",
    "tool_error_rate",
    "total_duration_ms",
    "active_duration_ms",
    "claude_working_time_ms",
    "cache_hit_rate",
    "tokens_per_turn_avg",
    "cost_estimate_usd",
    "tool_error_count",
    "has_errors",
    "max_consecutive_tool_calls",
    "longest_autonomous_chain",
    "avg_user_response_time_ms",
    "avg_claude_response_time_ms",
    "session_type",
    "skill_breakdown",
    "plugin_breakdown",
    "mcp_server_breakdown",
    "mcp_tool_breakdown",
    "effort_breakdown",
    "unattributed_calls",
    "agent_breakdown",
];

/// `session_type` is written as the empty string, which is what Go writes.
///
/// The column is reserved — `SessionInsight` does not carry it and no processor
/// produces one — so an upsert that preserved the stored value instead would be
/// preserving a value nothing has ever set. Overwriting keeps the row a pure
/// function of the transcript, which is the property `processor_version` is a
/// promise about.
const RESERVED_SESSION_TYPE: &str = "";

fn upsert_sql() -> String {
    let columns = WRITABLE_COLUMNS.join(", ");
    let placeholders = (1..=WRITABLE_COLUMNS.len() + 2)
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let updates = WRITABLE_COLUMNS
        .iter()
        .map(|c| format!("{c} = excluded.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO session_insights (session_id, project_path, {columns})
         VALUES ({placeholders})
         ON CONFLICT(session_id, project_path) DO UPDATE SET {updates}"
    )
}

/// Store one computed insight.
///
/// `scanned_at` is passed in rather than read from the clock here so every row
/// in one batch carries the same instant — the column is only ever read as a
/// timestamp, but a batch whose rows disagree by milliseconds is a batch whose
/// tests cannot assert on it.
pub fn upsert(
    tx: &Transaction,
    insight: &SessionInsight,
    project_path: &str,
    scanned_at: &str,
) -> Result<(), String> {
    let sql = upsert_sql();
    tx.execute(
        &sql,
        params![
            insight.session_id,
            project_path,
            super::processors::CURRENT_PROCESSOR_VERSION,
            scanned_at,
            insight.turn_count,
            insight.steps_per_turn_avg,
            insight.autonomy_score,
            insight.tool_calls_total,
            encode_breakdown(&insight.tool_breakdown)?,
            insight.tool_error_rate,
            insight.total_duration_ms,
            insight.active_duration_ms,
            insight.claude_working_time_ms,
            insight.cache_hit_rate,
            insight.tokens_per_turn_avg,
            insight.cost_estimate_usd,
            insight.tool_error_count,
            insight.has_errors,
            insight.max_consecutive_tool_calls,
            insight.longest_autonomous_chain,
            insight.avg_user_response_time_ms,
            insight.avg_claude_response_time_ms,
            RESERVED_SESSION_TYPE,
            encode_breakdown(&insight.skill_breakdown)?,
            encode_breakdown(&insight.plugin_breakdown)?,
            encode_breakdown(&insight.mcp_server_breakdown)?,
            encode_breakdown(&insight.mcp_tool_breakdown)?,
            encode_breakdown(&insight.effort_breakdown)?,
            insight.unattributed_calls,
            encode_breakdown(&insight.agent_breakdown)?,
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("upserting insight for {}: {e}", insight.session_id))
}

/// A breakdown map as the column stores it.
///
/// **`{}` for an empty map, never `""`** — the opposite of
/// `claude_session_cache.cost_by_model`, whose empty value really is the empty
/// string. Every column here declares `DEFAULT '{}'` and `summary.rs` decodes
/// with `json.Unmarshal`, which errors on `""` and yields an empty map that is
/// then indistinguishable from a real one; writing `{}` keeps the stored value
/// and the default identical. A `BTreeMap` marshals with sorted keys through
/// the Go encoder, which is what Go's `map[string]int` does too.
fn encode_breakdown(counts: &std::collections::BTreeMap<String, i64>) -> Result<String, String> {
    let bytes = gojson::to_vec_marshal(counts)
        .map_err(|e| format!("encoding a breakdown for session_insights: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("a breakdown is not utf-8: {e}"))
}

/// The instant a batch records as `scanned_at`.
///
/// `gotime::now_go_text` rather than an RFC 3339 string: every `DATETIME`
/// column in this schema holds Go's `time.Time.String()` and is compared **as
/// text**, so a row written in another spelling sorts into the wrong place
/// against every row the Go server wrote.
pub fn scanned_at_now() -> String {
    gotime::now_go_text()
}

/// Drop insight rows whose session is no longer in the cache.
///
/// **Not a delete by path**, which is what the cache's own reconcile does. A
/// claim shift moves a session to a new `file_path` and is an *update*, not a
/// discovery (#245) — the row is upserted under the new path and the old path's
/// delete then matches nothing. `session_insights` has no `file_path` at all,
/// so path is not even available as a wrong answer here; keying the delete on
/// "no cache row for this pair remains" is the only formulation that survives a
/// move.
///
/// It follows that this must run **after** the cache's delete pass, and that it
/// inherits that pass's protection for free: a config dir that could not be
/// listed keeps its cache rows, so its insights are not orphans and are not
/// touched. That is the property that stops an unmounted drive wiping an
/// account's insights.
pub fn delete_orphans(conn: &Connection) -> Result<usize, String> {
    conn.execute(
        "DELETE FROM session_insights
         WHERE NOT EXISTS (
             SELECT 1 FROM claude_session_cache c
              WHERE c.session_id = session_insights.session_id
                AND c.project_path = session_insights.project_path
         )",
        [],
    )
    .map_err(|e| format!("reconciling orphaned insights: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A database with the two tables this module touches, at their current
    /// shape. Built from the real migrations so a schema change cannot leave
    /// these tests passing against a table that no longer exists.
    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("in-memory db");
        crate::native::migrate::apply(&mut conn).expect("migrations");
        conn
    }

    fn cache_row(conn: &Connection, session_id: &str, project_path: &str, file_path: &str) {
        conn.execute(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity)
             VALUES (?1, ?2, ?3, '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00',
                     '2026-01-01 00:00:00+00:00')",
            params![session_id, project_path, file_path],
        )
        .expect("cache row");
    }

    fn insight(session_id: &str) -> SessionInsight {
        SessionInsight {
            session_id: session_id.to_string(),
            turn_count: 3,
            ..Default::default()
        }
    }

    fn stored_versions(conn: &Connection) -> Vec<(String, String, i64)> {
        let mut stmt = conn
            .prepare(
                "SELECT session_id, project_path, processor_version
                 FROM session_insights ORDER BY session_id, project_path",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    }

    fn write(conn: &mut Connection, insight: &SessionInsight, project_path: &str) {
        let tx = conn.transaction().expect("tx");
        upsert(&tx, insight, project_path, "2026-01-01 00:00:00+00:00").expect("upsert");
        tx.commit().expect("commit");
    }

    /// The whole reason this module is not a transcription of the Go store.
    ///
    /// One session id under two project paths is two sessions with two
    /// transcripts, so it must end as two rows. Against `ON CONFLICT(session_id)`
    /// the second write overwrites the first and the corpus silently loses a
    /// session's insight — the #362 defect, reintroduced by copying the Go SQL.
    #[test]
    fn a_session_id_under_two_projects_keeps_two_rows() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a", "/a/s1.jsonl");
        cache_row(&conn, "s1", "/b", "/b/s1.jsonl");

        write(&mut conn, &insight("s1"), "/a");
        write(&mut conn, &insight("s1"), "/b");

        assert_eq!(
            stored_versions(&conn),
            vec![
                (
                    "s1".into(),
                    "/a".into(),
                    super::super::processors::CURRENT_PROCESSOR_VERSION
                ),
                (
                    "s1".into(),
                    "/b".into(),
                    super::super::processors::CURRENT_PROCESSOR_VERSION
                ),
            ],
        );
    }

    /// The same pair written twice is one row, updated — otherwise every rescan
    /// would grow the table.
    #[test]
    fn the_same_pair_upserts_rather_than_duplicating() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a", "/a/s1.jsonl");

        write(&mut conn, &insight("s1"), "/a");
        conn.execute(
            "UPDATE session_insights SET processor_version = 0, turn_count = 99",
            [],
        )
        .expect("age the row");
        write(&mut conn, &insight("s1"), "/a");

        assert_eq!(stored_versions(&conn).len(), 1);
        let turn_count: i64 = conn
            .query_row("SELECT turn_count FROM session_insights", [], |r| r.get(0))
            .expect("turn_count");
        assert_eq!(turn_count, 3, "a reprocess must overwrite, not merge");
    }

    /// Both halves of `needs_processing`: a session with no row at all, and one
    /// whose row is behind the current version. A current row yields nothing.
    #[test]
    fn needs_processing_finds_missing_and_outdated_rows() {
        let mut conn = db();
        cache_row(&conn, "fresh", "/a", "/a/fresh.jsonl");
        cache_row(&conn, "stale", "/a", "/a/stale.jsonl");
        cache_row(&conn, "absent", "/a", "/a/absent.jsonl");

        write(&mut conn, &insight("fresh"), "/a");
        write(&mut conn, &insight("stale"), "/a");
        conn.execute(
            "UPDATE session_insights SET processor_version = 1 WHERE session_id = 'stale'",
            [],
        )
        .expect("age the row");

        let pending = {
            let mut p =
                needs_processing(&conn, super::super::processors::CURRENT_PROCESSOR_VERSION)
                    .expect("needs_processing");
            p.sort();
            p
        };
        assert_eq!(
            pending
                .iter()
                .map(|p| p.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["absent", "stale"],
        );
    }

    /// The join must use the whole key.
    ///
    /// With `ON c.session_id = i.session_id` the current row for `/a` satisfies
    /// the cache row for `/b` as well, so `/b` is reported done and its insight
    /// is never computed — a silent, permanent gap rather than a failure.
    #[test]
    fn needs_processing_joins_on_the_whole_key() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a", "/a/s1.jsonl");
        cache_row(&conn, "s1", "/b", "/b/s1.jsonl");
        write(&mut conn, &insight("s1"), "/a");

        let pending = needs_processing(&conn, super::super::processors::CURRENT_PROCESSOR_VERSION)
            .expect("needs_processing");
        assert_eq!(
            pending,
            vec![Pending {
                session_id: "s1".into(),
                project_path: "/b".into(),
                file_path: "/b/s1.jsonl".into(),
            }],
        );
    }

    /// An empty breakdown stores as `{}`, matching the column default and what
    /// `summary.rs` can decode. `""` would be a decode error per row.
    #[test]
    fn an_empty_breakdown_stores_as_an_empty_object() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a", "/a/s1.jsonl");
        write(&mut conn, &insight("s1"), "/a");

        let stored: String = conn
            .query_row("SELECT tool_breakdown FROM session_insights", [], |r| {
                r.get(0)
            })
            .expect("tool_breakdown");
        assert_eq!(stored, "{}");
    }

    #[test]
    fn a_breakdown_stores_with_sorted_keys() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a", "/a/s1.jsonl");
        let mut insight = insight("s1");
        insight.tool_breakdown = BTreeMap::from([("Write".into(), 2), ("Bash".into(), 5)]);
        write(&mut conn, &insight, "/a");

        let stored: String = conn
            .query_row("SELECT tool_breakdown FROM session_insights", [], |r| {
                r.get(0)
            })
            .expect("tool_breakdown");
        assert_eq!(stored, r#"{"Bash":5,"Write":2}"#);
    }

    /// The reconcile keys on the pair, not on the id: a session id that also
    /// exists under another project must keep the row for the project that is
    /// still cached.
    #[test]
    fn the_reconcile_drops_only_rows_with_no_cache_row() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a", "/a/s1.jsonl");
        cache_row(&conn, "s1", "/b", "/b/s1.jsonl");
        cache_row(&conn, "gone", "/a", "/a/gone.jsonl");
        write(&mut conn, &insight("s1"), "/a");
        write(&mut conn, &insight("s1"), "/b");
        write(&mut conn, &insight("gone"), "/a");

        conn.execute(
            "DELETE FROM claude_session_cache WHERE session_id = 'gone' OR project_path = '/b'",
            [],
        )
        .expect("reconcile the cache");

        assert_eq!(delete_orphans(&conn).expect("delete_orphans"), 2);
        assert_eq!(
            stored_versions(&conn)
                .iter()
                .map(|(s, p, _)| (s.clone(), p.clone()))
                .collect::<Vec<_>>(),
            vec![("s1".to_string(), "/a".to_string())],
        );
    }

    /// A moved transcript is an update, not a deletion. The cache row survives
    /// under its new path, so nothing here may touch its insight — a reconcile
    /// written against `file_path` would have deleted it.
    #[test]
    fn a_moved_transcript_keeps_its_insight() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a", "/old/s1.jsonl");
        write(&mut conn, &insight("s1"), "/a");

        conn.execute(
            "UPDATE claude_session_cache SET file_path = '/new/s1.jsonl'",
            [],
        )
        .expect("move the file");

        assert_eq!(delete_orphans(&conn).expect("delete_orphans"), 0);
        assert_eq!(stored_versions(&conn).len(), 1);
    }
}

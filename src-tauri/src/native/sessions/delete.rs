//! `DELETE /api/claude-sessions/{id}` and `DELETE /api/claude-sessions` (#711):
//! forgetting an expired session, and everything stored about it.
//!
//! Neither route has a Go ancestor. Since #705 a session whose transcript
//! vanished keeps its row for ever, so "forget this" needs a path of its own,
//! and this is the only one: the scanner never deletes a cache row.
//!
//! # Only an expired session is deletable
//!
//! A session whose transcript is on disk is refused, because deleting its row
//! would be undone by the next scan — minus the title and the favourite the
//! user typed, which are the two columns the scanner does not own. The single
//! delete decides "expired" with the same [`detail::expiry`] the `410` reads
//! use, so "this session answers 410" and "this session can be deleted" are one
//! fact. Both routes also look at the row's stored `file_path`, which catches a
//! transcript restored under a config dir that is no longer indexed.
//!
//! The rule is also a property of the cascade itself: [`delete_pairs`] removes a
//! cache row only while it is still stamped, so a row a scan un-expired between
//! the check and the write is skipped rather than deleted.
//!
//! # The cascade
//!
//! None of the tables has a foreign key, so nothing cascades by itself. A
//! session is keyed `(session_id, project_path)` and five of the seven tables
//! are keyed on that pair; **sub-agent and PR rows are keyed on the id alone**.
//! They are therefore removed only once no cache row carries the id any more —
//! a session id under two projects (the #362 family) keeps them for the pair
//! that survives.
//!
//! The transcript, and everything else under a Claude config dir, is never
//! touched.

use std::collections::BTreeSet;
use std::path::Path;

use axum::http::StatusCode;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use super::{detail, query};
use crate::native::gotime::GoTime;
use crate::native::writes::{decode_body, WriteError};
use crate::native::{db, gojson, insights, search, security_scan, settings, Answer};

/// How [`delete_pairs`] removes the pairs' search rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchMode {
    /// One rowid-keyed [`search::delete`] per pair: no scan of the index, so
    /// the right choice for a handful of pairs.
    Keyed,
    /// One [`search::delete_orphans`] after the cache rows are gone, whose
    /// cost does not grow with the batch. The right choice for a bulk delete —
    /// **never loop a predicate delete over `session_search`**, where a keyed
    /// delete that misses its key row scans the whole index.
    Sweep,
}

/// Delete each `(session_id, project_path)` pair that is still stamped
/// expired, with everything stored about it, and answer how many cache rows
/// went.
///
/// Runs on the caller's connection and opens no transaction: the caller owns
/// one, so the whole cascade commits or rolls back together. A pair with no
/// stamped cache row — unknown, or un-expired since the caller selected it — is
/// skipped whole, dependents included.
///
/// Search rows are normally gone already (`search::delete_orphans` drops an
/// expired pair's on every scan, #706); the step here covers a row expired but
/// not yet reconciled.
pub(crate) fn delete_pairs(
    tx: &Connection,
    pairs: &[(String, String)],
    search: SearchMode,
) -> Result<usize, String> {
    let mut deleted = 0;
    let mut ids: BTreeSet<&str> = BTreeSet::new();

    for (session_id, project_path) in pairs {
        let removed = tx
            .prepare_cached(
                "DELETE FROM claude_session_cache
                  WHERE session_id = ?1 AND project_path = ?2
                    AND transcript_expired_at IS NOT NULL",
            )
            .and_then(|mut stmt| stmt.execute(params![session_id, project_path]))
            .map_err(|e| format!("deleting an expired session's cache row: {e}"))?;
        if removed == 0 {
            continue;
        }
        deleted += removed;
        ids.insert(session_id);

        security_scan::store::delete_session(tx, session_id, project_path)?;
        insights::store::delete(tx, session_id, project_path)?;
        if search == SearchMode::Keyed {
            search::delete(tx, session_id, project_path)?;
        }
    }

    if search == SearchMode::Sweep && deleted > 0 {
        search::delete_orphans(tx)?;
    }

    // The two id-keyed tables, and only for an id no cache row carries any
    // more: a twin under another project still owns these rows.
    for session_id in ids {
        tx.execute(
            "DELETE FROM claude_subagent_cache
              WHERE parent_session_id = ?1
                AND NOT EXISTS (SELECT 1 FROM claude_session_cache WHERE session_id = ?1)",
            [session_id],
        )
        .map_err(|e| format!("deleting an expired session's sub-agents: {e}"))?;
        tx.execute(
            "DELETE FROM claude_session_pr
              WHERE session_id = ?1
                AND NOT EXISTS (SELECT 1 FROM claude_session_cache WHERE session_id = ?1)",
            [session_id],
        )
        .map_err(|e| format!("deleting an expired session's PR links: {e}"))?;
    }

    Ok(deleted)
}

fn open(db_path: &Path) -> Result<Connection, WriteError> {
    let conn = db::open_read_write(db_path)
        .map_err(|e| WriteError::Fallback(format!("opening database: {e}")))?;
    crate::native::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    Ok(conn)
}

/// `DELETE /api/claude-sessions/{id}` → `204`.
///
/// All-or-nothing over every pair carrying the id, mirroring [`detail::expiry`],
/// which classifies on the id alone: `404` when no cache row has it, `409` when
/// a transcript for it is on disk or any of its rows is not stamped expired —
/// and then nothing is deleted.
pub fn delete_one(db_path: &Path, session_id: &str) -> Result<Answer, WriteError> {
    let not_found = || WriteError::NotFoundMessage("session not found".to_string());
    if !detail::is_valid_session_id(session_id) {
        return Err(not_found());
    }

    // The walk over the config dirs happens before the write lock is taken.
    let expiry = detail::expiry(db_path, session_id).map_err(WriteError::Fallback)?;

    let mut conn = open(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin session delete: {e}")))?;

    let rows: Vec<(String, String, bool)> = tx
        .prepare(
            "SELECT project_path, file_path, transcript_expired_at IS NOT NULL
               FROM claude_session_cache WHERE session_id = ?1",
        )
        .and_then(|mut stmt| {
            stmt.query_map([session_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect()
        })
        .map_err(|e| WriteError::Fallback(format!("reading the session's cache rows: {e}")))?;
    if rows.is_empty() {
        return Err(not_found());
    }
    let deletable = expiry.is_some()
        && rows
            .iter()
            .all(|(_, file_path, stamped)| *stamped && !Path::new(file_path).exists());
    if !deletable {
        return Err(WriteError::ConflictMessage(
            "session transcript is not expired".to_string(),
        ));
    }

    let pairs: Vec<(String, String)> = rows
        .into_iter()
        .map(|(project_path, _, _)| (session_id.to_string(), project_path))
        .collect();
    let deleted = delete_pairs(&tx, &pairs, SearchMode::Keyed).map_err(WriteError::Fallback)?;

    // Nothing below this line may return `Fallback`.
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit session delete: {e}")))?;
    log::info!("expired claude session deleted rows={deleted}");
    Ok(Answer::no_content())
}

/// The body of `DELETE /api/claude-sessions`.
///
/// `Option`, so that a missing key and a `null` are the same thing and both
/// are the route's own 422 rather than a decode error. There is deliberately no
/// "delete everything" form.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DeleteBeforeRequest {
    before: Option<String>,
}

/// What the bulk delete answers, pinned by `parity/session_delete_golden.json`.
#[derive(Debug, Serialize)]
struct Deleted {
    deleted: usize,
}

/// `DELETE /api/claude-sessions` with `{"before":"<RFC 3339>"}` →
/// `200 {"deleted":N}`.
///
/// Deletes every expired pair whose `last_activity` is strictly before the
/// bound and whose stored `file_path` is not on disk. The candidates are
/// selected through [`query::build_filter`] — the sessions list's own
/// predicate, with `transcript=expired&ended_before=…` — so
/// `GET /api/claude-sessions/facets` with those two parameters counts exactly
/// the set this deletes, less any row whose file reappeared. It follows that a
/// hidden project's sessions, and those of a config dir no longer indexed, are
/// not deleted here: nothing the user could not see in that count goes.
pub fn delete_before(db_path: &Path, body: &[u8]) -> Result<Answer, WriteError> {
    let req = decode_body::<DeleteBeforeRequest>(body)?;
    let before = match req.before.as_deref() {
        None | Some("") => return Err(WriteError::validation("before", "before is required")),
        Some(raw) => GoTime::parse(raw).map_err(|_| {
            WriteError::validation("before", "before must be an RFC 3339 timestamp")
        })?,
    };

    let mut conn = open(db_path)?;
    let pairs = candidates(&conn, before).map_err(WriteError::Fallback)?;

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin expired sessions delete: {e}")))?;
    let deleted = delete_pairs(&tx, &pairs, SearchMode::Sweep).map_err(WriteError::Fallback)?;
    // Encoded before the commit, so nothing can fail after it.
    let body = gojson::to_vec(&Deleted { deleted })
        .map_err(|e| WriteError::Fallback(format!("encoding the delete count: {e}")))?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit expired sessions delete: {e}")))?;

    log::info!("expired claude sessions deleted count={deleted}");
    Ok(Answer::json_status(StatusCode::OK, body))
}

/// The pairs a bulk delete with this bound removes: expired, ended before it,
/// visible to the sessions list, and with no file at the stored path.
///
/// The existence check runs here, outside the transaction, so the write lock
/// is never held across a walk of the filesystem.
fn candidates(conn: &Connection, before: GoTime) -> Result<Vec<(String, String)>, String> {
    let data_settings = settings::load(conn);
    let q = query::SessionQuery {
        transcript: query::Transcript::Expired,
        ended_before: Some(before),
        ..Default::default()
    };
    let filter = query::build_filter(
        conn,
        &q,
        &data_settings.hidden_projects,
        &data_settings.indexed_config_dirs,
    )?;
    let sql = format!(
        "SELECT c.session_id, c.project_path, c.file_path FROM claude_session_cache c{}",
        filter.where_clause()
    );
    let rows: Vec<(String, String, String)> = conn
        .prepare(&sql)
        .and_then(|mut stmt| {
            stmt.query_map(rusqlite::params_from_iter(filter.args.iter()), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect()
        })
        .map_err(|e| format!("selecting expired sessions to delete: {e}"))?;

    Ok(rows
        .into_iter()
        .filter(|(_, _, file_path)| !Path::new(file_path).exists())
        .map(|(session_id, project_path, _)| (session_id, project_path))
        .collect())
}

#[cfg(test)]
#[path = "tests_delete.rs"]
mod tests;

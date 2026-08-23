//! The scheduled-task and job-history reads:
//! `GET /api/tasks`, `/api/tasks/{id}`, `/api/tasks/{id}/job-history`,
//! `/api/job-history` and `/api/job-history/{id}`.
//!
//! Mirrors `SQLiteTaskStore`'s five read methods
//! (`internal/storage/sqlite_task_store.go`), `taskService`'s wrappers around
//! them (`internal/service/task_service.go`) and the handlers in
//! `internal/api/tasks.go`.
//!
//! Reads only. Create, update, delete, pause, resume and the bulk job-history
//! delete stay with Go until the storage layer moves.
//!
//! Both trees live in one module because Go registers them together in
//! `mountTaskRoutes`, and `/api/tasks/{id}/job-history` belongs to both — a
//! registry entry is per area, not per path.
//!
//! Three things decide the bytes, none of them visible in the Go structs:
//!
//! 1. **`limit=0` means fifty.** The handler's own parser only rejects negative
//!    and unparsable values, and the service *then* maps any `limit <= 0` to 50.
//!    So `?limit=0` returns a full page rather than nothing. See [`page_limit`].
//! 2. **A nil `*time.Time` is an absent key, not `null`.** All four nullable
//!    timestamps carry `omitempty`, so `Option<GoTime>` with
//!    `skip_serializing_if` is the shape — and `next_run_at` is absent on every
//!    row Go writes, because nothing in the scheduler ever populates it.
//! 3. **A bad `schedule_config` fails the whole request.** That is the opposite
//!    of `chat_messages.blocks`, which swallows its decode error — the policy is
//!    per column, so mirror the Go call site rather than the neighbouring port.
//!    A stored `null` is *not* bad, though: Go decodes it into the struct's zero
//!    value without complaint, so the task ships `{}` and a 200. See
//!    [`scan_task`] — getting that wrong takes a whole list down over one row.

use std::path::Path;

use axum::http::{Method, StatusCode};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::db;
use super::gotime::GoTime;
use super::writes::{decode_body, finish, WriteError};

/// How a task repeats. Mirrors `storage.ScheduleConfig`.
///
/// Every field is `omitempty`, and only the ones the active `schedule_type`
/// uses are stored — so this serializes to `{}` for a `run_immediately` task
/// rather than to a shape full of zeros. It is a value struct on the Go side,
/// never a pointer, so the key is always present.
/// Since #275 this is decoded from a **request body** as well as from the
/// stored column, which is what the `null_is_zero_value` on every field is for:
/// `{"schedule_config":{"run_at":null}}` is a no-op to `encoding/json` and a
/// type error to serde, and it reaches this struct straight off the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleConfig {
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub run_at: String,
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub every_minutes: i64,
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub every_hours: i64,
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub every_days: i64,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub at_time: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub expression: String,
}

/// One scheduled task. Mirrors `storage.ScheduledTask`.
///
/// Field order is the Go struct's declaration order, which here happens to
/// match the `SELECT`'s column order — unlike `chats.rs`, where it does not.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub agent_slug: String,
    pub working_directory: String,
    pub model: String,
    pub settings_profile_id: String,
    pub timeout_minutes: i64,
    /// "run_immediately", "one_off", "interval" or "cron".
    pub schedule_type: String,
    pub schedule_config: ScheduleConfig,
    pub stop_after_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_after_time: Option<GoTime>,
    pub save_output: bool,
    /// "active" or "paused".
    pub status: String,
    pub run_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<GoTime>,
    pub last_run_status: String,
    /// Absent on every row the Go server writes: the column exists and is read
    /// back, but nothing in `internal/scheduler` ever populates it — the next
    /// fire time lives only inside the in-memory `gocron` scheduler. Read
    /// anyway, because a stored value must not be dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<GoTime>,
    pub created_at: GoTime,
    pub updated_at: GoTime,
}

/// One recorded run of a task. Mirrors `storage.JobHistory`.
#[derive(Debug, Clone, Serialize)]
pub struct JobHistory {
    pub id: String,
    pub task_id: String,
    pub task_name: String,
    pub agent_slug: String,
    /// "running", "success" or "failed".
    pub status: String,
    pub started_at: GoTime,
    /// Absent while the job is still running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<GoTime>,
    /// Milliseconds the scheduler measured, not a `time.Duration` — so it is a
    /// plain integer on the wire rather than a nanosecond count.
    pub duration_ms: i64,
    pub chat_session_id: String,
    pub model: String,
    pub prompt_preview: String,
    pub error_message: String,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_creation_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub response_text: String,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

const TASK_COLUMNS: &str =
    "SELECT id, name, description, prompt, agent_slug, working_directory, model,
       settings_profile_id, timeout_minutes, schedule_type, schedule_config,
       stop_after_count, stop_after_time, save_output, status, run_count, last_run_at,
       last_run_status, next_run_at, created_at, updated_at
FROM scheduled_tasks";

const JOB_COLUMNS: &str =
    "SELECT id, task_id, task_name, agent_slug, status, started_at, finished_at,
       duration_ms, chat_session_id, model, prompt_preview, error_message,
       total_input_tokens, total_output_tokens,
       total_cache_creation_tokens, total_cache_read_tokens, response_text
FROM job_history";

/// Every task, most recently created first, as the store orders them.
pub fn list_tasks(db_path: &Path) -> Result<Vec<ScheduledTask>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{TASK_COLUMNS}\nORDER BY created_at DESC");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("listing tasks: {e}"))?;
    let rows = stmt
        .query_map([], scan_task)
        .map_err(|e| format!("listing tasks: {e}"))?;

    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row.map_err(|e| format!("listing tasks: {e}"))?);
    }
    Ok(tasks)
}

/// One task by id, or `None` when there is no such row — which the caller turns
/// into the 404 Go returns.
pub fn get_task(db_path: &Path, id: &str) -> Result<Option<ScheduledTask>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{TASK_COLUMNS} WHERE id = ?");
    conn.query_row(&sql, [id], scan_task)
        .optional()
        .map_err(|e| format!("getting task {id:?}: {e}"))
}

/// One task's job history, newest run first.
///
/// Deliberately **does not check that the task exists**: neither does Go, so an
/// unknown id answers `200 []` rather than a 404. Falling back here would let Go
/// answer with the same empty list, but slower and for the wrong reason.
pub fn list_task_job_history(
    db_path: &Path,
    task_id: &str,
    limit: i64,
) -> Result<Vec<JobHistory>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{JOB_COLUMNS}\nWHERE task_id = ?\nORDER BY started_at DESC\nLIMIT ?");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("listing job history for task {task_id:?}: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![task_id, limit], scan_job)
        .map_err(|e| format!("listing job history for task {task_id:?}: {e}"))?;

    let mut history = Vec::new();
    for row in rows {
        history.push(row.map_err(|e| format!("listing job history for task {task_id:?}: {e}"))?);
    }
    Ok(history)
}

/// Every job history entry, newest run first, one page at a time.
pub fn list_all_job_history(
    db_path: &Path,
    limit: i64,
    offset: i64,
) -> Result<Vec<JobHistory>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{JOB_COLUMNS}\nORDER BY started_at DESC\nLIMIT ? OFFSET ?");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("listing all job history: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], scan_job)
        .map_err(|e| format!("listing all job history: {e}"))?;

    let mut history = Vec::new();
    for row in rows {
        history.push(row.map_err(|e| format!("listing all job history: {e}"))?);
    }
    Ok(history)
}

/// One job history entry by id, or `None` when there is no such row.
pub fn get_job_history(db_path: &Path, id: &str) -> Result<Option<JobHistory>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{JOB_COLUMNS} WHERE id = ?");
    conn.query_row(&sql, [id], scan_job)
        .optional()
        .map_err(|e| format!("getting job history {id:?}: {e}"))
}

fn scan_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledTask> {
    let config: String = row.get(10)?;
    Ok(ScheduledTask {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        prompt: row.get(3)?,
        agent_slug: row.get(4)?,
        working_directory: row.get(5)?,
        model: row.get(6)?,
        settings_profile_id: row.get(7)?,
        timeout_minutes: row.get(8)?,
        schedule_type: row.get(9)?,
        // An unparsable schedule config fails the whole read rather than
        // serving a task whose schedule is unknown.
        //
        // `Option` is what keeps a stored `null` out of that arm. Go unmarshals
        // a JSON `null` into a struct by leaving it at its zero value and
        // returning no error, so the task ships `"schedule_config":{}` and a
        // 200 — decoding straight into `ScheduleConfig` would reject it and
        // take the whole list down to a fallback with it. Verified against a Go
        // server built from this checkout.
        schedule_config: serde_json::from_str::<Option<ScheduleConfig>>(&config)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::other(format!(
                        "parsing schedule config: {e}"
                    ))),
                )
            })?
            .unwrap_or_default(),
        stop_after_count: row.get(11)?,
        stop_after_time: nullable_timestamp(row, 12)?,
        save_output: row.get(13)?,
        status: row.get(14)?,
        run_count: row.get(15)?,
        last_run_at: nullable_timestamp(row, 16)?,
        last_run_status: row.get(17)?,
        next_run_at: nullable_timestamp(row, 18)?,
        created_at: timestamp(row, 19)?,
        updated_at: timestamp(row, 20)?,
    })
}

fn scan_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobHistory> {
    Ok(JobHistory {
        id: row.get(0)?,
        task_id: row.get(1)?,
        task_name: row.get(2)?,
        agent_slug: row.get(3)?,
        status: row.get(4)?,
        started_at: timestamp(row, 5)?,
        finished_at: nullable_timestamp(row, 6)?,
        duration_ms: row.get(7)?,
        chat_session_id: row.get(8)?,
        model: row.get(9)?,
        prompt_preview: row.get(10)?,
        error_message: row.get(11)?,
        total_input_tokens: row.get(12)?,
        total_output_tokens: row.get(13)?,
        total_cache_creation_tokens: row.get(14)?,
        total_cache_read_tokens: row.get(15)?,
        response_text: row.get(16)?,
    })
}

/// Read a DATETIME column as the `time.Time` the Go driver round-trips.
fn timestamp(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<GoTime> {
    let text: String = row.get(index)?;
    super::gotime::from_sql_text(&text, index)
}

/// The same, for a column Go scans through `sql.NullTime` into a `*time.Time`.
fn nullable_timestamp(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<GoTime>> {
    match row.get::<_, Option<String>>(index)? {
        Some(text) => super::gotime::from_sql_text(&text, index).map(Some),
        None => Ok(None),
    }
}

// ─── Query parameters ─────────────────────────────────────────────────────────

/// Go's `maxQueryLimit`: a larger value is clamped, never rejected.
const MAX_QUERY_LIMIT: i64 = 500;

/// The default both list handlers pass to `parseQueryInt`.
const DEFAULT_LIMIT: i64 = 50;

/// `parseQueryInt` from `internal/api/tasks.go`.
///
/// Every rejection is silent: an absent, empty, unparsable or negative value
/// falls back to the default rather than 400-ing, and anything above
/// `maxQueryLimit` is clamped down to it.
fn parse_query_int(query: &str, key: &str, default: i64) -> i64 {
    let raw = super::query::value(query, key);
    if raw.is_empty() {
        return default;
    }
    match raw.parse::<i64>() {
        Ok(value) if value >= 0 => value.min(MAX_QUERY_LIMIT),
        _ => default,
    }
}

/// The page size a list read actually uses.
///
/// Two clamps in sequence, and the order is what makes `?limit=0` surprising:
/// the handler's parser lets a literal `0` through (it only rejects *negative*
/// values), and the service then maps every `limit <= 0` to 50. So `?limit=0`
/// asks for nothing and receives a full page.
fn page_limit(query: &str) -> i64 {
    let limit = parse_query_int(query, "limit", DEFAULT_LIMIT);
    if limit <= 0 {
        DEFAULT_LIMIT
    } else {
        limit
    }
}

/// `offset` cannot go negative — the parser already refused, and the service
/// clamps again.
fn page_offset(query: &str) -> i64 {
    parse_query_int(query, "offset", 0).max(0)
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "tasks",
    claims,
    serve,
};

/// Every path this module answers.
enum Route<'a> {
    TaskList,
    Task(&'a str),
    TaskPause(&'a str),
    TaskResume(&'a str),
    TaskJobHistory(&'a str),
    JobHistoryList,
    JobHistory(&'a str),
}

fn claims(method: &Method, path: &str) -> bool {
    match *method {
        Method::GET => matches!(
            route_of(path),
            Some(Route::TaskList)
                | Some(Route::Task(_))
                | Some(Route::TaskJobHistory(_))
                | Some(Route::JobHistoryList)
                | Some(Route::JobHistory(_))
        ),
        // #275 completed the set. The task writes were Go's because each also
        // registers or unregisters a cron entry, and until the scheduler moved
        // here a task created natively would have been stored and then never
        // fired. It is here now, so the write and the registration are once
        // again the same edit.
        Method::POST => matches!(
            route_of(path),
            Some(Route::TaskList) | Some(Route::TaskPause(_)) | Some(Route::TaskResume(_))
        ),
        Method::PUT => matches!(route_of(path), Some(Route::Task(_))),
        Method::DELETE => matches!(
            route_of(path),
            Some(Route::Task(_)) | Some(Route::JobHistoryList) | Some(Route::JobHistory(_))
        ),
        _ => false,
    }
}

/// Match this module's paths and nothing else.
///
/// The ids are single segments, so `/api/tasks/{id}/pause` and `/resume` cannot
/// be swallowed by the `/api/tasks/{id}` arm, and an empty id is not a match
/// because chi routes `/api/tasks/` to nothing. The three suffixed forms are
/// checked before the bare one for the same reason.
fn route_of(path: &str) -> Option<Route<'_>> {
    if path == "/api/tasks" {
        return Some(Route::TaskList);
    }
    if path == "/api/job-history" {
        return Some(Route::JobHistoryList);
    }
    if let Some(rest) = path.strip_prefix("/api/job-history/") {
        return segment(rest).map(Route::JobHistory);
    }
    if let Some(rest) = path.strip_prefix("/api/tasks/") {
        if let Some(id) = rest.strip_suffix("/job-history") {
            return segment(id).map(Route::TaskJobHistory);
        }
        if let Some(id) = rest.strip_suffix("/pause") {
            return segment(id).map(Route::TaskPause);
        }
        if let Some(id) = rest.strip_suffix("/resume") {
            return segment(id).map(Route::TaskResume);
        }
        return segment(rest).map(Route::Task);
    }
    None
}

fn segment(value: &str) -> Option<&str> {
    if value.is_empty() || value.contains('/') {
        return None;
    }
    Some(value)
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let db = &ctx.db_path;
    match (req.method.clone(), route_of(req.path)) {
        (Method::DELETE, Some(Route::JobHistory(id))) => finish(delete_job_history(db, id)),
        (Method::DELETE, Some(Route::JobHistoryList)) => {
            finish(bulk_delete_job_history(db, req.body))
        }
        (Method::DELETE, Some(Route::Task(id))) => finish(delete_task(db, id)),
        (Method::POST, Some(Route::TaskList)) => finish(create_task(db, req.body)),
        (Method::POST, Some(Route::TaskPause(id))) => finish(pause_task(db, id)),
        (Method::POST, Some(Route::TaskResume(id))) => finish(resume_task(db, id)),
        (Method::PUT, Some(Route::Task(id))) => finish(update_task(db, id, req.body)),
        (Method::GET, _) => serve_read(ctx, req),
        _ => Err(format!("{} {} is not ported", req.method, req.path)),
    }
}

fn serve_read(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let db = &ctx.db_path;
    let body = match route_of(req.path) {
        Some(Route::TaskList) => {
            super::gojson::to_vec(&list_tasks(db)?).map_err(|e| format!("encoding tasks: {e}"))?
        }

        // Falling back lets Go answer the 404, rather than this having to
        // reproduce its body and status.
        Some(Route::Task(id)) => match get_task(db, id)? {
            Some(task) => {
                super::gojson::to_vec(&task).map_err(|e| format!("encoding task: {e}"))?
            }
            None => return Err(format!("task {id:?} not found")),
        },

        // No existence check, matching Go: an unknown task is an empty list.
        Some(Route::TaskJobHistory(id)) => {
            let history = list_task_job_history(db, id, page_limit(req.query))?;
            super::gojson::to_vec(&history)
                .map_err(|e| format!("encoding task job history: {e}"))?
        }

        Some(Route::JobHistoryList) => {
            let history = list_all_job_history(db, page_limit(req.query), page_offset(req.query))?;
            super::gojson::to_vec(&history).map_err(|e| format!("encoding job history: {e}"))?
        }

        Some(Route::JobHistory(id)) => match get_job_history(db, id)? {
            Some(job) => {
                super::gojson::to_vec(&job).map_err(|e| format!("encoding job history: {e}"))?
            }
            None => return Err(format!("job history {id:?} not found")),
        },

        // The two POST-only paths reach `serve_read` from nowhere — `serve`
        // routes them by method first — so this arm is the same "not a read"
        // answer the `None` arm gives.
        Some(Route::TaskPause(_)) | Some(Route::TaskResume(_)) | None => {
            return Err(format!("{} is not a task read", req.path))
        }
    };
    Ok(super::Answer::json(body))
}

// ─── Writes ───────────────────────────────────────────────────────────────────

/// `BulkDeleteRequest` (`internal/api/types.go`).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct BulkDeleteRequest {
    /// A `null` element is `""` to Go, not an error (#295) — and an empty id
    /// simply matches no row, exactly as Go's does.
    ids: Option<super::gojson::GoList<String>>,
}

/// Go's `maxQueryLimit`, reused as the bulk-delete cap.
const MAX_BULK_IDS: usize = 500;

/// `taskService.DeleteJobHistory`.
///
/// Unlike the chat and agent deletes, this one is a genuine **404**: the service
/// reads the row first and returns a `NotFoundError`, which `httpErr` maps. The
/// store's own zero-rows error is unreachable behind that check.
fn delete_job_history(db_path: &Path, id: &str) -> Result<super::Answer, WriteError> {
    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin job history delete: {e}")))?;

    let exists: bool = tx
        .query_row("SELECT 1 FROM job_history WHERE id = ?1", [id], |_| {
            Ok(true)
        })
        .optional()
        .map_err(|e| WriteError::Fallback(format!("looking up job history: {e}")))?
        .unwrap_or(false);
    if !exists {
        return Err(WriteError::NotFound {
            resource: "job_history".to_string(),
            id: id.to_string(),
        });
    }

    tx.execute("DELETE FROM job_history WHERE id = ?1", [id])
        .map_err(|e| WriteError::Fallback(format!("deleting job history {id:?}: {e}")))?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit job history delete: {e}")))?;

    log::info!("job history deleted id={id:?}");
    Ok(super::Answer::no_content())
}

/// `handleBulkDeleteJobHistory`. Ids that do not exist are not an error.
fn bulk_delete_job_history(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<BulkDeleteRequest>(body)?;
    let ids = req.ids.unwrap_or_default();
    if ids.is_empty() {
        return Err(WriteError::BadRequest("ids must not be empty".to_string()));
    }
    if ids.len() > MAX_BULK_IDS {
        return Err(WriteError::BadRequest("too many ids (max 500)".to_string()));
    }

    let conn = open_for_write(db_path)?;
    // `vec!` rather than `iter::repeat_n`, which needs Rust 1.82 and this
    // crate's MSRV is 1.77.
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("DELETE FROM job_history WHERE id IN ({placeholders})");
    conn.execute(&sql, rusqlite::params_from_iter(ids.iter()))
        .map_err(|e| WriteError::Fallback(format!("bulk deleting job history: {e}")))?;

    // `len(ids)`, as Go's is: what was asked for rather than what matched.
    log::info!("job history bulk deleted count={}", ids.len());
    Ok(super::Answer::no_content())
}

fn open_for_write(db_path: &Path) -> Result<rusqlite::Connection, WriteError> {
    let conn = db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::gojson;
    use axum::http::StatusCode;

    const SCHEMA: &str = "
        CREATE TABLE scheduled_tasks (
            id                  TEXT PRIMARY KEY,
            name                TEXT NOT NULL,
            description         TEXT NOT NULL DEFAULT '',
            prompt              TEXT NOT NULL,
            agent_slug          TEXT NOT NULL DEFAULT '',
            working_directory   TEXT NOT NULL DEFAULT '',
            model               TEXT NOT NULL DEFAULT '',
            settings_profile_id TEXT NOT NULL DEFAULT '',
            timeout_minutes     INTEGER NOT NULL DEFAULT 30,
            schedule_type       TEXT NOT NULL DEFAULT 'one_off',
            schedule_config     TEXT NOT NULL DEFAULT '{}',
            stop_after_count    INTEGER NOT NULL DEFAULT 0,
            stop_after_time     DATETIME,
            status              TEXT NOT NULL DEFAULT 'active',
            run_count           INTEGER NOT NULL DEFAULT 0,
            last_run_at         DATETIME,
            last_run_status     TEXT NOT NULL DEFAULT '',
            next_run_at         DATETIME,
            created_at          DATETIME NOT NULL,
            updated_at          DATETIME NOT NULL,
            save_output         INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE job_history (
            id                          TEXT PRIMARY KEY,
            task_id                     TEXT NOT NULL,
            task_name                   TEXT NOT NULL,
            agent_slug                  TEXT NOT NULL DEFAULT '',
            status                      TEXT NOT NULL DEFAULT 'running',
            started_at                  DATETIME NOT NULL,
            finished_at                 DATETIME,
            duration_ms                 INTEGER NOT NULL DEFAULT 0,
            chat_session_id             TEXT NOT NULL DEFAULT '',
            model                       TEXT NOT NULL DEFAULT '',
            prompt_preview              TEXT NOT NULL DEFAULT '',
            error_message               TEXT NOT NULL DEFAULT '',
            total_input_tokens          INTEGER NOT NULL DEFAULT 0,
            total_output_tokens         INTEGER NOT NULL DEFAULT 0,
            total_cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            total_cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
            response_text               TEXT NOT NULL DEFAULT ''
        );";

    /// One fully-populated task and one left at its defaults, plus a finished
    /// and an unfinished run — the four shapes the wire distinguishes.
    fn fixture() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute_batch(
            r#"
            INSERT INTO scheduled_tasks
                (id, name, description, prompt, agent_slug, working_directory, model,
                 settings_profile_id, timeout_minutes, schedule_type, schedule_config,
                 stop_after_count, stop_after_time, save_output, status, run_count,
                 last_run_at, last_run_status, next_run_at, created_at, updated_at)
            VALUES
                ('bare', 'Bare', '', 'do it', 'writer', '', '', '', 30,
                 'run_immediately', '{}', 0, NULL, 0, 'active', 0,
                 NULL, '', NULL,
                 '2026-01-02 03:04:05 +0000 UTC', '2026-01-02 03:04:05 +0000 UTC'),
                ('full', 'Cron <report> & co', 'ünïcödé 😀', 'summarise', 'writer',
                 '/w', 'claude-opus-4-1', 'work-profile', 45,
                 'cron', '{"expression":"0 2 * * *"}', 10,
                 '2027-06-01 12:00:00 +0000 UTC', 1, 'paused', 7,
                 '2026-08-14 23:15:04.5 +0000 UTC', 'success',
                 '2026-08-16 02:00:00 +0000 UTC',
                 '2026-03-04 05:06:07.123456789 +0000 UTC', '2026-03-04 05:06:08 +0000 UTC');

            INSERT INTO job_history
                (id, task_id, task_name, agent_slug, status, started_at, finished_at,
                 duration_ms, chat_session_id, model, prompt_preview, error_message,
                 total_input_tokens, total_output_tokens, total_cache_creation_tokens,
                 total_cache_read_tokens, response_text)
            VALUES
                ('job-old', 'full', 'Cron <report> & co', 'writer', 'success',
                 '2026-08-14 02:00:00.123456789 +0000 UTC', '2026-08-14 02:04:31.5 +0000 UTC',
                 271500, 'chat-1', 'claude-opus-4-1', 'summarise <b>fast</b>', '',
                 1200, 340, 90, 7700, 'done & dusted'),
                ('job-new', 'full', 'Cron <report> & co', 'writer', 'running',
                 '2026-08-15 01:00:00 +0000 UTC', NULL,
                 0, '', '', '', '', 0, 0, 0, 0, '');
            "#,
        )
        .expect("seed");
        file
    }

    fn encoded(value: &impl Serialize) -> String {
        String::from_utf8(gojson::to_vec(value).expect("encode"))
            .expect("utf-8")
            .trim_end()
            .to_string()
    }

    #[test]
    fn tasks_are_ordered_by_most_recently_created() {
        let file = fixture();
        let tasks = list_tasks(file.path()).expect("list");
        assert_eq!(
            tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["full", "bare"]
        );
    }

    #[test]
    fn a_missing_task_or_job_is_none_not_an_error() {
        let file = fixture();
        assert!(get_task(file.path(), "nope").expect("get").is_none());
        assert!(get_task(file.path(), "bare").expect("get").is_some());
        assert!(get_job_history(file.path(), "nope").expect("get").is_none());
        assert!(get_job_history(file.path(), "job-new")
            .expect("get")
            .is_some());
    }

    /// The declaration order of `storage.ScheduledTask`, with every nullable
    /// timestamp present.
    #[test]
    fn a_populated_task_matches_gos_field_order() {
        let file = fixture();
        let tasks = list_tasks(file.path()).expect("list");
        assert_eq!(
            encoded(&tasks[0]),
            r#"{"id":"full","name":"Cron \u003creport\u003e \u0026 co","description":"ünïcödé 😀","prompt":"summarise","agent_slug":"writer","working_directory":"/w","model":"claude-opus-4-1","settings_profile_id":"work-profile","timeout_minutes":45,"schedule_type":"cron","schedule_config":{"expression":"0 2 * * *"},"stop_after_count":10,"stop_after_time":"2027-06-01T12:00:00Z","save_output":true,"status":"paused","run_count":7,"last_run_at":"2026-08-14T23:15:04.5Z","last_run_status":"success","next_run_at":"2026-08-16T02:00:00Z","created_at":"2026-03-04T05:06:07.123456789Z","updated_at":"2026-03-04T05:06:08Z"}"#
        );
    }

    /// A nil `*time.Time` is an **absent key**, not `null` — and an empty
    /// `ScheduleConfig` is `{}` rather than a shape full of zeros, because every
    /// one of its fields is `omitempty` too.
    #[test]
    fn a_bare_task_omits_its_nil_timestamps_and_empties_its_config() {
        let file = fixture();
        let tasks = list_tasks(file.path()).expect("list");
        assert_eq!(
            encoded(&tasks[1]),
            r#"{"id":"bare","name":"Bare","description":"","prompt":"do it","agent_slug":"writer","working_directory":"","model":"","settings_profile_id":"","timeout_minutes":30,"schedule_type":"run_immediately","schedule_config":{},"stop_after_count":0,"save_output":false,"status":"active","run_count":0,"last_run_status":"","created_at":"2026-01-02T03:04:05Z","updated_at":"2026-01-02T03:04:05Z"}"#
        );
    }

    #[test]
    fn job_history_is_newest_first_and_omits_an_unfinished_run() {
        let file = fixture();
        let history = list_all_job_history(file.path(), 50, 0).expect("list");
        assert_eq!(
            history.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(),
            vec!["job-new", "job-old"]
        );
        assert_eq!(
            encoded(&history[0]),
            r#"{"id":"job-new","task_id":"full","task_name":"Cron \u003creport\u003e \u0026 co","agent_slug":"writer","status":"running","started_at":"2026-08-15T01:00:00Z","duration_ms":0,"chat_session_id":"","model":"","prompt_preview":"","error_message":"","total_input_tokens":0,"total_output_tokens":0,"total_cache_creation_tokens":0,"total_cache_read_tokens":0,"response_text":""}"#
        );
        assert_eq!(
            encoded(&history[1]),
            r#"{"id":"job-old","task_id":"full","task_name":"Cron \u003creport\u003e \u0026 co","agent_slug":"writer","status":"success","started_at":"2026-08-14T02:00:00.123456789Z","finished_at":"2026-08-14T02:04:31.5Z","duration_ms":271500,"chat_session_id":"chat-1","model":"claude-opus-4-1","prompt_preview":"summarise \u003cb\u003efast\u003c/b\u003e","error_message":"","total_input_tokens":1200,"total_output_tokens":340,"total_cache_creation_tokens":90,"total_cache_read_tokens":7700,"response_text":"done \u0026 dusted"}"#
        );
    }

    #[test]
    fn limit_and_offset_page_the_history() {
        let file = fixture();
        let page = list_all_job_history(file.path(), 1, 1).expect("list");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, "job-old");
        assert!(list_all_job_history(file.path(), 2, 99)
            .expect("list")
            .is_empty());
    }

    /// Go does not check the task exists before listing its runs, so an unknown
    /// id is an empty list with a 200 — not the 404 `/api/tasks/{id}` gives.
    #[test]
    fn an_unknown_tasks_job_history_is_empty_rather_than_missing() {
        let file = fixture();
        assert!(list_task_job_history(file.path(), "nope", 50)
            .expect("list")
            .is_empty());
        assert_eq!(
            list_task_job_history(file.path(), "full", 50)
                .expect("list")
                .len(),
            2
        );
    }

    /// The whole of `parseQueryInt`, whose every rejection is silent.
    #[test]
    fn query_limits_clamp_and_fall_back_the_way_go_does() {
        assert_eq!(page_limit(""), 50, "absent");
        assert_eq!(page_limit("limit="), 50, "empty");
        assert_eq!(page_limit("limit=abc"), 50, "unparsable");
        assert_eq!(page_limit("limit=-3"), 50, "negative");
        assert_eq!(page_limit("limit=9999"), 500, "clamped to maxQueryLimit");
        assert_eq!(page_limit("limit=1"), 1);
        assert_eq!(page_limit("limit=500"), 500);

        // The one that surprises: the handler's parser only rejects *negative*
        // values, so a literal 0 reaches the service, which maps `<= 0` to 50.
        assert_eq!(page_limit("limit=0"), 50, "zero means a full page");

        assert_eq!(page_offset(""), 0);
        assert_eq!(page_offset("offset=abc"), 0);
        assert_eq!(page_offset("offset=-1"), 0);
        assert_eq!(page_offset("offset=7"), 7);
        assert_eq!(page_offset("offset=9999"), 500, "offset is clamped too");

        // A repeated key takes the first value, as `url.Values.Get` does.
        assert_eq!(page_limit("limit=3&limit=9"), 3);
        assert_eq!(page_limit("offset=2&limit=3"), 3);
    }

    /// An unparsable `schedule_config` fails the whole read — the opposite of
    /// `chat_messages.blocks`, which swallows its decode error. The policy is
    /// per column, and this one is Go's.
    #[test]
    fn an_unparsable_schedule_config_fails_the_read() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute_batch(
            "INSERT INTO scheduled_tasks (id, name, prompt, schedule_config, created_at, updated_at)
             VALUES ('broken', 'Broken', 'p', 'not json',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC');",
        )
        .expect("seed");

        assert!(list_tasks(file.path()).is_err());
        assert!(get_task(file.path(), "broken").is_err());
    }

    /// …but a stored JSON `null` is **not** unparsable. Go unmarshals `null`
    /// into a struct by leaving it at its zero value and returning no error, so
    /// the task ships `{}` and a 200. Decoding straight into `ScheduleConfig`
    /// rejects it, which would drop the whole list to a fallback over one row.
    #[test]
    fn a_null_schedule_config_is_an_empty_one_not_a_failure() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute_batch(
            "INSERT INTO scheduled_tasks (id, name, prompt, schedule_config, created_at, updated_at)
             VALUES ('nulled', 'Nulled', 'p', 'null',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC');",
        )
        .expect("seed");

        let task = get_task(file.path(), "nulled").expect("get").expect("task");
        assert!(
            encoded(&task).contains(r#""schedule_config":{},"#),
            "{}",
            encoded(&task)
        );
        assert_eq!(list_tasks(file.path()).expect("list").len(), 1);
    }

    #[test]
    fn every_task_and_job_history_path_is_routed_and_nothing_else_is() {
        let claimed = |p: &str| route_of(p).is_some();

        assert!(claimed("/api/tasks"));
        assert!(claimed("/api/tasks/abc-123"));
        assert!(claimed("/api/tasks/abc-123/job-history"));
        assert!(claimed("/api/job-history"));
        assert!(claimed("/api/job-history/abc-123"));

        // The two POST actions share the `/api/tasks/{id}` prefix, and the
        // suffixed arms are matched first so the bare one cannot swallow them.
        assert!(matches!(
            route_of("/api/tasks/abc-123/pause"),
            Some(Route::TaskPause("abc-123"))
        ));
        assert!(matches!(
            route_of("/api/tasks/abc-123/resume"),
            Some(Route::TaskResume("abc-123"))
        ));
        assert!(matches!(
            route_of("/api/tasks/abc-123"),
            Some(Route::Task("abc-123"))
        ));

        // chi routes neither trailing-slash form, and an empty id is not a
        // segment — including the empty id in front of a suffix.
        assert!(!claimed("/api/tasks/"));
        assert!(!claimed("/api/job-history/"));
        assert!(!claimed("/api/tasks//job-history"));
        assert!(!claimed("/api/tasks//pause"));
        assert!(!claimed("/api/tasks//resume"));
        assert!(!claimed("/api/tasks/a/b/job-history"));
        assert!(!claimed("/api/tasks/a/b/pause"));
        assert!(!claimed("/api/task"));
        assert!(!claimed("/api/job-historyx"));
    }

    // ─── Writes ───────────────────────────────────────────────────────────────

    // ─── Task writes (#275) ───────────────────────────────────────────────────

    fn migrated() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        file
    }

    fn created(file: &tempfile::NamedTempFile, body: &str) -> ScheduledTask {
        let answer = create_task(file.path(), body.as_bytes()).expect("create");
        assert_eq!(answer.status, StatusCode::CREATED);
        let id = list_tasks(file.path()).expect("list")[0].id.clone();
        get_task(file.path(), &id).expect("get").expect("task")
    }

    #[test]
    fn creating_a_task_answers_201_and_fills_in_gos_two_defaults() {
        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"Nightly","prompt":"go","schedule_type":"cron",
                "schedule_config":{"expression":"0 2 * * *"}}"#,
        );

        assert_eq!(task.name, "Nightly");
        assert_eq!(task.status, "active", "an empty status defaults to active");
        assert_eq!(task.timeout_minutes, 30, "an unset timeout defaults to 30");
        assert_eq!(task.schedule_config.expression, "0 2 * * *");
        assert!(!task.id.is_empty(), "a v4 uuid is minted");
        assert_eq!(task.run_count, 0);
        assert!(task.last_run_at.is_none());
    }

    #[test]
    fn an_empty_schedule_type_becomes_run_immediately_rather_than_a_422() {
        let file = migrated();
        let task = created(&file, r#"{"name":"Now","prompt":"go"}"#);
        assert_eq!(task.schedule_type, "run_immediately");
        // …and the stored config is `{}`, not a shape full of zeros.
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let stored: String = conn
            .query_row("SELECT schedule_config FROM scheduled_tasks", [], |r| {
                r.get(0)
            })
            .expect("read");
        assert_eq!(stored, "{}");
    }

    #[test]
    fn the_five_columns_the_request_cannot_reach_are_stored_at_their_zero_values() {
        // Go's handler copies nine fields out of the request and leaves the
        // rest zero, so a body naming them changes nothing. Reproduced rather
        // than "fixed" — accepting them here would store what Go discards.
        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"N","prompt":"p","working_directory":"/tmp","model":"opus",
                "settings_profile_id":"prof","stop_after_count":9}"#,
        );
        assert!(task.working_directory.is_empty());
        assert!(task.model.is_empty());
        assert!(task.settings_profile_id.is_empty());
        assert_eq!(task.stop_after_count, 0);
        assert!(task.stop_after_time.is_none());
    }

    #[test]
    fn validation_failures_are_422_with_gos_wording() {
        let file = migrated();
        let cases = [
            (
                r#"{"prompt":"p"}"#,
                r#"validation error for "name": name is required"#,
            ),
            (
                r#"{"name":"n"}"#,
                r#"validation error for "prompt": prompt is required"#,
            ),
            (
                r#"{"name":"n","prompt":"p","timeout_minutes":241}"#,
                r#"validation error for "timeout_minutes": timeout must be between 1 and 240 minutes"#,
            ),
            (
                r#"{"name":"n","prompt":"p","schedule_type":"weekly"}"#,
                r#"validation error for "schedule_type": must be run_immediately, one_off, interval, or cron"#,
            ),
            (
                r#"{"name":"n","prompt":"p","schedule_type":"one_off"}"#,
                r#"validation error for "schedule_config.run_at": run_at is required for one_off schedules"#,
            ),
            (
                r#"{"name":"n","prompt":"p","schedule_type":"interval"}"#,
                r#"validation error for "schedule_config": at least one of every_minutes, every_hours, or every_days is required for interval schedules"#,
            ),
            (
                r#"{"name":"n","prompt":"p","schedule_type":"cron"}"#,
                r#"validation error for "schedule_config.expression": expression is required for cron schedules"#,
            ),
        ];
        for (body, want) in cases {
            let err = create_task(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(err.message(), want, "for {body}");
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY, "for {body}");
        }
        assert!(
            list_tasks(file.path()).expect("list").is_empty(),
            "a rejected create stores nothing"
        );
    }

    #[test]
    fn a_negative_timeout_is_rejected_but_zero_is_defaulted() {
        // The message says "between 1 and 240" while the check admits 0 — Go's
        // wording against Go's check. Zero survives validation and is then
        // replaced by 30, so no row ever stores it.
        let file = migrated();
        let err = create_task(
            file.path(),
            br#"{"name":"n","prompt":"p","timeout_minutes":-1}"#,
        )
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let task = created(&file, r#"{"name":"n","prompt":"p","timeout_minutes":0}"#);
        assert_eq!(task.timeout_minutes, 30);
    }

    #[test]
    fn a_malformed_body_is_400_and_an_array_is_not_a_struct() {
        let file = migrated();
        for body in [&b"not json"[..], b"[]", b"[\"name\"]", b""] {
            let err = create_task(file.path(), body).unwrap_err();
            assert_eq!(err.message(), "invalid JSON body", "for {body:?}");
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "for {body:?}");
        }
        // #337: `schedule_config` is a nested struct, so an array there is the
        // same refusal one level down.
        let err = create_task(
            file.path(),
            br#"{"name":"n","prompt":"p","schedule_config":[]}"#,
        )
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_null_field_is_the_zero_value_rather_than_a_decode_failure() {
        // `encoding/json` treats every one of these as a no-op; serde would
        // reject them without `null_is_zero_value` — including the nested
        // struct and the fields *inside* it.
        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"n","prompt":"p","description":null,"agent_slug":null,
                "status":null,"timeout_minutes":null,"save_output":null,
                "schedule_type":null,"schedule_config":null}"#,
        );
        assert_eq!(task.schedule_type, "run_immediately");
        assert!(!task.save_output);

        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"n","prompt":"p","schedule_type":"cron",
                "schedule_config":{"expression":"@daily","run_at":null,"every_days":null}}"#,
        );
        assert_eq!(task.schedule_config.expression, "@daily");
    }

    #[test]
    fn updating_carries_the_run_history_over_and_clears_everything_else() {
        let file = migrated();
        {
            let conn = rusqlite::Connection::open(file.path()).expect("open");
            conn.execute(
                "INSERT INTO scheduled_tasks
                    (id, name, prompt, schedule_type, schedule_config, run_count,
                     last_run_at, last_run_status, created_at, updated_at)
                 VALUES ('t1','Old','p','cron','{\"expression\":\"@daily\"}',4,
                         '2026-01-01 00:00:00 +0000 UTC','success',
                         '2025-01-01 00:00:00 +0000 UTC','2025-01-01 00:00:00 +0000 UTC')",
                [],
            )
            .expect("seed");
        }

        update_task(
            file.path(),
            "t1",
            br#"{"name":"New","prompt":"q","schedule_type":"cron",
                 "schedule_config":{"expression":"@hourly"},"status":"paused"}"#,
        )
        .expect("update");

        let task = get_task(file.path(), "t1").expect("get").expect("task");
        assert_eq!(task.name, "New");
        assert_eq!(task.status, "paused");
        // Carried over from the stored row, not taken from the body.
        assert_eq!(task.run_count, 4);
        assert_eq!(task.last_run_status, "success");
        assert!(task.last_run_at.is_some());
        assert_eq!(
            task.created_at.to_rfc3339_nano(),
            "2025-01-01T00:00:00Z",
            "created_at is preserved"
        );
        assert!(
            task.updated_at.to_rfc3339_nano() != "2025-01-01T00:00:00Z",
            "updated_at is restamped"
        );
    }

    #[test]
    fn the_three_id_routes_are_404_for_an_unknown_task() {
        let file = migrated();
        for err in [
            update_task(file.path(), "nope", br#"{"name":"n","prompt":"p"}"#).unwrap_err(),
            delete_task(file.path(), "nope").unwrap_err(),
            pause_task(file.path(), "nope").unwrap_err(),
            resume_task(file.path(), "nope").unwrap_err(),
        ] {
            assert_eq!(err.status(), StatusCode::NOT_FOUND);
            assert_eq!(err.message(), r#"task "nope" not found"#);
        }
    }

    #[test]
    fn an_unknown_task_is_404_before_the_body_is_even_read() {
        // The lookup precedes the decode in Go's service too, so a malformed
        // body against a missing task is a 404 rather than a 400 — except that
        // the *handler* decodes first, which makes it a 400. Pinning the order
        // this port actually has.
        let file = migrated();
        let err = update_task(file.path(), "nope", b"not json").unwrap_err();
        assert_eq!(
            err.status(),
            StatusCode::BAD_REQUEST,
            "the handler decodes before the service looks the task up"
        );
    }

    #[test]
    fn pause_parks_the_task_and_resume_also_resets_its_run_history() {
        let file = migrated();
        let task = created(&file, r#"{"name":"n","prompt":"p"}"#);
        {
            let conn = rusqlite::Connection::open(file.path()).expect("open");
            conn.execute(
                "UPDATE scheduled_tasks SET run_count = 7, last_run_status = 'failed',
                    last_run_at = '2026-01-01 00:00:00 +0000 UTC' WHERE id = ?1",
                [&task.id],
            )
            .expect("seed history");
        }

        pause_task(file.path(), &task.id).expect("pause");
        let paused = get_task(file.path(), &task.id).expect("get").expect("task");
        assert_eq!(paused.status, "paused");
        assert_eq!(paused.run_count, 7, "pause leaves the history alone");

        resume_task(file.path(), &task.id).expect("resume");
        let resumed = get_task(file.path(), &task.id).expect("get").expect("task");
        assert_eq!(resumed.status, "active");
        // Without this a `stop_after_count` task would auto-pause on its first
        // fire after being resumed.
        assert_eq!(resumed.run_count, 0);
        assert!(resumed.last_run_at.is_none());
        assert!(resumed.last_run_status.is_empty());
    }

    #[test]
    fn deleting_a_task_answers_204_and_cascades_to_its_job_history() {
        let file = migrated_with_history();
        let answer = delete_task(file.path(), "t1").expect("delete");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert!(get_task(file.path(), "t1").expect("get").is_none());
        // Via `ON DELETE CASCADE`, which needs the per-connection
        // `foreign_keys=ON` — this assertion is what would catch its loss.
        assert!(history_ids(&file).is_empty());
    }

    fn migrated_with_history() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute_batch(
            "INSERT INTO scheduled_tasks (id, name, prompt) VALUES ('t1', 'T', 'p');
             INSERT INTO job_history (id, task_id, task_name, started_at)
             VALUES ('j1', 't1', 'T', '2026-01-01 00:00:00 +0000 UTC'),
                    ('j2', 't1', 'T', '2026-01-02 00:00:00 +0000 UTC'),
                    ('j3', 't1', 'T', '2026-01-03 00:00:00 +0000 UTC');",
        )
        .expect("seed");
        file
    }

    fn history_ids(file: &tempfile::NamedTempFile) -> Vec<String> {
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let mut stmt = conn
            .prepare("SELECT id FROM job_history ORDER BY id")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query");
        rows.map(|r| r.expect("row")).collect()
    }

    #[test]
    fn deleting_one_job_history_entry_answers_204() {
        let file = migrated_with_history();
        let answer = delete_job_history(file.path(), "j2").expect("delete");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert!(answer.body.is_none());
        assert_eq!(history_ids(&file), vec!["j1", "j3"]);
    }

    /// This one really is a 404 — the service checks the row exists and returns
    /// a `NotFoundError`, unlike the agent and chat deletes which are 500s.
    #[test]
    fn deleting_a_missing_job_history_entry_is_404() {
        let file = migrated_with_history();
        let err = delete_job_history(file.path(), "ghost").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), "job_history \"ghost\" not found");
        assert_eq!(history_ids(&file).len(), 3, "nothing deleted");
    }

    #[test]
    fn bulk_deleting_job_history_ignores_unknown_ids() {
        let file = migrated_with_history();
        let answer =
            bulk_delete_job_history(file.path(), br#"{"ids":["j1","j3","nope"]}"#).expect("bulk");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert_eq!(history_ids(&file), vec!["j2"]);
    }

    /// A `null` element is `""` to Go — no error — and an empty id matches no
    /// row, so `j1` still goes. Reverting the deserializer makes this a 400 for
    /// a request Go applies (#295).
    #[test]
    fn a_null_id_is_an_empty_string_rather_than_a_400() {
        let file = migrated_with_history();
        let answer = bulk_delete_job_history(file.path(), br#"{"ids":["j1",null]}"#).expect("bulk");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert_eq!(history_ids(&file), vec!["j2", "j3"]);
    }

    #[test]
    fn bulk_job_history_bounds_are_400() {
        let file = migrated_with_history();
        for (body, want) in [
            (r#"{}"#.to_string(), "ids must not be empty"),
            (r#"{"ids":[]}"#.to_string(), "ids must not be empty"),
            (
                format!(r#"{{"ids":[{}]}}"#, vec!["\"x\""; 501].join(",")),
                "too many ids (max 500)",
            ),
        ] {
            let err = bulk_delete_job_history(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(err.message(), want);
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        }
        assert_eq!(
            history_ids(&file).len(),
            3,
            "a rejected bulk deletes nothing"
        );
    }

    /// #275 moved the task writes here, which is only correct because the
    /// scheduler moved with them — each of these also registers or unregisters
    /// a timer, and a task stored without one would never fire.
    #[test]
    fn every_method_is_claimed_on_exactly_the_paths_go_mounts_it_on() {
        assert!(claims(&Method::DELETE, "/api/job-history"));
        assert!(claims(&Method::DELETE, "/api/job-history/j1"));

        assert!(claims(&Method::POST, "/api/tasks"));
        assert!(claims(&Method::PUT, "/api/tasks/t1"));
        assert!(claims(&Method::DELETE, "/api/tasks/t1"));
        assert!(claims(&Method::POST, "/api/tasks/t1/pause"));
        assert!(claims(&Method::POST, "/api/tasks/t1/resume"));

        // Mounted paths this module must *not* answer for the wrong method —
        // chi would 405, and claiming one would turn that into a native error.
        assert!(!claims(&Method::DELETE, "/api/tasks/t1/job-history"));
        assert!(!claims(&Method::PUT, "/api/tasks"));
        assert!(!claims(&Method::POST, "/api/tasks/t1"));
        assert!(!claims(&Method::POST, "/api/job-history"));
        assert!(!claims(&Method::PUT, "/api/tasks/t1/pause"));
        assert!(!claims(&Method::PATCH, "/api/tasks/t1"));
    }

    /// #335: the two job-history deletes, which are all this module claims.
    #[test]
    fn the_job_history_deletes_log_their_entity_and_outcome() {
        crate::native::writes::testlog::install();
        let file = migrated_with_history();

        delete_job_history(file.path(), "j1").expect("delete");
        crate::native::writes::testlog::assert_info_once(r#"job history deleted id="j1""#);

        bulk_delete_job_history(file.path(), br#"{"ids":["j2","j3"]}"#).expect("bulk");
        crate::native::writes::testlog::assert_info_present("job history bulk deleted count=2");
    }
}

// ─── Row writes shared with the scheduler (#275) ───────────────────────────────

/// `ScheduledTask.MarshalScheduleConfig` — the JSON stored in the
/// `schedule_config` column.
///
/// `to_vec_marshal`, not `to_vec`: this is `json.Marshal` into a column, not the
/// HTTP encoder, so there is no trailing newline. The struct's
/// `skip_serializing_if` attributes are what make a `run_immediately` task store
/// `{}` rather than a shape full of zeros, which is the value Go's `omitempty`
/// produces and the one the round trip has to preserve.
pub fn marshal_schedule_config(cfg: &ScheduleConfig) -> Result<String, String> {
    let bytes = super::gojson::to_vec_marshal(cfg)
        .map_err(|e| format!("marshaling schedule config: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("marshaling schedule config: {e}"))
}

/// A nullable DATETIME as the driver writes one: the Go string rendering, or
/// SQL `NULL` for a nil `*time.Time`.
fn nullable_time(value: Option<&GoTime>) -> Option<String> {
    value.map(|t| super::gotime::to_go_string_utc(*t))
}

/// `SQLiteTaskStore.UpdateTask`.
///
/// Stamps `updated_at` on the row **and on the struct**, because the handler
/// answers with the task it just wrote rather than re-reading it — so a caller
/// that returned the pre-write value would report a stale timestamp.
///
/// Shared by the five task writes and by the scheduler's own
/// `updateTaskAfterRun`/`autoPause`, so the column list exists once.
pub fn update_task_row(db_path: &Path, task: &mut ScheduledTask) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;
    update_task_in(&conn, task)
}

/// [`update_task_row`] against a connection the caller already holds — which is
/// what lets a handler read, check and write inside one transaction.
pub fn update_task_in(conn: &rusqlite::Connection, task: &mut ScheduledTask) -> Result<(), String> {
    let now = super::gotime::now_go_text();
    let config = marshal_schedule_config(&task.schedule_config)?;
    let affected = conn
        .execute(
            "UPDATE scheduled_tasks SET
                name = ?1, description = ?2, prompt = ?3, agent_slug = ?4,
                working_directory = ?5, model = ?6, settings_profile_id = ?7,
                timeout_minutes = ?8, schedule_type = ?9, schedule_config = ?10,
                stop_after_count = ?11, stop_after_time = ?12, save_output = ?13, status = ?14,
                run_count = ?15, last_run_at = ?16, last_run_status = ?17,
                next_run_at = ?18, updated_at = ?19
             WHERE id = ?20",
            rusqlite::params![
                task.name,
                task.description,
                task.prompt,
                task.agent_slug,
                task.working_directory,
                task.model,
                task.settings_profile_id,
                task.timeout_minutes,
                task.schedule_type,
                config,
                task.stop_after_count,
                nullable_time(task.stop_after_time.as_ref()),
                task.save_output,
                task.status,
                task.run_count,
                nullable_time(task.last_run_at.as_ref()),
                task.last_run_status,
                nullable_time(task.next_run_at.as_ref()),
                now,
                task.id,
            ],
        )
        .map_err(|e| format!("updating task {:?}: {e}", task.id))?;
    if affected == 0 {
        // Go's store returns this and every caller has already checked the row
        // exists, so it is unreachable through the API — but the scheduler
        // writes without that check, and a silently dropped write there would
        // lose a run counter.
        return Err(format!("task {:?} not found", task.id));
    }
    task.updated_at = super::gotime::from_sql_text(&now, 0)
        .map_err(|e| format!("re-reading the write timestamp: {e}"))?;
    Ok(())
}

/// `SQLiteTaskStore.CreateTask`, for a row whose id and timestamps the caller
/// has already stamped.
pub fn insert_task_in(conn: &rusqlite::Connection, task: &ScheduledTask) -> Result<(), String> {
    let config = marshal_schedule_config(&task.schedule_config)?;
    conn.execute(
        "INSERT INTO scheduled_tasks
            (id, name, description, prompt, agent_slug, working_directory, model,
             settings_profile_id, timeout_minutes, schedule_type, schedule_config,
             stop_after_count, stop_after_time, save_output, status, run_count, last_run_at,
             last_run_status, next_run_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21)",
        rusqlite::params![
            task.id,
            task.name,
            task.description,
            task.prompt,
            task.agent_slug,
            task.working_directory,
            task.model,
            task.settings_profile_id,
            task.timeout_minutes,
            task.schedule_type,
            config,
            task.stop_after_count,
            nullable_time(task.stop_after_time.as_ref()),
            task.save_output,
            task.status,
            task.run_count,
            nullable_time(task.last_run_at.as_ref()),
            task.last_run_status,
            nullable_time(task.next_run_at.as_ref()),
            super::gotime::to_go_string_utc(task.created_at),
            super::gotime::to_go_string_utc(task.updated_at),
        ],
    )
    .map_err(|e| format!("creating task: {e}"))?;
    Ok(())
}

/// `SQLiteTaskStore.CreateJobHistory`.
pub fn insert_job_history(db_path: &Path, job: &JobHistory) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;
    conn.execute(
        "INSERT INTO job_history
            (id, task_id, task_name, agent_slug, status, started_at, finished_at,
             duration_ms, chat_session_id, model, prompt_preview, error_message,
             total_input_tokens, total_output_tokens,
             total_cache_creation_tokens, total_cache_read_tokens, response_text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        rusqlite::params![
            job.id,
            job.task_id,
            job.task_name,
            job.agent_slug,
            job.status,
            super::gotime::to_go_string_utc(job.started_at),
            nullable_time(job.finished_at.as_ref()),
            job.duration_ms,
            job.chat_session_id,
            job.model,
            job.prompt_preview,
            job.error_message,
            job.total_input_tokens,
            job.total_output_tokens,
            job.total_cache_creation_tokens,
            job.total_cache_read_tokens,
            job.response_text,
        ],
    )
    .map_err(|e| format!("creating job history: {e}"))?;
    Ok(())
}

/// `SQLiteTaskStore.UpdateJobHistory`.
///
/// The column list is Go's, which is **narrower than the row**: `started_at`,
/// `model` and `prompt_preview` are not updated, so a finish cannot rewrite what
/// the initial insert recorded.
///
/// A zero-row update is not an error here, matching Go — `createInitialJobHistory`
/// logs an insert failure and returns the row anyway, so the run finishing
/// against a row that was never written is a reachable state and not one worth
/// failing a completed run over.
pub fn update_job_history(db_path: &Path, job: &JobHistory) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;
    conn.execute(
        "UPDATE job_history SET
            status = ?1, finished_at = ?2, duration_ms = ?3, chat_session_id = ?4,
            error_message = ?5, total_input_tokens = ?6, total_output_tokens = ?7,
            total_cache_creation_tokens = ?8, total_cache_read_tokens = ?9,
            response_text = ?10
         WHERE id = ?11",
        rusqlite::params![
            job.status,
            nullable_time(job.finished_at.as_ref()),
            job.duration_ms,
            job.chat_session_id,
            job.error_message,
            job.total_input_tokens,
            job.total_output_tokens,
            job.total_cache_creation_tokens,
            job.total_cache_read_tokens,
            job.response_text,
            job.id,
        ],
    )
    .map_err(|e| format!("updating job history {:?}: {e}", job.id))?;
    Ok(())
}

// ─── The task writes (#275) ───────────────────────────────────────────────────

/// `CreateTaskRequest` and `UpdateTaskRequest` (`internal/api/types.go`).
///
/// One struct for both, because the two Go types are field-for-field identical
/// — they are kept separate there for a divergence that has not happened yet,
/// and two identical structs here would only be two places to forget an
/// attribute.
///
/// **Note what is absent**: `working_directory`, `model`, `settings_profile_id`,
/// `stop_after_count` and `stop_after_time` are columns the table has and the
/// request body does not, so both handlers build a `ScheduledTask` with those at
/// their zero values. Reproduced rather than "fixed": a create that accepted a
/// working directory here would store one Go's create discards, and an update
/// that preserved the existing one would keep a value Go's update clears.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TaskRequest {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    name: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    description: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    agent_slug: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    prompt: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    schedule_type: String,
    /// [`GoStruct`] for #337: every field of `ScheduleConfig` has a default, so
    /// the derive's `visit_seq` arm would accept a JSON **array** of zero or
    /// more elements where Go answers `cannot unmarshal array`.
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    schedule_config: super::gojson::GoStruct<ScheduleConfig>,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    status: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    timeout_minutes: i64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    save_output: bool,
}

impl TaskRequest {
    /// The `storage.ScheduledTask` both handlers build from the request — the
    /// nine fields they copy, and nothing else.
    fn into_task(self) -> ScheduledTask {
        ScheduledTask {
            id: String::new(),
            name: self.name,
            description: self.description,
            prompt: self.prompt,
            agent_slug: self.agent_slug,
            working_directory: String::new(),
            model: String::new(),
            settings_profile_id: String::new(),
            timeout_minutes: self.timeout_minutes,
            schedule_type: self.schedule_type,
            schedule_config: self.schedule_config.0,
            stop_after_count: 0,
            stop_after_time: None,
            save_output: self.save_output,
            status: self.status,
            run_count: 0,
            last_run_at: None,
            last_run_status: String::new(),
            next_run_at: None,
            created_at: GoTime::default(),
            updated_at: GoTime::default(),
        }
    }
}

/// `validateTask` + `validateScheduleConfig`.
///
/// **It mutates**, which is not decoration: an empty `schedule_type` is
/// *defaulted* to `run_immediately` rather than rejected, and the defaulted
/// value is what gets stored and scheduled.
///
/// The `timeout_minutes` message says "between 1 and 240" while the check admits
/// 0 — Go's wording, kept, because a paraphrase would be a different string on
/// the wire. Zero is then replaced by 30 in the caller.
fn validate_task(task: &mut ScheduledTask) -> Result<(), WriteError> {
    if task.name.is_empty() {
        return Err(WriteError::validation("name", "name is required"));
    }
    if task.prompt.is_empty() {
        return Err(WriteError::validation("prompt", "prompt is required"));
    }
    if task.timeout_minutes < 0 || task.timeout_minutes > 240 {
        return Err(WriteError::validation(
            "timeout_minutes",
            "timeout must be between 1 and 240 minutes",
        ));
    }

    match task.schedule_type.as_str() {
        "run_immediately" | "one_off" | "interval" | "cron" => {}
        "" => task.schedule_type = "run_immediately".to_string(),
        _ => {
            return Err(WriteError::validation(
                "schedule_type",
                "must be run_immediately, one_off, interval, or cron",
            ))
        }
    }

    let cfg = &task.schedule_config;
    match task.schedule_type.as_str() {
        // No config at all: the task runs once, on creation.
        "run_immediately" => {}
        "one_off" if cfg.run_at.is_empty() => {
            return Err(WriteError::validation(
                "schedule_config.run_at",
                "run_at is required for one_off schedules",
            ))
        }
        "interval"
            if cfg.every_minutes == 0 && cfg.every_hours == 0 && cfg.every_days == 0 =>
        {
            return Err(WriteError::validation(
                "schedule_config",
                "at least one of every_minutes, every_hours, or every_days is required for interval schedules",
            ))
        }
        "cron" if cfg.expression.is_empty() => {
            return Err(WriteError::validation(
                "schedule_config.expression",
                "expression is required for cron schedules",
            ))
        }
        _ => {}
    }
    Ok(())
}

/// Go's `TimeoutMinutes == 0 → 30`, applied by both create and update *after*
/// validation — which is why a stored 0 is impossible while the validator still
/// admits one.
const DEFAULT_TIMEOUT_MINUTES: i64 = 30;

/// `taskService.CreateTask`.
fn create_task(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<TaskRequest>(body)?;
    let mut task = req.into_task();
    validate_task(&mut task)?;

    if task.status.is_empty() {
        task.status = "active".to_string();
    }
    if task.timeout_minutes == 0 {
        task.timeout_minutes = DEFAULT_TIMEOUT_MINUTES;
    }

    task.id = uuid::Uuid::new_v4().to_string();
    let now = super::gotime::now_go_text();
    let stamped = super::gotime::from_sql_text(&now, 0)
        .map_err(|e| WriteError::Fallback(format!("re-reading the write timestamp: {e}")))?;
    task.created_at = stamped;
    task.updated_at = stamped;

    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin task create: {e}")))?;
    insert_task_in(&tx, &task).map_err(WriteError::Fallback)?;

    // Everything fallible before the commit: an `Err` after it would answer
    // 500 for a task that was actually inserted, inviting a retry that inserts
    // a second one under a fresh id.
    let encoded = super::gojson::to_vec(&task)
        .map_err(|e| WriteError::Fallback(format!("encoding task: {e}")))?;

    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit task create: {e}")))?;
    log::info!("task created id={:?} name={:?}", task.id, task.name);

    // After the commit, exactly as Go schedules after the store returns — so a
    // task that fails to schedule is still stored, and the log line is the only
    // evidence. Nothing here can fail the request.
    if task.status == "active" {
        super::schedule::runtime::schedule_if_running(&task, "newly created");
    }
    Ok(super::Answer::json_status(StatusCode::CREATED, encoded))
}

/// `taskService.UpdateTask`.
///
/// Four fields are carried over from the stored row rather than taken from the
/// body — `run_count`, `last_run_at`, `last_run_status` and `created_at` — and
/// that is what stops an edit from resetting a task's history. `next_run_at` is
/// **not** among them, so an update clears it; nothing writes it, so this is
/// only observable on a row some other tool wrote.
fn update_task(db_path: &Path, id: &str, body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<TaskRequest>(body)?;
    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin task update: {e}")))?;

    let Some(existing) = get_task_in(&tx, id)? else {
        return Err(WriteError::NotFound {
            resource: "task".to_string(),
            id: id.to_string(),
        });
    };

    let mut task = req.into_task();
    task.id = id.to_string();
    task.run_count = existing.run_count;
    task.last_run_at = existing.last_run_at;
    task.last_run_status = existing.last_run_status;
    task.created_at = existing.created_at;

    validate_task(&mut task)?;
    if task.timeout_minutes == 0 {
        task.timeout_minutes = DEFAULT_TIMEOUT_MINUTES;
    }

    update_task_in(&tx, &mut task).map_err(WriteError::Fallback)?;
    let encoded = super::gojson::to_vec(&task)
        .map_err(|e| WriteError::Fallback(format!("encoding task: {e}")))?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit task update: {e}")))?;
    log::info!("task updated id={id:?} name={:?}", task.name);

    // **Always unschedule, then reschedule only if still active** — Go's order,
    // and the reason a task switched to `paused` by an edit stops firing.
    super::schedule::runtime::unschedule_if_running(id);
    if task.status == "active" {
        super::schedule::runtime::schedule_if_running(&task, "updated");
    }
    Ok(super::Answer::json(encoded))
}

/// `taskService.DeleteTask`.
///
/// The statement is a single `DELETE FROM scheduled_tasks`, but the task's job
/// history goes with it: `job_history.task_id` is
/// `REFERENCES scheduled_tasks(id) ON DELETE CASCADE`. **That only happens
/// because `foreign_keys=ON` is set per connection** (`db.rs`) — SQLite defaults
/// it off, and without it this would silently orphan every row instead, which is
/// a data difference no status code would reveal.
fn delete_task(db_path: &Path, id: &str) -> Result<super::Answer, WriteError> {
    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin task delete: {e}")))?;

    // Existence only, deliberately not the decoded row: the delete needs no
    // field, and decoding one would make a task whose `created_at` this port
    // cannot parse — a row some other tool wrote — undeletable. The three
    // routes below genuinely need the row and do decode it.
    let exists: bool = tx
        .query_row("SELECT 1 FROM scheduled_tasks WHERE id = ?1", [id], |_| {
            Ok(true)
        })
        .optional()
        .map_err(|e| WriteError::Fallback(format!("looking up task {id:?}: {e}")))?
        .unwrap_or(false);
    if !exists {
        return Err(WriteError::NotFound {
            resource: "task".to_string(),
            id: id.to_string(),
        });
    }

    // Go unschedules *before* deleting. Kept, even though the row is gone
    // either way: a timer that fired in between would find no task and return.
    super::schedule::runtime::unschedule_if_running(id);

    tx.execute("DELETE FROM scheduled_tasks WHERE id = ?1", [id])
        .map_err(|e| WriteError::Fallback(format!("deleting task {id:?}: {e}")))?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit task delete: {e}")))?;
    log::info!("task deleted id={id:?}");
    Ok(super::Answer::no_content())
}

/// `taskService.PauseTask`: park the task and drop its timer.
fn pause_task(db_path: &Path, id: &str) -> Result<super::Answer, WriteError> {
    let task = set_task_status(db_path, id, |task| {
        task.status = "paused".to_string();
    })?;
    log::info!("task paused id={id:?}");
    super::schedule::runtime::unschedule_if_running(id);
    encode_task(&task)
}

/// `taskService.ResumeTask`.
///
/// Resuming **resets the run history counters** — `run_count` to 0,
/// `last_run_at` to nil, `last_run_status` to empty — which pause does not. That
/// asymmetry is what makes a `stop_after_count` task runnable again: without it
/// a resumed task would auto-pause on its first fire.
fn resume_task(db_path: &Path, id: &str) -> Result<super::Answer, WriteError> {
    let task = set_task_status(db_path, id, |task| {
        task.status = "active".to_string();
        task.run_count = 0;
        task.last_run_at = None;
        task.last_run_status = String::new();
    })?;
    log::info!("task resumed id={id:?}");
    super::schedule::runtime::schedule_if_running(&task, "resumed");
    encode_task(&task)
}

/// The read-modify-write both status actions share, in one transaction.
fn set_task_status(
    db_path: &Path,
    id: &str,
    apply: impl FnOnce(&mut ScheduledTask),
) -> Result<ScheduledTask, WriteError> {
    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin task status change: {e}")))?;

    let Some(mut task) = get_task_in(&tx, id)? else {
        return Err(WriteError::NotFound {
            resource: "task".to_string(),
            id: id.to_string(),
        });
    };
    apply(&mut task);
    update_task_in(&tx, &mut task).map_err(WriteError::Fallback)?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit task status change: {e}")))?;
    Ok(task)
}

fn encode_task(task: &ScheduledTask) -> Result<super::Answer, WriteError> {
    let encoded = super::gojson::to_vec(task)
        .map_err(|e| WriteError::Fallback(format!("encoding task: {e}")))?;
    Ok(super::Answer::json(encoded))
}

/// [`get_task`] against a connection the caller already holds, so the existence
/// check and the write share one transaction.
pub fn get_task_in(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<ScheduledTask>, WriteError> {
    let sql = format!("{TASK_COLUMNS} WHERE id = ?");
    conn.query_row(&sql, [id], scan_task)
        .optional()
        .map_err(|e| WriteError::Fallback(format!("getting task {id:?}: {e}")))
}

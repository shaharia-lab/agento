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

use axum::http::Method;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::db;
use super::gotime::GoTime;

/// How a task repeats. Mirrors `storage.ScheduleConfig`.
///
/// Every field is `omitempty`, and only the ones the active `schedule_type`
/// uses are stored — so this serializes to `{}` for a `run_immediately` task
/// rather than to a shape full of zeros. It is a value struct on the Go side,
/// never a pointer, so the key is always present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_at: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub every_minutes: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub every_hours: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub every_days: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at_time: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
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
        // Go fails the whole read on an unparsable schedule config rather than
        // serving a task whose schedule is unknown. So does this: the error
        // reaches the proxy, which falls back to Go.
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

/// Which of the five reads a path is, if any.
enum Route<'a> {
    TaskList,
    Task(&'a str),
    TaskJobHistory(&'a str),
    JobHistoryList,
    JobHistory(&'a str),
}

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && route_of(path).is_some()
}

/// Match the five read paths and nothing else.
///
/// The ids are single segments, so `/api/tasks/{id}/pause` and `/resume` — both
/// POSTs — cannot be swallowed by the `/api/tasks/{id}` arm, and an empty id is
/// not a match because chi routes `/api/tasks/` to nothing.
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
        return match rest.strip_suffix("/job-history") {
            Some(id) => segment(id).map(Route::TaskJobHistory),
            None => segment(rest).map(Route::Task),
        };
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

        None => return Err(format!("{} is not a task read", req.path)),
    };
    Ok(super::Answer { body, probe: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::gojson;

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
    fn only_the_five_reads_are_routed() {
        let claimed = |p: &str| route_of(p).is_some();

        assert!(claimed("/api/tasks"));
        assert!(claimed("/api/tasks/abc-123"));
        assert!(claimed("/api/tasks/abc-123/job-history"));
        assert!(claimed("/api/job-history"));
        assert!(claimed("/api/job-history/abc-123"));

        // The POST actions share the `/api/tasks/{id}` prefix and must not be
        // swallowed by it.
        assert!(!claimed("/api/tasks/abc-123/pause"));
        assert!(!claimed("/api/tasks/abc-123/resume"));
        // chi routes neither trailing-slash form.
        assert!(!claimed("/api/tasks/"));
        assert!(!claimed("/api/job-history/"));
        assert!(!claimed("/api/tasks//job-history"));
        assert!(!claimed("/api/tasks/a/b/job-history"));
        assert!(!claimed("/api/task"));
        assert!(!claimed("/api/job-historyx"));

        // Only GET, so every write on these paths still forwards.
        assert!(!claims(&Method::POST, "/api/tasks"));
        assert!(!claims(&Method::PUT, "/api/tasks/abc-123"));
        assert!(!claims(&Method::DELETE, "/api/job-history"));
        assert!(claims(&Method::GET, "/api/tasks"));
    }
}

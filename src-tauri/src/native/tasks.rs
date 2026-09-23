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

/// One place a task's output is delivered after a run (#634, epic #626).
///
/// **Typed, with one sub-object per type keyed by the type's name**, so a new
/// destination type is a new optional field here and needs no migration:
/// `{"type":"slack","when":"success","slack":{...}}`. Only `slack` exists yet;
/// [`validate_destinations`] refuses any other `type`.
///
/// Stored whole as a JSON array in `scheduled_tasks.destinations` and decoded
/// from request bodies, so every scalar carries `null_is_zero_value` and the
/// nested object is a [`GoStruct`](super::gojson::GoStruct) — a type, never a
/// `deserialize_with` over a container.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDestination {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub r#type: String,
    /// `success` or `always`; an empty value is defaulted to `success` by
    /// validation, so a stored entry always carries one of the two.
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub when: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<super::gojson::GoStruct<SlackDestination>>,
}

/// A Slack destination's configuration: which connected Slack integration posts,
/// and to which channels.
///
/// `integration_id` is deliberately not a foreign key: a deleted integration
/// leaves the task's configuration in place (see
/// [`check_destination_integrations`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackDestination {
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub integration_id: String,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub channel_ids: super::gojson::GoList<String>,
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
    /// Where the output goes after a run (#634). **Omitted when empty**, not
    /// `[]` and not `null`, so a task without destinations keeps exactly the
    /// bytes it had before the field existed — the `inbound` precedent (#570).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<TaskDestination>,
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
    /// Where the run's output was delivered and how each went (#635), read
    /// from `job_deliveries`. **Last, and absent when empty** rather than `[]`,
    /// so a job with no deliveries keeps the bytes it had before the field
    /// existed. Never folded into `status` or `error_message`: a failed
    /// delivery does not fail the run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<JobDelivery>,
}

/// One delivery of a run's output to one channel (#635) — a `job_deliveries`
/// row. One per channel rather than per destination entry, so a destination
/// posting to two channels can succeed in one and fail in the other.
#[derive(Debug, Clone, Serialize)]
pub struct JobDelivery {
    pub id: String,
    /// The `job_history` row this belongs to. Not on the wire: it is nested
    /// under that row.
    #[serde(skip)]
    pub job_id: String,
    /// The entry's index in the task's `destinations` at dispatch time — the
    /// sort key, not on the wire.
    #[serde(skip)]
    pub position: i64,
    /// `"slack"`, as `TaskDestination::r#type`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Human-readable and denormalised — e.g. `Acme Slack · C0123ABCD` — so the
    /// history reads the same after the integration is deleted or the task
    /// edited.
    pub target: String,
    /// [`DELIVERY_PENDING`], [`DELIVERY_SENT`], [`DELIVERY_FAILED`] or
    /// [`DELIVERY_SKIPPED`].
    pub status: String,
    pub error: String,
    pub created_at: GoTime,
    /// Absent while the delivery is still pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<GoTime>,
}

/// A delivery dispatched and not yet finished. Needed on top of the three
/// outcomes because delivery outlives the run's `RunGuard`: a post lost to the
/// app quitting must still leave a row, which startup then fails.
pub const DELIVERY_PENDING: &str = "pending";
pub const DELIVERY_SENT: &str = "sent";
pub const DELIVERY_FAILED: &str = "failed";
pub const DELIVERY_SKIPPED: &str = "skipped";

/// The error a previous session's still-`pending` delivery is failed with at
/// startup by [`reap_pending_deliveries`].
pub const DELIVERY_INTERRUPTED: &str = "interrupted: app did not finish the delivery";

fn is_zero(value: &i64) -> bool {
    *value == 0
}

const TASK_COLUMNS: &str =
    "SELECT id, name, description, prompt, agent_slug, working_directory, model,
       settings_profile_id, timeout_minutes, schedule_type, schedule_config,
       stop_after_count, stop_after_time, save_output, status, run_count, last_run_at,
       last_run_status, next_run_at, created_at, updated_at, destinations
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
    attach_deliveries(&conn, &mut history)?;
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
    attach_deliveries(&conn, &mut history)?;
    Ok(history)
}

/// One job history entry by id, or `None` when there is no such row.
pub fn get_job_history(db_path: &Path, id: &str) -> Result<Option<JobHistory>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{JOB_COLUMNS} WHERE id = ?");
    let job = conn
        .query_row(&sql, [id], scan_job)
        .optional()
        .map_err(|e| format!("getting job history {id:?}: {e}"))?;
    let mut jobs: Vec<JobHistory> = job.into_iter().collect();
    attach_deliveries(&conn, &mut jobs)?;
    Ok(jobs.pop())
}

/// The most job ids one delivery lookup binds. The page limit is capped at
/// [`MAX_QUERY_LIMIT`], so a page is always one statement; the chunking only
/// keeps a larger caller under SQLite's bound-variable limit.
const DELIVERY_LOOKUP_CHUNK: usize = 500;

/// Fills each job's `deliveries` with **one** batched query per page rather
/// than one per job (#635), in `position` then `created_at` order.
fn attach_deliveries(conn: &rusqlite::Connection, jobs: &mut [JobHistory]) -> Result<(), String> {
    attach_deliveries_with(jobs, |ids| list_deliveries_for_jobs(conn, ids))
}

/// [`attach_deliveries`] with the lookup injected, so a test can count it.
fn attach_deliveries_with(
    jobs: &mut [JobHistory],
    mut load: impl FnMut(&[String]) -> Result<Vec<JobDelivery>, String>,
) -> Result<(), String> {
    if jobs.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = jobs.iter().map(|job| job.id.clone()).collect();
    let deliveries = load(&ids)?;
    let mut by_job: std::collections::HashMap<String, Vec<JobDelivery>> =
        std::collections::HashMap::new();
    for delivery in deliveries {
        by_job
            .entry(delivery.job_id.clone())
            .or_default()
            .push(delivery);
    }
    for job in jobs {
        if let Some(found) = by_job.remove(&job.id) {
            job.deliveries = found;
        }
    }
    Ok(())
}

/// Every delivery of the given jobs, grouped by job and in `position` then
/// `created_at` order within each.
pub fn list_deliveries_for_jobs(
    conn: &rusqlite::Connection,
    job_ids: &[String],
) -> Result<Vec<JobDelivery>, String> {
    let mut deliveries = Vec::new();
    for chunk in job_ids.chunks(DELIVERY_LOOKUP_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT id, job_id, position, type, target, status, error, created_at, finished_at
             FROM job_deliveries
             WHERE job_id IN ({placeholders})
             ORDER BY job_id, position, created_at, id"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("listing job deliveries: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok(JobDelivery {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    position: row.get(2)?,
                    r#type: row.get(3)?,
                    target: row.get(4)?,
                    status: row.get(5)?,
                    error: row.get(6)?,
                    created_at: timestamp(row, 7)?,
                    finished_at: nullable_timestamp(row, 8)?,
                })
            })
            .map_err(|e| format!("listing job deliveries: {e}"))?;
        for row in rows {
            deliveries.push(row.map_err(|e| format!("listing job deliveries: {e}"))?);
        }
    }
    Ok(deliveries)
}

fn scan_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledTask> {
    let config: String = row.get(10)?;
    let destinations: Option<String> = row.get(21)?;
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
        destinations: decode_destinations(destinations.as_deref()).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                21,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(e)),
            )
        })?,
        status: row.get(14)?,
        run_count: row.get(15)?,
        last_run_at: nullable_timestamp(row, 16)?,
        last_run_status: row.get(17)?,
        next_run_at: nullable_timestamp(row, 18)?,
        created_at: timestamp(row, 19)?,
        updated_at: timestamp(row, 20)?,
    })
}

/// The stored `destinations` column. The same policy as `schedule_config`: a
/// `NULL`, empty or JSON `null` value is an empty list, and anything unparsable
/// fails the read rather than serving a task whose delivery is unknown.
fn decode_destinations(stored: Option<&str>) -> Result<Vec<TaskDestination>, String> {
    match stored.map(str::trim) {
        None | Some("") => Ok(Vec::new()),
        Some(text) => serde_json::from_str::<Option<Vec<TaskDestination>>>(text)
            .map(Option::unwrap_or_default)
            .map_err(|e| format!("parsing destinations: {e}")),
    }
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
        deliveries: Vec::new(),
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

/// The routes here that exist **only** in the desktop build (#541).
///
/// The third owner of `parity/desktop_routes.json`, whose assertion is set
/// equality over the union of every owner's const — so this list and that file
/// move together or `the_desktop_only_routes_are_recorded_in_both_directions`
/// fails. Everything else this module claims came from Go and is recorded in
/// `read_routes.json` / `write_routes.json`, which are frozen records of Go's
/// surface and cannot carry a route Go never had.
pub const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/tasks/{id}/run"),
    ("POST", "/api/tasks/preview"),
];

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
    TaskRun(&'a str),
    /// `POST /api/tasks/preview` (#633). Only a `POST` is this route: every
    /// other method on the path is `Task("preview")`, as it was before — see
    /// [`route`].
    TaskPreview,
    TaskJobHistory(&'a str),
    JobHistoryList,
    JobHistory(&'a str),
}

fn claims(method: &Method, path: &str) -> bool {
    match *method {
        Method::GET => matches!(
            route(method, path),
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
            route(method, path),
            Some(Route::TaskList)
                | Some(Route::TaskPause(_))
                | Some(Route::TaskResume(_))
                | Some(Route::TaskRun(_))
                | Some(Route::TaskPreview)
        ),
        Method::PUT => matches!(route(method, path), Some(Route::Task(_))),
        Method::DELETE => matches!(
            route(method, path),
            Some(Route::Task(_)) | Some(Route::JobHistoryList) | Some(Route::JobHistory(_))
        ),
        _ => false,
    }
}

/// [`route_of`], with the one method-dependent path resolved: `preview` is the
/// preview route only for a `POST`, and a task id for everything else — so a
/// `GET /api/tasks/preview` is still an unknown task, exactly as it was before
/// the route existed. (A real id is a UUID, so it can never be `preview`.)
fn route<'a>(method: &Method, path: &'a str) -> Option<Route<'a>> {
    match route_of(path) {
        Some(Route::TaskPreview) if *method != Method::POST => Some(Route::Task("preview")),
        other => other,
    }
}

/// Match this module's paths and nothing else.
///
/// The ids are single segments, so `/api/tasks/{id}/pause` and `/resume` cannot
/// be swallowed by the `/api/tasks/{id}` arm, and an empty id is not a match
/// because chi routes `/api/tasks/` to nothing. The four suffixed forms are
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
        if let Some(id) = rest.strip_suffix("/run") {
            return segment(id).map(Route::TaskRun);
        }
        if rest == "preview" {
            return Some(Route::TaskPreview);
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
    match (req.method.clone(), route(req.method, req.path)) {
        (Method::DELETE, Some(Route::JobHistory(id))) => finish(delete_job_history(db, id)),
        (Method::DELETE, Some(Route::JobHistoryList)) => {
            finish(bulk_delete_job_history(db, req.body))
        }
        (Method::DELETE, Some(Route::Task(id))) => finish(delete_task(db, id)),
        (Method::POST, Some(Route::TaskList)) => finish(create_task(db, req.body)),
        (Method::POST, Some(Route::TaskPause(id))) => finish(pause_task(db, id)),
        (Method::POST, Some(Route::TaskResume(id))) => finish(resume_task(db, id)),
        (Method::POST, Some(Route::TaskRun(id))) => finish(run_task_now(id)),
        (Method::POST, Some(Route::TaskPreview)) => finish(preview_task(db, req.body)),
        (Method::PUT, Some(Route::Task(id))) => finish(update_task(db, id, req.body)),
        (Method::GET, _) => serve_read(ctx, req),
        _ => Err(format!("{} {} is not ported", req.method, req.path)),
    }
}

fn serve_read(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let db = &ctx.db_path;
    let body = match route(req.method, req.path) {
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

        // The four POST-only paths reach `serve_read` from nowhere — `serve`
        // routes them by method first — so this arm is the same "not a read"
        // answer the `None` arm gives.
        Some(Route::TaskPause(_))
        | Some(Route::TaskResume(_))
        | Some(Route::TaskRun(_))
        | Some(Route::TaskPreview)
        | None => return Err(format!("{} is not a task read", req.path)),
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
            save_output         INTEGER NOT NULL DEFAULT 0,
            destinations        TEXT NOT NULL DEFAULT '[]'
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
        );
        CREATE TABLE job_deliveries (
            id          TEXT PRIMARY KEY,
            job_id      TEXT NOT NULL REFERENCES job_history(id) ON DELETE CASCADE,
            position    INTEGER NOT NULL,
            type        TEXT NOT NULL,
            target      TEXT NOT NULL DEFAULT '',
            status      TEXT NOT NULL,
            error       TEXT NOT NULL DEFAULT '',
            created_at  DATETIME NOT NULL,
            finished_at DATETIME
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

    // ─── Delivery results (#635) ──────────────────────────────────────────────

    fn delivery(id: &str, job_id: &str, position: i64, created_at: &str) -> JobDelivery {
        JobDelivery {
            id: id.to_string(),
            job_id: job_id.to_string(),
            position,
            r#type: "slack".to_string(),
            target: "Acme Slack · C0123ABCD".to_string(),
            status: DELIVERY_PENDING.to_string(),
            error: String::new(),
            created_at: GoTime::parse_go_string(created_at).expect("time"),
            finished_at: None,
        }
    }

    fn delivery_row(path: &Path, id: &str) -> (String, String, bool) {
        let conn = rusqlite::Connection::open(path).expect("open");
        conn.query_row(
            "SELECT status, error, finished_at IS NOT NULL FROM job_deliveries WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("delivery row")
    }

    fn delivery_count(path: &Path) -> i64 {
        let conn = rusqlite::Connection::open(path).expect("open");
        conn.query_row("SELECT COUNT(*) FROM job_deliveries", [], |r| r.get(0))
            .expect("count")
    }

    /// The pre-#635 bytes, on all three reads: no `deliveries` key at all, not
    /// `[]` and not `null`. The exact bytes are pinned by
    /// `job_history_is_newest_first_and_omits_an_unfinished_run`.
    #[test]
    fn a_job_with_no_deliveries_ships_no_deliveries_key_on_any_read() {
        let file = fixture();
        let mut reads = list_all_job_history(file.path(), 50, 0).expect("all");
        reads.extend(list_task_job_history(file.path(), "full", 50).expect("task"));
        reads.push(
            get_job_history(file.path(), "job-old")
                .expect("get")
                .expect("job"),
        );
        assert_eq!(reads.len(), 5);
        for job in &reads {
            assert!(!encoded(job).contains("deliveries"), "{}", encoded(job));
        }
    }

    #[test]
    fn deliveries_ship_last_in_position_order_on_all_three_reads() {
        let file = fixture();
        // Inserted out of order: position decides, then created_at.
        let mut sent = delivery("d-sent", "job-old", 1, "2026-08-14 02:04:33 +0000 UTC");
        sent.status = DELIVERY_SENT.to_string();
        sent.finished_at =
            Some(GoTime::parse_go_string("2026-08-14 02:04:34.25 +0000 UTC").unwrap());
        insert_pending_delivery(file.path(), &sent).expect("insert");
        insert_pending_delivery(
            file.path(),
            &delivery("d-pending", "job-old", 0, "2026-08-14 02:04:32 +0000 UTC"),
        )
        .expect("insert");
        let mut failed = delivery("d-failed", "job-old", 0, "2026-08-14 02:04:31.9 +0000 UTC");
        failed.status = DELIVERY_FAILED.to_string();
        failed.error = "channel_not_found <C0>".to_string();
        failed.finished_at =
            Some(GoTime::parse_go_string("2026-08-14 02:04:35 +0000 UTC").unwrap());
        insert_pending_delivery(file.path(), &failed).expect("insert");

        let want = r#""response_text":"done \u0026 dusted","deliveries":[{"id":"d-failed","type":"slack","target":"Acme Slack · C0123ABCD","status":"failed","error":"channel_not_found \u003cC0\u003e","created_at":"2026-08-14T02:04:31.9Z","finished_at":"2026-08-14T02:04:35Z"},{"id":"d-pending","type":"slack","target":"Acme Slack · C0123ABCD","status":"pending","error":"","created_at":"2026-08-14T02:04:32Z"},{"id":"d-sent","type":"slack","target":"Acme Slack · C0123ABCD","status":"sent","error":"","created_at":"2026-08-14T02:04:33Z","finished_at":"2026-08-14T02:04:34.25Z"}]}"#;
        let all = list_all_job_history(file.path(), 50, 0).expect("all");
        let task = list_task_job_history(file.path(), "full", 50).expect("task");
        let one = get_job_history(file.path(), "job-old")
            .expect("get")
            .expect("job");
        for job in [&all[1], &task[1], &one] {
            assert!(encoded(job).ends_with(want), "{}", encoded(job));
        }
        // The other job on the same page is untouched.
        assert!(!encoded(&all[0]).contains("deliveries"));
    }

    /// One lookup for a whole 50-row page, not one per job.
    #[test]
    fn a_page_of_jobs_loads_its_deliveries_in_one_query() {
        let file = migrated_with_history();
        {
            let conn = rusqlite::Connection::open(file.path()).expect("open");
            for i in 0..50 {
                conn.execute(
                    "INSERT INTO job_history (id, task_id, task_name, started_at)
                     VALUES (?1, 't1', 'T', '2026-02-01 00:00:00 +0000 UTC')",
                    [format!("p{i:02}")],
                )
                .expect("job");
            }
        }
        for i in 0..50 {
            insert_pending_delivery(
                file.path(),
                &delivery(
                    &format!("d{i:02}"),
                    &format!("p{i:02}"),
                    0,
                    "2026-02-01 00:00:01 +0000 UTC",
                ),
            )
            .expect("delivery");
        }

        let conn = db::open_read_only(file.path()).expect("open");
        let sql = format!("{JOB_COLUMNS}\nWHERE id LIKE 'p%'\nORDER BY id");
        let mut jobs: Vec<JobHistory> = conn
            .prepare(&sql)
            .expect("prepare")
            .query_map([], scan_job)
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(jobs.len(), 50);
        // Each call is one statement: the whole page fits one chunk.
        let mut lookups = Vec::new();
        attach_deliveries_with(&mut jobs, |ids| {
            lookups.push(ids.len());
            list_deliveries_for_jobs(&conn, ids)
        })
        .expect("attach");
        assert_eq!(
            lookups,
            vec![50],
            "one lookup for the page, not one per job"
        );
        for (i, job) in jobs.iter().enumerate() {
            assert_eq!(job.deliveries.len(), 1, "{}", job.id);
            assert_eq!(job.deliveries[0].id, format!("d{i:02}"));
        }
    }

    fn migrated_with_deliveries() -> tempfile::NamedTempFile {
        let file = migrated_with_history();
        for (id, job) in [("d1", "j1"), ("d2", "j2"), ("d3", "j3")] {
            insert_pending_delivery(
                file.path(),
                &delivery(id, job, 0, "2026-01-01 00:00:01 +0000 UTC"),
            )
            .expect("delivery");
        }
        file
    }

    #[test]
    fn deleting_a_job_or_its_task_takes_its_deliveries_with_it() {
        let file = migrated_with_deliveries();
        // Not `j1`: `the_job_history_deletes_log_their_entity_and_outcome`
        // asserts that id's delete line appears exactly once in the shared log.
        delete_job_history(file.path(), "j3").expect("delete");
        assert_eq!(delivery_count(file.path()), 2);
        bulk_delete_job_history(file.path(), br#"{"ids":["j2"]}"#).expect("bulk");
        assert_eq!(delivery_count(file.path()), 1);
        delete_task(file.path(), "t1").expect("delete task");
        assert_eq!(delivery_count(file.path()), 0);
    }

    /// A failed delivery is not a failed run: the #556 tools notice on a
    /// `success` row survives it untouched.
    #[test]
    fn a_failed_delivery_leaves_the_runs_status_and_error_alone() {
        let file = migrated_with_deliveries();
        {
            let conn = rusqlite::Connection::open(file.path()).expect("open");
            conn.execute(
                "UPDATE job_history SET status = 'success', error_message = 'tools notice' WHERE id = 'j1'",
                [],
            )
            .expect("seed");
        }
        assert!(
            finish_delivery(file.path(), "d1", DELIVERY_FAILED, "not_in_channel").expect("finish")
        );
        let job = get_job_history(file.path(), "j1")
            .expect("get")
            .expect("job");
        assert_eq!(
            (job.status.as_str(), job.error_message.as_str()),
            ("success", "tools notice")
        );
        assert_eq!(job.deliveries[0].status, "failed");
        assert_eq!(job.deliveries[0].error, "not_in_channel");
        assert!(job.deliveries[0].finished_at.is_some());
    }

    #[test]
    fn a_delivery_finishes_once_and_a_reaped_one_cannot_be_resurrected() {
        let file = migrated_with_deliveries();
        assert!(finish_delivery(file.path(), "d1", DELIVERY_SENT, "").expect("finish"));
        assert!(!finish_delivery(file.path(), "d1", DELIVERY_FAILED, "late").expect("again"));
        assert_eq!(
            delivery_row(file.path(), "d1"),
            ("sent".to_string(), String::new(), true)
        );

        let booted = GoTime::parse_go_string("2026-06-01 00:00:00 +0000 UTC").unwrap();
        assert_eq!(
            reap_pending_deliveries(file.path(), booted, DELIVERY_INTERRUPTED).expect("reap"),
            2
        );
        assert!(!finish_delivery(file.path(), "d2", DELIVERY_SENT, "").expect("late finish"));
        assert_eq!(
            delivery_row(file.path(), "d2"),
            ("failed".to_string(), DELIVERY_INTERRUPTED.to_string(), true)
        );
        assert_eq!(
            delivery_row(file.path(), "d1").0,
            "sent",
            "a finished row is not reaped"
        );
    }

    #[test]
    fn the_reap_spares_deliveries_created_at_or_after_boot() {
        let file = migrated_with_history();
        let booted = "2026-01-01 00:00:01 +0000 UTC";
        for (id, created) in [
            ("before", "2026-01-01 00:00:00.999 +0000 UTC"),
            ("at", booted),
            ("after", "2026-01-01 00:00:01.5 +0000 UTC"),
        ] {
            insert_pending_delivery(file.path(), &delivery(id, "j1", 0, created)).expect("insert");
        }
        let reaped = reap_pending_deliveries(
            file.path(),
            GoTime::parse_go_string(booted).unwrap(),
            DELIVERY_INTERRUPTED,
        )
        .expect("reap");
        assert_eq!(reaped, 1);
        assert_eq!(delivery_row(file.path(), "before").0, "failed");
        assert_eq!(
            delivery_row(file.path(), "at"),
            ("pending".to_string(), String::new(), false)
        );
        assert_eq!(delivery_row(file.path(), "after").0, "pending");
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

    /// The inverse of `the_five_columns_the_request_cannot_reach_are_stored_at_
    /// their_zero_values`, kept in that shape rather than deleted (#540) — the
    /// way `a_full_length_positional_array` was inverted — so a later session
    /// reading "Go discards these" cannot restore the drop without a red test.
    #[test]
    fn the_five_columns_go_never_carried_now_reach_the_row() {
        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"N","prompt":"p","working_directory":"/tmp","model":"opus",
                "settings_profile_id":"prof","stop_after_count":9,
                "stop_after_time":"2027-06-01T12:00:00Z"}"#,
        );
        assert_eq!(task.working_directory, "/tmp");
        assert_eq!(task.model, "opus");
        assert_eq!(task.settings_profile_id, "prof");
        assert_eq!(task.stop_after_count, 9);
        assert_eq!(
            task.stop_after_time.map(|t| t.rfc3339_nano_utc()),
            Some("2027-06-01T12:00:00Z".to_string()),
        );
    }

    /// The whole hop the report was about: create with all five set, read them
    /// back off the *stored row*, then `PUT` a change to each and re-read.
    #[test]
    fn the_five_fields_round_trip_through_create_read_and_update() {
        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"N","prompt":"p","working_directory":"/w/one","model":"sonnet",
                "settings_profile_id":"prof-a","stop_after_count":3,
                "stop_after_time":"2027-06-01T12:00:00Z"}"#,
        );

        update_task(
            file.path(),
            &task.id,
            br#"{"name":"N","prompt":"p","working_directory":"/w/two","model":"opus",
                 "settings_profile_id":"prof-b","stop_after_count":7,
                 "stop_after_time":"2028-01-02T03:04:05Z"}"#,
        )
        .expect("update");

        let stored = get_task(file.path(), &task.id)
            .expect("read")
            .expect("task");
        assert_eq!(stored.working_directory, "/w/two");
        assert_eq!(stored.model, "opus");
        assert_eq!(stored.settings_profile_id, "prof-b");
        assert_eq!(stored.stop_after_count, 7);
        assert_eq!(
            stored.stop_after_time.map(|t| t.rfc3339_nano_utc()),
            Some("2028-01-02T03:04:05Z".to_string()),
        );
    }

    /// `stop_after_time` is the one genuinely nullable field of the five, so
    /// absent and an explicit `null` must agree — and a value must come back
    /// spelled exactly as it went in, since the read path is what the `full`
    /// golden pins.
    #[test]
    fn stop_after_time_is_none_when_absent_or_null_and_byte_identical_otherwise() {
        for body in [
            r#"{"name":"N","prompt":"p"}"#,
            r#"{"name":"N","prompt":"p","stop_after_time":null}"#,
        ] {
            let file = migrated();
            assert!(created(&file, body).stop_after_time.is_none(), "for {body}");
        }

        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"N","prompt":"p","stop_after_time":"2027-06-01T12:00:00Z"}"#,
        );
        let encoded =
            String::from_utf8(crate::native::gojson::to_vec(&task).expect("encode")).expect("utf8");
        assert!(
            encoded.contains(r#""stop_after_time":"2027-06-01T12:00:00Z""#),
            "re-emitted as it arrived, in {encoded}"
        );
    }

    /// An unparsable instant is a decode failure, not a silently dropped field
    /// — the same 400 any other mistyped value on this body answers.
    #[test]
    fn an_unparsable_stop_after_time_is_a_400_rather_than_a_silent_none() {
        let file = migrated();
        let err = create_task(
            file.path(),
            br#"{"name":"N","prompt":"p","stop_after_time":"the first of June"}"#,
        )
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert!(
            list_tasks(file.path()).expect("list").is_empty(),
            "a rejected create stores nothing"
        );
    }

    /// The documented `PUT` semantics: replace, not preserve. An omitted key
    /// resets its column, exactly as it does for `description` or `agent_slug`.
    #[test]
    fn an_omitted_field_on_update_resets_it_rather_than_preserving_it() {
        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"N","prompt":"p","working_directory":"/w","model":"opus",
                "settings_profile_id":"prof","stop_after_count":9,
                "stop_after_time":"2027-06-01T12:00:00Z"}"#,
        );

        update_task(file.path(), &task.id, br#"{"name":"N","prompt":"p"}"#).expect("update");

        let stored = get_task(file.path(), &task.id)
            .expect("read")
            .expect("task");
        assert!(stored.working_directory.is_empty());
        assert!(stored.model.is_empty());
        assert!(stored.settings_profile_id.is_empty());
        assert_eq!(stored.stop_after_count, 0);
        assert!(stored.stop_after_time.is_none());
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

    // ─── Delivery destinations (#634) ───────────────────────────────────────

    /// A migrated database with one Slack and one Telegram integration.
    fn migrated_with_integrations() -> tempfile::NamedTempFile {
        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "INSERT INTO integrations (id, name, type, enabled, created_at, updated_at) VALUES
                ('slack-1', 'Team Slack', 'slack', 1,
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC'),
                ('tg-1', 'Bot', 'telegram', 1,
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC');",
        )
        .expect("seed integrations");
        file
    }

    const ONE_SLACK_DESTINATION: &str = r#"{"name":"N","prompt":"p",
        "destinations":[{"type":"slack","slack":{"integration_id":"slack-1",
            "channel_ids":["C0123ABCD","C0456EFGH"]}}]}"#;

    fn stored_destinations(file: &tempfile::NamedTempFile, id: &str) -> String {
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.query_row(
            "SELECT destinations FROM scheduled_tasks WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .expect("read destinations")
    }

    #[test]
    fn a_slack_destination_round_trips_byte_identically_in_wire_order() {
        let file = migrated_with_integrations();
        let answer = create_task(file.path(), ONE_SLACK_DESTINATION.as_bytes()).expect("create");
        let body = answer.body.expect("a body");
        let created: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let id = created["id"].as_str().expect("id").to_string();

        // Key order `type, when, slack` then `integration_id, channel_ids`, the
        // list between `save_output` and `status`, and an omitted `when`
        // defaulted to `success`.
        let want = r#""save_output":false,"destinations":[{"type":"slack","when":"success","slack":{"integration_id":"slack-1","channel_ids":["C0123ABCD","C0456EFGH"]}}],"status":"active","#;
        let one = encoded(&get_task(file.path(), &id).expect("get").expect("task"));
        assert!(one.contains(want), "{one}");
        let listed = encoded(&list_tasks(file.path()).expect("list")[0]);
        assert_eq!(one, listed, "the list and the single read agree");
        assert_eq!(
            String::from_utf8(body).expect("utf-8").trim_end(),
            one,
            "the create answers the bytes a read returns"
        );
        assert_eq!(
            stored_destinations(&file, &id),
            r#"[{"type":"slack","when":"success","slack":{"integration_id":"slack-1","channel_ids":["C0123ABCD","C0456EFGH"]}}]"#
        );
    }

    #[test]
    fn a_task_without_destinations_ships_no_key_and_stores_an_empty_array() {
        let file = migrated();
        let task = created(&file, r#"{"name":"N","prompt":"p"}"#);
        let bytes = encoded(&get_task(file.path(), &task.id).expect("get").expect("task"));
        assert!(!bytes.contains("destinations"), "{bytes}");
        assert!(
            bytes.contains(r#""save_output":false,"status":"active","#),
            "{bytes}"
        );
        assert_eq!(stored_destinations(&file, &task.id), "[]");
    }

    #[test]
    fn a_put_with_destinations_omitted_null_or_empty_clears_them() {
        let file = migrated_with_integrations();
        for body in [
            r#"{"name":"N","prompt":"p"}"#,
            r#"{"name":"N","prompt":"p","destinations":null}"#,
            r#"{"name":"N","prompt":"p","destinations":[]}"#,
        ] {
            let task = created(&file, ONE_SLACK_DESTINATION);
            assert_eq!(task.destinations.len(), 1);
            update_task(file.path(), &task.id, body.as_bytes()).expect("update");
            let stored = get_task(file.path(), &task.id).expect("get").expect("task");
            assert!(stored.destinations.is_empty(), "for {body}");
            assert_eq!(stored_destinations(&file, &task.id), "[]", "for {body}");
        }
    }

    #[test]
    fn every_destination_rule_is_a_422_and_writes_nothing() {
        let file = migrated_with_integrations();
        let existing = created(&file, ONE_SLACK_DESTINATION);
        let before = encoded(
            &get_task(file.path(), &existing.id)
                .expect("get")
                .expect("task"),
        );

        let slack = |slack: &str| {
            format!(
                r#"{{"name":"N","prompt":"p","destinations":[{{"type":"slack","slack":{slack}}}]}}"#
            )
        };
        let cases = [
            (
                r#"{"name":"N","prompt":"p","destinations":[{"type":"email"}]}"#.to_string(),
                r#"validation error for "destinations[0].type": type must be slack"#,
            ),
            (
                r#"{"name":"N","prompt":"p","destinations":[{"type":"slack","when":"sometimes","slack":{"integration_id":"slack-1","channel_ids":["C0123ABCD"]}}]}"#.to_string(),
                r#"validation error for "destinations[0].when": when must be success or always"#,
            ),
            (
                r#"{"name":"N","prompt":"p","destinations":[{"type":"slack"}]}"#.to_string(),
                r#"validation error for "destinations[0].slack": slack is required for slack destinations"#,
            ),
            (
                slack(r#"{"channel_ids":["C0123ABCD"]}"#),
                r#"validation error for "destinations[0].slack.integration_id": integration_id is required for slack destinations"#,
            ),
            (
                slack(r#"{"integration_id":"slack-1","channel_ids":[]}"#),
                r#"validation error for "destinations[0].slack.channel_ids": channel_ids is required for slack destinations"#,
            ),
            (
                slack(r##"{"integration_id":"slack-1","channel_ids":["#general"]}"##),
                r##"validation error for "destinations[0].slack.channel_ids": channel id "#general" is not a Slack channel id"##,
            ),
            (
                slack(r#"{"integration_id":"slack-1","channel_ids":["c0123abcd"]}"#),
                r#"validation error for "destinations[0].slack.channel_ids": channel id "c0123abcd" is not a Slack channel id"#,
            ),
            (
                slack(r#"{"integration_id":"slack-1","channel_ids":["C0123"]}"#),
                r#"validation error for "destinations[0].slack.channel_ids": channel id "C0123" is not a Slack channel id"#,
            ),
            (
                slack(r#"{"integration_id":"slack-1","channel_ids":["C0123ABCD","C0123ABCD"]}"#),
                r#"validation error for "destinations[0].slack.channel_ids": channel id "C0123ABCD" is listed more than once"#,
            ),
            (
                slack(r#"{"integration_id":"tg-1","channel_ids":["C0123ABCD"]}"#),
                r#"validation error for "destinations[0].slack.integration_id": integration_id is not a Slack integration"#,
            ),
            (
                slack(r#"{"integration_id":"gone","channel_ids":["C0123ABCD"]}"#),
                r#"validation error for "destinations[0].slack.integration_id": integration_id does not name an integration"#,
            ),
        ];
        for (body, want) in &cases {
            let err = create_task(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(err.message(), *want, "create {body}");
            assert_eq!(
                err.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "create {body}"
            );

            let err = update_task(file.path(), &existing.id, body.as_bytes()).unwrap_err();
            assert_eq!(err.message(), *want, "update {body}");
            assert_eq!(
                err.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "update {body}"
            );
        }
        assert_eq!(
            list_tasks(file.path()).expect("list").len(),
            1,
            "no row written"
        );
        let after = encoded(
            &get_task(file.path(), &existing.id)
                .expect("get")
                .expect("task"),
        );
        assert_eq!(before, after, "no row changed");
    }

    #[test]
    fn the_index_in_a_field_path_names_the_offending_entry() {
        let file = migrated_with_integrations();
        let err = create_task(
            file.path(),
            br#"{"name":"N","prompt":"p","destinations":[
                {"type":"slack","when":"always","slack":{"integration_id":"slack-1","channel_ids":["G0123ABCD"]}},
                {"type":"slack","slack":{"integration_id":"slack-1","channel_ids":["D01"]}}]}"#,
        )
        .unwrap_err();
        assert_eq!(
            err.message(),
            r#"validation error for "destinations[1].slack.channel_ids": channel id "D01" is not a Slack channel id"#
        );
    }

    #[test]
    fn a_deleted_integration_is_grandfathered_on_put_and_refused_on_post() {
        let file = migrated_with_integrations();
        let task = created(&file, ONE_SLACK_DESTINATION);
        {
            let conn = rusqlite::Connection::open(file.path()).expect("open");
            conn.execute("DELETE FROM integrations WHERE id = 'slack-1'", [])
                .expect("delete integration");
        }

        // The form posts the whole task back: the stale entry must not make the
        // task uneditable.
        update_task(
            file.path(),
            &task.id,
            ONE_SLACK_DESTINATION
                .replace(r#""prompt":"p""#, r#""prompt":"edited""#)
                .as_bytes(),
        )
        .expect("an edit keeping the stored destination is accepted");
        let stored = get_task(file.path(), &task.id).expect("get").expect("task");
        assert_eq!(stored.prompt, "edited");
        assert_eq!(stored.destinations, task.destinations);

        // …but the same id is new to a create, and to a task that never had it.
        let err = create_task(file.path(), ONE_SLACK_DESTINATION.as_bytes()).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let other = created(&file, r#"{"name":"O","prompt":"p"}"#);
        let err =
            update_task(file.path(), &other.id, ONE_SLACK_DESTINATION.as_bytes()).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(get_task(file.path(), &other.id)
            .expect("get")
            .expect("task")
            .destinations
            .is_empty());
    }

    #[test]
    fn destinations_decode_by_the_go_list_and_struct_rules() {
        let file = migrated_with_integrations();
        // A `null` element is the zero entry, which validation then refuses.
        let err = create_task(
            file.path(),
            br#"{"name":"N","prompt":"p","destinations":[null]}"#,
        )
        .unwrap_err();
        assert_eq!(
            err.message(),
            r#"validation error for "destinations[0].type": type must be slack"#
        );
        // Wrong container shapes are a 400, not a zero value.
        for body in [
            r#"{"name":"N","prompt":"p","destinations":{}}"#,
            r#"{"name":"N","prompt":"p","destinations":[[]]}"#,
            r#"{"name":"N","prompt":"p","destinations":[{"type":"slack","slack":[]}]}"#,
        ] {
            let err = create_task(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "for {body}");
        }
        assert!(list_tasks(file.path()).expect("list").is_empty());
        // Null scalars inside an entry are zero values, not type errors.
        let task = created(
            &file,
            r#"{"name":"N","prompt":"p","destinations":[{"type":"slack","when":null,
                "slack":{"integration_id":"slack-1","channel_ids":["C0123ABCD"]}}]}"#,
        );
        assert_eq!(task.destinations[0].when, "success");
    }

    #[test]
    fn a_stored_empty_or_null_destinations_value_is_an_empty_list() {
        let file = migrated();
        let task = created(&file, r#"{"name":"N","prompt":"p"}"#);
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        for stored in ["", "null"] {
            conn.execute(
                "UPDATE scheduled_tasks SET destinations = ?1 WHERE id = ?2",
                [stored, task.id.as_str()],
            )
            .expect("seed");
            let read = get_task(file.path(), &task.id).expect("get").expect("task");
            assert!(read.destinations.is_empty(), "for {stored:?}");
        }
        conn.execute(
            "UPDATE scheduled_tasks SET destinations = 'not json' WHERE id = ?1",
            [&task.id],
        )
        .expect("seed");
        assert!(
            get_task(file.path(), &task.id).is_err(),
            "an unparsable list fails the read"
        );
    }

    #[test]
    fn pause_and_resume_leave_destinations_intact() {
        let file = migrated_with_integrations();
        let task = created(&file, ONE_SLACK_DESTINATION);
        pause_task(file.path(), &task.id).expect("pause");
        let paused = get_task(file.path(), &task.id).expect("get").expect("task");
        assert_eq!(paused.destinations, task.destinations);
        resume_task(file.path(), &task.id).expect("resume");
        let resumed = get_task(file.path(), &task.id).expect("get").expect("task");
        assert_eq!(resumed.destinations, task.destinations);
    }

    /// #330. Before this check the expression was inspected only at *schedule*
    /// time, after the row was committed — so every one of these answered 201
    /// and left an `active` task with no timer and no `next_run_at`, which is
    /// indistinguishable from a task that was simply never due.
    #[test]
    fn an_unusable_cron_expression_is_refused_at_save_time() {
        let file = migrated();
        let unparseable = r#"validation error for "schedule_config.expression": expression is not a valid cron schedule"#;
        let cases = [
            // A timezone prefix with nothing after it — the shape the issue is
            // named for, and the one a user reaches by pasting the first line
            // of a crontab example and forgetting the schedule.
            ("CRON_TZ=UTC", unparseable),
            ("TZ=", unparseable),
            ("CRON_TZ=", unparseable),
            ("TZ=Europe/Berlin", unparseable),
            // Not a prefix problem at all: too few fields.
            ("0 2 * *", unparseable),
            // Parses, and then never fires: February has no 30th, so the
            // five-year search inside `setup` comes back empty. A *different*
            // mistake from a typo, and it says so.
            (
                "0 0 30 2 *",
                r#"validation error for "schedule_config.expression": expression is a valid cron schedule but will never fire"#,
            ),
        ];
        for (expr, want) in cases {
            let body = format!(
                r#"{{"name":"n","prompt":"p","schedule_type":"cron",
                    "schedule_config":{{"expression":{}}}}}"#,
                serde_json::to_string(expr).expect("encode")
            );
            let err = create_task(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(err.message(), want, "for {expr:?}");
            assert_eq!(
                err.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "for {expr:?}"
            );
        }
        assert!(
            list_tasks(file.path()).expect("list").is_empty(),
            "a refused cron expression stores no row"
        );
    }

    /// The other half of the same check, and the one that matters more: the
    /// validator delegates to the scheduler's own `setup`, so anything the
    /// scheduler would have run has to survive the write path untouched.
    #[test]
    fn every_cron_expression_the_scheduler_accepts_still_saves() {
        for expr in [
            "@daily",
            "@hourly",
            "@every 1h30m",
            "CRON_TZ=Local 0 9 * * *",
            "CRON_TZ=Europe/Berlin 0 9 * * *",
            "0 2 * * *",
            "*/30 * * * *",
            "0 0 29 2 *", // a leap day: rare, but it does fire
        ] {
            let file = migrated();
            let body = format!(
                r#"{{"name":"n","prompt":"p","schedule_type":"cron",
                    "schedule_config":{{"expression":{}}}}}"#,
                serde_json::to_string(expr).expect("encode")
            );
            let task = created(&file, &body);
            assert_eq!(task.schedule_config.expression, expr);
            assert_eq!(task.status, "active");
        }
    }

    /// `update_task` shares `validate_task` but has its own handler, so a test
    /// that only exercised create would pass against half a fix — and here the
    /// stakes are higher, because a refusal that leaked through would overwrite
    /// a *working* schedule with one that never fires.
    #[test]
    fn the_update_path_refuses_an_unusable_cron_expression_and_keeps_the_stored_row() {
        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"n","prompt":"p","schedule_type":"cron",
                "schedule_config":{"expression":"0 2 * * *"}}"#,
        );

        let err = update_task(
            file.path(),
            &task.id,
            br#"{"name":"renamed","prompt":"p","schedule_type":"cron",
                 "schedule_config":{"expression":"CRON_TZ=UTC"}}"#,
        )
        .unwrap_err();
        assert_eq!(
            err.message(),
            r#"validation error for "schedule_config.expression": expression is not a valid cron schedule"#
        );
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // The refusal happens inside the transaction, before `update_task_in`,
        // so not one column moved — including the name the same body renamed.
        let stored = get_task(file.path(), &task.id)
            .expect("read")
            .expect("still there");
        assert_eq!(stored.name, "n");
        assert_eq!(stored.schedule_config.expression, "0 2 * * *");
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
        assert!(claims(&Method::POST, "/api/tasks/t1/run"));

        // Mounted paths this module must *not* answer for the wrong method —
        // chi would 405, and claiming one would turn that into a native error.
        assert!(!claims(&Method::DELETE, "/api/tasks/t1/job-history"));
        assert!(!claims(&Method::PUT, "/api/tasks"));
        assert!(!claims(&Method::POST, "/api/tasks/t1"));
        assert!(!claims(&Method::POST, "/api/job-history"));
        assert!(!claims(&Method::PUT, "/api/tasks/t1/pause"));
        assert!(!claims(&Method::GET, "/api/tasks/t1/run"));
        assert!(!claims(&Method::DELETE, "/api/tasks/t1/run"));
        assert!(!claims(&Method::PATCH, "/api/tasks/t1"));
    }

    /// #541. Driven through [`start_manual_run`] rather than [`run_task_now`],
    /// because `running()` reads the process-wide `OnceLock` a test may not
    /// install; `detached` is the same `Scheduler` without it.
    #[tokio::test]
    async fn a_manual_run_is_accepted_for_any_task_and_answers_with_its_job_id() {
        crate::native::writes::testlog::install();
        let file = migrated();
        // An agent that resolves to nothing, so the spawned run fails at
        // `resolve_agent` and never reaches a subprocess. What is under test is
        // the route's answer, not the run.
        let task = created(
            &file,
            r#"{"name":"n","prompt":"p","agent_slug":"no-such-agent"}"#,
        );
        // **Paused**, which is most of the point: the one control the product
        // had made a task un-runnable, and this route has to ignore it.
        pause_task(file.path(), &task.id).expect("pause");

        let scheduler = crate::native::schedule::runtime::detached(file.path());
        let answer = start_manual_run(&scheduler, &task.id).expect("run");
        assert_eq!(answer.status, StatusCode::ACCEPTED);

        let body: serde_json::Value =
            serde_json::from_slice(&answer.body.expect("body")).expect("json");
        assert_eq!(body["task_id"], serde_json::json!(task.id));
        let job_id = body["job_id"].as_str().expect("job_id").to_string();
        assert!(!job_id.is_empty());

        crate::native::writes::testlog::assert_info_once(&format!(
            r#"task run started id={:?} job_id={job_id:?}"#,
            task.id
        ));
    }

    #[tokio::test]
    async fn a_manual_run_of_an_unknown_task_is_404() {
        let file = migrated();
        let scheduler = crate::native::schedule::runtime::detached(file.path());
        let err = start_manual_run(&scheduler, "nope").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), r#"task "nope" not found"#);
    }

    /// The 409, driven off the in-flight map directly: holding the guard is
    /// exactly the state a run in flight leaves behind, and it is the only way
    /// to assert the refusal without racing a background run to finish.
    #[tokio::test]
    async fn a_second_manual_run_while_one_is_in_flight_is_409() {
        let file = migrated();
        let task = created(
            &file,
            r#"{"name":"n","prompt":"p","agent_slug":"no-such-agent"}"#,
        );
        let scheduler = crate::native::schedule::runtime::detached(file.path());

        let guard = scheduler.mark_running(&task.id);
        let err = start_manual_run(&scheduler, &task.id).unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.message(), "a run of this task is already in progress");
        // A *different* task is unaffected — the map is per task, not a lock on
        // the scheduler.
        let other = created(
            &file,
            r#"{"name":"o","prompt":"p","agent_slug":"no-such-agent"}"#,
        );
        assert_eq!(
            start_manual_run(&scheduler, &other.id).expect("run").status,
            StatusCode::ACCEPTED
        );

        // And the refusal lifts when the run ends.
        drop(guard);
        assert!(!scheduler.is_running(&task.id));
        assert_eq!(
            start_manual_run(&scheduler, &task.id).expect("run").status,
            StatusCode::ACCEPTED
        );
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

    // ─── The preview (#633) ───────────────────────────────────────────────────

    fn berlin() -> chrono_tz::Tz {
        "Europe/Berlin".parse().expect("zone")
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .expect("literal parses")
            .with_timezone(&chrono::Utc)
    }

    fn draft(body: &str) -> ScheduledTask {
        let mut task = decode_body::<TaskRequest>(body.as_bytes())
            .expect("decode")
            .into_task();
        if task.schedule_type.is_empty() {
            task.schedule_type = "run_immediately".to_string();
        }
        task
    }

    /// What the scheduler itself computes for the same draft, rendered the way
    /// the preview renders it.
    fn scheduler_says(
        task: &ScheduledTask,
        loc: chrono_tz::Tz,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<String> {
        let outcome = super::super::schedule::fire_times(
            &task.schedule_type,
            &task.schedule_config,
            loc,
            now,
            PREVIEW_RUNS,
        );
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        outcome
            .next_runs
            .into_iter()
            // A zero time ends the list: a one-off has one fire.
            .map_while(|f| f)
            .map(|f| f.rfc3339())
            .collect()
    }

    #[test]
    fn the_preview_route_is_a_post_and_every_other_method_is_still_a_task_id() {
        assert!(claims(&Method::POST, "/api/tasks/preview"));
        assert!(matches!(
            route(&Method::POST, "/api/tasks/preview"),
            Some(Route::TaskPreview)
        ));
        for method in [Method::GET, Method::PUT, Method::DELETE] {
            assert!(
                matches!(
                    route(&method, "/api/tasks/preview"),
                    Some(Route::Task("preview"))
                ),
                "{method}"
            );
        }
        // And a GET for it is the ordinary unknown task.
        let file = migrated();
        assert!(get_task(file.path(), "preview").expect("get").is_none());
        assert!(!claims(&Method::POST, "/api/tasks/preview/x"));
    }

    #[test]
    fn the_preview_lists_exactly_what_the_scheduler_would_fire() {
        let loc = berlin();
        let cases = [
            // Across the 2026-03-29 spring-forward day in Berlin.
            (
                r#"{"schedule_type":"cron","schedule_config":{"expression":"0 9 * * *"}}"#,
                "2026-03-28T10:00:00Z",
            ),
            (
                r#"{"schedule_type":"interval","schedule_config":{"every_days":1,"at_time":"00:00"}}"#,
                "2026-03-28T10:00:00Z",
            ),
            (
                r#"{"schedule_type":"interval","schedule_config":{"every_hours":24}}"#,
                "2026-03-28T10:00:00Z",
            ),
            (
                r#"{"schedule_type":"one_off","schedule_config":{"run_at":"2026-05-01T09:30:00+02:00"}}"#,
                "2026-03-28T10:00:00Z",
            ),
        ];
        for (body, now) in cases {
            let task = draft(body);
            let (runs, error) = preview_runs(&task, loc, at(now));
            assert_eq!(error, "", "{body}");
            assert!(!runs.is_empty(), "{body}");
            assert_eq!(runs, scheduler_says(&task, loc, at(now)), "{body}");
        }

        // The cron case really does cross the transition: 09:00 stays 09:00
        // while the offset moves from +01:00 to +02:00.
        let (runs, _) = preview_runs(
            &draft(r#"{"schedule_type":"cron","schedule_config":{"expression":"0 9 * * *"}}"#),
            loc,
            at("2026-03-28T10:00:00Z"),
        );
        assert_eq!(
            runs,
            [
                "2026-03-29T09:00:00+02:00",
                "2026-03-30T09:00:00+02:00",
                "2026-03-31T09:00:00+02:00"
            ]
        );
    }

    #[test]
    fn the_preview_honours_both_limits() {
        let loc = berlin();
        let now = at("2026-06-01T10:00:00Z");
        let (runs, _) = preview_runs(
            &draft(
                r#"{"schedule_type":"cron","schedule_config":{"expression":"0 9 * * *"},
                    "stop_after_time":"2026-06-03T06:00:00Z"}"#,
            ),
            loc,
            now,
        );
        // 06:00Z is 08:00 in Berlin, an hour before the second fire.
        assert_eq!(runs, ["2026-06-02T09:00:00+02:00"]);

        let (runs, _) = preview_runs(
            &draft(
                r#"{"schedule_type":"cron","schedule_config":{"expression":"0 9 * * *"},
                    "stop_after_count":2}"#,
            ),
            loc,
            now,
        );
        assert_eq!(runs.len(), 2);

        // A stop at exactly the fire still runs it: `should_auto_pause` is `>`.
        let (runs, _) = preview_runs(
            &draft(
                r#"{"schedule_type":"cron","schedule_config":{"expression":"0 9 * * *"},
                    "stop_after_time":"2026-06-02T07:00:00Z"}"#,
            ),
            loc,
            now,
        );
        assert_eq!(runs, ["2026-06-02T09:00:00+02:00"]);
    }

    #[test]
    fn run_immediately_previews_its_single_now_plus_two_seconds_fire() {
        let now = at("2026-06-01T10:00:00Z");
        for body in [r#"{"schedule_type":"run_immediately"}"#, "{}"] {
            let (runs, error) = preview_runs(&draft(body), berlin(), now);
            assert_eq!(error, "");
            assert_eq!(runs, ["2026-06-01T12:00:02+02:00"], "{body}");
        }
    }

    #[test]
    fn an_unusable_schedule_is_an_answer_not_a_refusal() {
        let now = at("2026-06-01T10:00:00Z");
        for (body, want) in [
            (
                r#"{"schedule_type":"cron","schedule_config":{"expression":"not cron"}}"#,
                "expression is not a valid cron schedule",
            ),
            (
                r#"{"schedule_type":"cron","schedule_config":{"expression":"0 0 30 2 *"}}"#,
                "expression is a valid cron schedule but will never fire",
            ),
            (
                r#"{"schedule_type":"one_off","schedule_config":{"run_at":"2020-01-01T00:00:00Z"}}"#,
                "The run time is in the past.",
            ),
            (r#"{"schedule_type":"one_off"}"#, "Choose a valid run time."),
            (
                r#"{"schedule_type":"interval"}"#,
                "This schedule will not run.",
            ),
        ] {
            let (runs, error) = preview_runs(&draft(body), berlin(), now);
            assert!(runs.is_empty(), "{body}");
            assert_eq!(error, want, "{body}");
        }

        // Through the handler: 200, not 422, and a draft with no name or prompt.
        let file = migrated();
        let answer = preview_task(
            file.path(),
            br#"{"schedule_type":"cron","schedule_config":{"expression":"nope"}}"#,
        )
        .expect("preview");
        assert_eq!(answer.status, StatusCode::OK);
        assert!(matches!(
            preview_task(file.path(), b"[]"),
            Err(WriteError::InvalidBody)
        ));
    }

    #[test]
    fn the_preview_body_is_in_wire_order_and_writes_nothing() {
        let file = migrated();
        let answer = preview_task(
            file.path(),
            br#"{"schedule_type":"cron","schedule_config":{"expression":"not cron"},
                "model":"opus","working_directory":"/work"}"#,
        )
        .expect("preview");
        assert_eq!(
            String::from_utf8(answer.body.expect("a body")).expect("utf-8"),
            r#"{"next_runs":[],"schedule_error":"expression is not a valid cron schedule","model":"opus","model_source":"task","working_directory":"/work","working_directory_source":"task"}"#.to_string() + "\n"
        );

        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let count = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .expect("count")
        };
        assert_eq!(count("scheduled_tasks"), 0);
        assert_eq!(count("job_history"), 0);
    }

    #[test]
    fn the_preview_resolves_the_model_and_directory_as_the_executor_does() {
        use super::super::chat::runner::TurnSettings;
        use super::super::schedule::executor::effective_execution;

        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "INSERT INTO agents (slug, name, model, capabilities) VALUES ('with', 'With', 'haiku', '{}');
             INSERT INTO agents (slug, name, model, capabilities) VALUES ('without', 'Without', '', '{}');",
        )
        .expect("seed agents");
        let run = |body: &str| effective_execution(file.path(), &draft(body)).expect("resolve");
        let src = |body: &str| {
            let e = run(body);
            (e.model, e.model_source)
        };

        // An agent's model beats the task's own; an agent without one is the
        // CLI's default, not Settings'.
        assert_eq!(
            src(r#"{"agent_slug":"with","model":"opus"}"#),
            ("haiku".to_string(), "agent")
        );
        assert_eq!(
            src(r#"{"agent_slug":"without","model":"opus"}"#),
            (String::new(), "cli_default")
        );
        assert_eq!(
            src(r#"{"agent_slug":"gone"}"#),
            (String::new(), "agent_missing")
        );
        // No agent: the task's, else Settings'.
        assert_eq!(src(r#"{"model":"opus"}"#), ("opus".to_string(), "task"));
        let settings_model = TurnSettings::from_db(file.path()).default_model();
        assert!(!settings_model.is_empty());
        assert_eq!(src("{}"), (settings_model, "settings"));
        // An unreadable database has no Settings to read.
        let nowhere = std::path::Path::new("/nonexistent/agento/definitely-not-a-db");
        let e = effective_execution(nowhere, &draft("{}")).expect("resolve");
        assert_eq!((e.model.as_str(), e.model_source), ("", "cli_default"));

        // The working directory: the task's, else Settings' chain.
        let e = run(r#"{"working_directory":"/srv/job"}"#);
        assert_eq!(
            (e.working_directory.as_str(), e.working_directory_source),
            ("/srv/job", "task")
        );
        let e = run(r#"{"agent_slug":"with"}"#);
        assert_eq!(
            e.working_directory,
            TurnSettings::from_db(file.path()).default_working_dir()
        );
        assert_eq!(e.working_directory_source, "settings");
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

/// The JSON stored in the `destinations` column: `[]` for none, never `null`,
/// so the column keeps the one spelling its `DEFAULT` gave every existing row.
/// `to_vec_marshal` for the same reason as [`marshal_schedule_config`].
fn marshal_destinations(destinations: &[TaskDestination]) -> Result<String, String> {
    let bytes = super::gojson::to_vec_marshal(&destinations)
        .map_err(|e| format!("marshaling destinations: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("marshaling destinations: {e}"))
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
    let destinations = marshal_destinations(&task.destinations)?;
    let affected = conn
        .execute(
            "UPDATE scheduled_tasks SET
                name = ?1, description = ?2, prompt = ?3, agent_slug = ?4,
                working_directory = ?5, model = ?6, settings_profile_id = ?7,
                timeout_minutes = ?8, schedule_type = ?9, schedule_config = ?10,
                stop_after_count = ?11, stop_after_time = ?12, save_output = ?13, status = ?14,
                run_count = ?15, last_run_at = ?16, last_run_status = ?17,
                next_run_at = ?18, updated_at = ?19, destinations = ?20
             WHERE id = ?21",
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
                destinations,
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
    let destinations = marshal_destinations(&task.destinations)?;
    conn.execute(
        "INSERT INTO scheduled_tasks
            (id, name, description, prompt, agent_slug, working_directory, model,
             settings_profile_id, timeout_minutes, schedule_type, schedule_config,
             stop_after_count, stop_after_time, save_output, status, run_count, last_run_at,
             last_run_status, next_run_at, created_at, updated_at, destinations)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22)",
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
            destinations,
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

/// Records the OS process a run spawned on its job row (#594).
///
/// Its own statement rather than a field of [`JobHistory`]: the pid is written
/// once, between the insert and the finish, and the wire type is the API's —
/// neither `GET` answers it, so adding it there would change a frozen response
/// to carry something no reader of the API asked for. Neither the insert nor
/// [`update_job_history`] names these columns, so nothing else overwrites them.
///
/// A zero-row update is not an error, for `update_job_history`'s reason: the
/// initial insert may have failed and been logged, and the run goes on.
pub fn record_job_process(
    db_path: &Path,
    job_id: &str,
    pid: u32,
    started_at: GoTime,
) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;
    conn.execute(
        "UPDATE job_history SET pid = ?1, pid_started_at = ?2 WHERE id = ?3",
        rusqlite::params![
            i64::from(pid),
            super::gotime::to_go_string_utc(started_at),
            job_id
        ],
    )
    .map_err(|e| format!("recording the process of job {job_id:?}: {e}"))?;
    Ok(())
}

/// A `job_history` row still marked `running`, as the startup reaper (#596)
/// needs it: when it started, and the process it recorded, if any.
#[derive(Debug, Clone)]
pub struct RunningJob {
    pub id: String,
    pub task_id: String,
    pub started_at: GoTime,
    /// `None` for a row written before migration 40, or whose run failed
    /// before it spawned — and for a stored value that is no pid at all.
    pub pid: Option<u32>,
    pub pid_started_at: Option<GoTime>,
}

/// Every row still marked `running`, oldest first.
pub fn list_running_jobs(db_path: &Path) -> Result<Vec<RunningJob>, String> {
    let conn = db::open_read_only(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, task_id, started_at, pid, pid_started_at FROM job_history
             WHERE status = 'running' ORDER BY started_at",
        )
        .map_err(|e| format!("listing running jobs: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RunningJob {
                id: row.get(0)?,
                task_id: row.get(1)?,
                started_at: timestamp(row, 2)?,
                pid: row
                    .get::<_, Option<i64>>(3)?
                    .and_then(|pid| u32::try_from(pid).ok()),
                pid_started_at: nullable_timestamp(row, 4)?,
            })
        })
        .map_err(|e| format!("listing running jobs: {e}"))?;
    let mut jobs = Vec::new();
    for row in rows {
        jobs.push(row.map_err(|e| format!("listing running jobs: {e}"))?);
    }
    Ok(jobs)
}

/// Fails a row a previous session left `running`, with `reason` as its error
/// message. Answers whether the row was changed.
///
/// **Only while it is still `running`**: a run that finishes between the
/// reaper's read and this write has written the truth, and it wins. The token
/// totals and response are left as the row holds them — a run that never
/// finished recorded none.
pub fn reap_job_history(
    db_path: &Path,
    id: &str,
    finished_at: GoTime,
    duration_ms: i64,
    reason: &str,
) -> Result<bool, String> {
    let conn = db::open_read_write(db_path)?;
    let changed = conn
        .execute(
            "UPDATE job_history SET status = 'failed', finished_at = ?1, duration_ms = ?2,
                error_message = ?3
             WHERE id = ?4 AND status = 'running'",
            rusqlite::params![
                super::gotime::to_go_string_utc(finished_at),
                duration_ms,
                reason,
                id
            ],
        )
        .map_err(|e| format!("reaping job history {id:?}: {e}"))?;
    Ok(changed > 0)
}

// ─── Delivery results (#635) ──────────────────────────────────────────────────
//
// Written through these three narrow functions and never through
// `update_job_history`, following #594's `record_job_process`: the run's finish
// cannot overwrite a delivery result, and a delivery result cannot touch the
// run's `status` or `error_message`.

/// Records a delivery as dispatched, before its post is attempted, so a post
/// lost to the app quitting still leaves a row for startup to fail.
pub fn insert_pending_delivery(db_path: &Path, delivery: &JobDelivery) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;
    conn.execute(
        "INSERT INTO job_deliveries
            (id, job_id, position, type, target, status, error, created_at, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            delivery.id,
            delivery.job_id,
            delivery.position,
            delivery.r#type,
            delivery.target,
            delivery.status,
            delivery.error,
            super::gotime::to_go_string_utc(delivery.created_at),
            nullable_time(delivery.finished_at.as_ref()),
        ],
    )
    .map_err(|e| format!("recording delivery {:?}: {e}", delivery.id))?;
    Ok(())
}

/// Finishes a pending delivery with its outcome. Answers whether the row
/// changed.
///
/// **Only while it is still `pending`**: a second finish is a no-op, and a
/// delivery startup already failed as interrupted cannot be flipped to `sent`
/// by a late finish — the reap wins, the same shape as [`reap_job_history`].
pub fn finish_delivery(
    db_path: &Path,
    id: &str,
    status: &str,
    error: &str,
) -> Result<bool, String> {
    let conn = db::open_read_write(db_path)?;
    let changed = conn
        .execute(
            "UPDATE job_deliveries SET status = ?1, error = ?2, finished_at = ?3
             WHERE id = ?4 AND status = 'pending'",
            rusqlite::params![status, error, super::gotime::now_go_text(), id],
        )
        .map_err(|e| format!("finishing delivery {id:?}: {e}"))?;
    Ok(changed > 0)
}

/// Fails every delivery a previous session left `pending`, with `reason` as its
/// error. Answers how many rows changed.
///
/// **Only a previous session's rows**: one created at or after `booted_at`
/// belongs to a delivery this session is still sending and is left alone. The
/// comparison is on parsed instants rather than the stored text, as
/// `Scheduler::reap_stale_runs` does for runs.
pub fn reap_pending_deliveries(
    db_path: &Path,
    booted_at: GoTime,
    reason: &str,
) -> Result<usize, String> {
    let conn = db::open_read_write(db_path)?;
    let stale = {
        let mut stmt = conn
            .prepare("SELECT id, created_at FROM job_deliveries WHERE status = 'pending'")
            .map_err(|e| format!("listing pending deliveries: {e}"))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, timestamp(row, 1)?)))
            .map_err(|e| format!("listing pending deliveries: {e}"))?;
        let mut stale = Vec::new();
        for row in rows {
            let (id, created_at) = row.map_err(|e| format!("listing pending deliveries: {e}"))?;
            if created_at.instant() < booted_at.instant() {
                stale.push(id);
            }
        }
        stale
    };
    let finished_at = super::gotime::now_go_text();
    let mut reaped = 0;
    for id in stale {
        reaped += conn
            .execute(
                "UPDATE job_deliveries SET status = 'failed', error = ?1, finished_at = ?2
                 WHERE id = ?3 AND status = 'pending'",
                rusqlite::params![reason, finished_at, id],
            )
            .map_err(|e| format!("reaping delivery {id:?}: {e}"))?;
    }
    Ok(reaped)
}

// ─── The task writes (#275) ───────────────────────────────────────────────────

/// `CreateTaskRequest` and `UpdateTaskRequest` (`internal/api/types.go`).
///
/// One struct for both, because the two Go types are field-for-field identical
/// — they are kept separate there for a divergence that has not happened yet,
/// and two identical structs here would only be two places to forget an
/// attribute.
///
/// **Five fields here have no Go ancestor, and that is deliberate** (#540).
/// `working_directory`, `model`, `settings_profile_id`, `stop_after_count` and
/// `stop_after_time` are columns the table has, the executor reads and the `GET`
/// returns, and Go's two request types never carried — so both handlers built a
/// `ScheduledTask` with them at their zero values and a form that posted them
/// got a `200` and stored nothing. The port reproduced that while a second
/// implementation shared the database; #391 deleted the Go tree, so the
/// constraint is gone and this is simply Agento's bug. **Do not "restore
/// parity" by taking them out again** — the Tasks form has edited all five
/// since before the port, and dropping them is silent data loss.
///
/// **The `PUT` semantics are replace, for these five as for every other field
/// on this route**: an omitted key resets the column to its zero value, because
/// the only client posts the whole record back and a per-field preserve rule
/// would make "clear my working directory" unexpressible. That is deliberately
/// *not* `gateway::config::update_provider`'s three-valued absent/empty/present
/// contract, nor the one #515 gave the integrations `PUT`: those exist because
/// the field is a secret the caller cannot read back and therefore cannot
/// resend, which is true of none of these five.
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

    // The five described in the doc comment above: Go's request types
    // stopped at `save_output`.
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    working_directory: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    model: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    settings_profile_id: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    stop_after_count: i64,
    /// The one genuinely nullable field, so a **plain** `Option` and no
    /// `null_is_zero_value`: absent and explicit `null` both mean `None`, which
    /// is what a bare `Option` already gives, and the column is `DATETIME NULL`
    /// rather than a zero-valued `NOT NULL`. Note this is also the one field
    /// where `deserialize_with` would change the shape rather than only the
    /// value — see `docs/internal/native.md` → *They are types rather than
    /// `deserialize_with` functions*.
    stop_after_time: Option<GoTime>,
    /// Delivery destinations (#634), replaced whole like every other field:
    /// absent, `null` and `[]` all store an empty list. Each element is a
    /// [`GoStruct`](super::gojson::GoStruct), so an array where an entry's
    /// object belongs is a 400 rather than a zero entry, while a `null`
    /// element is the zero entry — which validation then refuses by its type.
    destinations: Option<super::gojson::GoList<super::gojson::GoStruct<TaskDestination>>>,
}

impl TaskRequest {
    /// The `storage.ScheduledTask` both handlers build from the request — the
    /// fifteen fields they copy (`destinations` since #634), and nothing else.
    ///
    /// The other seven are the row's own, and they split three ways. `id` is
    /// minted on a create and re-set from the URL on an update; `updated_at` is
    /// restamped by every write. Four are taken from the **stored row** by
    /// `update_task` — `created_at`, `run_count`, `last_run_at` and
    /// `last_run_status` — which is what stops an edit resetting a task's
    /// history. `next_run_at` is the exception in both directions: nothing in
    /// this crate ever writes it, and an update **clears** it rather than
    /// carrying it over. See `update_task`'s own doc comment.
    fn into_task(self) -> ScheduledTask {
        ScheduledTask {
            id: String::new(),
            name: self.name,
            description: self.description,
            prompt: self.prompt,
            agent_slug: self.agent_slug,
            working_directory: self.working_directory,
            model: self.model,
            settings_profile_id: self.settings_profile_id,
            timeout_minutes: self.timeout_minutes,
            schedule_type: self.schedule_type,
            schedule_config: self.schedule_config.0,
            stop_after_count: self.stop_after_count,
            stop_after_time: self.stop_after_time,
            save_output: self.save_output,
            destinations: self
                .destinations
                .map(|list| list.0.into_iter().map(|entry| entry.0).collect())
                .unwrap_or_default(),
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
        "cron" => {
            if cfg.expression.is_empty() {
                return Err(WriteError::validation(
                    "schedule_config.expression",
                    "expression is required for cron schedules",
                ));
            }
            // #330: the expression is checked *here*, not at schedule time.
            // `create_task` commits the row and only then calls
            // `schedule_task`, whose failure is a log line — so before this
            // check an unusable expression produced a 201, an `active` task,
            // no timer and no `next_run_at`, forever, with the reconcile sweep
            // failing on it identically every minute. A validation error is
            // the only place the user can be told.
            //
            // The location is the scheduler's own, so a `CRON_TZ=Local` prefix
            // resolves at save time to what it will resolve to at run time.
            let loc = super::schedule::runtime::local_tz();
            let now = chrono::Utc::now();
            if let Err(err) = super::schedule::validate_cron(&cfg.expression, loc, now) {
                // Two different mistakes, so two different sentences: one is a
                // typo, the other is a date that does not exist.
                //
                // Both arms are named. `setup` on a `JobDefinition::Cron` can
                // reach exactly these two of `ScheduleError`'s eight — the
                // other six belong to the one-time, duration and daily job
                // types — so the wildcard is unreachable rather than a default,
                // and it is spelled `unreachable-ish` on purpose: under a bare
                // `_ =>` a variant added later would silently inherit the
                // *parse* wording and every test would stay green.
                let message = match err {
                    super::schedule::ScheduleError::CronParse => {
                        "expression is not a valid cron schedule"
                    }
                    super::schedule::ScheduleError::CronInvalid => {
                        "expression is a valid cron schedule but will never fire"
                    }
                    other => {
                        log::warn!(
                            "cron validation got an unexpected schedule error: {}",
                            other.class()
                        );
                        "expression is not a valid cron schedule"
                    }
                };
                return Err(WriteError::validation(
                    "schedule_config.expression",
                    message,
                ));
            }
        }
        _ => {}
    }
    validate_destinations(&mut task.destinations)
}

/// The shape of each delivery destination (#634) — everything decidable without
/// the database; [`check_destination_integrations`] does the rest inside the
/// write's transaction.
///
/// **It mutates**, like [`validate_task`]: an empty `when` becomes `success`,
/// and the defaulted value is what is stored. Field paths are indexed
/// (`destinations[0].slack.channel_ids`) so the form can point at the entry.
fn validate_destinations(destinations: &mut [TaskDestination]) -> Result<(), WriteError> {
    for (i, dest) in destinations.iter_mut().enumerate() {
        let field = |name: &str| format!("destinations[{i}].{name}");
        if dest.r#type != "slack" {
            return Err(WriteError::validation(&field("type"), "type must be slack"));
        }
        match dest.when.as_str() {
            "success" | "always" => {}
            "" => dest.when = "success".to_string(),
            _ => {
                return Err(WriteError::validation(
                    &field("when"),
                    "when must be success or always",
                ))
            }
        }
        let Some(slack) = dest.slack.as_deref() else {
            return Err(WriteError::validation(
                &field("slack"),
                "slack is required for slack destinations",
            ));
        };
        if slack.integration_id.is_empty() {
            return Err(WriteError::validation(
                &field("slack.integration_id"),
                "integration_id is required for slack destinations",
            ));
        }
        if slack.channel_ids.is_empty() {
            return Err(WriteError::validation(
                &field("slack.channel_ids"),
                "channel_ids is required for slack destinations",
            ));
        }
        for (j, channel) in slack.channel_ids.iter().enumerate() {
            if !is_slack_channel_id(channel) {
                return Err(WriteError::validation(
                    &field("slack.channel_ids"),
                    format!("channel id {channel:?} is not a Slack channel id"),
                ));
            }
            if slack.channel_ids[..j].contains(channel) {
                return Err(WriteError::validation(
                    &field("slack.channel_ids"),
                    format!("channel id {channel:?} is listed more than once"),
                ));
            }
        }
    }
    Ok(())
}

/// `^[CGD][A-Z0-9]{8,}$`: a public channel, private group or DM id as Slack
/// issues them. Spelled out rather than a regex because it is one character
/// class and a length.
fn is_slack_channel_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() >= 9
        && matches!(bytes[0], b'C' | b'G' | b'D')
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// The one database-backed destination check: each `integration_id` must name
/// a **Slack** integration. Runs inside the write's transaction, before the
/// insert or update, so a refusal writes nothing.
///
/// **A missing integration is grandfathered when this task already stores it.**
/// Deleting an integration keeps the task's configuration (the delivery shows a
/// warning instead), and the form posts the whole task back on every edit — so
/// a strict check would make such a task uneditable until the user noticed and
/// removed the entry. `stored` is the task's list before this write (empty on
/// a create), and a *new* id that names nothing is still refused. An
/// integration that exists with another type is refused either way: that is not
/// a stale reference but a wrong one.
fn check_destination_integrations(
    conn: &rusqlite::Connection,
    destinations: &[TaskDestination],
    stored: &[TaskDestination],
) -> Result<(), WriteError> {
    for (i, dest) in destinations.iter().enumerate() {
        let Some(slack) = dest.slack.as_deref() else {
            continue;
        };
        let id = &slack.integration_id;
        let integration_type: Option<String> = conn
            .query_row("SELECT type FROM integrations WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| WriteError::Fallback(format!("looking up integration {id:?}: {e}")))?;
        let field = format!("destinations[{i}].slack.integration_id");
        match integration_type.as_deref() {
            Some("slack") => {}
            Some(_) => {
                return Err(WriteError::validation(
                    &field,
                    "integration_id is not a Slack integration",
                ))
            }
            None => {
                let already_stored = stored
                    .iter()
                    .any(|d| d.slack.as_deref().is_some_and(|s| s.integration_id == *id));
                if !already_stored {
                    return Err(WriteError::validation(
                        &field,
                        "integration_id does not name an integration",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Go's `TimeoutMinutes == 0 → 30`, applied by both create and update *after*
/// validation — which is why a stored 0 is impossible while the validator still
/// admits one.
const DEFAULT_TIMEOUT_MINUTES: i64 = 30;

/// `taskService.CreateTask`.
///
/// `pub(crate)` for one caller outside this module: the scheduler's own
/// `a_stop_after_count_task_created_through_the_api_pauses_on_the_limit`
/// (#540), which has to build its task the way the app does rather than by
/// `INSERT`, or it cannot see the write hop that was dropping the budget.
pub(crate) fn create_task(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
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
    check_destination_integrations(&tx, &task.destinations, &[])?;
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
    check_destination_integrations(&tx, &task.destinations, &existing.destinations)?;
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

/// `POST /api/tasks/{id}/run` (#541): fire a task now, whatever its schedule
/// says.
///
/// **This route has no Go ancestor.** Nothing in the product could run a
/// configured task on demand; the only way to make one fire was to change its
/// schedule type to `run_immediately`, i.e. destroy the schedule under test in
/// order to test it once. See `ROUTES` for where it is recorded.
///
/// Four properties, each of which is the whole point of one of the issue's
/// acceptance criteria:
///
/// - **It answers within a request round-trip.** `Endpoint::serve` is a sync
///   `fn` on `spawn_blocking` and a run reaches 240 minutes, so the run is
///   spawned and the response is built here. `202 Accepted` carries the
///   `job_history` id the run will write — see [`TaskRunStarted`] for why that
///   row may not exist yet.
/// - **It refuses a second concurrent run of the same task**, `409`, through
///   the scheduler's in-flight map — claimed *here*, under one lock, so two
///   simultaneous requests answer one `202` and one `409` rather than both
///   passing a check and then both starting.
/// - **A paused task is runnable**, and so is one past its `stop_after_count`.
///   [`super::schedule::executor::run_manual`] deliberately does not go through
///   `due_task`.
/// - **It changes nothing about the schedule.** See [`RunKind::Manual`].
fn run_task_now(id: &str) -> Result<super::Answer, WriteError> {
    let Some(scheduler) = super::schedule::runtime::running() else {
        // No scheduler means no database (`shell_owns_scheduler`), which is not
        // a state a shipped build reaches — every other write here would have
        // answered 500 for the same reason.
        return Err(WriteError::Fallback(
            "the task scheduler is not running".to_string(),
        ));
    };
    start_manual_run(&scheduler, id)
}

/// [`run_task_now`] against a scheduler the caller supplies, so the tests can
/// drive it without the process-wide `OnceLock`.
///
/// The task is read through **the scheduler's** database rather than the
/// request context's. They are the same file in this process, and taking it
/// from one place is what stops the `404` check and the run itself from ever
/// disagreeing about which row is meant.
fn start_manual_run(
    scheduler: &std::sync::Arc<super::schedule::runtime::Scheduler>,
    id: &str,
) -> Result<super::Answer, WriteError> {
    let Some(task) = get_task(scheduler.db_path(), id).map_err(WriteError::Fallback)? else {
        return Err(WriteError::NotFound {
            resource: "task".to_string(),
            id: id.to_string(),
        });
    };

    let Some(guard) = scheduler.try_mark_running(id) else {
        return Err(WriteError::ConflictMessage(
            "a run of this task is already in progress".to_string(),
        ));
    };

    let job_id = uuid::Uuid::new_v4().to_string();
    // Built before anything is spawned: the write-path rule is that nothing
    // fallible may sit after the effect, and here the effect is a subprocess
    // rather than a commit.
    let body = super::gojson::to_vec(&TaskRunStarted {
        job_id: &job_id,
        task_id: id,
    })
    .map_err(|e| WriteError::Fallback(format!("encoding task run: {e}")))?;

    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return Err(WriteError::Fallback(
            "no tokio runtime to start a manual task run on".to_string(),
        ));
    };
    handle.spawn(super::schedule::executor::run_manual(
        std::sync::Arc::clone(scheduler),
        task,
        job_id.clone(),
        guard,
    ));

    log::info!("task run started id={id:?} job_id={job_id:?}");
    Ok(super::Answer::json_status(StatusCode::ACCEPTED, body))
}

/// How many fires the preview lists.
const PREVIEW_RUNS: usize = 3;

/// `POST /api/tasks/preview` (#633): what a draft task *would* do, computed by
/// the scheduler's own code and written nowhere.
///
/// **This route has no Go ancestor**; see `ROUTES`. It exists so the Tasks
/// form's create-mode inspector never needs a second implementation of the
/// schedule arithmetic — robfig's dialect, DST, gocron's `DailyJob` and the
/// `run_immediately` `now + 2s` all come from [`super::schedule::fire_times`],
/// in the location [`super::schedule::runtime::local_tz`] gives the scheduler.
///
/// - The body is a `TaskRequest`, so the same decode rules apply; a malformed
///   one is `400`. There is **no** `validate_task`: a draft may have no name or
///   prompt yet, and an unusable schedule is an answer (`200` with a
///   `schedule_error` and no runs) rather than a `422`.
/// - The limits are applied as the executor applies them to a new task
///   (`run_count` 0): a fire after `stop_after_time` would auto-pause instead of
///   running, and `stop_after_count` caps the list.
/// - The model and working directory are
///   [`super::schedule::executor::effective_execution`], the executor's own
///   precedence.
fn preview_task(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let mut task = decode_body::<TaskRequest>(body)?.into_task();
    // `validate_task`'s one mutation that changes what is scheduled.
    if task.schedule_type.is_empty() {
        task.schedule_type = "run_immediately".to_string();
    }
    let loc = super::schedule::runtime::local_tz();
    let (next_runs, schedule_error) = preview_runs(&task, loc, chrono::Utc::now());

    let execution = super::schedule::executor::effective_execution(db_path, &task)
        .map_err(WriteError::Fallback)?;
    let body = super::gojson::to_vec(&TaskPreview {
        next_runs,
        schedule_error,
        model: execution.model,
        model_source: execution.model_source,
        working_directory: execution.working_directory,
        working_directory_source: execution.working_directory_source,
    })
    .map_err(|e| WriteError::Fallback(format!("encoding task preview: {e}")))?;
    Ok(super::Answer::json(body))
}

/// The preview's fires, limits applied, as RFC 3339 in each fire's own offset —
/// or the sentence that says why there are none.
fn preview_runs(
    task: &ScheduledTask,
    loc: chrono_tz::Tz,
    now: chrono::DateTime<chrono::Utc>,
) -> (Vec<String>, String) {
    let outcome = super::schedule::fire_times(
        &task.schedule_type,
        &task.schedule_config,
        loc,
        now,
        PREVIEW_RUNS,
    );
    if let Some(class) = outcome.error {
        return (Vec::new(), schedule_error_sentence(class).to_string());
    }
    let cap = if task.stop_after_count > 0 {
        usize::try_from(task.stop_after_count).unwrap_or(usize::MAX)
    } else {
        usize::MAX
    };
    let runs = outcome
        .next_runs
        .into_iter()
        // A zero time is gocron's "no further run".
        .map_while(|fire| fire)
        // `should_auto_pause` pauses a task whose fire lands after the stop.
        .take_while(|fire| {
            task.stop_after_time
                .as_ref()
                .is_none_or(|stop| fire.instant <= stop.instant())
        })
        .take(cap)
        .map(|fire| fire.rfc3339())
        .collect();
    (runs, String::new())
}

/// A human sentence for a `fire_times` error class.
///
/// The two cron sentences are `validate_task`'s own, so the preview and the
/// save refuse an expression in the same words.
fn schedule_error_sentence(class: &str) -> &'static str {
    match class {
        "schedule:cron_parse" => "expression is not a valid cron schedule",
        "schedule:cron_invalid" => "expression is a valid cron schedule but will never fire",
        "schedule:one_time_past" => "The run time is in the past.",
        "build:run_at" => "Choose a valid run time.",
        _ => "This schedule will not run.",
    }
}

/// The `200` body of `POST /api/tasks/preview`, fields in wire order.
#[derive(Serialize)]
struct TaskPreview {
    next_runs: Vec<String>,
    schedule_error: String,
    model: String,
    model_source: &'static str,
    working_directory: String,
    working_directory_source: &'static str,
}

/// The `202` body of `POST /api/tasks/{id}/run`.
///
/// **The job id names a row that does not exist yet**, and a caller has to
/// tolerate that rather than assume a short window. The row is written by the
/// run's own preparation, and the run first waits for one of the scheduler's
/// three permits — so with three long runs already going (they reach 240
/// minutes) `GET /api/job-history/{job_id}` answers 404 until one of them ends.
/// That is a property of the queue, not a race to lose.
///
/// It is still worth answering with, because it is the only thing that makes
/// the started run *identifiable*: the alternative is a caller diffing the
/// history list and guessing which row is its own. Navigation on top of it
/// (#542) has to treat "not there yet" as a state rather than an error.
#[derive(Serialize)]
struct TaskRunStarted<'a> {
    job_id: &'a str,
    task_id: &'a str,
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

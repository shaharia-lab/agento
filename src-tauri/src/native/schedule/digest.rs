//! The weekly digest (#699): the first built-in recipe, a report on the
//! automations themselves rendered from what the database already holds.
//!
//! Three rules, each one the reason the recipe exists:
//!
//! - **No spawn.** Nothing here starts the Claude CLI or any other process, and
//!   nothing spends plan quota. [`gather`] is one read-only section the
//!   executor runs inside its own `db::blocking` preparation, and [`render`] is
//!   pure. A run that wants prose on top opts into the narrative, which is the
//!   executor's business, not this module's.
//! - **Stored cost only.** A run's cost is the `claude_session_cache` row of
//!   its session plus that session's sub-agents, reached through
//!   `job_history.chat_session_id → chat_sessions.sdk_session_id`. Nothing is
//!   re-priced: the pricing table is never read. A session the scanner has not
//!   reached yet is counted as *not yet priced*, a model the scanner could not
//!   price is listed by name, and either one turns the total into a floor
//!   (`≥`). Neither is ever treated as `$0`.
//! - **Deterministic.** The same database and the same window render the same
//!   bytes. Every list has a full sort key ending in an id, every map is a
//!   `BTreeMap`, and costs are summed in session-id order so the float total
//!   cannot depend on row order.
//!
//! What the digest never shows: an event's payload, which is text from outside
//! the app. An error message is cut to its first line and passed through the
//! Credentials Checker's `mask_text` before it is kept.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::native::db;
use crate::native::gotime::{self, GoTime};

/// How many failed runs, and how many failed deliveries, the digest lists by
/// name. The rest are one "+N more" line.
pub const MAX_LISTED: usize = 10;

/// The digest's window: the seven days that end when the run fires.
pub const WINDOW_DAYS: i64 = 7;

/// The most session ids one cost lookup binds, to stay under SQLite's
/// bound-variable limit.
const SESSION_LOOKUP_CHUNK: usize = 500;

/// Everything [`render`] reads, gathered once by [`gather`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DigestData {
    /// One entry per task that ran in the window, by task name then id.
    pub tasks: Vec<TaskRuns>,
    /// Every failed run in the window, oldest first.
    pub failed_runs: Vec<FailedRun>,
    pub tokens: Tokens,
    pub cost: Cost,
    /// Delivery attempts of the window's runs, by status.
    pub deliveries: BTreeMap<String, i64>,
    /// Every failed delivery of the window's runs, oldest run first.
    pub failed_deliveries: Vec<FailedDelivery>,
    /// Tasks with a non-zero event counter, by task name then id. These are
    /// lifetime totals: `scheduled_tasks` stores no per-event time (#681).
    pub events_not_run: Vec<EventCounters>,
}

/// One task's runs in the window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskRuns {
    pub task_id: String,
    pub task_name: String,
    pub by_status: BTreeMap<String, i64>,
    pub by_trigger: BTreeMap<String, i64>,
}

impl TaskRuns {
    fn total(&self) -> i64 {
        self.by_status.values().sum()
    }
}

/// A failed run, with the first line of its error, masked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedRun {
    pub job_id: String,
    pub task_name: String,
    pub started_at: DateTime<Utc>,
    pub error: String,
}

/// The window's token totals, from `job_history`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
}

/// The window's stored cost.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cost {
    /// The sum of every priced session's stored cost, sub-agents included.
    pub usd: f64,
    /// Distinct sessions whose cost is in `usd`.
    pub priced_sessions: i64,
    /// Distinct sessions the scanner has not reached yet.
    pub unpriced_sessions: i64,
    /// Models the scanner stored as unpriced, on any priced session.
    pub unpriced_models: BTreeSet<String>,
}

impl Cost {
    /// Whether `usd` is a floor rather than the total.
    fn is_floor(&self) -> bool {
        self.unpriced_sessions > 0 || !self.unpriced_models.is_empty()
    }
}

/// A delivery that failed, with its error's first line, masked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedDelivery {
    pub task_name: String,
    pub r#type: String,
    pub target: String,
    pub error: String,
}

/// A task's two event counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCounters {
    pub task_name: String,
    pub dropped: i64,
    pub rate_limited: i64,
}

/// The first line of `text`, masked. Masked whole first and cut after, so a
/// credential that spans the cut is still matched by its rule.
fn first_line_masked(text: &str) -> String {
    let masked = crate::native::security_scan::scan::mask_text(text);
    masked.lines().next().unwrap_or("").trim().to_string()
}

/// `t` as a DATETIME column stores it, for a text comparison against
/// `job_history.started_at`. Every stored time is UTC, which is what makes the
/// text order the time order.
fn bound(t: DateTime<Utc>) -> String {
    gotime::to_go_string_utc(GoTime::from_utc(t))
}

/// Read everything the digest shows for `[since, until)`. Read-only, and one
/// connection for every query.
pub fn gather(
    db_path: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<DigestData, String> {
    let conn = db::open_read_only(db_path)?;
    gather_in(&conn, since, until).map_err(|e| format!("gathering the digest: {e}"))
}

fn gather_in(
    conn: &rusqlite::Connection,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> rusqlite::Result<DigestData> {
    let (since, until) = (bound(since), bound(until));
    let window = rusqlite::params![since, until];
    let mut data = DigestData::default();

    // Runs, per task. The name is the task's current one, so a rename
    // mid-week does not split a task in two.
    let mut stmt = conn.prepare(
        "SELECT j.task_id, t.name, j.status, j.triggered_by, COUNT(*)
         FROM job_history j JOIN scheduled_tasks t ON t.id = j.task_id
         WHERE j.started_at >= ?1 AND j.started_at < ?2
         GROUP BY j.task_id, t.name, j.status, j.triggered_by",
    )?;
    let mut by_task: BTreeMap<(String, String), TaskRuns> = BTreeMap::new();
    let rows = stmt.query_map(window, |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (task_id, task_name, status, trigger, count) = row?;
        let entry = by_task
            .entry((task_name.clone(), task_id.clone()))
            .or_insert_with(|| TaskRuns {
                task_id,
                task_name,
                ..Default::default()
            });
        *entry.by_status.entry(status).or_default() += count;
        *entry.by_trigger.entry(trigger).or_default() += count;
    }
    data.tasks = by_task.into_values().collect();

    let mut stmt = conn.prepare(
        "SELECT j.id, t.name, j.started_at, j.error_message
         FROM job_history j JOIN scheduled_tasks t ON t.id = j.task_id
         WHERE j.started_at >= ?1 AND j.started_at < ?2 AND j.status = 'failed'
         ORDER BY j.started_at, t.name, j.id",
    )?;
    let rows = stmt.query_map(window, |row| {
        Ok(FailedRun {
            job_id: row.get(0)?,
            task_name: row.get(1)?,
            started_at: gotime::from_sql_text(&row.get::<_, String>(2)?, 2)?.instant(),
            error: first_line_masked(&row.get::<_, String>(3)?),
        })
    })?;
    for row in rows {
        data.failed_runs.push(row?);
    }

    data.tokens = conn.query_row(
        "SELECT COALESCE(SUM(total_input_tokens), 0), COALESCE(SUM(total_output_tokens), 0),
                COALESCE(SUM(total_cache_creation_tokens), 0),
                COALESCE(SUM(total_cache_read_tokens), 0)
         FROM job_history WHERE started_at >= ?1 AND started_at < ?2",
        window,
        |row| {
            Ok(Tokens {
                input: row.get(0)?,
                output: row.get(1)?,
                cache_creation: row.get(2)?,
                cache_read: row.get(3)?,
            })
        },
    )?;

    // The window's sessions: distinct, because a continued run resumes the
    // chat of the run it continues and must not count its cost twice. A run
    // with no chat, or a chat the CLI never answered, has no session to price.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.sdk_session_id
         FROM job_history j JOIN chat_sessions c ON c.id = j.chat_session_id
         WHERE j.started_at >= ?1 AND j.started_at < ?2 AND c.sdk_session_id != ''
         ORDER BY c.sdk_session_id",
    )?;
    let sessions = stmt
        .query_map(window, |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    data.cost = stored_cost(conn, &sessions)?;

    let mut stmt = conn.prepare(
        "SELECT d.status, COUNT(*)
         FROM job_deliveries d JOIN job_history j ON j.id = d.job_id
         WHERE j.started_at >= ?1 AND j.started_at < ?2
         GROUP BY d.status",
    )?;
    let rows = stmt.query_map(window, |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (status, count) = row?;
        data.deliveries.insert(status, count);
    }

    let mut stmt = conn.prepare(
        "SELECT t.name, d.type, d.target, d.error
         FROM job_deliveries d
         JOIN job_history j ON j.id = d.job_id
         JOIN scheduled_tasks t ON t.id = j.task_id
         WHERE j.started_at >= ?1 AND j.started_at < ?2 AND d.status = 'failed'
         ORDER BY j.started_at, t.name, j.id, d.position, d.id",
    )?;
    let rows = stmt.query_map(window, |row| {
        Ok(FailedDelivery {
            task_name: row.get(0)?,
            r#type: row.get(1)?,
            target: row.get(2)?,
            error: first_line_masked(&row.get::<_, String>(3)?),
        })
    })?;
    for row in rows {
        data.failed_deliveries.push(row?);
    }

    let mut stmt = conn.prepare(
        "SELECT name, dropped_event_count, rate_limited_event_count
         FROM scheduled_tasks
         WHERE dropped_event_count != 0 OR rate_limited_event_count != 0
         ORDER BY name, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(EventCounters {
            task_name: row.get(0)?,
            dropped: row.get(1)?,
            rate_limited: row.get(2)?,
        })
    })?;
    for row in rows {
        data.events_not_run.push(row?);
    }

    Ok(data)
}

/// The stored cost of `sessions` (sorted, distinct).
///
/// A session can be cached under more than one `project_path` (a claim shift
/// keeps the old row until it expires), so its own cost is the **largest** of
/// its rows rather than their sum. Sub-agent rows are keyed by the parent
/// session alone and are summed.
fn stored_cost(conn: &rusqlite::Connection, sessions: &[String]) -> rusqlite::Result<Cost> {
    let mut own: BTreeMap<String, f64> = BTreeMap::new();
    let mut subagents: BTreeMap<String, f64> = BTreeMap::new();
    let mut unpriced_models = BTreeSet::new();
    for chunk in sessions.chunks(SESSION_LOOKUP_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT session_id, total_cost_usd, unpriced_models
             FROM claude_session_cache WHERE session_id IN ({placeholders})
             ORDER BY session_id, project_path"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (session, cost, models) = row?;
            let entry = own.entry(session).or_insert(cost);
            *entry = entry.max(cost);
            unpriced_models.extend(models.lines().filter(|m| !m.is_empty()).map(str::to_string));
        }

        let sql = format!(
            "SELECT parent_session_id, total_cost_usd, unpriced_models
             FROM claude_subagent_cache WHERE parent_session_id IN ({placeholders})
             ORDER BY parent_session_id, agent_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (session, cost, models) = row?;
            *subagents.entry(session).or_default() += cost;
            unpriced_models.extend(models.lines().filter(|m| !m.is_empty()).map(str::to_string));
        }
    }

    // Summed in session-id order, so the total is the same float every time.
    let mut cost = Cost {
        unpriced_models,
        ..Default::default()
    };
    for session in sessions {
        match own.get(session) {
            Some(usd) => {
                cost.usd += usd + subagents.get(session).copied().unwrap_or(0.0);
                cost.priced_sessions += 1;
            }
            None => cost.unpriced_sessions += 1,
        }
    }
    Ok(cost)
}

/// `n` and the noun, singular for one.
fn count(n: i64, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

/// A count map as `2 success, 1 failed`, in key order.
fn breakdown(map: &BTreeMap<String, i64>) -> String {
    map.iter()
        .map(|(key, n)| format!("{n} {key}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A time in the digest's zone, to the minute.
fn local(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string()
}

/// The digest as plain text that reads the same in Slack, Telegram and email.
///
/// Pure: the same `data`, window and zone give the same bytes. The section
/// order is fixed — runs, failed runs, cost and tokens, deliveries, events not
/// run — and a week with no runs is one sentence where the first four would be.
pub fn render(data: &DigestData, since: DateTime<Utc>, until: DateTime<Utc>, tz: Tz) -> String {
    let mut out = format!(
        "Weekly digest: {} to {} ({tz})\n",
        local(since, tz),
        local(until, tz)
    );

    if data.tasks.is_empty() {
        out.push_str("\nNo task ran in these seven days.\n");
    } else {
        let mut all = BTreeMap::new();
        for task in &data.tasks {
            for (status, n) in &task.by_status {
                *all.entry(status.clone()).or_insert(0) += n;
            }
        }
        let runs: i64 = all.values().sum();
        out.push_str(&format!(
            "\nRuns\n{} across {}: {}.\n",
            count(runs, "run", "runs"),
            count(data.tasks.len() as i64, "task", "tasks"),
            breakdown(&all)
        ));
        for task in &data.tasks {
            out.push_str(&format!(
                "- {}: {} ({}); by trigger: {}\n",
                task.task_name,
                count(task.total(), "run", "runs"),
                breakdown(&task.by_status),
                breakdown(&task.by_trigger)
            ));
        }

        if !data.failed_runs.is_empty() {
            out.push_str("\nFailed runs\n");
            for run in data.failed_runs.iter().take(MAX_LISTED) {
                out.push_str(&format!(
                    "- {} {}: {}\n",
                    local(run.started_at, tz),
                    run.task_name,
                    if run.error.is_empty() {
                        "(no error message)"
                    } else {
                        &run.error
                    }
                ));
            }
            if data.failed_runs.len() > MAX_LISTED {
                out.push_str(&format!("+{} more\n", data.failed_runs.len() - MAX_LISTED));
            }
        }

        let tokens = &data.tokens;
        out.push_str(&format!(
            "\nCost and tokens\nTokens: {} input, {} output, {} cache write, {} cache read.\n",
            tokens.input, tokens.output, tokens.cache_creation, tokens.cache_read
        ));
        let cost = &data.cost;
        out.push_str(&format!(
            "Cost: {}${:.2} from {}.\n",
            if cost.is_floor() { "≥ " } else { "" },
            cost.usd,
            count(cost.priced_sessions, "priced session", "priced sessions")
        ));
        if cost.unpriced_sessions > 0 {
            out.push_str(&format!(
                "Not yet priced: {}, not scanned yet.\n",
                count(cost.unpriced_sessions, "session", "sessions")
            ));
        }
        if !cost.unpriced_models.is_empty() {
            out.push_str(&format!(
                "Unpriced models: {}.\n",
                cost.unpriced_models
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let deliveries: i64 = data.deliveries.values().sum();
        out.push_str("\nDeliveries\n");
        if deliveries == 0 {
            out.push_str("No delivery was attempted.\n");
        } else {
            out.push_str(&format!(
                "{}: {}.\n",
                count(deliveries, "delivery", "deliveries"),
                breakdown(&data.deliveries)
            ));
        }
        for failed in data.failed_deliveries.iter().take(MAX_LISTED) {
            let target = if failed.target.is_empty() {
                failed.r#type.clone()
            } else {
                format!("{} {}", failed.r#type, failed.target)
            };
            out.push_str(&format!(
                "- {} to {}: {}\n",
                failed.task_name,
                target,
                if failed.error.is_empty() {
                    "(no error message)"
                } else {
                    &failed.error
                }
            ));
        }
        if data.failed_deliveries.len() > MAX_LISTED {
            out.push_str(&format!(
                "+{} more\n",
                data.failed_deliveries.len() - MAX_LISTED
            ));
        }
    }

    if !data.events_not_run.is_empty() {
        out.push_str("\nEvents not run (running totals since each task was created)\n");
        for task in &data.events_not_run {
            out.push_str(&format!(
                "- {}: {} dropped, {} rate-limited\n",
                task.task_name, task.dropped, task.rate_limited
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests;

//! One scheduled run, end to end. Mirrors `internal/scheduler/executor.go`.
//!
//! # The rule this file is written around
//!
//! With the sidecar started `AGENTO_SCHEDULER=off`, **a fire that this process
//! declines is a fire that nothing serves.** There is no second implementation
//! behind it the way there is behind every claimed *route*, so the seam's
//! "return `Err` and let Go answer" is not available here. Every path therefore
//! ends in a `job_history` row: a task that cannot be interpolated, cannot find
//! its agent, or names tools this build cannot host records a **failed** run and
//! publishes the failed event. Silence is the one outcome that is not allowed,
//! because a job history with no row is indistinguishable from a task that was
//! not due.
//!
//! That last case is the one Go has no equivalent for. Go's `buildRunOptions`
//! can always supply every tool, since it *is* the process that hosts them;
//! [`crate::native::chat::runner::build_options`] can refuse (an agent naming an
//! `mcps.yaml` server, or a `whatsapp` integration this build dropped). In a
//! chat that refusal forwards. Here it is a recorded failure with the reason in
//! `error_message`, which is the only answer that leaves evidence.
//!
//! # What is deliberately not reproduced
//!
//! The OTel spans. `executeTask` roots a trace and `runTask` enriches it; the
//! desktop build exports no telemetry at all (#309), so there is nothing for a
//! span to reach. The `slog` lines are reproduced, because they are what a user
//! reads in the log file.

use std::sync::Arc;

use chrono::Utc;

use super::runtime::Scheduler;
use crate::native::agent_run::RunResult;
use crate::native::agents::{self, Agent};
use crate::native::chat::runner::TurnSettings;
use crate::native::db;
use crate::native::notifications;
use crate::native::tasks::{self, JobHistory, ScheduledTask};
use crate::native::template;

/// `executeTask`. The semaphore is the caller's; this is everything inside it.
pub async fn execute_task(scheduler: &Arc<Scheduler>, task_id: &str) {
    let due = {
        let (scheduler, task_id) = (Arc::clone(scheduler), task_id.to_string());
        db::blocking("scheduled run", move || due_task(&scheduler, &task_id)).await
    };
    let Some(Some(task)) = due else {
        return;
    };

    log::info!(
        "executing task task_id={:?} task_name={:?} run_count={}",
        task.id,
        task.name,
        task.run_count + 1
    );
    run_task(scheduler, task).await;
}

/// The load-and-check half of `executeTask`: the row, its status, and the
/// auto-pause rules. Synchronous, and called through [`db::blocking`].
fn due_task(scheduler: &Arc<Scheduler>, task_id: &str) -> Option<ScheduledTask> {
    let task = match tasks::get_task(scheduler.db_path(), task_id) {
        Ok(Some(task)) => task,
        // A task that vanished between the fire and the read. Go returns
        // silently; so does this — there is no row to attach a failure to.
        Ok(None) => return None,
        Err(e) => {
            log::error!("failed to load task for execution task_id={task_id:?} error={e}");
            return None;
        }
    };
    if task.status != "active" {
        return None;
    }

    if should_auto_pause(&task) {
        let reason = if task.stop_after_count > 0 && task.run_count >= task.stop_after_count {
            "stop_after_count reached"
        } else {
            "stop_after_time reached"
        };
        auto_pause(scheduler, task, reason);
        return None;
    }
    Some(task)
}

/// `shouldAutoPause`. Both conditions are checked before the run, so a task that
/// has already reached its limit never starts one.
fn should_auto_pause(task: &ScheduledTask) -> bool {
    if task.stop_after_count > 0 && task.run_count >= task.stop_after_count {
        return true;
    }
    match &task.stop_after_time {
        Some(stop) => Utc::now() > stop.instant(),
        None => false,
    }
}

/// `autoPause`: park the task and drop its timer.
fn auto_pause(scheduler: &Arc<Scheduler>, mut task: ScheduledTask, reason: &str) {
    log::info!("auto-pausing task task_id={:?} reason={reason:?}", task.id);
    task.status = "paused".to_string();
    if let Err(e) = tasks::update_task_row(scheduler.db_path(), &mut task) {
        log::error!("failed to auto-pause task task_id={:?} error={e}", task.id);
    }
    scheduler.unschedule_task(&task.id);
}

/// `runTask`: interpolate, create the session and the job row, run, record.
///
/// # Three sections, and the two on the ends are blocking
///
/// The shape is not arbitrary. Everything before the agent run and everything
/// after it is synchronous rusqlite, and the run itself is the only part that
/// awaits — often for hours. Written inline the two ends would park an axum
/// worker on `busy_timeout` (see [`db::blocking`]), three at a time because that
/// is what the scheduler's semaphore permits, which is enough to stall the SPA
/// and every SSE stream on a four-core machine. So [`prepare`] and [`finish`]
/// are whole synchronous sections handed to the pool, rather than eight
/// individually wrapped calls: the database work in one run is contiguous, and
/// splitting it any finer would only add hand-offs.
///
/// The notifications stay out here because [`publish`] already spawns and
/// deliberately does not wait — see its own note.
async fn run_task(scheduler: &Arc<Scheduler>, task: ScheduledTask) {
    let db_path = scheduler.db_path().to_path_buf();
    let started_at = Utc::now();

    let prepared = {
        let scheduler = Arc::clone(scheduler);
        db::blocking("scheduled run preparation", move || {
            prepare(&scheduler, task, started_at)
        })
        .await
    };
    // `None` is a panic in the section above. Every *handled* failure inside it
    // has already recorded its job row, which is what the module header's "never
    // silence" rule is about; a panic is a bug rather than an outcome, and it is
    // logged by `db::blocking`.
    let ready = match prepared {
        Some(Ok(ready)) => ready,
        Some(Err(failed)) => {
            publish_task_failed(&db_path, &failed.task, &failed.message);
            return;
        }
        None => return,
    };
    let Ready {
        task,
        job,
        chat_session_id,
        prompt,
        agent,
    } = ready;

    let result = run_agent(&db_path, &task, agent, &prompt).await;

    let recorded = {
        let (scheduler, session) = (Arc::clone(scheduler), chat_session_id.clone());
        db::blocking("scheduled run results", move || {
            finish(&scheduler, task, job, &session, &prompt, started_at, result)
        })
        .await
    };
    let Some(recorded) = recorded else {
        return;
    };

    if let Some(message) = &recorded.failure {
        publish_task_failed(&db_path, &recorded.task, message);
        return;
    }
    publish_task_finished(&db_path, &recorded.task, &recorded.job, &chat_session_id);

    log::info!(
        "task execution completed task_id={:?} task_name={:?} session_id={:?} run_count={}",
        recorded.task.id,
        recorded.task.name,
        chat_session_id,
        recorded.task.run_count
    );
}

/// What a run needs once everything that can fail before it has not.
struct Ready {
    task: ScheduledTask,
    job: JobHistory,
    chat_session_id: String,
    prompt: String,
    agent: Agent,
}

/// A failure that has already been recorded, carrying what the caller needs to
/// publish it. The task travels back because `update_task_after_run` re-reads
/// the row and writes the fresh counters onto it.
///
/// Boxed at the `Result` — a `ScheduledTask` is ~500 bytes, and the success side
/// carries one too, so the unboxed `Result` would pay for the larger of them on
/// every call. Failure is also the rarer half.
struct Failed {
    task: ScheduledTask,
    message: String,
}

/// `prepareTaskRun` plus `resolveAgentConfig`: everything `runTask` does before
/// the agent run, as one synchronous section.
///
/// Every `Err` here is *already recorded* — the two early failures write a
/// complete failed job row and the third finishes the running one — so the
/// caller's only remaining job is the notification, which must not happen on
/// this thread.
fn prepare(
    scheduler: &Arc<Scheduler>,
    mut task: ScheduledTask,
    started_at: chrono::DateTime<Utc>,
) -> Result<Ready, Box<Failed>> {
    let db_path = scheduler.db_path().to_path_buf();

    // `prepareTaskRun`. Both failures record a *complete* failed job row and
    // return — the run never reaches `createInitialJobHistory`, so there is no
    // running row to finish.
    let prompt = match template::interpolate(&task.prompt) {
        Ok(prompt) => prompt,
        Err(e) => {
            let message = format!("prompt interpolation: {e}");
            log::error!(
                "failed to interpolate prompt task_id={:?} error={e}",
                task.id
            );
            record_failed_run(scheduler, &mut task, started_at, "", &message);
            return Err(Box::new(Failed { task, message }));
        }
    };

    let chat_session_id = match create_task_session(&db_path, &task) {
        Ok(id) => id,
        Err(e) => {
            let message = format!("create session: {e}");
            log::error!(
                "failed to create chat session task_id={:?} error={e}",
                task.id
            );
            record_failed_run(scheduler, &mut task, started_at, "", &message);
            return Err(Box::new(Failed { task, message }));
        }
    };

    let mut job =
        create_initial_job_history(&db_path, &task, started_at, &chat_session_id, &prompt);

    // `resolveAgentConfig`. From here on there *is* a running row, so every
    // failure finishes it rather than creating a second.
    let agent = match resolve_agent(&db_path, &task) {
        Ok(agent) => agent,
        Err(e) => {
            let message = format!("resolve agent: {e}");
            log::error!(
                "failed to resolve agent config task_id={:?} error={e}",
                task.id
            );
            finish_job_history(&db_path, &mut job, started_at, "failed", &message, None, "");
            update_task_after_run(scheduler, &mut task, started_at, "failed");
            return Err(Box::new(Failed { task, message }));
        }
    };

    Ok(Ready {
        task,
        job,
        chat_session_id,
        prompt,
        agent,
    })
}

/// What [`finish`] wrote, for the caller to publish. `failure` carries the
/// message when the run itself failed.
struct Recorded {
    task: ScheduledTask,
    job: JobHistory,
    failure: Option<String>,
}

/// Everything `runTask` does after the agent run, as one synchronous section:
/// the session results, the job row, and the task's own counters.
fn finish(
    scheduler: &Arc<Scheduler>,
    mut task: ScheduledTask,
    mut job: JobHistory,
    chat_session_id: &str,
    prompt: &str,
    started_at: chrono::DateTime<Utc>,
    result: Result<RunResult, String>,
) -> Recorded {
    let db_path = scheduler.db_path().to_path_buf();

    let result = match result {
        Ok(result) => result,
        Err(e) => {
            log::error!("task execution failed task_id={:?} error={e}", task.id);
            finish_job_history(&db_path, &mut job, started_at, "failed", &e, None, "");
            update_task_after_run(scheduler, &mut task, started_at, "failed");
            return Recorded {
                task,
                job,
                failure: Some(e),
            };
        }
    };

    save_session_results(&db_path, chat_session_id, &result, prompt, started_at);
    // `task.SaveOutput` decides whether the answer is *stored*, not whether it
    // was produced — an unsaved run still has its tokens and duration recorded.
    let response_text = if task.save_output {
        result.answer.as_str()
    } else {
        ""
    };
    finish_job_history(
        &db_path,
        &mut job,
        started_at,
        "success",
        "",
        Some(&result),
        response_text,
    );
    update_task_after_run(scheduler, &mut task, started_at, "success");

    Recorded {
        task,
        job,
        failure: None,
    }
}

/// `createTaskSession`: the chat row a run's messages land in, titled after the
/// task.
///
/// Two writes, as Go has them, and the split is load-bearing on the failure
/// path rather than the success one: `createTaskSession` creates the session and
/// then updates its title, and a failed *title* update is logged at warn and the
/// run continues on the session it already has. Doing both in one transaction
/// would turn a cosmetic failure into `create session: …` — a run that never
/// happened.
fn create_task_session(db_path: &std::path::Path, task: &ScheduledTask) -> Result<String, String> {
    let mut conn = crate::native::db::open_read_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin task session: {e}"))?;

    let session = crate::native::chats::insert_session(
        &tx,
        &task.agent_slug,
        &task.working_directory,
        &task.model,
        &task.settings_profile_id,
    )
    .map_err(|e| e.message())?;

    tx.commit()
        .map_err(|e| format!("commit task session: {e}"))?;

    let title = format!("[Task] {}", task.name);
    if let Err(e) = conn.execute(
        "UPDATE chat_sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![title, crate::native::gotime::now_go_text(), session.id],
    ) {
        log::warn!("failed to update session title: {e}");
    }
    Ok(session.id)
}

/// `createInitialJobHistory`. A failed insert is logged and the run continues,
/// exactly as Go's does — the row it returns is used regardless, and the later
/// `UPDATE` simply matches nothing.
fn create_initial_job_history(
    db_path: &std::path::Path,
    task: &ScheduledTask,
    started_at: chrono::DateTime<Utc>,
    chat_session_id: &str,
    prompt: &str,
) -> JobHistory {
    let job = JobHistory {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task.id.clone(),
        task_name: task.name.clone(),
        agent_slug: task.agent_slug.clone(),
        status: "running".to_string(),
        started_at: crate::native::gotime::GoTime::from_utc(started_at),
        finished_at: None,
        duration_ms: 0,
        chat_session_id: chat_session_id.to_string(),
        model: task.model.clone(),
        prompt_preview: prompt_preview(prompt),
        error_message: String::new(),
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_creation_tokens: 0,
        total_cache_read_tokens: 0,
        response_text: String::new(),
    };
    if let Err(e) = tasks::insert_job_history(db_path, &job) {
        log::error!(
            "failed to create job history task_id={:?} error={e}",
            task.id
        );
    }
    job
}

/// Go's preview truncation: 200 **bytes** plus an ellipsis.
///
/// Bytes rather than characters, because `prompt[:200]` is a byte slice — and
/// that is why this cannot simply index: a prompt whose 200th byte falls inside
/// a multi-byte character would panic in Rust where Go produces invalid UTF-8 in
/// a `string`. The cut is moved *back* to the nearest boundary, which is the
/// only representable answer and differs from Go only for a prompt with a
/// multi-byte character straddling that exact offset.
fn prompt_preview(prompt: &str) -> String {
    const LIMIT: usize = 200;
    if prompt.len() <= LIMIT {
        return prompt.to_string();
    }
    let mut cut = LIMIT;
    while cut > 0 && !prompt.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &prompt[..cut])
}

/// `resolveAgentConfig`.
///
/// The no-agent branch returns a **synthesized** agent rather than `None`, and
/// that is load-bearing rather than cosmetic: Go builds a non-nil
/// `config.AgentConfig` there, and `resolveToolsAndMCP` gives a non-nil config
/// with empty capabilities **all twelve built-in tools** while a nil config gets
/// none at all. Passing `None` here would run a no-agent task with no
/// `--allowedTools` argument — a different command line for the same task.
fn resolve_agent(db_path: &std::path::Path, task: &ScheduledTask) -> Result<Agent, String> {
    if !task.agent_slug.is_empty() {
        return match agents::get(db_path, &task.agent_slug) {
            Ok(Some(agent)) => Ok(agent),
            Ok(None) => Err(format!("agent {:?} not found", task.agent_slug)),
            Err(e) => Err(format!("loading agent {:?}: {e}", task.agent_slug)),
        };
    }

    // `model := task.Model; if model == "" { settingsMgr.Get().DefaultModel }`.
    let mut model = task.model.clone();
    if model.is_empty() {
        model = TurnSettings::from_db(db_path).default_model();
    }
    Ok(Agent {
        name: String::new(),
        slug: String::new(),
        description: String::new(),
        model,
        thinking: "adaptive".to_string(),
        // Empty, so `appendPermissionOpts` falls to the bypass arm — which is
        // what an unattended run needs, since nothing is there to answer a
        // permission prompt.
        permission_mode: String::new(),
        system_prompt: String::new(),
        capabilities: Default::default(),
        claude_config_dir: String::new(),
    })
}

/// The scheduler's half of `agent.RunAgent`: resolve the system prompt, then
/// hand the run to [`crate::native::agent_run`].
///
/// The run itself is shared with the trigger dispatcher (#319) — see that
/// module for why the one-shot `query` rather than a `Session` is not a detail.
async fn run_agent(
    db_path: &std::path::Path,
    task: &ScheduledTask,
    agent: Agent,
    prompt: &str,
) -> Result<RunResult, String> {
    // `resolveSystemPrompt`'s strictness lives in `run_headless`, so both
    // headless callers get it — see that function.
    let spec = crate::native::agent_run::headless_spec(
        db_path,
        agent,
        task.working_directory.clone(),
        task.settings_profile_id.clone(),
    );
    let timeout = std::time::Duration::from_secs(
        u64::try_from(task.timeout_minutes.max(0)).unwrap_or(0) * 60,
    );
    crate::native::agent_run::run_headless(&spec, prompt, timeout).await
}

/// `saveSessionResults`: the run's totals onto the chat row, then the two
/// messages.
///
/// **The messages are written only when there is an answer**, which is Go's
/// `if result.Answer != ""` — so a run that produced nothing leaves the session
/// row updated and empty rather than storing a user turn with no reply.
fn save_session_results(
    db_path: &std::path::Path,
    chat_session_id: &str,
    result: &RunResult,
    prompt: &str,
    started_at: chrono::DateTime<Utc>,
) {
    if let Err(e) = write_session_results(db_path, chat_session_id, result, prompt, started_at) {
        log::warn!("failed to update chat session after execution: {e}");
    }
}

fn write_session_results(
    db_path: &std::path::Path,
    chat_session_id: &str,
    result: &RunResult,
    prompt: &str,
    started_at: chrono::DateTime<Utc>,
) -> Result<(), String> {
    let conn = crate::native::db::open_read_write(db_path)?;

    // **Three independent writes, not one transaction.** Go calls
    // `UpdateSession` and then `AppendMessage` twice, logging each failure on
    // its own — so a message that fails to store still leaves the session row
    // carrying `sdk_session_id` and the token totals. Wrapping them together
    // would roll the session update back too, losing the link to the run's
    // transcript over a failed message insert: a wider blast radius than the
    // code being ported has.
    conn.execute(
        "UPDATE chat_sessions SET
            sdk_session_id = ?1, total_input_tokens = ?2, total_output_tokens = ?3,
            total_cache_creation_tokens = ?4, total_cache_read_tokens = ?5, updated_at = ?6
         WHERE id = ?7",
        rusqlite::params![
            result.session_id,
            result.input_tokens,
            result.output_tokens,
            result.cache_creation_tokens,
            result.cache_read_tokens,
            crate::native::gotime::now_go_text(),
            chat_session_id,
        ],
    )
    .map_err(|e| format!("updating chat session: {e}"))?;

    if !result.answer.is_empty() {
        // The user turn carries `startedAt`, the assistant turn `time.Now()` —
        // so the pair brackets the run rather than sharing one instant.
        if let Err(e) = append_message(
            &conn,
            chat_session_id,
            "user",
            prompt,
            &crate::native::gotime::to_go_string_utc(crate::native::gotime::GoTime::from_utc(
                started_at,
            )),
        ) {
            log::warn!("failed to store user message: {e}");
        }
        if let Err(e) = append_message(
            &conn,
            chat_session_id,
            "assistant",
            &result.answer,
            &crate::native::gotime::now_go_text(),
        ) {
            log::warn!("failed to store assistant message: {e}");
        }
    }
    Ok(())
}

/// `ChatStore.AppendMessage` for a plain text turn.
///
/// **`id` is not in the column list**, and that is not a style choice:
/// `chat_messages.id` is `INTEGER PRIMARY KEY AUTOINCREMENT`, so supplying a
/// UUID for it is a `datatype mismatch` rather than a stored value — and since
/// the error propagates before the commit, it would take the session's own
/// `UPDATE` down with it, leaving every finished run with an empty chat, no
/// `sdk_session_id` and zeroed token totals. Go's `AppendMessage` and
/// [`crate::native::chat::persist`] both omit it for the same reason.
///
/// `blocks` is `'[]'` rather than `''` because every reader JSON-decodes it;
/// the column's own default says the same thing.
fn append_message(
    conn: &rusqlite::Connection,
    chat_session_id: &str,
    role: &str,
    content: &str,
    timestamp: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO chat_messages (session_id, role, content, blocks, timestamp)
         VALUES (?1, ?2, ?3, '[]', ?4)",
        rusqlite::params![chat_session_id, role, content, timestamp],
    )
    .map_err(|e| format!("storing {role} message: {e}"))?;
    Ok(())
}

/// `finishJobHistory`.
#[allow(clippy::too_many_arguments)]
fn finish_job_history(
    db_path: &std::path::Path,
    job: &mut JobHistory,
    started_at: chrono::DateTime<Utc>,
    status: &str,
    error_message: &str,
    result: Option<&RunResult>,
    response_text: &str,
) {
    let now = Utc::now();
    job.status = status.to_string();
    job.finished_at = Some(crate::native::gotime::GoTime::from_utc(now));
    job.duration_ms = (now - started_at).num_milliseconds();
    job.error_message = error_message.to_string();
    job.response_text = response_text.to_string();
    // A failed run passes no result, which is Go's zero `UsageStats` — the
    // totals are explicitly zeroed rather than left at whatever the row held.
    job.total_input_tokens = result.map_or(0, |r| r.input_tokens);
    job.total_output_tokens = result.map_or(0, |r| r.output_tokens);
    job.total_cache_creation_tokens = result.map_or(0, |r| r.cache_creation_tokens);
    job.total_cache_read_tokens = result.map_or(0, |r| r.cache_read_tokens);

    if let Err(e) = tasks::update_job_history(db_path, job) {
        log::error!("failed to update job history job_id={:?} error={e}", job.id);
    }
}

/// `updateTaskAfterRun`: the run counters, then the two auto-pause rules.
///
/// **The row is re-read inside the write's own transaction**, and the run's
/// changes are applied to *that* rather than to the snapshot the timer loaded.
/// Go writes the stale snapshot back wholesale, which clobbers any edit made
/// while the run was in flight — a task paused mid-run (timeouts reach 240
/// minutes) comes back `active`. Go got away with it because nothing
/// re-registered the cron entry, so the task stayed quiet until restart; here
/// `reconcile` would reinstall the timer within a minute and the "paused" task
/// would keep firing. Re-reading is the smaller divergence.
fn update_task_after_run(
    scheduler: &Arc<Scheduler>,
    task: &mut ScheduledTask,
    ran_at: chrono::DateTime<Utc>,
    status: &str,
) {
    // Only the schedule type is read off the snapshot; everything the write
    // decides is derived from the row it re-reads. See [`write_run_result`].
    let one_shot = task.schedule_type == "one_off" || task.schedule_type == "run_immediately";

    let task_id = task.id.clone();
    match write_run_result(scheduler, &task_id, ran_at, status, one_shot, task) {
        // A one-shot task is paused after its run so a restart does not re-run
        // it — the timer is already exhausted, but the *row* is what `Start`
        // reads.
        //
        // **Only when the row actually says so.** Dropping the timer after a
        // failed write would leave the task `active`, timer-less *and*
        // forgotten by the sweep — `unschedule_task` clears `swept` — so
        // `reconcile` would reinstall it, a `run_immediately` task would fire
        // two seconds later, the write would fail again: a full agent run every
        // minute, unbounded. A read-only data dir or a full disk is enough to
        // reach it. Leaving the timer alone is also what Go does when its
        // `UpdateTask` fails.
        Ok(paused) => {
            if paused {
                scheduler.unschedule_task(&task_id);
            }
        }
        Err(e) => log::error!("failed to update task after run task_id={task_id:?} error={e}"),
    }
}

/// The read-modify-write behind [`update_task_after_run`]. Answers whether the
/// row ended up paused, which is what decides the timer.
///
/// **Every field it writes is derived from the row it just read**, not from the
/// snapshot the timer loaded — that is the whole point. `status` is the case
/// that motivated it (a pause landing mid-run), but `run_count` is the same
/// hazard pointing the other way: `resume_task` resets it to 0 precisely so a
/// `stop_after_count` task becomes runnable again, and writing back
/// `snapshot + 1` would restore the old count and auto-pause the task on its
/// very next fire. Runs are long — up to 240 minutes — so both edits are
/// reachable.
fn write_run_result(
    scheduler: &Arc<Scheduler>,
    task_id: &str,
    ran_at: chrono::DateTime<Utc>,
    status: &str,
    one_shot: bool,
    caller_copy: &mut ScheduledTask,
) -> Result<bool, String> {
    let mut conn = crate::native::db::open_read_write(scheduler.db_path())?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin task update after run: {e}"))?;

    let Some(mut fresh) = tasks::get_task_in(&tx, task_id).map_err(|e| e.message())? else {
        // Deleted while the run was in flight. Go's UPDATE would match no row
        // and its store would report "not found"; there is nothing to write.
        return Ok(false);
    };

    fresh.run_count += 1;
    fresh.last_run_at = Some(crate::native::gotime::GoTime::from_utc(ran_at));
    fresh.last_run_status = status.to_string();

    // A one-shot task is parked after its run so a restart does not re-run it,
    // and any task is parked once it reaches its stop count.
    let pause =
        one_shot || (fresh.stop_after_count > 0 && fresh.run_count >= fresh.stop_after_count);
    if pause {
        fresh.status = "paused".to_string();
    }

    tasks::update_task_in(&tx, &mut fresh)?;
    tx.commit()
        .map_err(|e| format!("commit task update after run: {e}"))?;

    // `publishTaskFinished` reads these off the caller's copy after this
    // returns, so it reports what was stored rather than what was assumed.
    caller_copy.run_count = fresh.run_count;
    caller_copy.last_run_at = fresh.last_run_at;
    caller_copy.last_run_status = fresh.last_run_status;
    caller_copy.status = fresh.status;
    Ok(pause)
}

/// `recordFailedRun`: a job row that is created already finished, for a failure
/// that happened before there was a running row.
fn record_failed_run(
    scheduler: &Arc<Scheduler>,
    task: &mut ScheduledTask,
    started_at: chrono::DateTime<Utc>,
    chat_session_id: &str,
    error_message: &str,
) {
    let now = Utc::now();
    let job = JobHistory {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task.id.clone(),
        task_name: task.name.clone(),
        agent_slug: task.agent_slug.clone(),
        status: "failed".to_string(),
        started_at: crate::native::gotime::GoTime::from_utc(started_at),
        finished_at: Some(crate::native::gotime::GoTime::from_utc(now)),
        duration_ms: (now - started_at).num_milliseconds(),
        chat_session_id: chat_session_id.to_string(),
        // Go builds this row field by field and names neither, so both are the
        // zero value even though the task has a model and the prompt exists.
        model: String::new(),
        prompt_preview: String::new(),
        error_message: error_message.to_string(),
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_creation_tokens: 0,
        total_cache_read_tokens: 0,
        response_text: String::new(),
    };
    if let Err(e) = tasks::insert_job_history(scheduler.db_path(), &job) {
        log::error!(
            "failed to create failed job history task_id={:?} error={e}",
            task.id
        );
    }
    update_task_after_run(scheduler, task, started_at, "failed");
}

/// Hand a notification to the blocking pool and **do not wait for it**.
///
/// Go publishes to `eventbus`, which is a non-blocking channel send picked up by
/// one of three worker goroutines — a scheduled run never waits on SMTP. This is
/// the same shape, and both halves of it matter:
///
/// - **`spawn_blocking`**, because `smtp::send` is lettre's *blocking*
///   transport. Called inline it parks a tokio worker for up to the SMTP
///   timeout, and an unreachable mail host plus three finishing tasks starves
///   the proxy and every in-flight SSE chat stream on a four-core machine.
/// - **not awaited**, because the caller still holds the scheduler semaphore
///   permit. Awaiting would let a dead SMTP server throttle the scheduler to
///   three runs per timeout.
///
/// The payload is owned rather than borrowed for exactly that reason: it
/// outlives this call.
fn publish(db_path: &std::path::Path, event: &'static str, payload: Vec<(&'static str, String)>) {
    let db_path = db_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        notifications::handle(&db_path, event, &payload);
    });
}

/// `publishTaskFinished`. The keys are Go's, in Go's order — they become the
/// email body, one `key: value` per line.
fn publish_task_finished(
    db_path: &std::path::Path,
    task: &ScheduledTask,
    job: &JobHistory,
    chat_session_id: &str,
) {
    publish(
        db_path,
        notifications::event::TASK_FINISHED,
        vec![
            ("Task ID", task.id.clone()),
            ("Task Name", task.name.clone()),
            ("Task Description", task.description.clone()),
            ("Agent", task.agent_slug.clone()),
            ("Status", "Completed successfully".to_string()),
            ("Duration", format!("{} ms", job.duration_ms)),
            ("Run Count", task.run_count.to_string()),
            ("Model", job.model.clone()),
            ("Chat Session ID", chat_session_id.to_string()),
        ],
    );
}

/// `publishTaskFailed`.
fn publish_task_failed(db_path: &std::path::Path, task: &ScheduledTask, error_message: &str) {
    publish(
        db_path,
        notifications::event::TASK_FAILED,
        vec![
            ("Task ID", task.id.clone()),
            ("Task Name", task.name.clone()),
            ("Task Description", task.description.clone()),
            ("Agent", task.agent_slug.clone()),
            ("Status", "Failed".to_string()),
            ("Error", error_message.to_string()),
            ("Run Count", task.run_count.to_string()),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole of finding #1 on PR #365's second review: `chat_messages.id`
    /// is `INTEGER PRIMARY KEY AUTOINCREMENT`, so a supplied UUID is a
    /// `datatype mismatch` — and because the insert sits before the commit it
    /// took the session's own `UPDATE` down with it. Every finished run left an
    /// empty chat, no `sdk_session_id` and zero token totals, reported only as a
    /// `log::warn!`.
    ///
    /// Exercising the real statements against a migrated database is the point;
    /// nothing in the unit suite reached them before, which is exactly why this
    /// shipped.
    #[test]
    fn a_finished_run_persists_its_session_row_and_both_messages() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        drop(conn);

        let mut task = sample_task();
        task.name = "Nightly".to_string();
        let session_id = create_task_session(file.path(), &task).expect("create session");

        let result = RunResult {
            session_id: "sdk-session-9".to_string(),
            answer: "the answer".to_string(),
            input_tokens: 11,
            output_tokens: 22,
            cache_creation_tokens: 33,
            cache_read_tokens: 44,
        };
        let started_at = Utc::now();
        write_session_results(file.path(), &session_id, &result, "the prompt", started_at)
            .expect("the session write must not fail");

        let conn = rusqlite::Connection::open(file.path()).expect("reopen");
        let (title, sdk, input, output, creation, read): (String, String, i64, i64, i64, i64) =
            conn.query_row(
                "SELECT title, sdk_session_id, total_input_tokens, total_output_tokens,
                        total_cache_creation_tokens, total_cache_read_tokens
                 FROM chat_sessions WHERE id = ?1",
                [&session_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .expect("the session row");
        assert_eq!(title, "[Task] Nightly");
        assert_eq!(sdk, "sdk-session-9", "the run's transcript stays linked");
        assert_eq!((input, output, creation, read), (11, 22, 33, 44));

        let mut stmt = conn
            .prepare(
                "SELECT role, content, blocks FROM chat_messages
                 WHERE session_id = ?1 ORDER BY id",
            )
            .expect("prepare");
        let rows: Vec<(String, String, String)> = stmt
            .query_map([&session_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(rows.len(), 2, "the user turn and the assistant reply");
        assert_eq!(rows[0], ("user".into(), "the prompt".into(), "[]".into()));
        assert_eq!(
            rows[1],
            ("assistant".into(), "the answer".into(), "[]".into())
        );
    }

    #[test]
    fn a_run_that_produced_no_answer_updates_the_session_and_stores_no_messages() {
        // Go's `if result.Answer != ""`. The row still carries the sdk session
        // id, so the transcript is linked even when nothing was said.
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        drop(conn);

        let session_id = create_task_session(file.path(), &sample_task()).expect("session");
        let result = RunResult {
            session_id: "sdk-session-empty".to_string(),
            ..Default::default()
        };
        write_session_results(file.path(), &session_id, &result, "prompt", Utc::now())
            .expect("write");

        let conn = rusqlite::Connection::open(file.path()).expect("reopen");
        let messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1",
                [&session_id],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(messages, 0);
        let sdk: String = conn
            .query_row(
                "SELECT sdk_session_id FROM chat_sessions WHERE id = ?1",
                [&session_id],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(sdk, "sdk-session-empty");
    }

    #[test]
    fn the_job_history_rows_a_run_writes_are_readable_back() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO scheduled_tasks (id, name, prompt, created_at, updated_at)
             VALUES ('t1', 'T', 'p', '2026-01-01 00:00:00 +0000 UTC',
                     '2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed task");
        drop(conn);

        let mut task = sample_task();
        task.id = "t1".to_string();
        let started_at = Utc::now();
        let mut job =
            create_initial_job_history(file.path(), &task, started_at, "chat-1", "the prompt");
        assert_eq!(job.status, "running");

        let stored = tasks::get_job_history(file.path(), &job.id)
            .expect("read")
            .expect("the running row");
        assert_eq!(stored.status, "running");
        assert_eq!(stored.chat_session_id, "chat-1");
        assert!(stored.finished_at.is_none());

        let result = RunResult {
            input_tokens: 5,
            output_tokens: 6,
            ..Default::default()
        };
        finish_job_history(
            file.path(),
            &mut job,
            started_at,
            "success",
            "",
            Some(&result),
            "saved output",
        );

        let done = tasks::get_job_history(file.path(), &job.id)
            .expect("read")
            .expect("the finished row");
        assert_eq!(done.status, "success");
        assert_eq!(done.total_input_tokens, 5);
        assert_eq!(done.response_text, "saved output");
        assert!(done.finished_at.is_some());
        // The narrower UPDATE column list: the finish must not rewrite what the
        // insert recorded.
        assert_eq!(done.prompt_preview, "the prompt");
    }

    /// Finding #1 of PR #365's third review: a pause landed while the run was
    /// in flight must survive the run's own write-back.
    ///
    /// Go writes the stale snapshot back wholesale and clobbers it, and got
    /// away with it because nothing re-registered the cron entry. Here
    /// `reconcile` would reinstall the timer within a minute, so a "paused"
    /// task would go on firing.
    #[test]
    fn a_pause_that_lands_mid_run_survives_the_runs_own_write_back() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, name, prompt, schedule_type, schedule_config, status,
                 created_at, updated_at)
             VALUES ('t1','T','p','interval','{\"every_minutes\":5}','active',
                     '2026-01-01 00:00:00 +0000 UTC','2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed");
        drop(conn);

        // The snapshot the timer loaded, before the pause.
        let mut snapshot = sample_task();
        snapshot.id = "t1".to_string();
        snapshot.status = "active".to_string();

        // The user pauses while the run is in flight.
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute(
            "UPDATE scheduled_tasks SET status = 'paused' WHERE id = 't1'",
            [],
        )
        .expect("pause");
        drop(conn);

        let scheduler = test_scheduler(file.path());
        update_task_after_run(&scheduler, &mut snapshot, Utc::now(), "success");

        let stored = tasks::get_task(file.path(), "t1")
            .expect("read")
            .expect("row");
        assert_eq!(stored.status, "paused", "the run must not resurrect it");
        // …while the run's own fields are still recorded.
        assert_eq!(stored.run_count, 1);
        assert_eq!(stored.last_run_status, "success");
        assert!(stored.last_run_at.is_some());
    }

    #[test]
    fn a_one_shot_run_still_pauses_its_own_task() {
        // The other half of the same write: the run *does* own the pause when
        // it is the one imposing it.
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, name, prompt, schedule_type, schedule_config, status,
                 created_at, updated_at)
             VALUES ('t1','T','p','run_immediately','{}','active',
                     '2026-01-01 00:00:00 +0000 UTC','2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed");
        drop(conn);

        let mut snapshot = sample_task();
        snapshot.id = "t1".to_string();
        snapshot.schedule_type = "run_immediately".to_string();

        let scheduler = test_scheduler(file.path());
        update_task_after_run(&scheduler, &mut snapshot, Utc::now(), "success");

        let stored = tasks::get_task(file.path(), "t1")
            .expect("read")
            .expect("row");
        assert_eq!(stored.status, "paused", "a one-shot parks itself");
        assert_eq!(snapshot.status, "paused", "and the caller's copy agrees");
    }

    #[test]
    fn a_stop_after_count_run_pauses_on_the_limit() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, name, prompt, schedule_type, schedule_config, status,
                 run_count, stop_after_count, created_at, updated_at)
             VALUES ('t1','T','p','interval','{\"every_minutes\":5}','active',
                     1, 2, '2026-01-01 00:00:00 +0000 UTC','2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed");
        drop(conn);

        let mut snapshot = sample_task();
        snapshot.id = "t1".to_string();
        snapshot.run_count = 1;
        snapshot.stop_after_count = 2;

        let scheduler = test_scheduler(file.path());
        update_task_after_run(&scheduler, &mut snapshot, Utc::now(), "success");

        let stored = tasks::get_task(file.path(), "t1")
            .expect("read")
            .expect("row");
        assert_eq!(stored.run_count, 2);
        assert_eq!(stored.status, "paused", "the second run hits the limit");
    }

    #[test]
    fn a_task_deleted_mid_run_is_not_recreated_by_the_write_back() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        drop(conn);

        let mut snapshot = sample_task();
        snapshot.id = "gone".to_string();
        let scheduler = test_scheduler(file.path());
        // No row, no panic, no resurrection.
        update_task_after_run(&scheduler, &mut snapshot, Utc::now(), "success");
        assert!(tasks::get_task(file.path(), "gone")
            .expect("read")
            .is_none());
    }

    /// Finding #1 of PR #365's fourth review: a failed write-back must not
    /// leave a one-shot task timer-less, `active` **and** forgotten by the
    /// sweep, or `reconcile` reinstalls the timer, `run_immediately` fires two
    /// seconds later, the write fails again — a full agent run every minute,
    /// unbounded. A read-only data dir reaches it.
    ///
    /// Asserted through the observable consequence: after a failed write the
    /// task must still be known to the scheduler, so the sweep leaves it alone.
    #[tokio::test]
    async fn a_failed_write_back_does_not_hand_a_one_shot_task_to_the_sweep() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, name, prompt, schedule_type, schedule_config, status,
                 created_at, updated_at)
             VALUES ('t1','T','p','run_immediately','{}','active',
                     '2026-01-01 00:00:00 +0000 UTC','2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed");
        drop(conn);

        let scheduler = test_scheduler(&db);
        let task = tasks::get_task(&db, "t1").expect("read").expect("row");
        scheduler.schedule_task(&task).expect("schedule");
        assert!(scheduler.knows_task("t1"), "the sweep has seen it");

        // The write fails: the database is gone underneath the run.
        std::fs::remove_file(&db).expect("remove");
        let mut snapshot = task.clone();
        update_task_after_run(&scheduler, &mut snapshot, Utc::now(), "success");

        assert!(
            scheduler.knows_task("t1"),
            "a failed write must not forget the task; the sweep would reinstall \
             its timer and it would run again every minute"
        );
    }

    #[tokio::test]
    async fn a_successful_one_shot_write_back_does_release_the_task() {
        // The other direction: when the row really is paused, the timer goes
        // and the sweep may forget it — a later resume schedules it afresh.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, name, prompt, schedule_type, schedule_config, status,
                 created_at, updated_at)
             VALUES ('t1','T','p','run_immediately','{}','active',
                     '2026-01-01 00:00:00 +0000 UTC','2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed");
        drop(conn);

        let scheduler = test_scheduler(&db);
        let task = tasks::get_task(&db, "t1").expect("read").expect("row");
        scheduler.schedule_task(&task).expect("schedule");

        let mut snapshot = task.clone();
        update_task_after_run(&scheduler, &mut snapshot, Utc::now(), "success");

        assert_eq!(
            tasks::get_task(&db, "t1")
                .expect("read")
                .expect("row")
                .status,
            "paused"
        );
        assert!(!scheduler.knows_task("t1"), "the timer is released");
    }

    #[test]
    fn the_prompt_preview_cuts_at_200_bytes_and_never_splits_a_character() {
        assert_eq!(prompt_preview("short"), "short");

        let exactly = "a".repeat(200);
        assert_eq!(
            prompt_preview(&exactly),
            exactly,
            "200 is not over the limit"
        );

        let over = "a".repeat(201);
        assert_eq!(prompt_preview(&over), format!("{}...", "a".repeat(200)));

        // A three-byte character straddling byte 200: Go would slice through it
        // and store invalid UTF-8; the cut moves back to the boundary instead.
        let straddling = format!("{}€€€", "a".repeat(199));
        let preview = prompt_preview(&straddling);
        assert!(preview.ends_with("..."), "{preview}");
        assert_eq!(
            preview.trim_end_matches("...").len(),
            199,
            "cut back to the boundary rather than through the character"
        );
    }

    #[test]
    fn a_synthesized_agent_carries_the_tasks_model_and_adaptive_thinking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("missing.db");
        let mut task = sample_task();
        task.model = "opus".to_string();

        let agent = resolve_agent(&db, &task).expect("no slug is never an error");
        assert_eq!(agent.model, "opus");
        assert_eq!(agent.thinking, "adaptive");
        // Empty capabilities, which is what gives a no-agent task all twelve
        // built-in tools rather than none.
        assert!(agent.capabilities.built_in.is_none());
        assert!(
            agent.permission_mode.is_empty(),
            "bypass by falling through"
        );
    }

    #[test]
    fn stop_conditions_are_checked_before_a_run_not_after() {
        let mut task = sample_task();
        assert!(!should_auto_pause(&task), "no limits set");

        task.stop_after_count = 3;
        task.run_count = 2;
        assert!(!should_auto_pause(&task), "one run left");
        task.run_count = 3;
        assert!(should_auto_pause(&task), "limit reached");

        // A zero count is "no limit", not "stop immediately".
        task.stop_after_count = 0;
        assert!(!should_auto_pause(&task));

        task.stop_after_time = Some(crate::native::gotime::GoTime::from_utc(
            Utc::now() - chrono::Duration::hours(1),
        ));
        assert!(should_auto_pause(&task));
        task.stop_after_time = Some(crate::native::gotime::GoTime::from_utc(
            Utc::now() + chrono::Duration::hours(1),
        ));
        assert!(!should_auto_pause(&task));
    }

    fn test_scheduler(db_path: &std::path::Path) -> Arc<Scheduler> {
        super::super::runtime::detached(db_path)
    }

    fn sample_task() -> ScheduledTask {
        ScheduledTask {
            id: "t1".to_string(),
            name: "nightly".to_string(),
            description: String::new(),
            prompt: "go".to_string(),
            agent_slug: String::new(),
            working_directory: String::new(),
            model: String::new(),
            settings_profile_id: String::new(),
            timeout_minutes: 30,
            schedule_type: "interval".to_string(),
            schedule_config: Default::default(),
            stop_after_count: 0,
            stop_after_time: None,
            save_output: false,
            status: "active".to_string(),
            run_count: 0,
            last_run_at: None,
            last_run_status: String::new(),
            next_run_at: None,
            created_at: Default::default(),
            updated_at: Default::default(),
        }
    }
}

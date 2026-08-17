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
use crate::native::agents::{self, Agent};
use crate::native::chat::runner::{self, RunSpec, TurnSettings};
use crate::native::notifications;
use crate::native::tasks::{self, JobHistory, ScheduledTask};
use crate::native::template;

/// What one run produced. `agent.AgentResult`, narrowed to the fields the
/// scheduler stores — the thinking, cost and per-model breakdowns are collected
/// by Go and then dropped by this caller.
#[derive(Debug, Default)]
struct RunResult {
    session_id: String,
    answer: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
}

/// `executeTask`. The semaphore is the caller's; this is everything inside it.
pub async fn execute_task(scheduler: &Arc<Scheduler>, task_id: &str) {
    let db_path = scheduler.db_path().to_path_buf();

    let task = match tasks::get_task(&db_path, task_id) {
        Ok(Some(task)) => task,
        // A task that vanished between the fire and the read. Go returns
        // silently; so does this — there is no row to attach a failure to.
        Ok(None) => return,
        Err(e) => {
            log::error!("failed to load task for execution task_id={task_id:?} error={e}");
            return;
        }
    };
    if task.status != "active" {
        return;
    }

    if should_auto_pause(&task) {
        let reason = if task.stop_after_count > 0 && task.run_count >= task.stop_after_count {
            "stop_after_count reached"
        } else {
            "stop_after_time reached"
        };
        auto_pause(scheduler, task, reason);
        return;
    }

    log::info!(
        "executing task task_id={:?} task_name={:?} run_count={}",
        task.id,
        task.name,
        task.run_count + 1
    );
    run_task(scheduler, task).await;
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
async fn run_task(scheduler: &Arc<Scheduler>, mut task: ScheduledTask) {
    let db_path = scheduler.db_path().to_path_buf();
    let started_at = Utc::now();

    // `prepareTaskRun`. Both failures record a *complete* failed job row and
    // return — the run never reaches `createInitialJobHistory`, so there is no
    // running row to finish.
    let prompt = match template::interpolate(&task.prompt) {
        Ok(prompt) => prompt,
        Err(e) => {
            let msg = format!("prompt interpolation: {e}");
            log::error!(
                "failed to interpolate prompt task_id={:?} error={e}",
                task.id
            );
            record_failed_run(scheduler, &mut task, started_at, "", &msg);
            publish_task_failed(&db_path, &task, &msg);
            return;
        }
    };

    let chat_session_id = match create_task_session(&db_path, &task) {
        Ok(id) => id,
        Err(e) => {
            let msg = format!("create session: {e}");
            log::error!(
                "failed to create chat session task_id={:?} error={e}",
                task.id
            );
            record_failed_run(scheduler, &mut task, started_at, "", &msg);
            publish_task_failed(&db_path, &task, &msg);
            return;
        }
    };

    let mut job =
        create_initial_job_history(&db_path, &task, started_at, &chat_session_id, &prompt);

    // `resolveAgentConfig`. From here on there *is* a running row, so every
    // failure finishes it rather than creating a second.
    let agent = match resolve_agent(&db_path, &task) {
        Ok(agent) => agent,
        Err(e) => {
            let msg = format!("resolve agent: {e}");
            log::error!(
                "failed to resolve agent config task_id={:?} error={e}",
                task.id
            );
            finish_job_history(&db_path, &mut job, started_at, "failed", &msg, None, "");
            update_task_after_run(scheduler, &mut task, started_at, "failed");
            publish_task_failed(&db_path, &task, &msg);
            return;
        }
    };

    let result = run_agent(&db_path, &task, agent, &prompt).await;
    let result = match result {
        Ok(result) => result,
        Err(e) => {
            log::error!("task execution failed task_id={:?} error={e}", task.id);
            finish_job_history(&db_path, &mut job, started_at, "failed", &e, None, "");
            update_task_after_run(scheduler, &mut task, started_at, "failed");
            publish_task_failed(&db_path, &task, &e);
            return;
        }
    };

    save_session_results(&db_path, &chat_session_id, &result, &prompt, started_at);
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
    publish_task_finished(&db_path, &task, &job, &chat_session_id);

    log::info!(
        "task execution completed task_id={:?} task_name={:?} session_id={:?} run_count={}",
        task.id,
        task.name,
        chat_session_id,
        task.run_count
    );
}

/// `createTaskSession`: the chat row a run's messages land in, titled after the
/// task.
///
/// Go creates the row and then updates it with the title, which is two writes
/// and one visible intermediate state. This does it in one transaction with the
/// title already set — the row Go leaves behind is identical, and the run is the
/// only reader of it in between.
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

    let title = format!("[Task] {}", task.name);
    tx.execute(
        "UPDATE chat_sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![title, crate::native::gotime::now_go_text(), session.id],
    )
    .map_err(|e| format!("updating session title: {e}"))?;

    tx.commit()
        .map_err(|e| format!("commit task session: {e}"))?;
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

/// `agent.RunAgent` + `collectRunResult`, for a run with no interactive
/// permission handler and no SSE.
///
/// The timeout is `context.WithTimeout(parentCtx, task.TimeoutMinutes)`. It
/// wraps the *whole* drain, so a subprocess that stops producing events is
/// abandoned rather than waited on forever, and the session is closed on the way
/// out either way.
async fn run_agent(
    db_path: &std::path::Path,
    task: &ScheduledTask,
    agent: Agent,
    prompt: &str,
) -> Result<RunResult, String> {
    let settings = Arc::new(TurnSettings::from_db(db_path));
    let spec = RunSpec {
        agent: Some(agent),
        // Never called: `resolve_agent` always yields an agent, because Go's
        // scheduler always has a non-nil config. Present because `RunSpec`
        // models the chat's branch too.
        no_agent_model: Box::new(String::new),
        settings,
        working_dir: task.working_directory.clone(),
        settings_profile_id: task.settings_profile_id.clone(),
        resume_session_id: None,
        // `buildRunOptions` sets neither session field, so the CLI picks its own
        // id and `saveSessionResults` stores it afterwards.
        custom_session_id: String::new(),
    };

    // The refusal this port has that Go does not — an agent whose tools cannot
    // be hosted here. A recorded failure, never silence. See the module header.
    let (options, tool_servers) = runner::build_options(&spec, None)
        .await
        .map_err(|e| format!("agent tools unavailable in this build: {e}"))?;

    let timeout = std::time::Duration::from_secs(
        u64::try_from(task.timeout_minutes.max(0)).unwrap_or(0) * 60,
    );

    let mut session = crate::claude::session::Session::new(options)
        .await
        .map_err(|e| format!("starting agent: {e}"))?;
    if let Err(e) = session.send(prompt).await {
        session.close();
        drop(tool_servers);
        return Err(format!("starting agent: sending prompt: {e}"));
    }

    let collected = tokio::time::timeout(timeout, collect_run_result(&mut session)).await;
    session.close();
    // The in-process tool listeners outlive the subprocess and stop when
    // dropped, so this is after `close` rather than before it.
    drop(tool_servers);

    match collected {
        Ok(result) => result,
        // `context.DeadlineExceeded` reaches Go's caller as the run error too.
        Err(_) => Err("context deadline exceeded".to_string()),
    }
}

/// `collectRunResult`: drain every event, keep the last result.
///
/// The drain does **not** stop at the result event, and that is Go's comment
/// rather than an accident: the subprocess still has its transcript to write,
/// and returning early would race the scanner against a half-written file.
async fn collect_run_result(
    session: &mut crate::claude::session::Session,
) -> Result<RunResult, String> {
    let mut result: Option<RunResult> = None;
    let mut result_err: Option<String> = None;

    while let Some(event) = session.next_event().await {
        let Some(r) = event.result.as_ref() else {
            continue;
        };
        if r.is_error {
            result_err = Some(build_result_error(r));
        } else {
            result = Some(RunResult {
                session_id: r.session_id.clone(),
                answer: r.result.clone(),
                input_tokens: r.usage.input_tokens,
                output_tokens: r.usage.output_tokens,
                cache_creation_tokens: r.usage.cache_creation_input_tokens,
                cache_read_tokens: r.usage.cache_read_input_tokens,
            });
        }
    }

    if let Some(err) = result_err {
        return Err(err);
    }
    result.ok_or_else(|| "agent finished without returning a result".to_string())
}

/// `buildResultError`, message for message.
fn build_result_error(r: &crate::claude::messages::Result) -> String {
    let mut msg = r.result.clone();
    if msg.is_empty() && !r.errors.is_empty() {
        msg = r.errors.join("; ");
    }
    if msg.is_empty() {
        msg = format!("subtype={}", r.subtype);
    }
    format!("agent error: {msg}")
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
    let mut conn = crate::native::db::open_read_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin session results: {e}"))?;

    tx.execute(
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
        append_message(
            &tx,
            chat_session_id,
            "user",
            prompt,
            &crate::native::gotime::to_go_string_utc(crate::native::gotime::GoTime::from_utc(
                started_at,
            )),
        )?;
        append_message(
            &tx,
            chat_session_id,
            "assistant",
            &result.answer,
            &crate::native::gotime::now_go_text(),
        )?;
    }

    tx.commit()
        .map_err(|e| format!("commit session results: {e}"))
}

/// `ChatStore.AppendMessage` for a plain text turn.
fn append_message(
    tx: &rusqlite::Transaction,
    chat_session_id: &str,
    role: &str,
    content: &str,
    timestamp: &str,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO chat_messages (id, session_id, role, content, blocks, timestamp)
         VALUES (?1, ?2, ?3, ?4, '', ?5)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            chat_session_id,
            role,
            content,
            timestamp,
        ],
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
fn update_task_after_run(
    scheduler: &Arc<Scheduler>,
    task: &mut ScheduledTask,
    ran_at: chrono::DateTime<Utc>,
    status: &str,
) {
    task.run_count += 1;
    task.last_run_at = Some(crate::native::gotime::GoTime::from_utc(ran_at));
    task.last_run_status = status.to_string();

    // A one-shot task is paused after its run so a restart does not re-run it —
    // the timer is already exhausted, but the *row* is what `Start` reads.
    if task.schedule_type == "one_off" || task.schedule_type == "run_immediately" {
        task.status = "paused".to_string();
        scheduler.unschedule_task(&task.id);
    }
    if task.stop_after_count > 0 && task.run_count >= task.stop_after_count {
        task.status = "paused".to_string();
        scheduler.unschedule_task(&task.id);
    }

    if let Err(e) = tasks::update_task_row(scheduler.db_path(), task) {
        log::error!(
            "failed to update task after run task_id={:?} error={e}",
            task.id
        );
    }
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

/// `publishTaskFinished`. The keys are Go's, in Go's order — they become the
/// email body, one `key: value` per line.
fn publish_task_finished(
    db_path: &std::path::Path,
    task: &ScheduledTask,
    job: &JobHistory,
    chat_session_id: &str,
) {
    notifications::handle(
        db_path,
        notifications::event::TASK_FINISHED,
        &[
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
    notifications::handle(
        db_path,
        notifications::event::TASK_FAILED,
        &[
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
    fn a_result_error_prefers_the_message_then_the_errors_then_the_subtype() {
        let mut r = crate::claude::messages::Result {
            subtype: "error_max_turns".to_string(),
            ..Default::default()
        };
        assert_eq!(
            build_result_error(&r),
            "agent error: subtype=error_max_turns"
        );

        r.errors = vec!["one".to_string(), "two".to_string()];
        assert_eq!(build_result_error(&r), "agent error: one; two");

        r.result = "the message".to_string();
        assert_eq!(build_result_error(&r), "agent error: the message");
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

//! A whole scheduled run, driven against a **scripted fake CLI** (#275).
//!
//! Everything else about the scheduler is tested a layer at a time: the fire
//! times against `scheduler_vectors.json`, the row writes against a temp
//! database, the HTTP answers against a live Go server. None of that answers the
//! question a user actually has — *does a scheduled task run?* — because the
//! answer depends on the pieces meeting: `build_options` with **no** permission
//! handler and no pinned session id, a real subprocess, `collect_run_result`
//! draining it, and five separate writes landing in three tables.
//!
//! That gap is not hypothetical. The first version of this port supplied a UUID
//! for `chat_messages.id`, which is `INTEGER PRIMARY KEY AUTOINCREMENT`; every
//! successful run rolled its whole session transaction back and reported it as
//! one `log::warn`. Unit tests over the pieces all passed.
//!
//! The fake is a small Python program, the same technique `chat_turn.rs` uses:
//! no `claude` binary, no API key, runs in CI like any other test.

use std::path::{Path, PathBuf};

/// `AGENTO_CLAUDE_EXECUTABLE` is process-wide, so the tests that set it are
/// serialized against each other.
fn env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn python3() -> Option<String> {
    for candidate in ["python3", "python"] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// A CLI that acknowledges `initialize` and then emits `emit` verbatim on the
/// first user message.
///
/// It records what it was started with beside itself — its cwd in `cwd`, its
/// argv in `argv` (read back by [`argv`]) and the `initialize` request in
/// `initialize`, which is where the system prompt travels (#629).
fn fake_cli(dir: &Path, emit: &str) -> PathBuf {
    let script = format!(
        r#"#!/usr/bin/env {python}
import json, os, sys

with open({cwd}, "w") as _cwd:
    _cwd.write(os.getcwd())
with open({argv}, "w") as _argv:
    _argv.write(json.dumps(sys.argv))

def say(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def raw(line):
    sys.stdout.write(line + "\n")
    sys.stdout.flush()

def ack(request_id):
    say({{"type": "control_response",
         "response": {{"subtype": "success", "request_id": request_id,
                       "response": {{"models": [{{"value": "fake", "displayName": "Fake"}}],
                                     "account": {{"apiProvider": "fake"}},
                                     "output_style": "default"}}}}}})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    req = msg.get("request") or {{}}
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        with open({initialize}, "w") as _init:
            _init.write(json.dumps(req))
        ack(req.get("request_id") or msg.get("request_id"))
        continue
    if msg.get("type") == "user":
{emit}
        # **Deliberately no exit.** A real CLI in session mode stays alive for
        # the next send — that is what `session_mode` means — so a fake that
        # exited here would close stdout, end the event stream for free, and
        # give a false green to a drain that never terminates on its own.
        continue
"#,
        python = python3().unwrap_or_else(|| "python3".into()),
        cwd = serde_json::to_string(&dir.join("cwd").to_string_lossy()).unwrap(),
        argv = serde_json::to_string(&dir.join("argv").to_string_lossy()).unwrap(),
        initialize = serde_json::to_string(&dir.join("initialize").to_string_lossy()).unwrap(),
        emit = emit,
    );
    let path = dir.join("fake-claude");
    std::fs::write(&path, script).expect("write fake CLI");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake CLI");
    }
    path
}

/// The argv the fake CLI was last started with, program name first.
fn argv(dir: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(dir.join("argv")).expect("the CLI ran");
    serde_json::from_str(&raw).expect("argv is a JSON list")
}

/// The value following `flag` in `argv`, or `None` when the flag is absent.
fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let i = argv.iter().position(|a| a == flag)?;
    argv.get(i + 1).map(String::as_str)
}

/// A database with one active task, ready to fire.
///
/// The task has **no agent** — `agent_slug` is left at its `''` default — so
/// every test built on it runs through the executor's stand-in agent.
/// `a_task_with_no_agent_runs_on_the_default_model_with_every_built_in_tool`
/// is the one that pins what that stand-in actually spawns.
fn migrated_with_task(path: &Path, schedule_type: &str, save_output: bool) -> String {
    let mut conn = rusqlite::Connection::open(path).expect("open");
    agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO scheduled_tasks
            (id, name, description, prompt, schedule_type, schedule_config, status,
             timeout_minutes, save_output, created_at, updated_at)
         VALUES ('task-1', 'Nightly', 'd', 'summarise the day', ?1, '{}', 'active',
                 30, ?2, '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        rusqlite::params![schedule_type, save_output],
    )
    .expect("seed task");
    "task-1".to_string()
}

/// A database with one active task, ready to fire, plus optional overrides.
fn migrated_with(
    path: &Path,
    prompt: &str,
    agent_slug: &str,
    timeout_minutes: i64,
    agent_capabilities: Option<&str>,
) -> String {
    let mut conn = rusqlite::Connection::open(path).expect("open");
    agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    if let Some(caps) = agent_capabilities {
        conn.execute(
            "INSERT INTO agents (slug, name, capabilities) VALUES (?1, 'A', ?2)",
            rusqlite::params![agent_slug, caps],
        )
        .expect("seed agent");
    }
    conn.execute(
        "INSERT INTO scheduled_tasks
            (id, name, description, prompt, agent_slug, schedule_type, schedule_config,
             status, timeout_minutes, save_output, created_at, updated_at)
         VALUES ('task-1', 'Nightly', 'd', ?1, ?2, 'cron', '{}', 'active', ?3, 1,
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        rusqlite::params![prompt, agent_slug, timeout_minutes],
    )
    .expect("seed task");
    "task-1".to_string()
}

fn session_count(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.query_row("SELECT COUNT(*) FROM chat_sessions", [], |r| r.get(0))
        .expect("count")
}

fn job_rows(path: &Path) -> Vec<(String, String, String, i64, i64)> {
    let conn = rusqlite::Connection::open(path).expect("open");
    let mut stmt = conn
        .prepare(
            "SELECT status, error_message, response_text, total_input_tokens,
                    total_output_tokens
             FROM job_history ORDER BY started_at",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

#[tokio::test]
async fn a_scheduled_run_records_a_successful_job_and_persists_its_chat() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);

    let cli = fake_cli(
        dir.path(),
        r#"        raw('{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}')
        raw('{"type":"result","subtype":"success","is_error":false,"result":"the summary","session_id":"sdk-run-1","usage":{"input_tokens":11,"output_tokens":22,"cache_creation_input_tokens":3,"cache_read_input_tokens":4}}')"#,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    // The job history says the run happened, and carries the usage.
    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "exactly one run: {jobs:?}");
    let (status, error, response, input, output) = &jobs[0];
    assert_eq!(status, "success", "error was {error:?}");
    assert!(error.is_empty());
    assert_eq!(response, "the summary", "save_output stores the answer");
    assert_eq!((*input, *output), (11, 22));

    // The chat the run created carries the CLI's own session id — the link to
    // the transcript — and both turns.
    let conn = rusqlite::Connection::open(&db).expect("open");
    let (title, sdk): (String, String) = conn
        .query_row("SELECT title, sdk_session_id FROM chat_sessions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("the session row");
    assert_eq!(title, "[Task] Nightly");
    assert_eq!(sdk, "sdk-run-1");

    let mut stmt = conn
        .prepare("SELECT role, content FROM chat_messages ORDER BY id")
        .expect("prepare");
    let messages: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(
        messages,
        vec![
            ("user".to_string(), "summarise the day".to_string()),
            ("assistant".to_string(), "the summary".to_string()),
        ]
    );

    // …and the task's own counters moved.
    let task = agento_lib::native::tasks::get_task(&db, &task_id)
        .expect("read")
        .expect("row");
    assert_eq!(task.run_count, 1);
    assert_eq!(task.last_run_status, "success");
    assert!(task.last_run_at.is_some());
    assert_eq!(task.status, "active", "a cron task keeps running");
}

/// #559, the executor's half: a task with no `working_directory` runs the CLI
/// in the settings default, not in whatever directory this process has.
///
/// The default is pointed at a directory that does **not** exist yet, because
/// nothing else creates `<temp>/agento/work` on a fresh install — a spawn into
/// a missing cwd would fail the run rather than merely misplace it.
#[tokio::test]
async fn a_task_with_no_working_directory_runs_in_the_settings_default() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);
    let default = dir.path().join("settings-default").join("work");
    rusqlite::Connection::open(&db)
        .expect("open")
        .execute(
            "INSERT INTO user_settings (id, default_working_dir) VALUES (1, ?1)",
            [default.to_string_lossy()],
        )
        .expect("seed settings");

    let cli = fake_cli(
        dir.path(),
        r#"        raw('{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"sdk-cwd","usage":{"input_tokens":1,"output_tokens":1}}')"#,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    // The stored rung is the subject; a developer's own export would win.
    std::env::remove_var("AGENTO_WORKING_DIR");

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "exactly one run: {jobs:?}");
    assert_eq!(jobs[0].0, "success", "error was {:?}", jobs[0].1);
    assert!(default.is_dir(), "the default directory was created");
    let recorded = std::fs::read_to_string(dir.path().join("cwd")).expect("the CLI ran");
    // Canonicalised on both sides: macOS's temp dir is behind a symlink, and
    // `os.getcwd()` answers the resolved path.
    assert_eq!(
        std::fs::canonicalize(recorded).expect("recorded cwd"),
        std::fs::canonicalize(&default).expect("default dir"),
        "the task ran outside the settings default"
    );
}

/// #541, end to end: a **paused** task, sitting **at** its `stop_after_count`,
/// run on demand.
///
/// Both halves of the acceptance criteria in one pass, because they pull
/// against each other: the run must be as complete as a scheduled one (a
/// `job_history` row with the answer and the token counts, a persisted chat)
/// while changing nothing the schedule owns. The fixture is the state a timer
/// refuses outright — `execute_task` on this row returns silently, which the
/// first assertion pins — so nothing here can pass by accident on the scheduled
/// path.
#[tokio::test]
async fn a_manual_run_fires_a_paused_task_at_its_limit_and_moves_no_counter() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);
    {
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute(
            "UPDATE scheduled_tasks
                SET status = 'paused', run_count = 2, stop_after_count = 2
              WHERE id = ?1",
            [&task_id],
        )
        .expect("park the task on its limit");
    }

    let cli = fake_cli(
        dir.path(),
        r#"        raw('{"type":"result","subtype":"success","is_error":false,"result":"tried it","session_id":"sdk-manual-1","usage":{"input_tokens":5,"output_tokens":6}}')"#,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);

    // The timer path declines this row, silently and with no row — which is
    // exactly the gap the manual route exists to fill.
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;
    assert!(
        job_rows(&db).is_empty(),
        "a paused task is not due; nothing may be recorded for it"
    );

    let guard = scheduler
        .try_mark_running(&task_id)
        .expect("nothing is in flight");
    assert!(
        scheduler.try_mark_running(&task_id).is_none(),
        "and a second claim on the same task is refused, which is the route's 409"
    );
    agento_lib::native::schedule::executor::run_manual(
        std::sync::Arc::clone(&scheduler),
        agento_lib::native::tasks::get_task(&db, &task_id)
            .expect("read")
            .expect("row"),
        "job-manual-1".to_string(),
        guard,
    )
    .await;

    // As useful as a scheduled run's: status, output and usage all present,
    // under the id the route would have answered with.
    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "the manual run is recorded: {jobs:?}");
    let (status, error, response, input, output) = &jobs[0];
    assert_eq!(status, "success", "error was {error:?}");
    assert_eq!(response, "tried it");
    assert_eq!((*input, *output), (5, 6));
    {
        let conn = rusqlite::Connection::open(&db).expect("open");
        let id: String = conn
            .query_row("SELECT id FROM job_history", [], |r| r.get(0))
            .expect("the job row");
        assert_eq!(id, "job-manual-1", "the route's id is the row's id");
    }
    assert_eq!(session_count(&db), 1, "and the chat is persisted");

    // …and the schedule is exactly where it was.
    let task = agento_lib::native::tasks::get_task(&db, &task_id)
        .expect("read")
        .expect("row");
    assert_eq!(task.run_count, 2, "no budget spent");
    assert_eq!(task.status, "paused", "still paused; nothing resumed it");
    assert!(task.last_run_at.is_none(), "not the schedule's last run");
    assert!(task.last_run_status.is_empty());
    assert!(task.next_run_at.is_none(), "the next fire has not moved");

    // The guard went with the run, so the task is runnable again.
    assert!(!scheduler.is_running(&task_id));
}

/// #629: what a task with **no agent** actually runs with, through the
/// `POST /api/tasks/{id}/run` path.
///
/// The other tests here already run no-agent tasks, but only assert that they
/// succeed. This one pins what the executor's stand-in agent hands the CLI: the
/// Settings default model, every built-in tool, no system prompt, no MCP
/// servers, and permissions bypassed. A `resolve_agent` that answered `None`
/// instead would drop `--allowedTools` altogether and still pass the others.
#[tokio::test]
async fn a_task_with_no_agent_runs_on_the_default_model_with_every_built_in_tool() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);
    rusqlite::Connection::open(&db)
        .expect("open")
        .execute(
            "INSERT INTO user_settings (id, default_model) VALUES (1, 'opus')",
            [],
        )
        .expect("seed settings");

    let cli = fake_cli(
        dir.path(),
        r#"        raw('{"type":"result","subtype":"success","is_error":false,"result":"no agent needed","session_id":"sdk-no-agent","usage":{"input_tokens":1,"output_tokens":1}}')"#,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    // The stored model is the subject; either export beats it in
    // `settings::resolve`.
    std::env::remove_var("AGENTO_DEFAULT_MODEL");
    std::env::remove_var("ANTHROPIC_DEFAULT_SONNET_MODEL");

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    let guard = scheduler
        .try_mark_running(&task_id)
        .expect("nothing is in flight");
    agento_lib::native::schedule::executor::run_manual(
        std::sync::Arc::clone(&scheduler),
        agento_lib::native::tasks::get_task(&db, &task_id)
            .expect("read")
            .expect("row"),
        "job-no-agent".to_string(),
        guard,
    )
    .await;

    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "the run is recorded: {jobs:?}");
    assert_eq!(jobs[0].0, "success", "error was {:?}", jobs[0].1);
    {
        let conn = rusqlite::Connection::open(&db).expect("open");
        let job_agent: String = conn
            .query_row("SELECT agent_slug FROM job_history", [], |r| r.get(0))
            .expect("the job row");
        assert_eq!(job_agent, "", "the job names no agent");
        let chat_agent: String = conn
            .query_row("SELECT agent_slug FROM chat_sessions", [], |r| r.get(0))
            .expect("the session row");
        assert_eq!(chat_agent, "", "nor does the chat it created");
    }

    let argv = argv(dir.path());
    assert_eq!(
        flag_value(&argv, "--model"),
        Some("opus"),
        "the Settings default model: {argv:?}"
    );
    assert_eq!(
        flag_value(&argv, "--allowedTools"),
        Some(
            "Read,Write,Edit,Bash,Glob,Grep,WebFetch,WebSearch,Task,TaskOutput,TaskStop,NotebookEdit"
        ),
        "every built-in tool, in `ALL_BUILT_IN_TOOLS` order: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--mcp-config"),
        "no MCP servers: {argv:?}"
    );
    assert!(
        !argv
            .iter()
            .any(|a| a == "--system-prompt" || a == "--append-system-prompt"),
        "no system prompt flag: {argv:?}"
    );
    // An empty `permission_mode` falls to the bypass arm of `build_options`.
    assert_eq!(
        flag_value(&argv, "--permission-mode"),
        Some("bypassPermissions"),
        "{argv:?}"
    );
    assert!(
        argv.iter()
            .any(|a| a == "--allow-dangerously-skip-permissions"),
        "{argv:?}"
    );

    // The system prompt rides the `initialize` request, not the argv, so that
    // is where its absence has to be read.
    let init: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("initialize")).expect("initialized"),
    )
    .expect("initialize is JSON");
    assert_eq!(init["systemPrompt"], "", "no system prompt: {init}");
    assert_eq!(
        init["appendSystemPrompt"], "",
        "nor an appended one: {init}"
    );
}

/// #541: a task deleted while its manual run waited for a permit is not run.
///
/// The wait is for one of three slots that a 240-minute run can hold, so this
/// window is not the timer's milliseconds — and `job_history.task_id` cascades
/// from `scheduled_tasks`, so running anyway would spawn the agent and then
/// fail to insert the row that explains it. `execute_task` gets this from
/// `due_task`'s vanished-row arm; `run_manual` skips `due_task` and has to
/// re-read for itself.
#[tokio::test]
async fn a_manual_run_whose_task_was_deleted_while_it_queued_does_not_run() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);

    let cli = fake_cli(
        dir.path(),
        r#"        raw('{"type":"result","subtype":"success","is_error":false,"result":"ran anyway","session_id":"sdk-ghost","usage":{}}')"#,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    let task = agento_lib::native::tasks::get_task(&db, &task_id)
        .expect("read")
        .expect("row");
    let guard = scheduler
        .try_mark_running(&task_id)
        .expect("nothing is in flight");

    // The row goes after the route read it and before the run reaches the
    // database — which is exactly what the permit wait makes reachable.
    {
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute("DELETE FROM scheduled_tasks WHERE id = ?1", [&task_id])
            .expect("delete");
    }

    agento_lib::native::schedule::executor::run_manual(
        std::sync::Arc::clone(&scheduler),
        task,
        "job-ghost".to_string(),
        guard,
    )
    .await;

    assert!(
        job_rows(&db).is_empty(),
        "nothing ran, so there is nothing to record"
    );
    assert_eq!(
        session_count(&db),
        0,
        "and no chat session was created for a task that is gone"
    );
    assert!(!scheduler.is_running(&task_id), "the guard was released");
}

#[tokio::test]
async fn an_error_result_is_a_failed_job_with_gos_wording() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "run_immediately", false);

    let cli = fake_cli(
        dir.path(),
        r#"        raw('{"type":"result","subtype":"error_during_execution","is_error":true,"result":"it broke","session_id":"sdk-run-2","usage":{}}')"#,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "the failure is recorded, not swallowed");
    let (status, error, response, _, _) = &jobs[0];
    assert_eq!(status, "failed");
    // `buildResultError`: the message wins over the subtype.
    assert_eq!(error, "agent error: it broke");
    assert!(response.is_empty());

    // A `run_immediately` task parks itself after its run, whatever the outcome.
    let task = agento_lib::native::tasks::get_task(&db, &task_id)
        .expect("read")
        .expect("row");
    assert_eq!(task.status, "paused");
    assert_eq!(task.run_count, 1);
    assert_eq!(task.last_run_status, "failed");
}

/// The rule the whole executor is written around: a run this build **cannot**
/// serve is a recorded failure, never silence.
///
/// With the sidecar started `AGENTO_SCHEDULER=off` there is no second
/// implementation behind a fire, so a job history with no row would be
/// indistinguishable from a task that was not due. `build_options` still
/// refuses an agent naming an MCP server nothing resolves — here one with no
/// integration row and no `mcps.yaml` entry (#375 made the second half of that
/// resolvable; a name in neither is still a refusal).
#[tokio::test]
async fn an_agent_whose_tools_this_build_cannot_host_is_a_recorded_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with(
        &db,
        "go",
        "needs-mcp",
        30,
        Some(r#"{"mcp":{"no-such-integration":{"tools":["x"]}}}"#),
    );

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "the refusal is recorded, not silent");
    let (status, error, ..) = &jobs[0];
    assert_eq!(status, "failed");
    // The message is `build_options`'s own, passed through under a neutral
    // prefix — rewriting every failure from that function as "cannot host your
    // tools" would misattribute a port-bind or SQLite error, and this row is the
    // only evidence the run leaves.
    //
    // **Only the prefix is asserted, deliberately.** `mcp_plan` resolves an
    // agent's MCP names against `paths::database_path()` — the *process-wide*
    // database, not this run's — and in a debug build that path is hardcoded to
    // `~/.agento-desktop-dev`. So which failure this reaches depends on whether
    // the developer has a dev install: locally it is "no integration row named
    // no-such-integration", on a CI runner it is "unable to open database
    // file". Pinning either would make the test a property of the machine. The
    // rule under test holds for both, and it is the rule that matters: a run
    // this build cannot set up leaves a recorded failure, never silence.
    //
    // (The two paths are the same file in production — `lib.rs` starts the
    // scheduler with `paths::database_path()` — so this is a testing artefact
    // rather than a live divergence.)
    assert!(
        error.starts_with("agent setup: "),
        "the failure has to name itself: {error:?}"
    );

    // The chat row was created before the refusal, exactly as Go creates it
    // before resolving the agent — and the task still counts the attempt.
    assert_eq!(session_count(&db), 1);
    let task = agento_lib::native::tasks::get_task(&db, &task_id)
        .expect("read")
        .expect("row");
    assert_eq!(task.run_count, 1);
    assert_eq!(task.last_run_status, "failed");
}

/// An unresolvable `{{name}}` in the *task's* prompt fails the run before
/// anything is created — `prepareTaskRun`'s first step.
#[tokio::test]
async fn an_unresolvable_prompt_variable_fails_the_run_before_it_starts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with(&db, "report for {{quarter}}", "", 30, None);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1);
    let (status, error, ..) = &jobs[0];
    assert_eq!(status, "failed");
    assert_eq!(
        error,
        r#"prompt interpolation: missing required template variable: "quarter""#
    );
    // Nothing was started, so no chat exists — `recordFailedRun` carries an
    // empty `chat_session_id`.
    assert_eq!(session_count(&db), 0);
}

/// The deadline covers the whole run, not just the event drain.
///
/// A zero timeout expires before the subprocess can produce anything, so this
/// reaches the deadline through whichever stage happens to be running —
/// `build_options`, the spawn, or the drain. All three are inside it, which is
/// the property under test; the recorded error is the same either way.
#[tokio::test]
async fn a_run_that_outlives_its_timeout_is_recorded_as_a_deadline() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with(&db, "go", "", 0, None);

    // Emits nothing at all and stays alive: the drain would wait forever.
    let cli = fake_cli(dir.path(), "        pass");

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1);
    let (status, error, ..) = &jobs[0];
    assert_eq!(status, "failed");
    assert_eq!(
        error, "context deadline exceeded",
        "Go's `context.DeadlineExceeded` reaches the caller as this"
    );
}

/// A run whose database work is blocked must not stall the runtime (#366).
///
/// This is the only test here that measures *latency of something else* rather
/// than what a run wrote, because that is what the defect was: `execute_task`
/// did its rusqlite work inline on an axum worker, and `db::open_read_write`
/// sets a five-second `busy_timeout`, so a run that met a contended write lock
/// parked a worker for up to five seconds. Tokio runs one worker per core and
/// the scheduler's semaphore permits three runs at once, so on a four-core
/// machine that is three of the four — the SPA and every SSE stream sharing the
/// runtime are left with one.
///
/// The shape is deliberate and each part is load-bearing:
///
/// - **one worker thread**, so a single parked worker is the whole runtime;
/// - **`tokio::spawn`** rather than awaiting the run here, because
///   `block_on` runs the test body on the calling thread, not on a worker —
///   awaiting inline would block a thread the ticker never wanted;
/// - **a plain OS thread** holds the lock, so the contention comes from outside
///   the runtime exactly as the Go sidecar's writes and the session scanner's
///   batch writer do.
///
/// Verified against the defect rather than assumed: with `prepare` called inline
/// in place of the `db::blocking` hand-off, the longest gap goes from ~11 ms to
/// 1,547 ms — the whole hold — and the ticker advances 5 times instead of ~150.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_contended_write_lock_does_not_stall_the_runtime() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);

    let cli = fake_cli(
        dir.path(),
        r#"        raw('{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s","usage":{"input_tokens":1,"output_tokens":1}}')"#,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    /// How long the lock is held. Long enough that a parked worker is
    /// unmistakable, short enough to stay well inside the 5s `busy_timeout` so
    /// the run itself still succeeds.
    const HOLD: Duration = Duration::from_millis(1_500);

    // The file must already be WAL, which in production it always is — the Go
    // server sets it, persistently, before anything else opens it. Left in the
    // default rollback journal, `open_read_write`'s own `PRAGMA journal_mode=WAL`
    // is a *mode change* needing an exclusive lock, and it fails outright
    // ("database is locked") in about a millisecond instead of waiting on
    // `busy_timeout` — which would make this test measure the wrong thing
    // entirely.
    agento_lib::native::db::open_read_write(&db).expect("convert the fixture to WAL");

    // A writer outside the runtime, holding the lock the run needs.
    let (holding_tx, holding_rx) = std::sync::mpsc::channel();
    let lock_db = db.clone();
    let holder = std::thread::spawn(move || {
        let mut conn = rusqlite::Connection::open(&lock_db).expect("open");
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("begin immediate");
        holding_tx.send(()).expect("signal");
        std::thread::sleep(HOLD);
        tx.rollback().expect("rollback");
    });
    holding_rx.recv().expect("the writer took the lock");

    // The thing that must keep running. It records the **longest** gap between
    // its own ticks, which is what a parked worker shows up as.
    //
    // `last` is seeded out here, before the spawn, and that is not a detail: a
    // starved ticker is never *polled*, so seeding it on the first poll would
    // start the clock after the stall and measure nothing. The first version of
    // this test did exactly that and passed against the unfixed executor.
    let worst_gap_ms = Arc::new(AtomicU64::new(0));
    let ticks = Arc::new(AtomicU64::new(0));
    let ticker = {
        let (worst_gap_ms, ticks, mut last) = (
            Arc::clone(&worst_gap_ms),
            Arc::clone(&ticks),
            Instant::now(),
        );
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let now = Instant::now();
                let gap = u64::try_from(now.duration_since(last).as_millis()).unwrap_or(u64::MAX);
                worst_gap_ms.fetch_max(gap, Ordering::Relaxed);
                ticks.fetch_add(1, Ordering::Relaxed);
                last = now;
            }
        })
    };

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    let run = tokio::spawn(async move {
        agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;
    });

    run.await.expect("the run finished");
    ticker.abort();
    holder.join().expect("the writer finished");

    let worst = worst_gap_ms.load(Ordering::Relaxed);
    assert!(
        worst < 500,
        "the runtime stalled for {worst} ms while the write lock was held \
         (the hold is {} ms; anything near it means the run blocked a worker)",
        HOLD.as_millis()
    );
    // The gap alone would also read as healthy if the ticker had simply been
    // cancelled early, so assert it really ran throughout: the hold is 1.5 s of
    // 10 ms ticks, and a third of them is a wide margin for a loaded CI box.
    let ticks = ticks.load(Ordering::Relaxed);
    assert!(
        ticks > 50,
        "the ticker only advanced {ticks} times across a {} ms hold",
        HOLD.as_millis()
    );

    // …and the run itself still completed, so this is not passing because
    // nothing happened.
    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "the run still recorded a job: {jobs:?}");
    assert_eq!(jobs[0].0, "success", "error was {:?}", jobs[0].1);
}

/// #556 on the path with nobody watching: a scheduled run whose `init` frame
/// drops a hosted server's tools **completes normally** and says so on its
/// `job_history` row.
///
/// The row is the only record a scheduled run leaves — nobody is reading the
/// app log when a cron task fires at 03:00 — and `error_message` is its one
/// free-text column, so the notice lands there while the status stays
/// `success`. That pairing is deliberate: the run answered, and a mismatch is
/// never a refusal.
#[tokio::test]
async fn a_run_whose_init_drops_the_tools_still_succeeds_and_records_it() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with(
        &db,
        "what time is it?",
        "clock",
        30,
        Some(r#"{"local":["current_time"]}"#),
    );

    let cli = fake_cli(
        dir.path(),
        r#"        say({"type": "system", "subtype": "init", "session_id": "sdk-run-2",
             "tools": ["Read"],
             "mcp_servers": [{"name": "local-tools", "status": "connected"}]})
        raw('{"type":"result","subtype":"success","is_error":false,"result":"the summary","session_id":"sdk-run-2","usage":{"input_tokens":5,"output_tokens":6}}')"#,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "exactly one run: {jobs:?}");
    let (status, error, response, ..) = &jobs[0];
    assert_eq!(status, "success", "a dropped tool list never fails the run");
    assert_eq!(
        response, "the summary",
        "the run produced its normal answer"
    );
    assert_eq!(
        error,
        "local-tools tools were hosted but the Claude CLI did not offer them to the model; see the app log"
    );

    let task = agento_lib::native::tasks::get_task(&db, &task_id)
        .expect("read")
        .expect("row");
    assert_eq!(task.last_run_status, "success");
}

/// #594: the CLI a run spawns leads its own process group, its pid is on the
/// job row **before the run's output is read**, and a signal to that group
/// reaches everything the CLI started — not just the CLI.
///
/// The fake reads the job row itself when the prompt arrives, which is after
/// the handshake and therefore after the spawn hook was awaited: a pid written
/// any later — by the finish, say — would read back `NULL` here. It also starts
/// a grandchild, the stand-in for an MCP server or a Bash tool's child, which
/// is the process a pid-only signal would leave behind.
#[cfg(unix)]
#[tokio::test]
async fn a_run_records_the_pid_of_a_process_group_leader_before_its_output_is_read() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);
    let info = dir.path().join("process.json");

    let cli = fake_cli(
        dir.path(),
        &format!(
            r#"        import sqlite3, subprocess
        grandchild = subprocess.Popen(["sleep", "60"], stdin=subprocess.DEVNULL,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        con = sqlite3.connect({db})
        recorded = con.execute("SELECT pid, pid_started_at FROM job_history").fetchone()
        con.close()
        with open({info}, "w") as out:
            out.write(json.dumps({{"pid": os.getpid(), "pgid": os.getpgid(0),
                                   "grandchild": grandchild.pid,
                                   "recorded_pid": recorded[0],
                                   "recorded_at": recorded[1]}}))
        raw('{{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"sdk-pid","usage":{{"input_tokens":1,"output_tokens":1}}}}')"#,
            db = serde_json::to_string(&db.to_string_lossy()).unwrap(),
            info = serde_json::to_string(&info.to_string_lossy()).unwrap(),
        ),
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 1, "exactly one run: {jobs:?}");
    assert_eq!(jobs[0].0, "success", "error was {:?}", jobs[0].1);

    let seen: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&info).expect("the fake wrote what it saw"))
            .expect("json");
    let pid = seen["pid"].as_i64().expect("pid");
    let grandchild = seen["grandchild"].as_i64().expect("grandchild") as libc::pid_t;

    // Whatever else fails, the grandchild must not outlive the test by a minute.
    struct Reap(libc::pid_t);
    impl Drop for Reap {
        fn drop(&mut self) {
            // SAFETY: kill(2) on a pid this test's fake started.
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
    let reap = Reap(grandchild);

    assert_eq!(
        seen["pgid"].as_i64(),
        Some(pid),
        "the CLI leads its own process group"
    );
    assert_eq!(
        seen["recorded_pid"].as_i64(),
        Some(pid),
        "the pid was on the row before the run produced anything: {seen}"
    );
    assert!(
        seen["recorded_at"]
            .as_str()
            .is_some_and(|at| at.ends_with("+0000 UTC")),
        "the spawn time is stored as a Go UTC time: {seen}"
    );

    // …and the finish did not overwrite either column.
    let conn = rusqlite::Connection::open(&db).expect("open");
    let (stored_pid, stored_at): (Option<i64>, Option<String>) = conn
        .query_row("SELECT pid, pid_started_at FROM job_history", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("row");
    assert_eq!(stored_pid, Some(pid));
    assert_eq!(stored_at.as_deref(), seen["recorded_at"].as_str());

    // The grandchild is still alive — the run's own shutdown signals the pid,
    // not the group — and a signal to the group the stored pid names reaches it.
    // SAFETY: kill(2) with signal 0 only probes; the group is this test's fake.
    assert_eq!(unsafe { libc::kill(grandchild, 0) }, 0, "grandchild alive");
    let signalled = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    assert_eq!(signalled, 0, "the stored pid names a live process group");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    // An orphaned, killed grandchild is reaped by init (or a subreaper); until
    // then it is a zombie that still answers signal 0, hence the poll.
    while unsafe { libc::kill(grandchild, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the grandchild survived a signal to the group"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Reaped, so the pid is free for the OS to reuse: never signal it again.
    std::mem::forget(reap);
}

/// A `job_history` row for the startup reaper to find (#596).
fn seed_running_job(
    path: &Path,
    id: &str,
    started_at: &str,
    pid: Option<u32>,
    pid_started_at: Option<std::time::SystemTime>,
) {
    let conn = rusqlite::Connection::open(path).expect("open");
    let spawned = pid_started_at.map(|at| {
        agento_lib::native::gotime::to_go_string_utc(agento_lib::native::gotime::GoTime::from_utc(
            at.into(),
        ))
    });
    conn.execute(
        "INSERT INTO job_history (id, task_id, task_name, status, started_at, pid, pid_started_at)
         VALUES (?1, 'task-1', 'Nightly', 'running', ?2, ?3, ?4)",
        rusqlite::params![id, started_at, pid.map(i64::from), spawned],
    )
    .expect("seed running job");
}

/// A `pending` `job_deliveries` row for the startup reaper to find (#635).
fn seed_pending_delivery(path: &Path, id: &str, job_id: &str, created_at: &str) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.execute(
        "INSERT INTO job_deliveries (id, job_id, position, type, target, status, created_at)
         VALUES (?1, ?2, 0, 'slack', 'Acme · C0123ABCD', 'pending', ?3)",
        rusqlite::params![id, job_id, created_at],
    )
    .expect("seed pending delivery");
}

fn delivery_outcome(path: &Path, id: &str) -> (String, String, bool) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.query_row(
        "SELECT status, error, finished_at IS NOT NULL FROM job_deliveries WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .expect("delivery row")
}

fn job_outcome(path: &Path, id: &str) -> (String, String, bool) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.query_row(
        "SELECT status, error_message, finished_at IS NOT NULL FROM job_history WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .expect("row")
}

/// The previous session's rows, one per case the reaper distinguishes — and
/// one of this session's, which it must not touch (#596).
#[cfg(unix)]
#[test]
fn the_startup_reaper_stops_a_surviving_orphan_and_fails_every_stale_row() {
    use std::os::unix::process::CommandExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    migrated_with_task(&db, "cron", true);
    let long_ago = "2026-01-01 00:00:00 +0000 UTC";

    // The orphan: a CLI stand-in leading its own group, with a grandchild in it.
    let mut orphan = std::process::Command::new("sh")
        .args(["-c", "sleep 600 & wait"])
        .process_group(0)
        .spawn()
        .expect("spawn the orphan");
    let orphan_pid = orphan.id();
    seed_running_job(
        &db,
        "j-orphan",
        long_ago,
        Some(orphan_pid),
        Some(std::time::SystemTime::now()),
    );
    // Reaped as init would reap a real orphan, so its group can empty.
    let orphan_waiter = std::thread::spawn(move || orphan.wait());

    // A process that has already exited.
    let mut gone = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    let gone_pid = gone.id();
    gone.wait().expect("wait true");
    seed_running_job(
        &db,
        "j-gone",
        long_ago,
        Some(gone_pid),
        Some(std::time::SystemTime::now()),
    );

    // A row written before the pid column existed.
    seed_running_job(&db, "j-nopid", long_ago, None, None);

    // A pid that is alive but is some other process: it started a day after
    // the one the row recorded.
    let mut stranger = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn the stranger");
    seed_running_job(
        &db,
        "j-reused",
        long_ago,
        Some(stranger.id()),
        Some(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400)),
    );

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);

    // A run of this session, started after the scheduler was built.
    let later =
        agento_lib::native::gotime::to_go_string_utc(agento_lib::native::gotime::GoTime::from_utc(
            chrono::Utc::now() + chrono::Duration::minutes(5),
        ));
    seed_running_job(&db, "j-this-session", &later, None, None);

    // A delivery the previous session dispatched and never finished (#635),
    // and one this session has in flight.
    seed_pending_delivery(&db, "d-stale", "j-gone", long_ago);
    seed_pending_delivery(&db, "d-this-session", "j-this-session", &later);

    let reaped = scheduler.reap_stale_runs().expect("reap");
    assert_eq!((reaped.recovered, reaped.abandoned), (1, 3), "{reaped:?}");

    assert_eq!(
        job_outcome(&db, "j-orphan"),
        (
            "failed".to_string(),
            "orphaned: recovered on startup".to_string(),
            true
        )
    );
    let status = orphan_waiter.join().expect("join").expect("wait");
    assert!(!status.success(), "the orphan was stopped: {status:?}");
    // SAFETY: signal 0 only probes; the group was this test's orphan.
    let group_left = unsafe { libc::kill(-(orphan_pid as libc::pid_t), 0) };
    assert_ne!(group_left, 0, "the grandchild went with its group");

    for id in ["j-gone", "j-nopid", "j-reused"] {
        assert_eq!(
            job_outcome(&db, id),
            (
                "failed".to_string(),
                "orphaned: app did not exit cleanly".to_string(),
                true
            ),
            "{id}"
        );
    }
    assert!(
        stranger.try_wait().expect("try_wait").is_none(),
        "a reused pid is never signalled"
    );
    let _ = stranger.kill();
    let _ = stranger.wait();

    assert_eq!(
        job_outcome(&db, "j-this-session"),
        ("running".to_string(), String::new(), false),
        "a run of this session is not the reaper's"
    );

    // A second pass finds nothing left to do.
    let again = scheduler.reap_stale_runs().expect("reap again");
    assert_eq!((again.recovered, again.abandoned), (0, 0));

    // The deliveries: the stale one is failed, and the run it belongs to keeps
    // the reaper's own reason — a delivery never rewrites its run's row.
    assert_eq!(
        scheduler
            .reap_pending_deliveries()
            .expect("reap deliveries"),
        1
    );
    assert_eq!(
        delivery_outcome(&db, "d-stale"),
        (
            "failed".to_string(),
            "interrupted: app did not finish the delivery".to_string(),
            true
        )
    );
    assert_eq!(
        delivery_outcome(&db, "d-this-session"),
        ("pending".to_string(), String::new(), false),
        "a delivery of this session is not the reaper's"
    );
    assert_eq!(
        job_outcome(&db, "j-gone").1,
        "orphaned: app did not exit cleanly"
    );
    assert_eq!(scheduler.reap_pending_deliveries().expect("again"), 0);
}

/// A reaped task is not left blocked: nothing marks it in flight, so a manual
/// run is accepted and its next fire runs normally (#596).
#[cfg(unix)]
#[tokio::test]
async fn a_task_whose_stale_run_was_reaped_fires_normally_afterwards() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);
    seed_running_job(&db, "j-stale", "2026-01-01 00:00:00 +0000 UTC", None, None);

    let cli = fake_cli(
        dir.path(),
        r#"        raw('{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"sdk-reaped","usage":{"input_tokens":1,"output_tokens":1}}')"#,
    );
    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    let pass = std::sync::Arc::clone(&scheduler);
    tokio::task::spawn_blocking(move || pass.reap_stale_runs())
        .await
        .expect("join")
        .expect("reap");

    assert!(!scheduler.is_running(&task_id));
    drop(
        scheduler
            .try_mark_running(&task_id)
            .expect("a manual run is not refused with a 409"),
    );

    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;
    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    assert_eq!(jobs[0].0, "failed", "the stale row, reaped");
    assert_eq!(jobs[1].0, "success", "the next fire, run: {:?}", jobs[1].1);
}

// ─── Delivery (#636) ──────────────────────────────────────────────────────────
//
// Driven through the `test-hooks` Fake destination: `fake` sends, `fake-fail`
// fails, `fake-hang` never finishes. Each records the report it was handed.

fn set_destinations(path: &Path, task_id: &str, json: &str) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.execute(
        "UPDATE scheduled_tasks SET destinations = ?1 WHERE id = ?2",
        [json, task_id],
    )
    .expect("set destinations");
}

/// `(type, status, error)` per delivery of `job_id`, in read order.
fn deliveries(path: &Path, job_id: &str) -> Vec<(String, String, String)> {
    let conn = rusqlite::Connection::open(path).expect("open");
    let mut stmt = conn
        .prepare(
            "SELECT type, status, error FROM job_deliveries
             WHERE job_id = ?1 ORDER BY position, created_at, id",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([job_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

/// Polls until `job_id` has `n` deliveries and none is `pending`: delivery
/// finishes on a task of its own, after the run has returned.
async fn settled(path: &Path, job_id: &str, n: usize) -> Vec<(String, String, String)> {
    for _ in 0..500 {
        let rows = deliveries(path, job_id);
        if rows.len() == n && rows.iter().all(|r| r.1 != "pending") {
            return rows;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("deliveries never settled: {:?}", deliveries(path, job_id));
}

fn only_job_id(path: &Path) -> String {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.query_row("SELECT id FROM job_history", [], |r| r.get(0))
        .expect("exactly one job row")
}

const ANSWERING_CLI: &str = r#"        raw('{"type":"result","subtype":"success","is_error":false,"result":"the full answer","session_id":"sdk-d-1","usage":{"input_tokens":1,"output_tokens":2}}')"#;

/// A timer fire and `POST /api/tasks/{id}/run` produce the same delivery rows,
/// on the run's own job id — and with `save_output` off the destination still
/// receives the whole answer while the job row stores `""`.
#[tokio::test]
async fn a_run_delivers_its_unsaved_answer_the_same_way_timed_or_manual() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    for manual in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("agento.db");
        let task_id = migrated_with_task(&db, "cron", false);
        set_destinations(
            &db,
            &task_id,
            r#"[{"type":"fake","when":"always"},{"type":"fake","when":"success"}]"#,
        );
        let cli = fake_cli(dir.path(), ANSWERING_CLI);

        let _env = env_lock().lock().await;
        std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
        let scheduler = agento_lib::native::schedule::runtime::detached(&db);
        if manual {
            let guard = scheduler.try_mark_running(&task_id).expect("free");
            agento_lib::native::schedule::executor::run_manual(
                std::sync::Arc::clone(&scheduler),
                agento_lib::native::tasks::get_task(&db, &task_id)
                    .expect("read")
                    .expect("row"),
                "job-delivery-manual".to_string(),
                guard,
            )
            .await;
        } else {
            agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;
        }

        let job_id = only_job_id(&db);
        let (status, error, response, _, _) = job_rows(&db).remove(0);
        assert_eq!(status, "success", "manual={manual} error was {error:?}");
        assert_eq!(response, "", "manual={manual}: save_output is off");
        assert_eq!(
            settled(&db, &job_id, 2).await,
            vec![
                ("fake".into(), "sent".into(), String::new()),
                ("fake".into(), "sent".into(), String::new()),
            ],
            "manual={manual}"
        );
        let received = agento_lib::native::schedule::delivery::fake_received(&job_id);
        assert_eq!(received.len(), 2, "manual={manual}");
        assert!(
            received.iter().all(|r| r.answer == "the full answer"
                && r.status == "success"
                && r.task_name == "Nightly"),
            "manual={manual}: {received:?}"
        );
    }
}

/// A delivery's failure is the delivery's: the run stays `success` and its
/// `error_message` keeps what the run wrote.
#[tokio::test]
async fn a_failed_delivery_leaves_the_runs_status_alone() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);
    set_destinations(&db, &task_id, r#"[{"type":"fake-fail","when":"success"}]"#);
    let cli = fake_cli(dir.path(), ANSWERING_CLI);

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;

    let job_id = only_job_id(&db);
    assert_eq!(
        settled(&db, &job_id, 1).await,
        vec![(
            "fake-fail".into(),
            "failed".into(),
            "fake destination failed".into()
        )]
    );
    let (status, error, response, _, _) = job_rows(&db).remove(0);
    assert_eq!(status, "success");
    assert_eq!(error, "");
    assert_eq!(response, "the full answer");
}

/// A destination that never answers holds nothing the run owns: the run
/// returns, its permit and in-flight entry are released, the next run fires,
/// and the delivery is still `pending` for #635's reaper to find.
#[tokio::test]
async fn a_hanging_destination_does_not_hold_the_run_or_its_permit() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);
    set_destinations(&db, &task_id, r#"[{"type":"fake-hang","when":"always"}]"#);
    let cli = fake_cli(dir.path(), ANSWERING_CLI);

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    let permits = scheduler.semaphore().available_permits();

    for n in 0..2 {
        let guard = scheduler.try_mark_running(&task_id).expect("released");
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            agento_lib::native::schedule::executor::run_manual(
                std::sync::Arc::clone(&scheduler),
                agento_lib::native::tasks::get_task(&db, &task_id)
                    .expect("read")
                    .expect("row"),
                format!("job-hang-{n}"),
                guard,
            ),
        )
        .await
        .expect("the run returns without waiting for its delivery");
        assert!(
            !scheduler.is_running(&task_id),
            "run {n} released its guard"
        );
        assert_eq!(
            scheduler.semaphore().available_permits(),
            permits,
            "run {n} released its permit"
        );
        // The pending row is written on the delivery's own task; give it a
        // moment, then it stays pending because the post never finishes.
        for _ in 0..500 {
            if !deliveries(&db, &format!("job-hang-{n}")).is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            deliveries(&db, &format!("job-hang-{n}")),
            vec![("fake-hang".into(), "pending".into(), String::new())]
        );
    }
    let jobs = job_rows(&db);
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().all(|j| j.0 == "success"), "{jobs:?}");
}

/// A task with no destinations spawns nothing and writes nothing.
#[tokio::test]
async fn a_task_with_no_destinations_records_no_deliveries() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let task_id = migrated_with_task(&db, "cron", true);
    let cli = fake_cli(dir.path(), ANSWERING_CLI);

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    let scheduler = agento_lib::native::schedule::runtime::detached(&db);
    agento_lib::native::schedule::executor::execute_task(&scheduler, &task_id).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let conn = rusqlite::Connection::open(&db).expect("open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM job_deliveries", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0);
}

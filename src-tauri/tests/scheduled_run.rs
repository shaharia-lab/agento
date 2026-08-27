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
fn fake_cli(dir: &Path, emit: &str) -> PathBuf {
    let script = format!(
        r#"#!/usr/bin/env {python}
import json, sys

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

/// A database with one active task, ready to fire.
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

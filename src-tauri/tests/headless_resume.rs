//! One headless turn on an **existing** chat, driven against a scripted fake
//! CLI (#564).
//!
//! `tests/scheduled_run.rs` answers "does a scheduled task run?"; this file
//! answers the two questions resuming adds, neither of which any unit test can
//! see:
//!
//! - **Does the second turn actually resume the first?** The whole mechanism is
//!   one `--resume <id>` on a command line the code under test never returns, so
//!   the only honest assertion is the argv the subprocess was invoked with.
//! - **Do a UI turn and a headless turn exclude each other?** The busy lock is a
//!   process global reached from two unrelated call sites, and a missing
//!   `try_lock` is invisible until two CLI processes are writing one chat row.
//!
//! It inherits `scheduled_run.rs`'s **trap**: the fake CLI never exits after the
//! result. A run that went through `claude::client::Session` rather than the
//! one-shot `query` would never see its event channel close and would sit until
//! its timeout, so every test here would hang instead of failing cleanly — which
//! is why the one that would hang longest wraps its own await in a deadline that
//! names the trap.

use std::path::{Path, PathBuf};

use agento_lib::native::agent_run::{self, ExecutionSettings};

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

/// A chat id unique to the calling test.
///
/// The live-session registry is a **process global** — a chat turn is process
/// state — and cargo runs these tests in parallel in one binary, so a shared id
/// would make one test see another's busy lock. That is a collision, not a
/// finding.
fn unique_id(label: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("{label}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn argv_log(dir: &Path) -> PathBuf {
    dir.join("argv.jsonl")
}

/// The file the gated fake CLI touches once it has read the user message, and
/// the one it then waits for before answering.
fn gate_started(dir: &Path) -> PathBuf {
    dir.join("gate.started")
}
fn gate_release(dir: &Path) -> PathBuf {
    dir.join("gate.release")
}

/// A CLI that records **its own argv** on start, acknowledges `initialize`, and
/// emits `emit` verbatim on the first user message.
///
/// The argv line is what the `--resume` assertions read: the chain under test is
/// a `RunSpec` field reaching `Options::build_args` reaching a command line, and
/// inspecting an `Options` in the test would skip the last two links.
///
/// `gated` makes it touch `gate.started` and then block until `gate.release`
/// appears, which is how a test gets a run that is provably *in flight*.
fn fake_cli(dir: &Path, emit: &str, gated: bool) -> PathBuf {
    let script = format!(
        r#"#!/usr/bin/env {python}
import json, os, sys, time

with open({argv_log}, "a") as _argv:
    _argv.write(json.dumps(sys.argv) + "\n")
    _argv.flush()

GATED = {gated}
STARTED = {started}
RELEASE = {release}

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
        if GATED:
            open(STARTED, "w").close()
            while not os.path.exists(RELEASE):
                time.sleep(0.02)
{emit}
        # **Deliberately no exit**, as `scheduled_run.rs` explains: a real CLI in
        # session mode stays alive for the next send, so a fake that exited here
        # would close stdout, end the event stream for free, and pass against a
        # drain that never terminates on its own.
        continue
"#,
        python = python3().unwrap_or_else(|| "python3".into()),
        argv_log = json_str(&argv_log(dir)),
        gated = if gated { "True" } else { "False" },
        started = json_str(&gate_started(dir)),
        release = json_str(&gate_release(dir)),
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

fn json_str(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy()).expect("encode path")
}

/// Every argv the fake CLI has been invoked with, in order.
fn spawns(dir: &Path) -> Vec<Vec<String>> {
    let Ok(raw) = std::fs::read_to_string(argv_log(dir)) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("argv line"))
        .collect()
}

/// The value of `--resume` in one spawn, or `None` when the flag is absent.
fn resumed_with(argv: &[String]) -> Option<&str> {
    let at = argv.iter().position(|a| a == "--resume")?;
    argv.get(at + 1).map(String::as_str)
}

/// A migrated database with one chat row, seeded as if some earlier turn had
/// already run against it.
fn migrated_with_chat(path: &Path, chat_id: &str, sdk_session_id: &str, totals: [i64; 4]) {
    let mut conn = rusqlite::Connection::open(path).expect("open");
    agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO chat_sessions
            (id, title, agent_slug, sdk_session_id, working_directory, model,
             settings_profile_id, permission_mode, total_input_tokens, total_output_tokens,
             total_cache_creation_tokens, total_cache_read_tokens, created_at, updated_at)
         VALUES (?1, 'New Chat', '', ?2, '', '', '', '', ?3, ?4, ?5, ?6,
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        rusqlite::params![
            chat_id,
            sdk_session_id,
            totals[0],
            totals[1],
            totals[2],
            totals[3],
        ],
    )
    .expect("seed chat");
}

fn sdk_session_id(path: &Path, chat_id: &str) -> String {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.query_row(
        "SELECT sdk_session_id FROM chat_sessions WHERE id = ?1",
        [chat_id],
        |r| r.get(0),
    )
    .expect("the session row")
}

fn totals(path: &Path, chat_id: &str) -> [i64; 4] {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.query_row(
        "SELECT total_input_tokens, total_output_tokens, total_cache_creation_tokens,
                total_cache_read_tokens
         FROM chat_sessions WHERE id = ?1",
        [chat_id],
        |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?]),
    )
    .expect("the session row")
}

fn messages(path: &Path, chat_id: &str) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(path).expect("open");
    let mut stmt = conn
        .prepare("SELECT role, content FROM chat_messages WHERE session_id = ?1 ORDER BY id")
        .expect("prepare");
    let rows = stmt
        .query_map([chat_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

fn result_frame(answer: &str, session_id: &str, usage: [i64; 4]) -> String {
    format!(
        r#"        raw('{{"type":"result","subtype":"success","is_error":false,"result":"{answer}","session_id":"{session_id}","usage":{{"input_tokens":{},"output_tokens":{},"cache_creation_input_tokens":{},"cache_read_input_tokens":{}}}}}')"#,
        usage[0], usage[1], usage[2], usage[3],
    )
}

/// The first acceptance criterion, both halves: a chat that has never run gets a
/// fresh CLI session and stores the id the CLI minted; the next turn on the same
/// chat is spawned with `--resume <that id>`.
///
/// Also the trap test for this path. The fake never exits, so a `run_resumed`
/// built on `Session` rather than the one-shot `query` would drain forever —
/// hence the explicit deadline around the await, well under the run's own
/// 10-minute timeout.
#[tokio::test]
async fn the_first_turn_mints_a_session_and_the_second_resumes_it() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let chat = unique_id("resume-chat");
    migrated_with_chat(&db, &chat, "", [0; 4]);

    let cli = fake_cli(
        dir.path(),
        &result_frame("the answer", "sdk-minted-1", [11, 22, 3, 4]),
        false,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let first = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        agent_run::run_resumed(
            &db,
            &chat,
            "first question",
            &ExecutionSettings::default(),
            std::time::Duration::from_secs(600),
        ),
    )
    .await
    .expect("the drain must terminate: a `Session` here would hang until the timeout")
    .expect("the run");
    assert_eq!(first.answer, "the answer");

    let spawned = spawns(dir.path());
    assert_eq!(spawned.len(), 1, "one spawn so far: {spawned:?}");
    assert_eq!(
        resumed_with(&spawned[0]),
        None,
        "a chat that has never run has no session to resume"
    );
    assert_eq!(
        sdk_session_id(&db, &chat),
        "sdk-minted-1",
        "the CLI's own id is written back, which is what the next turn resumes"
    );
    assert_eq!(
        messages(&db, &chat),
        vec![
            ("user".to_string(), "first question".to_string()),
            ("assistant".to_string(), "the answer".to_string()),
        ]
    );

    agent_run::run_resumed(
        &db,
        &chat,
        "second question",
        &ExecutionSettings::default(),
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("the second run");

    let spawned = spawns(dir.path());
    assert_eq!(spawned.len(), 2, "two spawns: {spawned:?}");
    assert_eq!(
        resumed_with(&spawned[1]),
        Some("sdk-minted-1"),
        "the second turn continues the session the first minted"
    );
}

/// The busy lock, in the direction a headless run is refused.
///
/// The assertion that matters is the second one: refusing *after* spawning would
/// still leave two CLI processes on one chat, which is the whole hazard.
#[tokio::test]
async fn a_resumed_run_is_refused_while_the_chat_is_busy_and_spawns_nothing() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let chat = unique_id("busy-chat");
    migrated_with_chat(&db, &chat, "sdk-prior", [0; 4]);

    let cli = fake_cli(
        dir.path(),
        &result_frame("unreachable", "sdk-prior", [1, 1, 1, 1]),
        false,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    assert!(
        agento_lib::native::chat::live::registry().try_lock(&chat),
        "the lock starts free"
    );

    let refused = agent_run::run_resumed(
        &db,
        &chat,
        "while busy",
        &ExecutionSettings::default(),
        std::time::Duration::from_secs(60),
    )
    .await
    .expect_err("a busy chat must refuse");
    assert_eq!(refused, agento_lib::native::chat::live::CHAT_BUSY);
    assert!(
        spawns(dir.path()).is_empty(),
        "the refusal happens before anything is spawned"
    );

    agento_lib::native::chat::live::registry().release(&chat);
}

/// The busy lock, in the other direction: a UI `POST /api/chats/{id}/messages`
/// meets the existing 409 while a headless turn is running.
///
/// The gate is what makes "while running" real rather than hopeful — the CLI has
/// read the prompt and is holding it, so the lock is provably held by a live
/// subprocess rather than by a race that happened to land.
#[tokio::test]
async fn a_ui_turn_is_refused_while_a_resumed_run_holds_the_lock() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let chat = unique_id("collide-chat");
    migrated_with_chat(&db, &chat, "sdk-prior", [0; 4]);

    let cli = fake_cli(
        dir.path(),
        &result_frame("the answer", "sdk-prior", [1, 2, 3, 4]),
        true,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let running = tokio::spawn({
        let (db, chat) = (db.clone(), chat.clone());
        async move {
            agent_run::run_resumed(
                &db,
                &chat,
                "the headless turn",
                &ExecutionSettings::default(),
                std::time::Duration::from_secs(600),
            )
            .await
        }
    });

    // The CLI has the prompt and is parked on the gate: the run is in flight.
    let started = gate_started(dir.path());
    for _ in 0..1500 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(started.exists(), "the fake CLI never reached the gate");

    let response =
        agento_lib::native::chat::turn::run(db.clone(), chat.clone(), "from the UI".into())
            .await
            .expect("the route answers");
    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    let body = {
        use http_body_util::BodyExt;
        let collected = response.into_body().collect().await.expect("body");
        String::from_utf8(collected.to_bytes().to_vec()).expect("utf8")
    };
    assert!(
        body.contains(agento_lib::native::chat::live::CHAT_BUSY),
        "the UI gets the existing busy answer, not a second run: {body}"
    );

    std::fs::write(gate_release(dir.path()), b"go").expect("release the gate");
    running.await.expect("the task").expect("the run");

    assert_eq!(
        spawns(dir.path()).len(),
        1,
        "the refused UI turn spawned nothing"
    );
    // The lock is free again, which is what the guard's `Drop` is for.
    assert!(agento_lib::native::chat::live::registry().try_lock(&chat));
    agento_lib::native::chat::live::registry().release(&chat);
}

/// A turn that produced no final text: the user message is stored, no assistant
/// message is, and the chat keeps the session id it already had.
///
/// The result frame carries an **empty** `session_id`, which is what an
/// interrupted turn reports — blanking the column there would make the next turn
/// start a new CLI session instead of resuming this one.
#[tokio::test]
async fn a_turn_with_no_final_text_stores_the_user_message_and_keeps_the_session_id() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let chat = unique_id("empty-chat");
    migrated_with_chat(&db, &chat, "sdk-prior", [0; 4]);

    let cli = fake_cli(dir.path(), &result_frame("", "", [5, 6, 7, 8]), false);

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    agent_run::run_resumed(
        &db,
        &chat,
        "a question with no answer",
        &ExecutionSettings::default(),
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("the run");

    assert_eq!(
        messages(&db, &chat),
        vec![("user".to_string(), "a question with no answer".to_string())],
        "the user turn is stored even with no answer, and nothing else is"
    );
    assert_eq!(
        sdk_session_id(&db, &chat),
        "sdk-prior",
        "an empty reported id leaves the previous one in place"
    );
}

/// The divergence from both existing headless write-backs: the four totals are
/// **incremented**, so a chat a user has also typed into keeps the UI turns'
/// usage.
///
/// The row is seeded as if a UI turn had already run — replacing rather than
/// incrementing would answer the run's own numbers instead of the sum, which is
/// exactly what `executor.rs` and `dispatcher.rs` do and what this path must
/// not.
#[tokio::test]
async fn token_totals_are_summed_across_a_ui_turn_and_a_headless_turn() {
    if python3().is_none() {
        eprintln!("skipping: no python3 to script the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let chat = unique_id("totals-chat");
    // What one UI turn left behind.
    migrated_with_chat(&db, &chat, "sdk-prior", [11, 22, 3, 4]);

    let cli = fake_cli(
        dir.path(),
        &result_frame("the answer", "sdk-prior", [5, 6, 7, 8]),
        false,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    agent_run::run_resumed(
        &db,
        &chat,
        "the headless turn",
        &ExecutionSettings::default(),
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("the run");

    assert_eq!(
        totals(&db, &chat),
        [16, 28, 10, 12],
        "the headless turn adds to the UI turn's usage rather than replacing it"
    );
}

/// A chat id nothing owns: refused, and the lock it took is given back.
///
/// The second half is the one that matters — a leaked lock wedges the chat for
/// the life of the process, and the missing-chat path is the earliest return
/// there is.
#[tokio::test]
async fn a_missing_chat_is_an_error_and_releases_the_lock_it_took() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    let chat = unique_id("missing-chat");
    migrated_with_chat(&db, &chat, "", [0; 4]);

    let absent = unique_id("nobody");
    let err = agent_run::run_resumed(
        &db,
        &absent,
        "hello",
        &ExecutionSettings::default(),
        std::time::Duration::from_secs(60),
    )
    .await
    .expect_err("no such chat");
    assert!(err.contains("not found"), "{err}");

    assert!(
        agento_lib::native::chat::live::registry().try_lock(&absent),
        "the guard released the lock on the early return"
    );
    agento_lib::native::chat::live::registry().release(&absent);
}

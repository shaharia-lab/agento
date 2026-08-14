//! The Claude Agent SDK port, driven against a **scripted fake CLI**.
//!
//! This is the technique the Go SDK proved, and it is here for the same reason:
//! the guarantees that matter are properties of a *sequence* the spawn path
//! performs, not of any single function. Only a real subprocess on the other end
//! of real pipes can show that `initialize` is written before the user message,
//! that the wait for the acknowledgement is a wait, or that an unanswered
//! `control_request` is what hangs a session.
//!
//! The fake is a small Python program that logs every stdin line it receives to
//! a JSONL file and replies according to a per-test script. No `claude` binary
//! and no API key are involved, so these run in CI like any other test.
//!
//! The one imported subtlety from the Go tests: the acknowledgement is sent from
//! a **separate thread after a deliberate sleep**, and the `__ack_sent__` marker
//! is written *before* the reply is flushed. The sleep is what makes "waited for
//! the acknowledgement" distinguishable from "wrote the user message
//! immediately" — in a racing implementation the user message lands inside that
//! window. Marking before flushing removes the test's own race, since once the
//! reply is on the wire the SDK may write the next message instantly.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agento_lib::claude::{self, Options, PermissionResult};

/// Skips a test when the machine has no `python3`, the way the Go suite skips
/// on Windows: the fake CLI is a script, not a compiled fixture.
fn python3() -> Option<String> {
    for candidate in ["python3", "python"] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Writes an executable fake `claude` into `dir` and returns its path.
///
/// `emit` is Python appended to the request loop; it receives `msg` (the
/// decoded stdin line), and may call `ack(request_id)` or `say(obj)`.
fn fake_cli(dir: &Path, log_path: &Path, emit: &str) -> PathBuf {
    let script = format!(
        r#"#!/usr/bin/env {python}
import json, sys, threading, time

LOG = {log}

log = open(LOG, "a")
lock = threading.Lock()

def record(entry):
    with lock:
        log.write(json.dumps(entry) + "\n")
        log.flush()

def say(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def ack(request_id, body=None):
    # Deliberately slow, and on its own thread so stdin keeps being read and
    # logged meanwhile. That is what lets a test tell "waited for the
    # acknowledgement" from "wrote the user message immediately".
    time.sleep(0.5)
    if body is None:
        body = {{"models": [{{"value": "fake", "displayName": "Fake"}}],
                 "account": {{"apiProvider": "fake"}},
                 "output_style": "default"}}
    # Mark BEFORE flushing: once the response is on the wire the SDK may write
    # the next message immediately, and the reader thread would log it ahead of
    # a marker recorded afterwards.
    record({{"type": "__ack_sent__"}})
    say({{"type": "control_response",
         "response": {{"subtype": "success", "request_id": request_id, "response": body}}}})

def ack_now(request_id):
    say({{"type": "control_response",
         "response": {{"subtype": "success", "request_id": request_id,
                      "response": {{"account": {{"apiProvider": "fake"}}}}}}}})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    record(msg)
    req = msg.get("request") or {{}}
{emit}
"#,
        python = python3().unwrap_or_else(|| "python3".into()),
        log = serde_json::to_string(&log_path.to_string_lossy()).unwrap(),
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

/// The message types the fake CLI received, in order — `control_request`s
/// labelled with their subtype, so ordering assertions read as a sequence.
fn logged(log_path: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(log_path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            let kind = v.get("type")?.as_str()?;
            if kind == "control_request" {
                let subtype = v
                    .get("request")
                    .and_then(|r| r.get("subtype"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                Some(format!("control_request:{subtype}"))
            } else {
                Some(kind.to_string())
            }
        })
        .collect()
}

/// Polls the log until it holds at least `n` entries, so assertions do not race
/// the fake CLI's own writes.
async fn wait_for_logged(log_path: &Path, n: usize) -> Vec<String> {
    for _ in 0..500 {
        let entries = logged(log_path);
        if entries.len() >= n {
            return entries;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    logged(log_path)
}

/// The full stdin line for the first message of the given type.
fn logged_message(log_path: &Path, kind: &str) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(log_path).ok()?;
    raw.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v.get("type").and_then(|t| t.as_str()) == Some(kind))
}

fn options_for(exe: &Path) -> Options {
    Options::new()
        .with_claude_executable(exe.to_string_lossy().into_owned())
        .with_init_timeout(Duration::from_secs(10))
}

/// Whether a spawn failure is the `ETXTBSY` race rather than a real one.
///
/// These tests write an executable and then exec it, from several threads at
/// once. On Linux `execve` refuses a file any process has open for writing, and
/// a `fork` in one thread duplicates another thread's still-open write fd into
/// the child until that child's own `exec` closes it. So thread A can be
/// refused its own freshly written script because thread B forked mid-write.
/// It is a property of the harness — one process writing and executing many
/// files concurrently — not of the SDK, which never writes what it spawns.
fn is_text_file_busy(err: &claude::Error) -> bool {
    err.to_string().contains("Text file busy")
}

/// Starts a query, retrying past the `ETXTBSY` race described above.
async fn query(prompt: &str, opts: Options) -> claude::Stream {
    for _ in 0..50 {
        match claude::query(prompt, opts.clone()).await {
            Err(e) if is_text_file_busy(&e) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other.expect("query"),
        }
    }
    panic!("the fake CLI stayed busy for a second");
}

/// Starts a query that is expected to fail, retrying past the `ETXTBSY` race so
/// the harness's own flake cannot be mistaken for the failure under test.
async fn query_err(prompt: &str, opts: Options) -> claude::Error {
    for _ in 0..50 {
        match claude::query(prompt, opts.clone()).await {
            Err(e) if is_text_file_busy(&e) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return e,
            Ok(_) => panic!("expected the query to fail"),
        }
    }
    panic!("the fake CLI stayed busy for a second");
}

/// Opens a session, retrying past the `ETXTBSY` race described above.
async fn session(opts: Options) -> claude::Session {
    for _ in 0..50 {
        match claude::Session::new(opts.clone()).await {
            Err(e) if is_text_file_busy(&e) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other.expect("session"),
        }
    }
    panic!("the fake CLI stayed busy for a second");
}

// ─── The handshake ordering ──────────────────────────────────────────────────

#[tokio::test]
async fn the_user_message_waits_for_the_initialize_acknowledgement() {
    if python3().is_none() {
        eprintln!("skipping: no python3 for the fake CLI");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        threading.Thread(target=ack, args=(msg.get("request_id"),), daemon=True).start()"#,
    );

    let stream = query("hello", options_for(&exe)).await;

    let entries = wait_for_logged(&log, 3).await;
    assert_eq!(
        entries,
        vec!["control_request:initialize", "__ack_sent__", "user"],
        "the turn must not start before the CLI has configured MCP servers, agents and hooks"
    );

    // The handshake payload is the SDK's only source of truth for what the CLI
    // offers, so it has to survive into the stream.
    assert_eq!(stream.supported_models().len(), 1);
    assert_eq!(stream.supported_models()[0].value, "fake");
    assert_eq!(stream.account_info().api_provider, "fake");
    assert_eq!(stream.output_style().0, "default");
}

#[tokio::test]
async fn a_session_writes_the_handshake_and_nothing_else() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        threading.Thread(target=ack, args=(msg.get("request_id"),), daemon=True).start()"#,
    );

    let session = session(options_for(&exe)).await;

    let entries = wait_for_logged(&log, 2).await;
    assert_eq!(entries, vec!["control_request:initialize", "__ack_sent__"]);

    // The first turn only begins when the caller sends one.
    session.send("now we begin").await.unwrap();
    let entries = wait_for_logged(&log, 3).await;
    assert_eq!(entries.last().unwrap(), "user");
}

#[tokio::test]
async fn a_cli_that_never_acknowledges_fails_as_a_timeout() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    // Logs everything, answers nothing.
    let exe = fake_cli(dir.path(), &log, "    pass");

    let opts = Options::new()
        .with_claude_executable(exe.to_string_lossy().into_owned())
        .with_init_timeout(Duration::from_millis(750));

    let started = std::time::Instant::now();
    let err = query_err("hello", opts).await;
    let elapsed = started.elapsed();

    match err {
        claude::Error::Initialize { timeout, .. } => {
            assert!(timeout, "a silent CLI is a timeout, not a rejection")
        }
        other => panic!("expected an initialize timeout, got {other}"),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "the timeout must bound startup, took {elapsed:?}"
    );
    assert!(
        !logged(&log).contains(&"user".to_string()),
        "a failed handshake must not have started a turn"
    );
}

#[tokio::test]
async fn a_rejected_initialize_is_reported_as_a_rejection() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        say({"type": "control_response",
             "response": {"subtype": "error", "request_id": msg.get("request_id"),
                          "error": "sdkMcpServers and webSearchIsolationExemptMcpServers must be arrays of strings"}})"#,
    );

    let err = query_err("hello", options_for(&exe)).await;
    match err {
        claude::Error::Initialize { timeout, message } => {
            assert!(!timeout);
            assert!(
                message.contains("arrays of strings"),
                "the CLI's own reason must survive: {message}"
            );
        }
        other => panic!("expected an initialize rejection, got {other}"),
    }
}

// ─── A turn end to end ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_turn_streams_its_events_and_ends_on_the_result() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "system", "subtype": "init", "session_id": "s-1",
             "capabilities": ["interrupt_receipt_v1"]})
        say({"type": "assistant", "session_id": "s-1",
             "message": {"role": "assistant", "model": "fake",
                         "content": [{"type": "text", "text": "four"}]}})
        say({"type": "result", "subtype": "success", "session_id": "s-1",
             "is_error": False, "num_turns": 1, "result": "four",
             "total_cost_usd": 0.25, "terminal_reason": "completed",
             "usage": {"input_tokens": 10, "output_tokens": 2},
             "modelUsage": {"fake": {"inputTokens": 10, "outputTokens": 2, "costUSD": 0.25}}})"#,
    );

    let mut stream = query("2+2?", options_for(&exe)).await;

    let mut kinds = Vec::new();
    let mut text = String::new();
    let mut result = None;
    while let Some(event) = stream.next_event().await {
        kinds.push(event.event_type.clone());
        if let Some(assistant) = &event.assistant {
            text.push_str(&assistant.text());
        }
        if let Some(r) = event.result {
            result = Some(r);
        }
    }

    assert_eq!(kinds, vec!["system", "assistant", "result"]);
    assert_eq!(text, "four");

    let result = result.expect("the run must produce a result");
    assert!(!result.is_error);
    assert_eq!(result.result, "four");
    assert_eq!(result.total_cost_usd, 0.25);
    assert_eq!(result.terminal_reason.as_str(), "completed");
    assert!(!result.terminal_reason.aborted());
    assert_eq!(result.usage.input_tokens, 10);
    // modelUsage is camelCase inside an otherwise snake_case message.
    assert_eq!(result.model_usages["fake"].cost_usd, 0.25);

    // capabilities come from system/init, not from the handshake.
    assert_eq!(
        stream.capabilities(),
        Some(vec!["interrupt_receipt_v1".to_string()])
    );
}

#[tokio::test]
async fn run_returns_the_final_result_and_surfaces_an_error_one() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "result", "subtype": "error_during_execution", "is_error": True,
             "terminal_reason": "aborted_streaming", "api_error_status": 529,
             "errors": ["upstream said no"]})"#,
    );

    let err = claude::run("go", options_for(&exe)).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "claude: agent error (error_during_execution, aborted_streaming, HTTP 529): upstream said no"
    );
}

// ─── The permission round trip ───────────────────────────────────────────────

#[tokio::test]
async fn an_allow_echoes_the_original_input_back_verbatim() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "control_request", "request_id": "perm-1",
             "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                         "tool_use_id": "tu_9", "blocked_path": "/etc",
                         "input": {"command": "ls -la"}}})"#,
    );

    let handler: claude::PermissionHandler = std::sync::Arc::new(|tool, _input, ctx| {
        Box::pin(async move {
            assert_eq!(tool, "Bash");
            assert_eq!(ctx.tool_use_id, "tu_9");
            assert_eq!(ctx.blocked_path, "/etc");
            PermissionResult::allow()
        })
    });

    let opts = options_for(&exe)
        .with_default_permissions()
        .with_permission_handler(handler);
    let _stream = query("run it", opts).await;

    // The reply is a control_response written back on stdin, so it shows up in
    // the fake CLI's own log.
    for _ in 0..250 {
        if let Some(reply) = logged_message(&log, "control_response") {
            let response = &reply["response"];
            assert_eq!(response["subtype"], "success");
            assert_eq!(response["request_id"], "perm-1");
            assert_eq!(response["response"]["behavior"], "allow");
            // The CLI expects the input it should actually run; an allow that
            // omitted it would leave the call with nothing to execute.
            assert_eq!(response["response"]["updatedInput"]["command"], "ls -la");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the SDK never answered the permission request; a missing reply hangs the CLI");
}

#[tokio::test]
async fn a_deny_carries_its_message_and_optional_interrupt() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "control_request", "request_id": "perm-2",
             "request": {"subtype": "can_use_tool", "tool_name": "Write",
                         "input": {"path": "/etc/passwd"}}})"#,
    );

    let handler: claude::PermissionHandler = std::sync::Arc::new(|_, _, _| {
        Box::pin(async {
            PermissionResult::Deny {
                message: "not that file".into(),
                interrupt: true,
            }
        })
    });

    let opts = options_for(&exe)
        .with_default_permissions()
        .with_permission_handler(handler);
    let _stream = query("write it", opts).await;

    for _ in 0..250 {
        if let Some(reply) = logged_message(&log, "control_response") {
            let response = &reply["response"]["response"];
            assert_eq!(response["behavior"], "deny");
            assert_eq!(response["message"], "not that file");
            assert_eq!(response["interrupt"], true);
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the SDK never answered the permission request");
}

#[tokio::test]
async fn no_handler_fails_closed_rather_than_allowing() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "control_request", "request_id": "perm-3",
             "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                         "input": {"command": "rm -rf /"}}})"#,
    );

    // No handler registered: answering would grant a permission nobody approved.
    let _stream = query("go", options_for(&exe)).await;

    for _ in 0..250 {
        if let Some(reply) = logged_message(&log, "control_response") {
            let response = &reply["response"];
            assert_eq!(response["subtype"], "error");
            assert_eq!(response["request_id"], "perm-3");
            assert_eq!(response["error"], "canUseTool callback is not provided");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("an unanswered can_use_tool hangs the CLI");
}

#[tokio::test]
async fn an_unknown_control_request_is_still_acknowledged() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "control_request", "request_id": "note-1",
             "request": {"subtype": "some_future_notification"}})"#,
    );

    let _stream = query("go", options_for(&exe)).await;

    for _ in 0..250 {
        if let Some(reply) = logged_message(&log, "control_response") {
            assert_eq!(reply["response"]["subtype"], "success");
            assert_eq!(reply["response"]["request_id"], "note-1");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("every inbound control_request must be answered, including ones we only acknowledge");
}

// ─── Outbound control requests ───────────────────────────────────────────────

#[tokio::test]
async fn interrupt_leaves_the_session_alive_and_decodes_its_receipt() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "control_request" and req.get("subtype") == "interrupt":
        say({"type": "control_response",
             "response": {"subtype": "success", "request_id": msg.get("request_id"),
                          "response": {"still_queued": ["u-1", None, "u-2"]}}})"#,
    );

    let session = session(options_for(&exe)).await;
    let receipt = session.interrupt().await.unwrap().expect("a receipt");
    // Non-string elements are filtered; a JSON null would otherwise smuggle in
    // an empty id.
    assert_eq!(receipt.still_queued, vec!["u-1", "u-2"]);

    // The session is still usable: interrupt aborts the turn, not the process.
    session.send("next turn").await.unwrap();
    let entries = wait_for_logged(&log, 4).await;
    assert!(entries.contains(&"control_request:interrupt".to_string()));
    assert_eq!(entries.last().unwrap(), "user");
}

#[tokio::test]
async fn set_permission_mode_sends_mode_not_permission_mode() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "control_request":
        say({"type": "control_response",
             "response": {"subtype": "success", "request_id": msg.get("request_id")}})"#,
    );

    let session = session(options_for(&exe)).await;
    session
        .set_permission_mode(claude::permission_mode::PLAN)
        .await
        .unwrap();

    let raw = std::fs::read_to_string(&log).unwrap();
    let sent = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["request"]["subtype"] == "set_permission_mode")
        .expect("the request must have been written");

    assert_eq!(sent["request"]["mode"], "plan");
    assert!(
        sent["request"].get("permission_mode").is_none(),
        "the outbound request and the inbound notification do not share a spelling"
    );
}

#[tokio::test]
async fn a_control_request_reports_the_clis_error_text() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "control_request":
        say({"type": "control_response",
             "response": {"subtype": "error", "request_id": msg.get("request_id"),
                          "error": "unsupported subtype"}})"#,
    );

    let session = session(options_for(&exe)).await;
    let err = session.set_model("nope").await.unwrap_err();
    assert_eq!(err.to_string(), "claude: set_model: unsupported subtype");
}

// ─── Process-level failures ──────────────────────────────────────────────────

#[tokio::test]
async fn a_missing_binary_is_reported_as_such() {
    let opts = Options::new().with_claude_executable("/nonexistent/definitely/not/claude");
    let err = claude::query("hi", opts).await.unwrap_err();
    match err {
        claude::Error::CliNotFound { executable } => {
            assert!(executable.contains("not/claude"))
        }
        other => panic!("expected CliNotFound, got {other}"),
    }
}

#[tokio::test]
async fn a_crash_after_the_handshake_surfaces_stderr_as_a_system_error() {
    if python3().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))
    elif msg.get("type") == "user":
        sys.stderr.write("something went badly wrong\n")
        sys.stderr.flush()
        sys.exit(3)"#,
    );

    let mut stream = query("go", options_for(&exe)).await;

    let mut saw = None;
    while let Some(event) = stream.next_event().await {
        if let Some(system) = &event.system {
            if system.subtype == "error" {
                saw = Some(system.error.clone());
            }
        }
    }

    // A process that dies without a result would otherwise end the stream
    // silently, and the caller would have nothing to report.
    assert_eq!(saw.as_deref(), Some("something went badly wrong"));
}

#[tokio::test]
async fn dropping_a_stream_does_not_leave_the_subprocess_running() {
    // The liveness check reads /proc, so the assertion is only meaningful on
    // Linux; elsewhere it would be a guess rather than a check.
    if python3().is_none() || !cfg!(target_os = "linux") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("stdin.jsonl");
    // Acknowledges, then blocks forever on stdin — the shape of a live session.
    let exe = fake_cli(
        dir.path(),
        &log,
        r#"    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack_now(msg.get("request_id"))"#,
    );

    let marker = exe.to_string_lossy().into_owned();
    {
        let _session = session(options_for(&exe)).await;
        assert!(process_alive(&marker), "the fake CLI should be running");
    }

    for _ in 0..300 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if !process_alive(&marker) {
            return;
        }
    }
    panic!("the subprocess outlived its stream; a cancelled chat would leak one per turn");
}

/// Whether any process was launched with `marker` in its command line. Linux
/// only; elsewhere the assertion is skipped rather than guessed at.
fn process_alive(marker: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let cmdline = entry.path().join("cmdline");
        if let Ok(raw) = std::fs::read(&cmdline) {
            if String::from_utf8_lossy(&raw).contains(marker) {
                return true;
            }
        }
    }
    false
}

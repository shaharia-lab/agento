//! The chat turn, driven against a **scripted fake CLI** (#276).
//!
//! Every rule this port has to honour is a property of a *sequence*, not of a
//! function: that `result` does not end the stream when an `AskUserQuestion` is
//! pending, that an `AskUserQuestion` is answered by *denying* the tool, that a
//! turn with no final text persists nothing, that a subprocess dying emits no
//! frame at all. A unit test can reach none of them, which is why the SDK's own
//! suite uses this technique and why this file reuses it.
//!
//! The fake is a small Python program: no `claude` binary, no API key, runs in
//! CI like any other test.

use std::path::{Path, PathBuf};

use agento_lib::native::chat::persist;
use agento_lib::native::chat::turn::TurnState;
use agento_lib::native::chats;

/// The `log` sink the #335 assertions read.
///
/// A separate copy from `writes::testlog` because this is a different crate and
/// that one is `#[cfg(test)]` on the library. Installed once per process, since
/// `log::set_boxed_logger` allows exactly one.
mod testlog {
    use std::sync::{Mutex, Once, OnceLock};

    static LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    static INIT: Once = Once::new();

    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            if let Ok(mut lines) = lines().lock() {
                lines.push(format!("{} {}", record.level(), record.args()));
            }
        }
        fn flush(&self) {}
    }

    fn lines() -> &'static Mutex<Vec<String>> {
        LINES.get_or_init(Mutex::default)
    }

    pub fn install() {
        INIT.call_once(|| {
            let _ = log::set_boxed_logger(Box::new(Capture));
            log::set_max_level(log::LevelFilter::Trace);
        });
    }

    pub fn matching(needle: &str) -> Vec<String> {
        install();
        lines()
            .lock()
            .map(|lines| {
                lines
                    .iter()
                    .filter(|line| line.contains(needle))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The commit runs in a task detached from the request, so its line can
    /// arrive after the body has ended.
    pub async fn wait_for(needle: &str) -> String {
        for _ in 0..200 {
            if let Some(line) = matching(needle).into_iter().next() {
                return line;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("no log line matching {needle:?}");
    }
}

/// Skips when the machine has no `python3`, as the SDK suite does.
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

/// A chat id unique to the calling test.
///
/// The live-session registry is a **process global** — it has to be, since a
/// chat turn is process state — and cargo runs tests in parallel in one
/// process. Sharing an id would make one test see another's busy lock and get
/// the 409, which is a test collision rather than a finding.
fn unique_id(label: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("{label}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// A database with the real schema and one chat row.
fn migrated_with_chat(id: &str) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let mut conn = rusqlite::Connection::open(file.path()).expect("open");
    agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO chat_sessions (id, title, agent_slug, created_at, updated_at)
         VALUES (?1, 'New Chat', '', '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        [id],
    )
    .expect("seed chat");
    file
}

/// The same, plus an agent whose capabilities name **one** built-in tool.
///
/// That is what makes `wrap_permission_handler` wrap the handler at all: an
/// empty allowlist returns the inner one unwrapped, so a test that skipped the
/// agent would exercise the bypass rather than the allowlist. `built_in` only,
/// so the allowlist is exactly the one tool — the local server has its own
/// fixture below, and an agent naming an MCP server still forwards to Go.
fn migrated_with_allowlisted_agent(id: &str) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let mut conn = rusqlite::Connection::open(file.path()).expect("open");
    agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO agents (slug, name, capabilities) VALUES ('gated', 'Gated', ?1)",
        [r#"{"built_in":["Read"]}"#],
    )
    .expect("seed agent");
    conn.execute(
        "INSERT INTO chat_sessions (id, title, agent_slug, created_at, updated_at)
         VALUES (?1, 'New Chat', 'gated', '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        [id],
    )
    .expect("seed chat");
    file
}

/// The same, plus an agent whose capabilities name the **local** in-process MCP
/// server (#310) rather than a built-in.
///
/// That is the case `runner::build_options` used to refuse outright, so before
/// #310 a chat on this agent forwarded to Go and none of the turn machinery ran
/// at all.
fn migrated_with_local_tool_agent(id: &str) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let mut conn = rusqlite::Connection::open(file.path()).expect("open");
    agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO agents (slug, name, capabilities) VALUES ('clock', 'Clock', ?1)",
        [r#"{"local":["current_time"]}"#],
    )
    .expect("seed agent");
    conn.execute(
        "INSERT INTO chat_sessions (id, title, agent_slug, created_at, updated_at)
         VALUES (?1, 'New Chat', 'clock', '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        [id],
    )
    .expect("seed chat");
    file
}

fn chat_row(id: &str) -> agento_lib::native::chat::runner::ChatRow {
    agento_lib::native::chat::runner::ChatRow {
        id: id.to_string(),
        title: "New Chat".into(),
        agent_slug: String::new(),
        sdk_session_id: String::new(),
        working_dir: String::new(),
        model: String::new(),
        settings_profile_id: String::new(),
    }
}

fn messages(file: &tempfile::NamedTempFile, id: &str) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(file.path()).expect("open");
    let mut stmt = conn
        .prepare("SELECT role, content FROM chat_messages WHERE session_id = ?1 ORDER BY id")
        .expect("prepare");
    let rows = stmt
        .query_map([id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

// ─── The persistence rules ────────────────────────────────────────────────────

/// The headline rule, and the half of it that is easy to over-apply: no
/// messages, but the session row is still written.
#[test]
fn an_interrupted_turn_persists_no_messages_but_still_updates_the_session() {
    let file = migrated_with_chat("c1");
    let state = TurnState {
        // No final text: the stream was interrupted.
        input_tokens: 42,
        ..Default::default()
    };
    persist::commit(file.path(), &chat_row("c1"), "my question", &state, true).expect("commit");

    assert!(
        messages(&file, "c1").is_empty(),
        "not even the user message may be stored"
    );

    let conn = rusqlite::Connection::open(file.path()).expect("open");
    let (title, tokens): (String, i64) = conn
        .query_row(
            "SELECT title, total_input_tokens FROM chat_sessions WHERE id='c1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");
    // A title derived from a message that was never stored — Go's behaviour,
    // and the reason "persists nothing" cannot be read as covering the row.
    assert_eq!(title, "my question");
    assert_eq!(tokens, 42);
}

#[test]
fn a_completed_turn_stores_the_user_message_and_the_reply_in_order() {
    let file = migrated_with_chat("c1");
    let state = TurnState {
        assistant_text: "the answer".into(),
        sdk_session_id: "sdk-1".into(),
        ..Default::default()
    };
    persist::commit(file.path(), &chat_row("c1"), "the question", &state, true).expect("commit");

    assert_eq!(
        messages(&file, "c1"),
        vec![
            ("user".to_string(), "the question".to_string()),
            ("assistant".to_string(), "the answer".to_string()),
        ]
    );
}

/// The blocks a turn accumulated are stored with the assistant message, and a
/// `tool_use` input keeps its bytes — key order and number spelling included.
#[test]
fn tool_use_input_survives_the_round_trip_byte_for_byte() {
    let file = migrated_with_chat("c1");
    let mut blocks = Vec::new();
    persist::append_assistant_blocks(
        &mut blocks,
        br#"{"message":{"content":[
            {"type":"tool_use","id":"t1","name":"Bash","input":{"z":1.50,"a":[1,2]}}
        ]}}"#,
    );
    let state = TurnState {
        assistant_text: "done".into(),
        blocks,
        ..Default::default()
    };
    persist::commit(file.path(), &chat_row("c1"), "run it", &state, false).expect("commit");

    // Read back through the same path the chat detail endpoint uses.
    let detail = chats::get(file.path(), "c1")
        .expect("get")
        .expect("chat exists");
    let assistant = detail
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .expect("assistant message");
    let input = assistant.blocks[0]
        .input
        .as_ref()
        .expect("tool_use input")
        .get();
    assert_eq!(
        input, r#"{"z":1.50,"a":[1,2]}"#,
        "a round trip through a JSON value would sort the keys and respell 1.50"
    );
}

// ─── The sequence rules, against a real subprocess ────────────────────────────

/// Writes an executable fake `claude` that replies with a scripted sequence.
///
/// Every stdin line is appended to `<dir>/stdin.jsonl`, which is what lets a
/// test assert on what the SDK **wrote back** rather than only on the frames it
/// emitted. The permission round trip is invisible without it: its whole
/// observable effect on the CLI side is a `control_response`.
fn fake_cli(dir: &Path, emit: &str) -> PathBuf {
    let script = format!(
        r#"#!/usr/bin/env {python}
import json, sys, threading, time

LOG = {log}
_log = open(LOG, "a")
_lock = threading.Lock()

def record(entry):
    with _lock:
        _log.write(json.dumps(entry) + "\n")
        _log.flush()

def say(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def raw(line):
    # Exact bytes. json.dumps would insert spaces after ':' and ',' and would
    # renormalise 1.50 to 1.5 — which is precisely what a byte-exactness test
    # must not depend on.
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
    record(msg)
    req = msg.get("request") or {{}}
{emit}
"#,
        python = python3().unwrap_or_else(|| "python3".into()),
        log = serde_json::to_string(&stdin_log(dir).to_string_lossy()).unwrap(),
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

/// Where [`fake_cli`] records the lines it was sent.
fn stdin_log(dir: &Path) -> PathBuf {
    dir.join("stdin.jsonl")
}

/// Everything the SDK wrote to the CLI, decoded, in order.
fn written(dir: &Path) -> Vec<serde_json::Value> {
    let Ok(raw) = std::fs::read_to_string(stdin_log(dir)) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The payload of the `control_response` answering `request_id`, polled until it
/// appears — the SDK writes it from the reader task, after the frame the test
/// was waiting on.
async fn control_response_for(dir: &Path, request_id: &str) -> serde_json::Value {
    for _ in 0..200 {
        for line in written(dir) {
            if line.get("type").and_then(|t| t.as_str()) != Some("control_response") {
                continue;
            }
            let response = &line["response"];
            if response.get("request_id").and_then(|r| r.as_str()) == Some(request_id) {
                return response["response"].clone();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!(
        "no control_response for {request_id:?}; wrote: {:?}",
        written(dir)
    );
}

/// Collect an SSE body into frames, splitting on the blank line.
fn frames(body: &str) -> Vec<(String, String)> {
    body.split("\n\n")
        .filter(|chunk| !chunk.trim().is_empty())
        .map(|chunk| {
            let mut event = String::new();
            let mut data = String::new();
            for line in chunk.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    event = rest.to_string();
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data = rest.to_string();
                }
            }
            (event, data)
        })
        .collect()
}

/// The CLI's own lines are forwarded **verbatim**, named by their `type`, and a
/// `result` ends the turn when nothing is pending.
#[tokio::test]
async fn a_turn_forwards_the_cli_lines_verbatim_and_ends_on_the_final_result() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    // A `tool_use` input with an unsorted key and a trailing-zero float: if any
    // layer re-encodes, this is what changes.
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        raw('{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"t1","name":"Bash","input":{"z":1.50,"a":1}}]}}')
        raw('{"type":"result","subtype":"success","is_error":false,"result":"all done","session_id":"sdk-xyz","usage":{"input_tokens":3,"output_tokens":4}}')
"#,
    );

    let id = unique_id("verbatim");
    let body = run_turn(&cli, &id, "hello").await;
    let frames = frames(&body);

    let names: Vec<&str> = frames.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(names, vec!["assistant", "result"], "body was: {body}");

    // Verbatim: the assistant frame carries the CLI's bytes, unsorted keys and
    // `1.50` intact.
    // The whole assistant line, byte for byte — unsorted keys and the trailing
    // zero intact. Any layer that decoded and re-encoded would fail here.
    assert_eq!(
        frames[0].1,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"t1","name":"Bash","input":{"z":1.50,"a":1}}]}}"#
    );
}

/// The rule most likely to be got wrong: `result` means "turn done", not
/// "stream done". With an `AskUserQuestion` pending the same subprocess carries
/// on, so **one HTTP request spans two turns and two `result` frames**.
#[tokio::test]
async fn an_ask_user_question_keeps_the_stream_open_past_the_first_result() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        text = json.dumps(msg.get("message", {}).get("content", ""))
        if "second" not in text:
            raw('{"type":"assistant","message":{"content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{"z":1.50,"question":"which one?"}}]}}')
            say({"type": "result", "subtype": "success", "is_error": False,
                 "result": "", "session_id": "sdk-1",
                 "usage": {"input_tokens": 3, "output_tokens": 4}})
        else:
            say({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "thanks"}
            ]}})
            say({"type": "result", "subtype": "success", "is_error": False,
                 "result": "final answer", "session_id": "sdk-1",
                 "usage": {"input_tokens": 5, "output_tokens": 6}})
"#,
    );

    let id = unique_id("askuser");
    let (body, answered, db) = run_turn_answering_keeping_db(&cli, &id, "hello", "second").await;
    assert!(answered, "the answer was never delivered");
    let frames = frames(&body);
    let names: Vec<&str> = frames.iter().map(|(e, _)| e.as_str()).collect();

    // The synthetic prompt sits between the two results — proof the first one
    // did not end the stream.
    assert_eq!(
        names,
        vec![
            "assistant",
            "result",
            "user_input_required",
            "assistant",
            "result"
        ],
        "body was: {body}"
    );

    // Byte for byte, including the out-of-order key and the trailing zero. A
    // round trip through a `serde_json::Value` yields `{"question":…,"z":1.5}`
    // — which is exactly what this path used to do, and what the single-key
    // payload this test started with could not detect.
    assert_eq!(
        frames[2].1, r#"{"input":{"z":1.50,"question":"which one?"}}"#,
        "the prompt must carry the tool input verbatim"
    );

    // The multi-turn persistence claim, which the frame names alone could not
    // reach (#298): blocks and token totals **accumulate** across the turns of
    // one request, while `assistant_text` is reset — so the stored reply is the
    // *second* turn's text, not both concatenated, and the row carries both
    // turns' tokens.
    let conn = rusqlite::Connection::open(db.path()).expect("open");
    let stored: Vec<(String, String)> = conn
        .prepare("SELECT role, content FROM chat_messages WHERE session_id = ?1 ORDER BY id")
        .expect("prepare")
        .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(
        stored,
        vec![
            ("user".to_string(), "hello".to_string()),
            ("assistant".to_string(), "final answer".to_string()),
        ],
        "one user message and the *last* turn's text, not both turns concatenated"
    );

    // Both `tool_use` blocks survive, in order — that is the accumulation half.
    let blocks: String = conn
        .query_row(
            "SELECT blocks FROM chat_messages WHERE role = 'assistant'",
            [],
            |r| r.get(0),
        )
        .expect("blocks");
    assert!(blocks.contains(r#""name":"AskUserQuestion""#), "{blocks}");

    let (input, output): (i64, i64) = conn
        .query_row(
            "SELECT total_input_tokens, total_output_tokens FROM chat_sessions WHERE id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("totals");
    // 3+5 and 4+6: **both** turns' usage, which is the accumulation. Resetting
    // per turn the way `assistant_text` is reset would store 5 and 6.
    assert_eq!(
        (input, output),
        (8, 10),
        "token totals accumulate across the turns of one request"
    );
}

/// A mid-stream failure is a `result` with `is_error: true`, never an `error`
/// event — the 200 was committed before the first frame.
#[tokio::test]
async fn a_mid_stream_failure_arrives_as_an_error_result_and_ends_the_turn() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        raw('{"type":"result","subtype":"error_during_execution","is_error":true,"result":"","session_id":"sdk-err","usage":{}}')
        raw('{"type":"assistant","message":{"content":[{"type":"text","text":"unreachable"}]}}')
"#,
    );

    let id = unique_id("midfail");
    let body = run_turn(&cli, &id, "hello").await;
    let frames = frames(&body);
    let names: Vec<&str> = frames.iter().map(|(e, _)| e.as_str()).collect();

    assert_eq!(names, vec!["result"], "the turn ends immediately: {body}");
    assert!(frames[0].1.contains(r#""is_error":true"#));
    assert!(
        !body.contains("error\ndata"),
        "there is no `error` event type on this path"
    );
}

/// A client that disconnects while a prompt is pending must tear the turn down:
/// release the busy lock, close the subprocess and let the commit run.
///
/// This is the case that had no coverage and no code. The permission handler is
/// awaited **inline on the SDK's reader task**, so while it is parked nothing
/// arrives and the stream loop has nothing to send — the disconnect is
/// invisible unless something races it explicitly. Without that race, closing a
/// tab left the chat answering `409 session is busy` for the life of the
/// process and leaked a `claude` subprocess.
///
/// This drives the **post-result continuation** and the loop's own arm. The
/// third wait — the `can_use_tool` permission round trip — needs the fake to
/// issue a control request, which is #298.
#[tokio::test]
async fn a_disconnect_while_a_prompt_is_pending_releases_the_chat() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    // Asks a question and then says nothing more, so the turn parks exactly
    // where a real one waits for the user.
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        raw('{"type":"assistant","message":{"content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{"question":"stuck?"}}]}}')
        raw('{"type":"result","subtype":"success","is_error":false,"result":"","session_id":"sdk-1","usage":{}}')
"#,
    );

    let id = unique_id("disconnect");
    let file = migrated_with_chat(&id);
    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let response = agento_lib::native::chat::turn::run(
        file.path().to_path_buf(),
        id.clone(),
        "hello".to_string(),
    )
    .await
    .expect("the turn should stream");

    // Read until the prompt frame arrives, *then* disconnect.
    //
    // Dropping the response immediately would not test this: the first
    // `out.send` would fail and tear the turn down through the pre-existing
    // error path, which passes with the whole disconnect fix reverted. Waiting
    // for `user_input_required` parks the turn in the post-result continuation,
    // where nothing is being sent and only the disconnect race can free it.
    read_until(response, "user_input_required").await;

    assert_released(&id, "the post-result continuation").await;
}

/// The other half of the same rule, for the event loop's own arm.
///
/// The CLI here says **nothing** after initialize, so the turn parks in
/// `stream_events` with no frame ever written — meaning no failing send can
/// notice the disconnect and `out.closed()` is the only thing that can.
#[tokio::test]
async fn a_disconnect_while_the_loop_waits_on_a_silent_cli_releases_the_chat() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
"#,
    );

    let id = unique_id("disconnect-loop");
    let file = migrated_with_chat(&id);
    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let response = agento_lib::native::chat::turn::run(
        file.path().to_path_buf(),
        id.clone(),
        "hello".to_string(),
    )
    .await
    .expect("the turn should stream");

    // No frame will ever arrive, so dropping now is a tab closed on a turn that
    // is waiting on a tool call — the case with no send to fail.
    drop(response);

    assert_released(&id, "the event loop's disconnect arm").await;
}

/// The SSE body must end when the **turn** ends, not when the subprocess's
/// stdout closes.
///
/// Nothing may hold a strong clone of the body sender past the turn. The
/// permission handler is the tempting place to put one — it needs to notice a
/// disconnect — but the SDK's reader task owns that handler until stdout hits
/// EOF, which a grandchild can defer indefinitely. Here the CLI backgrounds a
/// `sleep` that inherits stdout and then exits, which is what any `Bash` call
/// starting a dev server or watcher does. The frontend clears its streaming
/// state only when the body ends, so a body that outlives the turn is a chat
/// stuck mid-stream with the composer blocked.
#[tokio::test]
async fn the_body_ends_with_the_turn_even_when_a_grandchild_holds_stdout_open() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        import subprocess
        # Inherits stdout and outlives us: stdout never reaches EOF.
        subprocess.Popen(["sleep", "30"])
        raw('{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}')
        raw('{"type":"result","subtype":"success","is_error":false,"result":"hi","session_id":"sdk-1","usage":{}}')
        sys.exit(0)
"#,
    );

    let id = unique_id("grandchild");
    let file = migrated_with_chat(&id);
    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);

    let response = agento_lib::native::chat::turn::run(
        file.path().to_path_buf(),
        id.clone(),
        "hello".to_string(),
    )
    .await
    .expect("the turn should stream");

    // 10s is far below the grandchild's 30s and far above a healthy teardown,
    // which is milliseconds — so this cannot be flaky in either direction.
    let started = std::time::Instant::now();
    let drained = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        use http_body_util::BodyExt;
        response.into_body().collect().await.map(|c| c.to_bytes())
    })
    .await;

    let body = drained
        .expect("the body outlived the turn — something still holds a strong body sender")
        .expect("body error");
    assert!(
        String::from_utf8_lossy(&body).contains("\"hi\""),
        "the turn should still have streamed its reply"
    );
    eprintln!("body ended in {:?}", started.elapsed());
}

/// Consume frames until one contains `marker`, then disconnect by dropping the
/// body. Returns once the turn is parked and the client is gone.
async fn read_until(response: axum::http::Response<axum::body::Body>, marker: &str) {
    use http_body_util::BodyExt;

    let mut body = response.into_body();
    let mut seen = String::new();
    while !seen.contains(marker) {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(20), body.frame())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for a frame containing {marker:?}"))
            .unwrap_or_else(|| panic!("stream ended before {marker:?}; saw: {seen}"))
            .expect("frame error");
        if let Some(chunk) = frame.data_ref() {
            seen.push_str(&String::from_utf8_lossy(chunk));
        }
    }
    drop(body);
}

/// Poll rather than sleep a fixed amount: teardown is asynchronous, and a fixed
/// wait is either flaky or slow.
async fn assert_released(id: &str, which: &str) {
    let mut released = false;
    for _ in 0..200 {
        if agento_lib::native::chat::live::registry().get(id).is_none()
            && agento_lib::native::chat::live::registry().try_lock(id)
        {
            released = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        released,
        "a disconnected turn must release the busy lock via {which}, or the chat is wedged"
    );
}

// ─── Harness ──────────────────────────────────────────────────────────────────

/// Drive one turn against the fake CLI and collect the whole SSE body.
async fn run_turn(cli: &Path, chat_id: &str, content: &str) -> String {
    let (body, _, _db) = run_turn_inner(cli, chat_id, content, None).await;
    body
}

/// The same, answering the first `user_input_required` with `answer` — and
/// keeping the database, so the caller can assert on what the turn **stored**
/// rather than only on the frames it emitted.
async fn run_turn_answering_keeping_db(
    cli: &Path,
    chat_id: &str,
    content: &str,
    answer: &str,
) -> (String, bool, tempfile::NamedTempFile) {
    run_turn_inner(cli, chat_id, content, Some(answer.to_string())).await
}

// ─── The permission round trip (#298) ─────────────────────────────────────────
//
// Until now `tests/chat_turn.rs` drove `AskUserQuestion` as an assistant
// `tool_use` block, which exercises `extract_ask_user_question` and the
// post-result continuation — and *not* the permission handler. Everything below
// goes through a real `can_use_tool` control request, which is the only way to
// reach `build_permission_handler`, `wrap_permission_handler`, and the deny
// that carries the user's answer back to the model.

/// Drive a turn whose fake CLI issues a `can_use_tool`, answering the prompt
/// through the live registry the way `/input` and `/permission` do.
///
/// Returns the body; the caller reads what was written back from the fake CLI's
/// own directory with [`control_response_for`].
async fn run_permission_turn(
    cli: &Path,
    file: &tempfile::NamedTempFile,
    chat_id: &str,
    answer: Answer,
) -> String {
    use http_body_util::BodyExt;

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", cli);

    let response = agento_lib::native::chat::turn::run(
        file.path().to_path_buf(),
        chat_id.to_string(),
        "hello".to_string(),
    )
    .await
    .expect("the turn should stream");

    let id = chat_id.to_string();
    let deliver = tokio::spawn(async move {
        for _ in 0..200 {
            if let Some((_, input_tx, perm_tx)) =
                agento_lib::native::chat::live::registry().get(&id)
            {
                let sent = match &answer {
                    Answer::Text(text) => input_tx.send(text.clone()).await.is_ok(),
                    Answer::Permission(allow) => perm_tx.send(*allow).await.is_ok(),
                    Answer::None => return,
                };
                if sent {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    });

    // Bounded for the same reason `collect_with_timeout` is — and for the
    // `Answer::None` case the bound is the *point*: if a prompt is raised that
    // should not have been, nothing ever answers it and the turn parks.
    let collected = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        response.into_body().collect(),
    )
    .await
    .expect("the turn parked — if nothing was meant to be answered, a prompt was raised that should not have been")
    .expect("body");
    deliver.abort();
    String::from_utf8(collected.to_bytes().to_vec()).expect("utf8")
}

enum Answer {
    /// `POST /input` — the `AskUserQuestion` reply.
    Text(String),
    /// `POST /permission` — the allow/deny for an ordinary tool.
    Permission(bool),
    /// Nothing is answered, because nothing should be asked.
    None,
}

/// The headline rule `desktop/CLAUDE.md` listed as "pinned by
/// `tests/chat_turn.rs`" and which nothing actually reached: **`AskUserQuestion`
/// is answered by *denying* the tool with the user's text as the message.** That
/// is how the answer gets to the model without the tool ever running.
///
/// Reverting `PermissionResult::deny(answer)` to an `allow` — the obvious
/// "fix" — leaves every frame in this test unchanged and fails only on the
/// written-back behaviour.
#[tokio::test]
async fn an_ask_user_question_is_answered_by_denying_the_tool_with_the_users_text() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "control_request", "request_id": "perm-1",
             "request": {"subtype": "can_use_tool", "tool_name": "AskUserQuestion",
                         "input": {"question": "which one?"}}})
    elif msg.get("type") == "control_response":
        say({"type": "result", "subtype": "success", "is_error": False,
             "result": "done", "session_id": "sdk-1", "usage": {}})
"#,
    );

    let id = unique_id("perm-ask");
    let file = migrated_with_chat(&id);
    let body = run_permission_turn(&cli, &file, &id, Answer::Text("the second one".into())).await;

    // The prompt reaches the client as the synthetic event, carrying the tool's
    // own input.
    let frames = frames(&body);
    let names: Vec<&str> = frames.iter().map(|(e, _)| e.as_str()).collect();
    assert!(
        names.contains(&"user_input_required"),
        "no prompt frame; body was: {body}"
    );

    // …and the answer goes back as a **denial**, not an allow.
    let answered = control_response_for(dir.path(), "perm-1").await;
    assert_eq!(answered["behavior"], "deny", "answered: {answered}");
    assert_eq!(answered["message"], "the second one");
}

/// An ordinary tool is a genuine allow/deny prompt, answered through
/// `/permission`. The frame is `permission_request` and it carries the tool's
/// name.
#[tokio::test]
async fn an_ordinary_tool_prompts_for_permission_and_the_answer_is_written_back() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };

    for (allow, want_behavior) in [(true, "allow"), (false, "deny")] {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = fake_cli(
            dir.path(),
            r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "control_request", "request_id": "perm-1",
             "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                         "input": {"command": "ls"}}})
    elif msg.get("type") == "control_response":
        say({"type": "result", "subtype": "success", "is_error": False,
             "result": "done", "session_id": "sdk-1", "usage": {}})
"#,
        );

        let id = unique_id("perm-tool");
        let file = migrated_with_chat(&id);
        let body = run_permission_turn(&cli, &file, &id, Answer::Permission(allow)).await;

        let frames = frames(&body);
        let prompt = frames
            .iter()
            .find(|(event, _)| event == "permission_request")
            .unwrap_or_else(|| panic!("no permission_request frame; body was: {body}"));
        assert_eq!(prompt.1, r#"{"tool_name":"Bash","input":{"command":"ls"}}"#);

        let answered = control_response_for(dir.path(), "perm-1").await;
        assert_eq!(answered["behavior"], want_behavior, "answered: {answered}");
        if !allow {
            // Go's own wording, which the frontend shows.
            assert_eq!(answered["message"], "Permission denied by user");
        }
    }
}

/// `wrap_permission_handler`'s allowlist: a tool the agent does not name is
/// denied **without the user ever seeing a prompt**.
///
/// The absence is the assertion, so this also proves the frame in the previous
/// test was not incidental — and nothing answers here, because nothing should
/// be asked.
#[tokio::test]
async fn a_tool_outside_the_agents_allowlist_is_denied_without_a_prompt() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "control_request", "request_id": "perm-1",
             "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                         "input": {"command": "rm -rf /"}}})
    elif msg.get("type") == "control_response":
        say({"type": "result", "subtype": "success", "is_error": False,
             "result": "done", "session_id": "sdk-1", "usage": {}})
"#,
    );

    let id = unique_id("perm-gated");
    let file = migrated_with_allowlisted_agent(&id);
    let body = run_permission_turn(&cli, &file, &id, Answer::None).await;

    assert!(
        !body.contains("permission_request"),
        "a tool outside the allowlist must not reach the user; body was: {body}"
    );

    let answered = control_response_for(dir.path(), "perm-1").await;
    assert_eq!(answered["behavior"], "deny", "answered: {answered}");
    assert_eq!(
        answered["message"],
        "tool \"Bash\" is not in this agent's allowed capabilities"
    );
}

/// …and `AskUserQuestion` is exempt from that allowlist, because it is the
/// interactive Q&A mechanism rather than a capability. The same agent, which
/// names only `Read`, still prompts for it.
#[tokio::test]
async fn ask_user_question_bypasses_the_allowlist() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        say({"type": "control_request", "request_id": "perm-1",
             "request": {"subtype": "can_use_tool", "tool_name": "AskUserQuestion",
                         "input": {"question": "which?"}}})
    elif msg.get("type") == "control_response":
        say({"type": "result", "subtype": "success", "is_error": False,
             "result": "done", "session_id": "sdk-1", "usage": {}})
"#,
    );

    let id = unique_id("perm-bypass");
    let file = migrated_with_allowlisted_agent(&id);
    let body = run_permission_turn(&cli, &file, &id, Answer::Text("mine".into())).await;

    assert!(
        body.contains("user_input_required"),
        "AskUserQuestion must reach the user even when the agent does not name it; body was: {body}"
    );
    let answered = control_response_for(dir.path(), "perm-1").await;
    assert_eq!(answered["behavior"], "deny");
    assert_eq!(answered["message"], "mine");
}

/// Serialises the turn tests.
///
/// `AGENTO_CLAUDE_EXECUTABLE` is **process-global**, and cargo runs these in
/// parallel in one process — so without this each test would overwrite the
/// others' fake CLI path and they would spawn each other's scripts. The lock is
/// held for the whole turn, not just the `set_var`, because the variable is read
/// when the subprocess spawns rather than when it is set.
fn env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn run_turn_inner(
    cli: &Path,
    chat_id: &str,
    content: &str,
    answer: Option<String>,
) -> (String, bool, tempfile::NamedTempFile) {
    let _env = env_lock().lock().await;
    let file = migrated_with_chat(chat_id);
    // The SDK resolves the executable from this variable, so the fake stands in
    // for a real `claude` without any code path knowing.
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", cli);

    let response = agento_lib::native::chat::turn::run(
        file.path().to_path_buf(),
        chat_id.to_string(),
        content.to_string(),
    )
    .await
    .expect("the turn should stream");

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        response
            .headers()
            .get("x-accel-buffering")
            .and_then(|v| v.to_str().ok()),
        Some("no")
    );

    let mut answered = false;
    if let Some(answer) = answer {
        // Deliver the answer once the prompt has been emitted. The registry is
        // the same one `/input` uses, so this exercises that path.
        let chat_id = chat_id.to_string();
        let handle = tokio::spawn(async move {
            for _ in 0..200 {
                if let Some((_, input_tx, _)) =
                    agento_lib::native::chat::live::registry().get(&chat_id)
                {
                    if input_tx.send(answer.clone()).await.is_ok() {
                        return true;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            false
        });
        let collected = collect_with_timeout(response).await;
        answered = handle.await.unwrap_or(false);
        return (
            String::from_utf8(collected.to_bytes().to_vec()).expect("utf8"),
            answered,
            file,
        );
    }

    let collected = collect_with_timeout(response).await;
    (
        String::from_utf8(collected.to_bytes().to_vec()).expect("utf8"),
        answered,
        file,
    )
}

/// Collect a turn's body, bounded.
///
/// A hang rather than a failure is this suite's characteristic break — the turn
/// parks on a wait nothing will satisfy — so an unbounded `collect` turns a
/// regression into a CI job that sits until the runner's own limit and reports
/// nothing useful. The bound is what makes it a test failure with a name.
async fn collect_with_timeout(
    response: axum::http::Response<axum::body::Body>,
) -> http_body_util::Collected<axum::body::Bytes> {
    use http_body_util::BodyExt;

    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        response.into_body().collect(),
    )
    .await
    .expect("the turn should end rather than park on a wait nothing satisfies")
    .expect("body")
}

// ─── The local in-process tools server (#310) ─────────────────────────────────

/// Drive one turn against a database the caller seeded, and collect its body.
///
/// [`run_turn`] makes its own agent-less chat; this is the same thing for the
/// tests whose whole subject is *which agent* the chat names.
async fn run_turn_on(
    cli: &Path,
    file: &tempfile::NamedTempFile,
    chat_id: &str,
    content: &str,
) -> String {
    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", cli);

    let response = agento_lib::native::chat::turn::run(
        file.path().to_path_buf(),
        chat_id.to_string(),
        content.to_string(),
    )
    .await
    .expect("the turn should stream rather than forward");

    assert_eq!(response.status(), 200);
    let collected = collect_with_timeout(response).await;
    String::from_utf8(collected.to_bytes().to_vec()).expect("utf8")
}

/// The acceptance criterion of #310, end to end: an agent whose only tool is
/// the local in-process MCP server runs **natively**, and the tool it calls is
/// really answered by this process.
///
/// The fake CLI does what the real one does with `--mcp-config`: it reads the
/// server's URL and headers out of its own argv and dials the loopback
/// listener. That makes this the one test covering the whole chain at once —
/// `build_options` starting the server, `Options::build_args` putting it on the
/// command line, the bearer token travelling in `headers`, the stateless
/// transport answering a bare `tools/call`, and the answer text coming back as
/// a `tool_result` the turn stores.
///
/// Before #310 none of it ran: `build_options` refused this agent and the whole
/// turn forwarded to Go.
#[tokio::test]
async fn a_chat_using_a_local_tool_runs_natively_end_to_end() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        import urllib.request
        # `--mcp-config` is inline JSON in argv, exactly as the real CLI reads
        # it. A KeyError or a ValueError here is the assertion: it means the
        # server was never registered under the name the CLI prefixes with.
        cfg = json.loads(sys.argv[sys.argv.index("--mcp-config") + 1])
        server = cfg["mcpServers"]["local-tools"]
        headers = {"Content-Type": "application/json",
                   "Accept": "application/json, text/event-stream"}
        headers.update(server.get("headers") or {})
        call = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                           "params": {"name": "current_time",
                                      "arguments": {"timezone": "Asia/Tokyo"}}}).encode()
        reply = json.loads(urllib.request.urlopen(
            urllib.request.Request(server["url"], data=call, headers=headers)).read().decode())
        text = reply["result"]["content"][0]["text"]
        say({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "t1",
             "name": "mcp__local-tools__current_time",
             "input": {"timezone": "Asia/Tokyo"}}]}})
        say({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": text}]}})
        say({"type": "assistant", "message": {"content": [
            {"type": "text", "text": text}]}})
        say({"type": "result", "subtype": "success", "is_error": False,
             "result": text, "session_id": "sdk-local",
             "usage": {"input_tokens": 1, "output_tokens": 2}})
"#,
    );

    let id = unique_id("localtool");
    let file = migrated_with_local_tool_agent(&id);
    let body = run_turn_on(&cli, &file, &id, "what time is it in Tokyo?").await;

    // The tool really ran in this process: only the local server could have
    // produced this sentence, and only from Go's format string.
    assert!(
        body.contains("Current time in Asia/Tokyo: "),
        "the local tool was never reached; body was: {body}"
    );
    assert!(body.contains("(ISO 8601: "), "body was: {body}");

    // The block that gets stored carries the qualified name — the string that
    // is already in existing agents' allowlists and in transcripts on disk.
    let detail = chats::get(file.path(), &id)
        .expect("get")
        .expect("chat exists");
    let assistant = detail
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .expect("assistant message");
    let names: Vec<&str> = assistant
        .blocks
        .iter()
        .map(|b| b.name.as_str())
        .filter(|n| !n.is_empty())
        .collect();
    assert_eq!(
        names,
        vec!["mcp__local-tools__current_time"],
        "renaming either half of this breaks every agent that already names it"
    );
}

/// #335: the five service-layer lines one `AskUserQuestion` turn produces.
///
/// Driven through this suite rather than a unit test for the reason the whole
/// file exists: every one of them is a property of a *sequence*. `agent session
/// started` needs a live subprocess, the three `AskUserQuestion` lines need a
/// result that is not the end of the stream, and `message committed` is emitted
/// from the task detached from the request — so it can arrive after the body
/// has, which is why it is awaited rather than read.
#[tokio::test]
async fn one_turn_emits_the_service_layer_lines_go_emits() {
    let Some(_) = python3() else {
        eprintln!("skipping: no python3");
        return;
    };
    testlog::install();

    let dir = tempfile::tempdir().expect("tempdir");
    let cli = fake_cli(
        dir.path(),
        r#"
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
    elif msg.get("type") == "user":
        text = json.dumps(msg.get("message", {}).get("content", ""))
        if "second" not in text:
            raw('{"type":"assistant","message":{"content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{"question":"which one?"}}]}}')
            say({"type": "result", "subtype": "success", "is_error": False,
                 "result": "", "session_id": "sdk-logged",
                 "usage": {"input_tokens": 1, "output_tokens": 1}})
        else:
            say({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "thanks"}
            ]}})
            say({"type": "result", "subtype": "success", "is_error": False,
                 "result": "final answer", "session_id": "sdk-logged",
                 "usage": {"input_tokens": 1, "output_tokens": 1}})
"#,
    );

    let id = unique_id("logged");
    let (_body, answered, _db) = run_turn_answering_keeping_db(&cli, &id, "hello", "second").await;
    assert!(answered, "the prompt was never answered");

    // Each line carries the chat id, so a suite running in parallel cannot
    // supply another test's.
    for needle in [
        format!(r#"agent session started session_id="{id}""#),
        format!(r#"AskUserQuestion detected in stream session_id="{id}""#),
        format!(r#"sending user_input_required, waiting for answer session_id="{id}""#),
        format!(r#"received user answer, resuming session session_id="{id}""#),
    ] {
        let found = testlog::matching(&needle);
        assert_eq!(found.len(), 1, "expected one {needle:?}: {found:?}");
        assert!(found[0].starts_with("INFO "), "{}", found[0]);
    }

    // The commit is detached from the request, so this one is awaited.
    let committed = testlog::wait_for(&format!(r#"message committed session_id="{id}""#)).await;
    assert!(
        committed.contains(r#"sdk_session_id="sdk-logged""#),
        "{committed}"
    );
    assert!(committed.starts_with("INFO "), "{committed}");
}

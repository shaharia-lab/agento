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
fn fake_cli(dir: &Path, emit: &str) -> PathBuf {
    let script = format!(
        r#"#!/usr/bin/env {python}
import json, sys, threading, time

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
    req = msg.get("request") or {{}}
{emit}
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
            say({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "q1", "name": "AskUserQuestion",
                 "input": {"question": "which one?"}}
            ]}})
            say({"type": "result", "subtype": "success", "is_error": False,
                 "result": "", "session_id": "sdk-1", "usage": {}})
        else:
            say({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "thanks"}
            ]}})
            say({"type": "result", "subtype": "success", "is_error": False,
                 "result": "final answer", "session_id": "sdk-1", "usage": {}})
"#,
    );

    let id = unique_id("askuser");
    let (body, answered) = run_turn_answering(&cli, &id, "hello", "second").await;
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

    let prompt = &frames[2].1;
    assert!(
        prompt.contains(r#""input":{"question":"which one?"}"#),
        "the prompt carries the tool input verbatim: {prompt}"
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

// ─── Harness ──────────────────────────────────────────────────────────────────

/// Drive one turn against the fake CLI and collect the whole SSE body.
async fn run_turn(cli: &Path, chat_id: &str, content: &str) -> String {
    let (body, _) = run_turn_inner(cli, chat_id, content, None).await;
    body
}

/// The same, answering the first `user_input_required` with `answer`.
async fn run_turn_answering(
    cli: &Path,
    chat_id: &str,
    content: &str,
    answer: &str,
) -> (String, bool) {
    run_turn_inner(cli, chat_id, content, Some(answer.to_string())).await
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
) -> (String, bool) {
    use http_body_util::BodyExt;

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
        let collected = response.into_body().collect().await.expect("body");
        answered = handle.await.unwrap_or(false);
        return (
            String::from_utf8(collected.to_bytes().to_vec()).expect("utf8"),
            answered,
        );
    }

    let collected = response.into_body().collect().await.expect("body");
    (
        String::from_utf8(collected.to_bytes().to_vec()).expect("utf8"),
        answered,
    )
}

//! The `app_mention` handler, end to end (#568).
//!
//! **These live in the library, not in `src-tauri/tests/`, and they have to.**
//! Every assertion here is about a request Agento *sends to Slack* — the
//! threaded reply, its `thread_ts`, the permalink — and the seam that points a
//! Slack request at a fake is `client::API_BASE`, which is `#[cfg(test)]` on
//! this crate precisely so it cannot exist in a shipped binary. An
//! integration-test crate cannot reach it. `trigger/dispatcher.rs` records the
//! same constraint at the same wall, which is why `tests/trigger_run.rs` stops
//! one function short of Telegram's reply.
//!
//! So this file is `tests/slack_socket.rs`'s fake-server half and
//! `tests/headless_resume.rs`'s fake-CLI half, in one place: an axum fake Slack
//! that records every call, and a Python CLI that records its argv, refuses to
//! overlap with itself and answers a different sentence each time it is
//! spawned.
//!
//! Two process-global seams are taken in a fixed order everywhere below —
//! `api_base_lock` and then the `AGENTO_CLAUDE_EXECUTABLE` lock — because other
//! suites in this binary take the first one alone.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::Uri;

use crate::native::agent_run::RunResult;
use crate::native::integrations::slack::client::{api_base_lock, set_api_base};
use crate::native::integrations::slack::socket::AppMention;

use super::{reply_for, strip_mention, ERROR_REPLY, NO_RESPONSE_REPLY};

const BOT_USER: &str = "U0BOT";
const CHANNEL: &str = "C1";

// ─── The fake Slack ──────────────────────────────────────────────────────────

/// One request the fake answered: which method, and the body it carried.
#[derive(Clone, Debug)]
struct Call {
    method: String,
    body: String,
}

#[derive(Clone, Default)]
struct FakeSlack {
    calls: Arc<Mutex<Vec<Call>>>,
    /// Cleared to make `auth.test` refuse, which is the one Slack failure the
    /// handler cannot work around: without the bot user id it can neither strip
    /// the mention nor apply the empty-remainder rule.
    auth_works: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeSlack {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("the fake's lock").clone()
    }

    /// The `text` of every `chat.postMessage`, in the order Slack received them.
    fn posted(&self) -> Vec<(String, String)> {
        self.calls()
            .iter()
            .filter(|call| call.method == "chat.postMessage")
            .map(|call| {
                let payload: serde_json::Value =
                    serde_json::from_str(&call.body).expect("a JSON post body");
                (
                    payload["thread_ts"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    payload["text"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }
}

async fn serve(State(state): State<FakeSlack>, uri: Uri, body: String) -> axum::response::Response {
    let method = uri.path().trim_start_matches('/').to_string();
    state.calls.lock().expect("the fake's lock").push(Call {
        method: method.clone(),
        body,
    });
    let reply = match method.as_str() {
        "auth.test" if !state.auth_works.load(std::sync::atomic::Ordering::Relaxed) => {
            serde_json::json!({"ok": false, "error": "invalid_auth"})
        }
        "auth.test" => serde_json::json!({"ok": true, "user_id": BOT_USER, "team": "T"}),
        "conversations.info" => {
            serde_json::json!({"ok": true, "channel": {"id": CHANNEL, "name": "general"}})
        }
        "chat.getPermalink" => {
            serde_json::json!({"ok": true, "permalink": "https://slack.example/p/1"})
        }
        "chat.postMessage" => serde_json::json!({"ok": true, "ts": "1700000000.000200"}),
        _ => serde_json::json!({"ok": false, "error": "unknown_method"}),
    };
    axum::response::IntoResponse::into_response(axum::Json(reply))
}

/// Points every Slack request at a recording fake for as long as the guard lives.
async fn fake_slack() -> FakeSlack {
    let state = FakeSlack {
        calls: Arc::default(),
        auth_works: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    let app = axum::Router::new()
        .fallback(serve)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake slack");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    set_api_base(Some(base));
    state
}

// ─── The fake CLI ────────────────────────────────────────────────────────────

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

fn json_str(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy()).expect("encode path")
}

fn argv_log(dir: &Path) -> PathBuf {
    dir.join("argv.jsonl")
}
/// Exists while a fake CLI is answering. Two at once is the bug.
fn busy_marker(dir: &Path) -> PathBuf {
    dir.join("busy")
}
/// Written if a fake CLI ever found [`busy_marker`] already there.
fn overlap_marker(dir: &Path) -> PathBuf {
    dir.join("overlap")
}

/// A CLI that records its argv, refuses to overlap with itself, holds for
/// `hold_ms`, and answers `answer` with `{n}` replaced by how many times it has
/// been asked.
///
/// It inherits `tests/scheduled_run.rs`'s trap deliberately — **no exit after
/// the result** — so a run that went through `claude::client::Session` rather
/// than the one-shot `query` hangs rather than passing, which is what the
/// deadline around every await below is for.
fn fake_cli(dir: &Path, answer: &str, session: &str, is_error: bool, hold_ms: u64) -> PathBuf {
    let script = format!(
        r#"#!/usr/bin/env {python}
import json, os, sys, time

with open({argv_log}, "a") as _argv:
    _argv.write(json.dumps({{"argv": sys.argv, "cwd": os.getcwd()}}) + "\n")
    _argv.flush()

ANSWER = {answer}
SESSION = {session}
IS_ERROR = {is_error}
HOLD = {hold}
BUSY = {busy}
OVERLAP = {overlap}
COUNTER = {counter}

def say(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
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
        try:
            n = int(open(COUNTER).read()) + 1
        except Exception:
            n = 1
        open(COUNTER, "w").write(str(n))
        if os.path.exists(BUSY):
            open(OVERLAP, "w").close()
        open(BUSY, "w").close()
        time.sleep(HOLD / 1000.0)
        os.remove(BUSY)
        say({{"type": "result", "subtype": "success", "is_error": IS_ERROR,
             "result": ANSWER.replace("{{n}}", str(n)),
             "session_id": SESSION + "-" + str(n),
             "usage": {{"input_tokens": 1, "output_tokens": 2,
                       "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}}})
        # Deliberately no exit; see the doc comment.
        continue
"#,
        python = python3().unwrap_or_else(|| "python3".into()),
        argv_log = json_str(&argv_log(dir)),
        answer = serde_json::to_string(answer).expect("encode answer"),
        session = serde_json::to_string(session).expect("encode session"),
        is_error = if is_error { "True" } else { "False" },
        hold = hold_ms,
        busy = json_str(&busy_marker(dir)),
        overlap = json_str(&overlap_marker(dir)),
        counter = json_str(&dir.join("counter")),
    );
    let path = dir.join("fake-claude");
    std::fs::write(&path, script).expect("write the fake CLI");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod the fake CLI");
    }
    path
}

/// Every `(argv, cwd)` the fake CLI was invoked with, in order.
///
/// The working directory has no command-line flag — it is `Command::current_dir`
/// — so the process reporting its own is the only way to see it.
fn spawns(dir: &Path) -> Vec<(Vec<String>, String)> {
    let Ok(raw) = std::fs::read_to_string(argv_log(dir)) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line).expect("a spawn line");
            (
                serde_json::from_value(entry["argv"].clone()).expect("argv"),
                entry["cwd"].as_str().expect("cwd").to_string(),
            )
        })
        .collect()
}

fn flag<'a>(argv: &'a [String], name: &str) -> Option<&'a str> {
    let at = argv.iter().position(|arg| arg == name)?;
    argv.get(at + 1).map(String::as_str)
}

// ─── The database ────────────────────────────────────────────────────────────

/// One Slack integration, with inbound on, and no trigger rules yet.
fn migrated(dir: &Path, integration_id: &str) -> PathBuf {
    let db_path = dir.join("agento.db");
    let mut conn = rusqlite::Connection::open(&db_path).expect("open");
    crate::native::migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO integrations
            (id, name, type, enabled, credentials, services, created_at, updated_at,
             inbound_enabled, inbound_status, inbound_error)
         VALUES (?1, 'Slack', 'slack', 1, '{\"bot_token\":\"xoxb-t\"}', '{}',
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC', 1, '', '')",
        [integration_id],
    )
    .expect("seed the integration");
    db_path
}

/// A trigger rule, oldest-first by the `created_at` this is called with.
#[allow(clippy::too_many_arguments)] // one parameter per stored column, by design
fn seed_rule(
    db_path: &Path,
    integration_id: &str,
    id: &str,
    enabled: bool,
    channels: &str,
    model: &str,
    working_directory: &str,
    created_at: &str,
) {
    let conn = rusqlite::Connection::open(db_path).expect("open");
    conn.execute(
        "INSERT INTO trigger_rules
            (id, integration_id, name, agent_slug, enabled, filter_prefix, filter_keywords,
             filter_chat_ids, model, working_directory, settings_profile_id, permission_mode,
             timeout_minutes, created_at, updated_at)
         VALUES (?1, ?2, ?1, '', ?3, '', '[]', ?4, ?5, ?6, '', 'plan', 0, ?7, ?7)",
        rusqlite::params![
            id,
            integration_id,
            enabled,
            channels,
            model,
            working_directory,
            created_at
        ],
    )
    .expect("seed a rule");
}

fn messages(db_path: &Path, chat_id: &str) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(db_path).expect("open");
    let mut stmt = conn
        .prepare("SELECT role, content FROM chat_messages WHERE session_id = ?1 ORDER BY id")
        .expect("prepare");
    let rows = stmt
        .query_map([chat_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query");
    rows.map(|row| row.expect("a row")).collect()
}

/// `(chat_id, permalink)` for every mapped thread.
fn threads(db_path: &Path) -> Vec<(String, String, String)> {
    let conn = rusqlite::Connection::open(db_path).expect("open");
    let mut stmt = conn
        .prepare("SELECT thread_ts, chat_id, permalink FROM inbound_threads ORDER BY rowid")
        .expect("prepare");
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query");
    rows.map(|row| row.expect("a row")).collect()
}

fn chat_title(db_path: &Path, chat_id: &str) -> String {
    let conn = rusqlite::Connection::open(db_path).expect("open");
    conn.query_row(
        "SELECT title FROM chat_sessions WHERE id = ?1",
        [chat_id],
        |row| row.get(0),
    )
    .expect("the chat exists")
}

// ─── Driving ─────────────────────────────────────────────────────────────────

fn mention(text: &str, ts: &str, thread_ts: &str, integration_id: &str) -> AppMention {
    AppMention {
        integration_id: integration_id.to_string(),
        channel: CHANNEL.to_string(),
        user: "U1".to_string(),
        text: text.to_string(),
        ts: ts.to_string(),
        thread_ts: thread_ts.to_string(),
        event_id: format!("Ev{ts}"),
    }
}

/// The deadline every await below carries, naming the trap it catches.
async fn finish(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(45), future)
        .await
        .expect("the handler must finish, not hang: a `Session` here never sees stdout close");
}

/// Wait for `path` to appear, or give up. Never blocks the runtime thread: the
/// task it is waiting on is running on it.
async fn wait_for(path: &Path) {
    for _ in 0..2000 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{} never appeared", path.display());
}

// ─── The pure parts ──────────────────────────────────────────────────────────

/// The bot's own mention goes; everyone else's stays.
#[test]
fn only_the_bots_own_mention_is_stripped() {
    for (text, want) in [
        ("<@U0BOT> hello", "hello"),
        ("<@U0BOT|agento> hello", "hello"),
        ("hey <@U0BOT> what is up", "hey  what is up"),
        ("<@U0BOT>", ""),
        ("   <@U0BOT>   \n ", ""),
        // Somebody else, and somebody whose id merely starts the same way.
        ("<@U1> hello", "<@U1> hello"),
        ("<@U0BOTX> hello", "<@U0BOTX> hello"),
        ("<@U0BOT> tell <@U9> about it", "tell <@U9> about it"),
        // Nothing to strip is the text, trimmed.
        ("  plain  ", "plain"),
    ] {
        assert_eq!(strip_mention(text, BOT_USER), want, "stripping {text:?}");
    }
}

/// Every ending has a sentence, and a timeout has the same one as a failure —
/// `agent_run` reports a deadline as an `Err`, and #568's acceptance asks for
/// `ERROR_REPLY` on both.
#[test]
fn every_run_outcome_has_a_sentence() {
    let answered = RunResult {
        answer: "## Title\n\n[a](https://b)".to_string(),
        ..RunResult::default()
    };
    assert_eq!(reply_for(Ok(answered), "c"), "*Title*\n\n<https://b|a>");
    assert_eq!(reply_for(Ok(RunResult::default()), "c"), NO_RESPONSE_REPLY);
    assert_eq!(reply_for(Err("boom".to_string()), "c"), ERROR_REPLY);
    assert_eq!(
        reply_for(
            Err(crate::native::agent_run::DEADLINE_EXCEEDED.to_string()),
            "c"
        ),
        ERROR_REPLY,
        "a timeout is a failure with the same sentence"
    );
}

/// #568's last acceptance criterion, as a property of this file rather than of
/// one run: the message a stranger wrote is logged in exactly one place, and
/// that place is `debug`.
#[test]
fn the_prompt_is_logged_at_debug_and_nowhere_else() {
    // Whitespace-collapsed so the assertion survives rustfmt wrapping a call
    // across lines, which is what it does to the one line that matches.
    let source = include_str!("../inbound.rs");
    let flat = source.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut carrying = Vec::new();
    let mut rest = flat.as_str();
    while let Some(at) = rest.find("log::") {
        let tail = &rest[at..];
        let end = tail.find(");").map_or(tail.len(), |offset| offset + 2);
        let call = &tail[..end];
        // The word, not one binding: the message's own text is carried by
        // whatever is called `prompt` here and by `mention.text`, and a guard
        // that named today's binding would go quiet on a rename.
        if call.contains("prompt") || call.contains("mention.text") {
            carrying.push(call.to_string());
        }
        rest = &tail[end..];
    }

    assert_eq!(
        carrying.len(),
        1,
        "exactly one log line may carry the message's own words: {carrying:?}"
    );
    assert!(
        carrying[0].starts_with("log::debug!"),
        "and it must be `debug`: {}",
        carrying[0]
    );
}

// ─── End to end ──────────────────────────────────────────────────────────────

/// The first acceptance criterion, all of it: the chat carries the rule's
/// execution settings, the thread is mapped with its permalink, and the reply
/// lands in a thread hanging off the message that started it.
#[tokio::test]
async fn a_top_level_mention_starts_a_chat_and_answers_in_its_thread() {
    if python3().is_none() {
        eprintln!("no python3; skipping");
        return;
    }
    let _base = api_base_lock().await;
    let slack = fake_slack().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("repo");
    std::fs::create_dir(&cwd).expect("mkdir repo");
    let canonical = std::fs::canonicalize(&cwd).expect("canonicalize");
    let db = migrated(dir.path(), "s-start");
    seed_rule(
        &db,
        "s-start",
        "r",
        true,
        "[]",
        "rule-model",
        cwd.to_str().expect("utf-8 path"),
        "2026-01-01 00:00:00 +0000 UTC",
    );
    let cli = fake_cli(
        dir.path(),
        "## Title\n\nsee [a](https://b)",
        "sess",
        false,
        0,
    );

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    let handler = super::handler(&db, "s-start", "xoxb-t");
    finish(handler(mention(
        "<@U0BOT> summarise the day",
        "1700000000.000100",
        "",
        "s-start",
    )))
    .await;
    std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE");
    set_api_base(None);

    let mapped = threads(&db);
    assert_eq!(mapped.len(), 1, "one thread, mapped");
    let (thread_ts, chat_id, permalink) = mapped[0].clone();
    assert_eq!(
        thread_ts, "1700000000.000100",
        "a top-level mention's own ts is the thread"
    );
    assert_eq!(permalink, "https://slack.example/p/1");
    assert_eq!(
        chat_title(&db, &chat_id),
        "[Slack] #general: summarise the day",
        "the channel's name, and the prompt with the bot's mention already gone"
    );
    assert_eq!(
        messages(&db, &chat_id),
        vec![
            ("user".to_string(), "summarise the day".to_string()),
            (
                "assistant".to_string(),
                "## Title\n\nsee [a](https://b)".to_string()
            ),
        ],
        "the chat holds what was said, as Markdown; only Slack sees mrkdwn"
    );

    assert_eq!(
        slack.posted(),
        vec![(
            "1700000000.000100".to_string(),
            "*Title*\n\nsee <https://b|a>".to_string()
        )],
        "one reply, in the thread, converted"
    );

    let argv = spawns(dir.path());
    assert_eq!(argv.len(), 1, "one CLI process");
    assert_eq!(flag(&argv[0].0, "--model"), Some("rule-model"));
    assert_eq!(flag(&argv[0].0, "--permission-mode"), Some("plan"));
    assert_eq!(flag(&argv[0].0, "--resume"), None, "nothing to resume yet");
    assert_eq!(
        Path::new(&argv[0].1),
        canonical,
        "and the rule's working directory is the process's own"
    );
}

/// Acceptance criteria 2 and 4 together, because they are the same run: the
/// second mention arrives while the first is still in the CLI, and it must
/// resume the same chat, in order, without a second process for it.
#[tokio::test]
async fn a_second_mention_in_the_thread_queues_and_resumes_the_same_chat() {
    if python3().is_none() {
        eprintln!("no python3; skipping");
        return;
    }
    let _base = api_base_lock().await;
    let slack = fake_slack().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = migrated(dir.path(), "s-resume");
    seed_rule(
        &db,
        "s-resume",
        "r",
        true,
        "[]",
        "",
        "",
        "2026-01-01 00:00:00 +0000 UTC",
    );
    // Long enough that the second mention provably arrives mid-run.
    let cli = fake_cli(dir.path(), "answer {n}", "sess", false, 1200);

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    let handler = super::handler(&db, "s-resume", "xoxb-t");

    let first = tokio::spawn({
        let handler = Arc::clone(&handler);
        async move {
            handler(mention(
                "<@U0BOT> first",
                "1700000000.000100",
                "",
                "s-resume",
            ))
            .await;
        }
    });
    // Only once the first run is inside the CLI is the second one provably a
    // *concurrent* mention rather than a sequential one.
    wait_for(&busy_marker(dir.path())).await;
    let second = tokio::spawn({
        let handler = Arc::clone(&handler);
        async move {
            handler(mention(
                "<@U0BOT> second",
                "1700000000.000300",
                "1700000000.000100",
                "s-resume",
            ))
            .await;
        }
    });
    finish(async move {
        first.await.expect("the first handler");
        second.await.expect("the second handler");
    })
    .await;
    std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE");
    set_api_base(None);

    assert!(
        !overlap_marker(dir.path()).exists(),
        "two turns on one thread must never have two CLI processes answering at once"
    );
    let mapped = threads(&db);
    assert_eq!(mapped.len(), 1, "one thread, one chat — the second resumed");
    let chat_id = mapped[0].1.clone();

    assert_eq!(
        slack.posted(),
        vec![
            ("1700000000.000100".to_string(), "answer 1".to_string()),
            ("1700000000.000100".to_string(), "answer 2".to_string()),
        ],
        "both replies in the same thread, in arrival order"
    );
    assert_eq!(
        messages(&db, &chat_id)
            .iter()
            .map(|(role, content)| format!("{role}:{content}"))
            .collect::<Vec<_>>(),
        vec![
            "user:first".to_string(),
            "assistant:answer 1".to_string(),
            "user:second".to_string(),
            "assistant:answer 2".to_string(),
        ],
        "one chat holds both turns"
    );

    let argv = spawns(dir.path());
    assert_eq!(argv.len(), 2, "two runs, one after the other");
    assert_eq!(flag(&argv[0].0, "--resume"), None);
    assert_eq!(
        flag(&argv[1].0, "--resume"),
        Some("sess-1"),
        "the second turn resumes the session id the first one minted"
    );
}

/// Acceptance criterion 3's two handler-side ignores. A mention from a bot is
/// the third, and it never reaches this module — `Envelope::app_mention` drops
/// it, where `a_bot_authored_app_mention_is_not_work` pins it.
#[tokio::test]
async fn an_unmapped_thread_and_an_empty_remainder_produce_nothing() {
    let _base = api_base_lock().await;
    let slack = fake_slack().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = migrated(dir.path(), "s-ignore");
    seed_rule(
        &db,
        "s-ignore",
        "r",
        true,
        "[]",
        "",
        "",
        "2026-01-01 00:00:00 +0000 UTC",
    );
    // No `AGENTO_CLAUDE_EXECUTABLE` at all: a run reaching the CLI here would
    // fail loudly rather than quietly answering.
    let handler = super::handler(&db, "s-ignore", "xoxb-t");

    finish(handler(mention(
        "<@U0BOT> hello",
        "1700000000.000300",
        "1700000000.000100",
        "s-ignore",
    )))
    .await;
    finish(handler(mention(
        "  <@U0BOT>  ",
        "1700000000.000400",
        "",
        "s-ignore",
    )))
    .await;
    set_api_base(None);

    assert!(threads(&db).is_empty(), "no thread was mapped");
    assert!(
        slack.posted().is_empty(),
        "and nothing was said in the channel"
    );
    let conn = rusqlite::Connection::open(&db).expect("open");
    let chats: i64 = conn
        .query_row("SELECT count(*) FROM chat_sessions", [], |row| row.get(0))
        .expect("count");
    assert_eq!(chats, 0, "no chat was created");
}

/// A failing run still answers, and the chat still holds what was asked — the
/// failure is visible in Slack *and* in the app.
#[tokio::test]
async fn a_failed_run_answers_the_failure_sentence_and_keeps_the_question() {
    if python3().is_none() {
        eprintln!("no python3; skipping");
        return;
    }
    let _base = api_base_lock().await;
    let slack = fake_slack().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = migrated(dir.path(), "s-fail");
    seed_rule(
        &db,
        "s-fail",
        "r",
        true,
        "[]",
        "",
        "",
        "2026-01-01 00:00:00 +0000 UTC",
    );
    let cli = fake_cli(dir.path(), "not reached", "sess", true, 0);

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    let handler = super::handler(&db, "s-fail", "xoxb-t");
    finish(handler(mention(
        "<@U0BOT> break",
        "1700000000.000100",
        "",
        "s-fail",
    )))
    .await;
    std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE");
    set_api_base(None);

    assert_eq!(
        slack.posted(),
        vec![("1700000000.000100".to_string(), ERROR_REPLY.to_string())]
    );
    let chat_id = threads(&db)[0].1.clone();
    assert_eq!(
        messages(&db, &chat_id),
        vec![("user".to_string(), "break".to_string())],
        "the question is stored with no answer, so the failure is visible in the app too"
    );
}

/// #565's selection rule, as the user experiences it: turning a channel's own
/// rule off silences that channel and leaves every other channel answering.
#[tokio::test]
async fn a_disabled_channel_rule_silences_that_channel_only() {
    if python3().is_none() {
        eprintln!("no python3; skipping");
        return;
    }
    let _base = api_base_lock().await;
    let slack = fake_slack().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = migrated(dir.path(), "s-off");
    // Oldest first, as `load_rules` returns them: the workspace default, then
    // the channel-specific rule that is switched off.
    seed_rule(
        &db,
        "s-off",
        "default",
        true,
        "[]",
        "",
        "",
        "2026-01-01 00:00:00 +0000 UTC",
    );
    seed_rule(
        &db,
        "s-off",
        "silenced",
        false,
        r#"["C1"]"#,
        "",
        "",
        "2026-02-01 00:00:00 +0000 UTC",
    );
    let cli = fake_cli(dir.path(), "answered", "sess", false, 0);

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    let handler = super::handler(&db, "s-off", "xoxb-t");

    // `CHANNEL` is the silenced one.
    finish(handler(mention(
        "<@U0BOT> hello",
        "1700000000.000100",
        "",
        "s-off",
    )))
    .await;
    assert!(
        slack.posted().is_empty(),
        "a disabled channel-specific rule is that channel's off switch, not a \
         fall-through to the workspace default"
    );

    let mut elsewhere = mention("<@U0BOT> hello", "1700000000.000200", "", "s-off");
    elsewhere.channel = "C2".to_string();
    finish(handler(elsewhere)).await;
    std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE");
    set_api_base(None);

    assert_eq!(
        slack.posted(),
        vec![("1700000000.000200".to_string(), "answered".to_string())],
        "and every other channel still runs on the workspace default"
    );
}

/// Review round 1, finding 4: the one Slack failure the handler cannot work
/// around must not answer into a thread Agento never started.
///
/// `auth.test` is resolved **after** the mapping decision for exactly this
/// reason. A stranger's thread gets nothing; a thread of Agento's own gets the
/// failure sentence, because silence there is the outcome that is never allowed.
#[tokio::test]
async fn an_auth_failure_answers_only_in_a_thread_agento_started() {
    let _base = api_base_lock().await;
    let slack = fake_slack().await;
    slack
        .auth_works
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let dir = tempfile::tempdir().expect("tempdir");
    let db = migrated(dir.path(), "s-auth");
    seed_rule(
        &db,
        "s-auth",
        "r",
        true,
        "[]",
        "",
        "",
        "2026-01-01 00:00:00 +0000 UTC",
    );
    let handler = super::handler(&db, "s-auth", "xoxb-t");

    finish(handler(mention(
        "<@U0BOT> hello",
        "1700000000.000300",
        "1700000000.000100",
        "s-auth",
    )))
    .await;
    assert!(
        slack.posted().is_empty(),
        "a mention in an unmapped thread produces no reply, whatever else failed"
    );

    finish(handler(mention(
        "<@U0BOT> hello",
        "1700000000.000400",
        "",
        "s-auth",
    )))
    .await;
    set_api_base(None);
    assert_eq!(
        slack.posted(),
        vec![("1700000000.000400".to_string(), ERROR_REPLY.to_string())],
        "and a top-level mention, whose thread is Agento's own, is told"
    );
    assert!(
        threads(&db).is_empty(),
        "no chat is started for a mention that cannot be read"
    );
}

/// Review round 1, finding 2: the dispatcher's ten permits bound `claude`
/// subprocesses, so they are taken around the **run** and not around the
/// handler.
///
/// Structural rather than behavioural on purpose: the semaphore is a process
/// global shared with every other transport, so a test that drained it to
/// observe the difference would stall unrelated tests in this binary. What can
/// be pinned deterministically is *where* it is taken, and moving it back is a
/// one-line change that nothing else would notice — ten mentions queued in one
/// Slack thread would silently hold every permit while one of them ran.
#[test]
fn the_global_bound_is_taken_around_the_run_and_not_around_the_wait() {
    let flat = include_str!("../inbound.rs")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let acquire = flat
        .find("dispatcher::semaphore().acquire()")
        .expect("the run must take the dispatcher's permit");
    let prompt_line = flat
        .find("\"slack mention prompt")
        .expect("the turn logs its prompt before it runs");
    let run = flat
        .find("agent_run::run_resumed(")
        .expect("the turn calls run_resumed");
    // Presence alone would stay green if the acquire moved back to `accept`,
    // which is the regression this test is named for. Its *position* is the
    // claim: after the chat has been resolved, immediately before the run.
    assert!(
        prompt_line < acquire && acquire < run,
        "the permit must be taken between resolving the chat and running it, \
         not while a mention waits for its thread's turn"
    );
    assert!(
        !include_str!("../socket.rs").contains("semaphore().acquire"),
        "and the transport must not take it: a mention waiting for its thread's \
         turn is not a `claude` subprocess"
    );
}

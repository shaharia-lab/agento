//! Socket Mode against a **real Slack workspace** (#571).
//!
//! `tests/slack_socket.rs` proves the worker against a fake Slack, which is the
//! right shape for every rule about backoff, dedup and status — and is exactly
//! the thing that cannot catch a wrong belief about Slack's own bytes. This file
//! opens a real Socket Mode connection to a real app, has a real person mention
//! it in a real channel, and waits for the agent's answer to appear in the
//! thread. What it proves is the envelope shapes, the scopes, and the manifest
//! in `docs/user-guide.md`.
//!
//! `#[ignore]`d, like the other `*_live.rs` suites, and additionally skipped
//! when its environment is not set. Run it by hand:
//!
//! ```bash
//! export AGENTO_SLACK_TEST_BOT_TOKEN=xoxb-…    # Bot User OAuth Token
//! export AGENTO_SLACK_TEST_APP_TOKEN=xapp-…    # App-Level Token, connections:write
//! export AGENTO_SLACK_TEST_USER_TOKEN=xoxp-…   # a *user* token, to post the mention
//! export AGENTO_SLACK_TEST_CHANNEL=C0123ABCDEF # the app must be invited to it
//! cargo test --test slack_socket_live -- --ignored --nocapture
//! ```
//!
//! # Why there is a fourth token, when the issue named three
//!
//! **A mention posted with the bot token is dropped before it reaches the
//! handler**, by the branch in `socket.rs` that refuses any `app_mention`
//! carrying `bot_id` or a message `subtype`. That branch is correct and is one
//! of the things this suite documents, so the driver cannot be the bot — the
//! mention has to come from a human, which means a user token (`xoxp-`).
//!
//! It is a happy accident that this keeps the app's own scopes honest. The
//! *driver* — this file's own calls — needs `chat.postMessage` to post the
//! mention and `conversations.replies` to read the thread back, and both go out
//! on the user token, so neither can paper over a scope the manifest forgot. The
//! driver touches the bot token only for `auth.test`, which needs no scope, and
//! for deleting the app's own messages in the tear-down, which needs the
//! `chat:write` the manifest already has. Everything else the bot token does
//! here is the **handler under test** spending it exactly as the app does.
//!
//! # Which manifest scopes a run actually pins
//!
//! All three, but only because of the last assertion. `chat:write` and
//! `app_mentions:read` are load-bearing by construction — without them there is
//! no event and no reply, and the test times out. **`channels:read` is not**:
//! `Inbound::channel_name` swallows a failed `conversations.info` and falls back
//! to the raw channel id, so a two-scope app would otherwise pass green. The
//! chat's stored title is what closes that hole — `[Slack] #<name>: …` holds the
//! channel's *name* when the call worked and its *id* when it did not, and they
//! are never the same string.
//!
//! # What has to be true of the machine
//!
//! A signed-in Claude Code CLI, because the reply is a real agent run and its
//! cost is real. The run is `default` permission mode in a scratch directory and
//! is asked for one word, so it should be a few cents; a rule pointed anywhere
//! else is not this test's business.
//!
//! # Secrets
//!
//! No token is ever interpolated into an assertion, and the one place a panic
//! message is produced runs it through [`scrub`] first — so a failure that
//! quotes a Slack response back at you still cannot print what you exported.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use agento_lib::native::integrations::registry;
use agento_lib::native::integrations::slack::inbound;
use agento_lib::native::integrations::slack::socket::{self, SocketOptions, STATUS_CONNECTED};
use agento_lib::native::{db, migrate};

/// The integration id every row in this suite hangs off.
///
/// `registry`'s status-epoch map is keyed by integration id and this binary is
/// one process, so a second test here would need its own — see
/// `tests/slack_socket.rs::fixture_db` on what a shared id costs.
const INTEGRATION_ID: &str = "slack-socket-live";

/// How long to wait for an agent's answer to land in the thread.
///
/// A real run through the Claude CLI is tens of seconds on a good day, and the
/// per-thread queue means the second mention waits for the first to finish.
const REPLY_TIMEOUT: Duration = Duration::from_secs(240);

/// What the mention asks for. Short, tool-free and unambiguous, so a failure is
/// about Slack rather than about the model having a hard day.
const FIRST_PROMPT: &str = "Reply with exactly one word: PONG. Nothing else.";

/// The follow-up, which is only answerable if the chat was resumed rather than
/// started again — but the assertion is on the stored rows, not on this text.
const SECOND_PROMPT: &str = "What single word did you just reply? Say only that word.";

/// The four environment variables, read once.
struct Env {
    bot_token: String,
    app_token: String,
    user_token: String,
    channel: String,
}

impl Env {
    fn read() -> Option<Self> {
        Some(Self {
            bot_token: non_empty("AGENTO_SLACK_TEST_BOT_TOKEN")?,
            app_token: non_empty("AGENTO_SLACK_TEST_APP_TOKEN")?,
            user_token: non_empty("AGENTO_SLACK_TEST_USER_TOKEN")?,
            channel: non_empty("AGENTO_SLACK_TEST_CHANNEL")?,
        })
    }

    /// Every secret this process holds, for [`scrub`].
    fn secrets(&self) -> [&str; 3] {
        [&self.bot_token, &self.app_token, &self.user_token]
    }
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Replace anything secret with a marker.
///
/// The failure path quotes Slack's own responses, and a token pasted into the
/// wrong variable comes back inside some of them.
fn scrub(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, "<redacted>");
        }
    }
    out
}

/// Skips rather than fails when the CLI is absent, as `claude_mcp_live.rs` does.
fn claude_cli_present() -> bool {
    Command::new("claude")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// A migrated database with one Slack integration and one trigger rule that
/// answers in `channel`.
///
/// The rule is deliberately narrow: it names the one channel rather than
/// matching every channel the app is in, and it runs in `work_dir` under the
/// `default` permission mode. That is the shape `docs/user-guide.md` tells a
/// reader to use, so it is the shape the test exercises.
///
/// `agent_slug` is empty on purpose — `runner::load` reads that as "no agent",
/// so the fixture needs no `agents` row. A slug naming an agent that does not
/// exist is an `Err` and the mention would be answered with the error sentence.
/// The `/api` writes refuse an empty slug; this is a direct insert, not a
/// `POST`, so it is reachable here and nowhere a user can get to.
fn fixture_db(dir: &Path, work_dir: &Path, channel: &str) -> PathBuf {
    let db_path = dir.join("agento.db");
    let mut conn = db::ensure_database(&db_path).expect("create the database");
    migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO integrations
            (id, name, type, enabled, credentials, services, created_at, updated_at,
             inbound_enabled, inbound_status, inbound_error)
         VALUES (?1, 'Slack (live test)', 'slack', 1, '{}', '{}',
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC', 1, '', '')",
        [INTEGRATION_ID],
    )
    .expect("seed the integration");
    conn.execute(
        "INSERT INTO trigger_rules
            (id, integration_id, name, agent_slug, enabled, filter_prefix, filter_keywords,
             filter_chat_ids, model, working_directory, settings_profile_id, permission_mode,
             timeout_minutes, created_at, updated_at)
         VALUES ('live-rule', ?1, 'live-rule', '', 1, '', '[]', ?2, '', ?3, '', 'default',
                 3, '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        rusqlite::params![
            INTEGRATION_ID,
            // A **JSON array**, not the bare id. `dispatcher::decode_list`
            // answers an empty `Vec` for anything it cannot parse, and
            // `select_rule_for_channel` reads that as "every channel" — so a
            // bare id here would still pass, against a rule that is not the
            // narrow one this fixture claims to be.
            serde_json::json!([channel]).to_string(),
            work_dir.to_string_lossy().to_string()
        ],
    )
    .expect("seed the rule");
    db_path
}

/// The worker's own report, read back the way the UI reads it.
fn inbound_state(db_path: &Path) -> (String, String) {
    let conn = db::open_read_only(db_path).expect("open");
    conn.query_row(
        "SELECT inbound_status, inbound_error FROM integrations WHERE id = ?1",
        [INTEGRATION_ID],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("read the inbound state")
}

fn scalar(db_path: &Path, sql: &str) -> i64 {
    let conn = db::open_read_only(db_path).expect("open");
    conn.query_row(sql, [], |row| row.get(0)).expect("scalar")
}

/// Built through `native::http::client_builder`, as every HTTP client in this
/// repository is (#514) — not because a test is bound by that rule, but because
/// the point of this suite is the bytes that actually go to Slack, and the
/// builder is where the user agent and the proxy handling come from.
fn client() -> reqwest::Client {
    agento_lib::native::http::client_builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build a client")
}

/// One Slack Web API call, with `ok` deciding rather than the HTTP status —
/// which is Slack's own rule, and the one `client.rs` reproduces.
async fn slack(
    token: &str,
    method: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let response = client()
        .post(format!("https://slack.com/api/{method}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{method}: {e}"))?;
    let parsed: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("{method}: reading the response: {e}"))?;
    if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let reason = parsed
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown_error");
        return Err(format!("{method}: {reason}"));
    }
    Ok(parsed)
}

/// Post a message and answer its `ts`.
async fn post(
    token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<String, String> {
    let mut body = serde_json::json!({ "channel": channel, "text": text });
    if let Some(thread_ts) = thread_ts {
        body["thread_ts"] = serde_json::Value::String(thread_ts.to_string());
    }
    let answer = slack(token, "chat.postMessage", body).await?;
    answer
        .get("ts")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "chat.postMessage answered no ts".to_string())
}

/// Every message in a thread, oldest first.
async fn replies(
    token: &str,
    channel: &str,
    thread_ts: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let answer = slack(
        token,
        "conversations.replies",
        serde_json::json!({ "channel": channel, "ts": thread_ts, "limit": 50 }),
    )
    .await?;
    Ok(answer
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Wait for the app to answer *after* `after_ts`, and answer what it said.
///
/// **Keyed on the timestamp, not on a running count.** An answer longer than
/// 4000 characters is posted as several messages (`mrkdwn::split`), so "wait
/// until two bot messages exist" is satisfied by one long first answer — and
/// the resume assertions would then hold without a resume having happened.
/// Every message strictly newer than the mention that provoked it is that
/// mention's answer, however many parts it arrives in.
///
/// **The wait has a deadline and the deadline names the hang**: a worker that
/// never connected, a rule that never matched and a model that is simply slow
/// are indistinguishable from a test that only waits.
async fn await_reply_after(
    env: &Env,
    bot_user: &str,
    thread_ts: &str,
    after_ts: &str,
) -> Result<Vec<String>, String> {
    let after: f64 = after_ts
        .parse()
        .map_err(|_| format!("a slack ts that is not a number: {after_ts:?}"))?;
    let deadline = Instant::now() + REPLY_TIMEOUT;
    while Instant::now() < deadline {
        let messages = replies(&env.user_token, &env.channel, thread_ts).await?;
        let said: Vec<String> = messages
            .iter()
            .filter(|m| m.get("user").and_then(serde_json::Value::as_str) == Some(bot_user))
            .filter(|m| {
                m.get("ts")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|ts| ts.parse::<f64>().ok())
                    .is_some_and(|ts| ts > after)
            })
            .filter_map(|m| m.get("text").and_then(serde_json::Value::as_str))
            .map(str::to_string)
            .collect();
        if !said.is_empty() {
            return Ok(said);
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    Err(format!(
        "waited {REPLY_TIMEOUT:?} for the app to answer in the thread after {after_ts}, and it did not"
    ))
}

/// Best effort, and deliberately so: a thread left behind is untidy, a test that
/// fails because the tidying failed is worse.
async fn delete_thread(env: &Env, bot_user: &str, thread_ts: &str) {
    let Ok(messages) = replies(&env.user_token, &env.channel, thread_ts).await else {
        return;
    };
    for message in messages.iter().rev() {
        let Some(ts) = message.get("ts").and_then(serde_json::Value::as_str) else {
            continue;
        };
        // A user token cannot delete the app's messages and the bot token cannot
        // delete the person's, so each message goes out on the token that posted
        // it.
        let token = if message.get("user").and_then(serde_json::Value::as_str) == Some(bot_user) {
            &env.bot_token
        } else {
            &env.user_token
        };
        let _ = slack(
            token,
            "chat.delete",
            serde_json::json!({ "channel": env.channel, "ts": ts }),
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real Slack workspace and the AGENTO_SLACK_TEST_* tokens"]
async fn a_real_mention_starts_a_chat_and_a_reply_in_the_thread_resumes_it() {
    let Some(env) = Env::read() else {
        eprintln!(
            "skipping: set AGENTO_SLACK_TEST_BOT_TOKEN, _APP_TOKEN, _USER_TOKEN and _CHANNEL"
        );
        return;
    };
    if !claude_cli_present() {
        eprintln!("skipping: no claude CLI on PATH, so no agent can answer");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let work = tempfile::tempdir().expect("work tempdir");
    let db_path = fixture_db(dir.path(), work.path(), &env.channel);

    // The production handler and the production options — the only thing this
    // test substitutes is the database underneath them.
    let worker = socket::start(
        &db_path,
        INTEGRATION_ID,
        &env.app_token,
        SocketOptions {
            handler: inbound::handler(&db_path, INTEGRATION_ID, &env.bot_token),
            ..Default::default()
        },
    );
    worker.accept(registry::registry().retire_socket(INTEGRATION_ID));

    let outcome = run(&env, &db_path).await;

    // Both paths, and in this order: stop the worker before the assertion, so a
    // failure does not leave a live socket in a test binary that is about to
    // panic.
    drop(worker);
    registry::registry().retire_socket(INTEGRATION_ID);
    let thread = match &outcome {
        Ok(thread) => Some(thread.clone()),
        Err((_, thread)) => thread.clone(),
    };
    if let Some((bot_user, thread_ts)) = thread {
        delete_thread(&env, &bot_user, &thread_ts).await;
    }

    if let Err((why, _)) = outcome {
        panic!("{}", scrub(&why, &env.secrets()));
    }
}

/// The body, as a `Result` so the caller can tear down on either path.
///
/// The error carries the thread it got as far as creating, when it created one,
/// so a failed run cleans up after itself as thoroughly as a passing one.
type Thread = (String, String);

async fn run(env: &Env, db_path: &Path) -> Result<Thread, (String, Option<Thread>)> {
    let fail = |why: String| -> (String, Option<Thread>) { (why, None) };

    let (status, error) = await_status(db_path, STATUS_CONNECTED).await;
    if status != STATUS_CONNECTED {
        return Err(fail(format!(
            "the socket never connected: inbound_status={status:?} inbound_error={error:?}"
        )));
    }

    // Who the app is, so the test can tell its replies from its own posts. This
    // is `auth.test` on the *bot* token, the same call the handler makes.
    let identity = slack(&env.bot_token, "auth.test", serde_json::json!({}))
        .await
        .map_err(&fail)?;
    let bot_user = identity
        .get("user_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| fail("auth.test answered no user_id".to_string()))?
        .to_string();

    let thread_ts = post(
        &env.user_token,
        &env.channel,
        &format!("<@{bot_user}> {FIRST_PROMPT}"),
        None,
    )
    .await
    .map_err(&fail)?;
    let thread: Thread = (bot_user.clone(), thread_ts.clone());
    let carry = |why: String| (why, Some(thread.clone()));

    let first = await_reply_after(env, &bot_user, &thread_ts, &thread_ts)
        .await
        .map_err(&carry)?;
    if first[0].trim().is_empty() {
        return Err(carry("the app replied with nothing at all".to_string()));
    }
    if first[0].contains("Sorry, something went wrong") {
        let (_, error) = inbound_state(db_path);
        return Err(carry(format!(
            "the run failed; the reply was the error sentence. inbound_error={error:?}"
        )));
    }
    eprintln!("first reply: {:?}", first[0]);

    // One row, one chat, and the thread is keyed on the mention that started it.
    let mapped: (String, String) = {
        let conn = db::open_read_only(db_path).expect("open");
        conn.query_row(
            "SELECT thread_ts, chat_id FROM inbound_threads WHERE integration_id = ?1",
            [INTEGRATION_ID],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| {
            carry(format!(
                "no inbound_threads row after the first mention: {e}"
            ))
        })?
    };
    if mapped.0 != thread_ts {
        return Err(carry(format!(
            "the thread was mapped on {:?}, not on the mention's own ts {thread_ts:?}",
            mapped.0
        )));
    }

    // The follow-up, inside the thread. This is the whole point: a second chat
    // here would mean the mapping was not read, and the agent would have lost
    // everything it was told.
    let follow_up_ts = post(
        &env.user_token,
        &env.channel,
        &format!("<@{bot_user}> {SECOND_PROMPT}"),
        Some(&thread_ts),
    )
    .await
    .map_err(&carry)?;
    let second = await_reply_after(env, &bot_user, &thread_ts, &follow_up_ts)
        .await
        .map_err(&carry)?;
    eprintln!("second reply: {:?}", second[0]);

    let chats = scalar(db_path, "SELECT COUNT(*) FROM chat_sessions");
    if chats != 1 {
        return Err(carry(format!(
            "two mentions in one thread produced {chats} chats; the second did not resume the first"
        )));
    }
    let threads = scalar(db_path, "SELECT COUNT(*) FROM inbound_threads");
    if threads != 1 {
        return Err(carry(format!("{threads} inbound_threads rows, expected 1")));
    }
    // **The only assertion that pins `channels:read`** — see the header. The
    // title is `[Slack] #<name>: <prompt>`, and `channel_name` falls back to the
    // channel *id* when `conversations.info` is refused, so a title carrying the
    // id is a manifest missing that scope rather than a broken test.
    let title: String = {
        let conn = db::open_read_only(db_path).expect("open");
        conn.query_row(
            "SELECT title FROM chat_sessions WHERE id = ?1",
            [&mapped.1],
            |row| row.get(0),
        )
        .expect("read the chat title")
    };
    if title.contains(&env.channel) {
        return Err(carry(format!(
            "the chat is titled {title:?}, which carries the channel id rather \
             than its name — `conversations.info` did not answer. Either the app \
             is missing `channels:read` (or `groups:read`, if the test channel \
             is private), or AGENTO_SLACK_TEST_CHANNEL is a channel *name* where \
             it has to be an id"
        )));
    }

    let same: i64 = {
        let conn = db::open_read_only(db_path).expect("open");
        conn.query_row(
            "SELECT COUNT(*) FROM inbound_threads WHERE chat_id = ?1",
            [&mapped.1],
            |row| row.get(0),
        )
        .expect("count")
    };
    if same != 1 {
        return Err(carry(
            "the resumed turn was not recorded against the first chat".to_string(),
        ));
    }

    Ok(thread)
}

/// Poll the stored status until it is `wanted`, or answer whatever it settled on.
///
/// The status is written from a `db::blocking` task, so it lands a moment after
/// the transition it reports; and a first connection is a real network round
/// trip to Slack, so the deadline is generous rather than tight.
async fn await_status(db_path: &Path, wanted: &str) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = (String::new(), String::new());
    while Instant::now() < deadline {
        last = inbound_state(db_path);
        if last.0 == wanted {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    last
}

/// The one thing in this file that can be proved without a workspace, and the
/// one worth proving: **a failure must not print what you exported.**
///
/// It is not `#[ignore]`d, so `cargo test` and CI run it. Take [`scrub`] out and
/// this goes red; every other assertion here needs Slack.
#[test]
fn a_failure_message_never_names_a_token() {
    let bot = "xoxb-0000-not-a-real-token";
    let app = "xapp-1-A0-1-secretsecret";
    let user = "xoxp-0000-also-not-real";
    let message = format!(
        "chat.postMessage: invalid_auth (sent {bot}), apps.connections.open used {app} \
         and the driver used {user}"
    );

    let scrubbed = scrub(&message, &[bot, app, user]);

    for secret in [bot, app, user] {
        assert!(
            !scrubbed.contains(secret),
            "a token survived scrubbing: {scrubbed}"
        );
    }
    assert!(
        scrubbed.contains("invalid_auth"),
        "scrubbing ate the diagnosis: {scrubbed}"
    );
    // An empty variable must not turn every character into a marker.
    assert_eq!(scrub("nothing secret here", &[""]), "nothing secret here");
}

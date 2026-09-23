//! A scheduled task's output, posted to Slack (#637, epic #626).
//!
//! The `slack` arm of `schedule::delivery` calls [`deliver_channel`] once per
//! configured channel, with the token `registry::slack_delivery_token` resolved.
//! Rules, each enforced here:
//!
//! - **Summary, then thread.** One top-level `chat.postMessage` naming the task,
//!   its status, its duration and its job; its `ts` becomes the `thread_ts` of
//!   the replies that carry the output. A failed summary attempts no replies.
//! - **The output is [`mrkdwn::to_mrkdwn`] then [`mrkdwn::split`]**, posted one
//!   chunk at a time and in order, as `Inbound::post` does — so a broadcast
//!   mention in model text never reaches Slack raw. The task name in the
//!   summary is user text posted as the app, and gets the same `&`/`<`/`>`
//!   escape.
//! - **A partial thread is a failure.** The first failed reply stops the
//!   channel and is recorded as `reply i of n failed`.
//! - **Errors are sentences.** Slack's `error` code is mapped by [`readable`];
//!   the client already turns a 429 into a rate-limit sentence and lets `ok`
//!   decide over the HTTP status. Nothing is retried in v1.
//! - **No token in any result.** Results are built from Slack's error code and
//!   the client's token-free messages alone.
//! - **One thread per run continues the run's chat (#642).** After the first
//!   summary that posts, and *before* its replies, the thread is written to
//!   `inbound_threads` against the run's chat, so an `@app` reply there resumes
//!   the session through the inbound handler. The mapping is a [`ThreadMapping`]
//!   the caller offers through `&mut Option`, and [`deliver_channel`] takes it
//!   when it tries — so a later channel, or a later Slack destination, is never
//!   offered it again and never trips `inbound_threads`'s `UNIQUE (chat_id)`. A
//!   failed insert is a `warn`, never a failed delivery.

use std::path::PathBuf;

use crate::claude::CancellationToken;
use crate::native::db;
use crate::native::trigger::dispatcher::NO_RESPONSE_REPLY;

use super::client::{api_error_code, Client};
use super::{inbound, mrkdwn};

/// What one run says to Slack. `body` is raw Markdown: the answer on a
/// successful run, the failure message on a failed one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub task_name: String,
    /// `true` for a successful run.
    pub succeeded: bool,
    pub duration_ms: i64,
    pub job_id: String,
    pub body: String,
}

/// How one channel's post ended. `Sent` carries the summary's `ts`, which is
/// the thread the output landed in. The `inbound_threads` row for that thread
/// (#642) is written inside [`deliver_channel`], before the replies, not from
/// this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOutcome {
    Sent { ts: String },
    Failed(String),
}

/// One channel's result from [`deliver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelResult {
    pub channel_id: String,
    pub outcome: ChannelOutcome,
}

/// The `inbound_threads` row a delivered summary's thread becomes: the
/// integration that posted it and the chat the run wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMapping {
    pub db_path: PathBuf,
    pub integration_id: String,
    pub chat_id: String,
}

/// Post `run` to every channel in turn. A failure on one channel never stops
/// the next; `mapping` goes to the first channel whose summary posts.
pub async fn deliver(
    token: &str,
    channel_ids: &[String],
    run: &RunSummary,
    mapping: &mut Option<ThreadMapping>,
) -> Vec<ChannelResult> {
    let mut out = Vec::with_capacity(channel_ids.len());
    for channel in channel_ids {
        out.push(ChannelResult {
            channel_id: channel.clone(),
            outcome: deliver_channel(token, channel, run, mapping).await,
        });
    }
    out
}

/// The summary, then the output in its thread, to one channel.
///
/// When the summary posts and `mapping` is `Some`, it is taken and the thread
/// mapped before any reply is sent; see the module header.
pub async fn deliver_channel(
    token: &str,
    channel: &str,
    run: &RunSummary,
    mapping: &mut Option<ThreadMapping>,
) -> ChannelOutcome {
    let client = Client::new(token);
    // Nothing cancels a delivery but the client's own 60s timeout per call.
    let ct = CancellationToken::new();

    let summary = serde_json::json!({
        "channel": channel,
        "text": summary_text(run),
        "mrkdwn": true,
    });
    let ts = match post(&client, &ct, &summary).await {
        Ok(body) => match message_ts(&body) {
            Some(ts) => ts,
            None => return ChannelOutcome::Failed("Slack returned no message ts".to_string()),
        },
        Err(e) => return ChannelOutcome::Failed(readable(&e)),
    };

    // Before the replies, so a reply typed while the output is still posting
    // already finds its thread mapped.
    if let Some(mapping) = mapping.take() {
        map_thread(&client, channel, &ts, mapping).await;
    }

    let chunks = mrkdwn::split(
        &mrkdwn::to_mrkdwn(&thread_body(run)),
        mrkdwn::MAX_MESSAGE_CHARS,
    );
    let n = chunks.len();
    for (i, chunk) in chunks.into_iter().enumerate() {
        let reply = serde_json::json!({
            "channel": channel,
            "thread_ts": ts,
            "text": chunk,
            "mrkdwn": true,
        });
        if let Err(e) = post(&client, &ct, &reply).await {
            return ChannelOutcome::Failed(format!(
                "posted the summary; reply {} of {n} failed: {}",
                i + 1,
                readable(&e)
            ));
        }
    }
    ChannelOutcome::Sent { ts }
}

/// Write the thread under the summary `ts` to `inbound_threads`. Loud rather
/// than fatal, as `Inbound::start_chat` treats the same insert: the output is
/// still delivered, and only the reply-to-continue is lost.
async fn map_thread(client: &Client, channel: &str, ts: &str, mapping: ThreadMapping) {
    let permalink = inbound::permalink(client, channel, ts).await;
    let (channel, ts) = (channel.to_string(), ts.to_string());
    let chat_id = mapping.chat_id.clone();
    match db::blocking("slack delivery thread map", move || {
        inbound::insert_thread(
            &mapping.db_path,
            &mapping.integration_id,
            &channel,
            &ts,
            &mapping.chat_id,
            &permalink,
        )
    })
    .await
    {
        Some(Ok(())) => {}
        Some(Err(e)) => {
            log::warn!(
                "failed to map a delivered slack thread to its chat chat_id={chat_id:?}: {e}"
            );
        }
        None => log::warn!("the slack delivery thread map did not finish chat_id={chat_id:?}"),
    }
}

async fn post(
    client: &Client,
    ct: &CancellationToken,
    payload: &serde_json::Value,
) -> Result<String, String> {
    let body = crate::native::gojson::to_vec_marshal(payload)
        .map_err(|e| format!("encoding a slack message: {e}"))?;
    client.call_json(ct, "chat.postMessage", body).await
}

/// The `ts` of a `chat.postMessage` answer; `None` when absent or empty. The
/// client returns the body verbatim, so only `ts` is decoded.
fn message_ts(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let ts = value.get("ts")?.as_str()?;
    (!ts.is_empty()).then(|| ts.to_string())
}

/// What the thread carries: the answer, the failure, or — so a thread is never
/// empty — the inbound path's no-response sentence.
fn thread_body(run: &RunSummary) -> String {
    if !run.body.trim().is_empty() {
        run.body.clone()
    } else if run.succeeded {
        NO_RESPONSE_REPLY.to_string()
    } else {
        "The run failed.".to_string()
    }
}

/// The top-level message: `*<task>* · succeeded · 3m 12s`, then the job line.
///
/// Agento has no URL a Slack reader could follow, so the "link" is the job id
/// and where to find it.
fn summary_text(run: &RunSummary) -> String {
    let name = if run.task_name.trim().is_empty() {
        "Scheduled task".to_string()
    } else {
        escape(&run.task_name)
    };
    let status = if run.succeeded { "succeeded" } else { "failed" };
    format!(
        "*{name}* · {status} · {}\nJob {} · open Agento → Jobs for the full record",
        format_duration(run.duration_ms),
        escape(&run.job_id)
    )
}

/// Slack's three control characters, as [`mrkdwn::to_mrkdwn`] escapes them — a
/// task name can then never open a `<!channel>` or a link.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `850ms`, `12s`, `3m 12s`, `1h 4m`.
pub(crate) fn format_duration(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// A Slack client error as a sentence the Jobs view can show.
///
/// The client reports an `ok: false` answer as `slack API error (<method>):
/// <code>`; known codes become advice, anything else — a rate limit, a failed
/// request, an unknown code — passes through as the client wrote it.
fn readable(e: &str) -> String {
    let Some(code) = api_error_code(e) else {
        return e.to_string();
    };
    let sentence = match code {
        "not_in_channel" => {
            "the bot is not a member of this channel — invite it with /invite @<app>"
        }
        "channel_not_found" => {
            "channel not found — check the ID, and that the bot was invited if the channel is private"
        }
        "is_archived" => "the channel is archived",
        "invalid_auth" | "token_revoked" | "account_inactive" | "not_authed" => {
            "the Slack token was rejected — reconnect the integration"
        }
        "msg_too_long" => "the message was too long for Slack",
        _ => return e.to_string(),
    };
    format!("{sentence} ({code})")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::Uri;

    use super::super::client::{api_base_lock, set_api_base};
    use super::*;

    const TOKEN: &str = "xoxb-delivery-SUPER-SECRET";
    const TS: &str = "1700000000.000100";

    /// One `chat.postMessage` the fake answered: `(channel, thread_ts, text)`.
    type Post = (String, String, String);

    /// How the fake answers: `reply(n, channel, thread_ts)` for the `n`th call
    /// (from 0) gives the status and body.
    type Reply = fn(usize, &str, &str) -> (u16, Vec<(&'static str, &'static str)>, String);

    #[derive(Clone)]
    struct Fake {
        posts: Arc<Mutex<Vec<Post>>>,
        auth: Arc<Mutex<Vec<String>>>,
        /// Every method called, in order, `chat.getPermalink` included.
        methods: Arc<Mutex<Vec<String>>>,
        reply: Reply,
    }

    impl Fake {
        fn posts(&self) -> Vec<Post> {
            self.posts.lock().expect("lock").clone()
        }
    }

    async fn serve(
        State(state): State<Fake>,
        headers: axum::http::HeaderMap,
        uri: Uri,
        body: String,
    ) -> axum::response::Response {
        state
            .methods
            .lock()
            .expect("lock")
            .push(uri.path().trim_start_matches('/').to_string());
        if uri.path() == "/chat.getPermalink" {
            return axum::response::IntoResponse::into_response(axum::Json(
                serde_json::json!({"ok": true, "permalink": "https://slack.example/p/1"}),
            ));
        }
        assert_eq!(uri.path(), "/chat.postMessage");
        let payload: serde_json::Value = serde_json::from_str(&body).expect("a JSON body");
        let field = |k: &str| payload[k].as_str().unwrap_or_default().to_string();
        let (channel, thread_ts) = (field("channel"), field("thread_ts"));
        state.auth.lock().expect("lock").push(
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string(),
        );
        let n = {
            let mut posts = state.posts.lock().expect("lock");
            posts.push((channel.clone(), thread_ts.clone(), field("text")));
            posts.len() - 1
        };
        let (status, headers, body) = (state.reply)(n, &channel, &thread_ts);
        let mut response = axum::response::Response::new(axum::body::Body::from(body));
        *response.status_mut() = axum::http::StatusCode::from_u16(status).expect("status");
        for (k, v) in headers {
            response
                .headers_mut()
                .insert(k, axum::http::HeaderValue::from_static(v));
        }
        response
    }

    async fn fake(reply: Reply) -> Fake {
        let state = Fake {
            posts: Arc::default(),
            auth: Arc::default(),
            methods: Arc::default(),
            reply,
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

    fn ok(_: usize, _: &str, _: &str) -> (u16, Vec<(&'static str, &'static str)>, String) {
        (200, vec![], format!(r#"{{"ok":true,"ts":"{TS}"}}"#))
    }

    fn run(body: &str) -> RunSummary {
        RunSummary {
            task_name: "Daily <brief> & co".into(),
            succeeded: true,
            duration_ms: 192_000,
            job_id: "job-1".into(),
            body: body.into(),
        }
    }

    fn channels(ids: &[&str]) -> Vec<String> {
        ids.iter().map(ToString::to_string).collect()
    }

    fn assert_no_token(results: &[ChannelResult]) {
        for r in results {
            assert!(!format!("{r:?}").contains(TOKEN), "{r:?}");
        }
    }

    #[tokio::test]
    async fn a_summary_then_the_output_in_its_thread() {
        let _guard = api_base_lock().await;
        let fake = fake(ok).await;
        let long = "word ".repeat(2000);
        let results = deliver(TOKEN, &channels(&["C1", "C2"]), &run(&long), &mut None).await;
        set_api_base(None);

        let expected = mrkdwn::split(&mrkdwn::to_mrkdwn(&long), mrkdwn::MAX_MESSAGE_CHARS);
        assert!(expected.len() > 1, "the answer must need several chunks");
        assert_eq!(
            results,
            vec![
                ChannelResult {
                    channel_id: "C1".into(),
                    outcome: ChannelOutcome::Sent { ts: TS.into() }
                },
                ChannelResult {
                    channel_id: "C2".into(),
                    outcome: ChannelOutcome::Sent { ts: TS.into() }
                },
            ]
        );
        let posts = fake.posts();
        assert_eq!(posts.len(), 2 * (1 + expected.len()));
        for channel in ["C1", "C2"] {
            let mine: Vec<_> = posts.iter().filter(|p| p.0 == channel).collect();
            assert_eq!(mine[0].1, "", "the summary is top-level");
            assert_eq!(
                mine[0].2,
                "*Daily &lt;brief&gt; &amp; co* · succeeded · 3m 12s\n\
                 Job job-1 · open Agento → Jobs for the full record"
            );
            let replies: Vec<_> = mine[1..].iter().map(|p| p.2.clone()).collect();
            assert_eq!(replies, expected, "in order, one per chunk");
            assert!(mine[1..].iter().all(|p| p.1 == TS));
        }
        assert!(fake
            .auth
            .lock()
            .expect("lock")
            .iter()
            .all(|h| h == &format!("Bearer {TOKEN}")));
        assert_no_token(&results);
    }

    #[tokio::test]
    async fn a_broadcast_in_the_output_never_reaches_slack_raw() {
        let _guard = api_base_lock().await;
        let fake = fake(ok).await;
        let results = deliver(
            TOKEN,
            &channels(&["C1"]),
            &run("hey <!channel> and <!here>, also [](!channel)"),
            &mut None,
        )
        .await;
        set_api_base(None);

        assert!(matches!(results[0].outcome, ChannelOutcome::Sent { .. }));
        let posts = fake.posts();
        assert_eq!(posts.len(), 2);
        assert!(posts.iter().all(|p| !p.2.contains("<!")), "{posts:?}");
    }

    #[tokio::test]
    async fn an_empty_answer_still_posts_a_reply() {
        let _guard = api_base_lock().await;
        let fake = fake(ok).await;
        deliver(TOKEN, &channels(&["C1"]), &run(""), &mut None).await;
        set_api_base(None);
        assert_eq!(fake.posts()[1].2, NO_RESPONSE_REPLY);
    }

    #[tokio::test]
    async fn not_in_channel_says_to_invite_the_bot_and_the_next_channel_still_posts() {
        let _guard = api_base_lock().await;
        let fake = fake(|_, channel, _| {
            if channel == "C1" {
                (
                    200,
                    vec![],
                    r#"{"ok":false,"error":"not_in_channel"}"#.into(),
                )
            } else {
                ok(0, channel, "")
            }
        })
        .await;
        let results = deliver(TOKEN, &channels(&["C1", "C2"]), &run("hi"), &mut None).await;
        set_api_base(None);

        let ChannelOutcome::Failed(e) = &results[0].outcome else {
            panic!("{results:?}");
        };
        assert!(e.contains("invite it"), "{e}");
        assert!(matches!(results[1].outcome, ChannelOutcome::Sent { .. }));
        let posts = fake.posts();
        assert_eq!(
            posts.iter().filter(|p| p.0 == "C1").count(),
            1,
            "a failed summary attempts no replies"
        );
        assert_eq!(posts.iter().filter(|p| p.0 == "C2").count(), 2);
        assert_no_token(&results);
    }

    #[tokio::test]
    async fn channel_not_found_is_a_sentence() {
        let _guard = api_base_lock().await;
        let _fake = fake(|_, _, _| {
            (
                200,
                vec![],
                r#"{"ok":false,"error":"channel_not_found"}"#.into(),
            )
        })
        .await;
        let results = deliver(TOKEN, &channels(&["C9"]), &run("hi"), &mut None).await;
        set_api_base(None);
        assert_eq!(
            results[0].outcome,
            ChannelOutcome::Failed(
                "channel not found — check the ID, and that the bot was invited if the \
                 channel is private (channel_not_found)"
                    .into()
            )
        );
    }

    #[tokio::test]
    async fn a_rate_limit_is_a_failure_with_or_without_retry_after() {
        let _guard = api_base_lock().await;
        let _fake = fake(|n, _, _| {
            let headers = if n == 0 {
                vec![("retry-after", "30")]
            } else {
                vec![]
            };
            (429, headers, String::new())
        })
        .await;
        let results = deliver(TOKEN, &channels(&["C1", "C2"]), &run("hi"), &mut None).await;
        set_api_base(None);
        assert_eq!(
            results[0].outcome,
            ChannelOutcome::Failed(
                "slack rate limited (chat.postMessage), retry after 30 seconds".into()
            )
        );
        assert_eq!(
            results[1].outcome,
            ChannelOutcome::Failed(
                "slack rate limited (chat.postMessage), retry after  seconds".into()
            )
        );
    }

    #[tokio::test]
    async fn ok_decides_over_the_http_status() {
        let _guard = api_base_lock().await;
        let _fake = fake(|_, _, _| (500, vec![], format!(r#"{{"ok":true,"ts":"{TS}"}}"#))).await;
        let results = deliver(TOKEN, &channels(&["C1"]), &run("hi"), &mut None).await;
        set_api_base(None);
        assert_eq!(results[0].outcome, ChannelOutcome::Sent { ts: TS.into() });
    }

    #[tokio::test]
    async fn a_summary_without_a_ts_is_a_failure() {
        let _guard = api_base_lock().await;
        let fake = fake(|_, _, _| (200, vec![], r#"{"ok":true}"#.into())).await;
        let results = deliver(TOKEN, &channels(&["C1"]), &run("hi"), &mut None).await;
        set_api_base(None);
        assert_eq!(
            results[0].outcome,
            ChannelOutcome::Failed("Slack returned no message ts".into())
        );
        assert_eq!(fake.posts().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_reply_stops_the_thread_and_says_which() {
        let _guard = api_base_lock().await;
        let fake = fake(|n, channel, thread| {
            if n == 2 {
                (200, vec![], r#"{"ok":false,"error":"invalid_auth"}"#.into())
            } else {
                ok(n, channel, thread)
            }
        })
        .await;
        let long = "word ".repeat(2000);
        let n = mrkdwn::split(&mrkdwn::to_mrkdwn(&long), mrkdwn::MAX_MESSAGE_CHARS).len();
        let results = deliver(TOKEN, &channels(&["C1"]), &run(&long), &mut None).await;
        set_api_base(None);
        assert_eq!(
            results[0].outcome,
            ChannelOutcome::Failed(format!(
                "posted the summary; reply 2 of {n} failed: the Slack token was rejected — \
                 reconnect the integration (invalid_auth)"
            ))
        );
        assert_eq!(fake.posts().len(), 3, "nothing after the failed reply");
        assert_no_token(&results);
    }

    // ─── The thread mapping (#642) ──────────────────────────────────────────

    /// Every post answers `ok` with a `ts` of its own, so two summaries never
    /// share a thread.
    fn ok_distinct(n: usize, _: &str, _: &str) -> (u16, Vec<(&'static str, &'static str)>, String) {
        (
            200,
            vec![],
            format!(r#"{{"ok":true,"ts":"1700000000.{n:06}"}}"#),
        )
    }

    /// A migrated database holding integration `s1` and chats `chat-a`, `chat-b`.
    fn mapped_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp db");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute_batch(
            "INSERT INTO integrations
                (id, name, type, enabled, credentials, services, created_at, updated_at)
             VALUES ('s1', 'Slack', 'slack', 1, '{}', '{}', '', '');
             INSERT INTO chat_sessions (id, agent_slug, created_at, updated_at)
             VALUES ('chat-a', '', '', ''), ('chat-b', '', '', '');",
        )
        .expect("seed");
        file
    }

    fn mapping(file: &tempfile::NamedTempFile, chat_id: &str) -> Option<ThreadMapping> {
        Some(ThreadMapping {
            db_path: file.path().to_path_buf(),
            integration_id: "s1".into(),
            chat_id: chat_id.into(),
        })
    }

    /// `(channel_id, thread_ts, chat_id, permalink)` for every mapped thread.
    fn mapped(file: &tempfile::NamedTempFile) -> Vec<(String, String, String, String)> {
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let mut stmt = conn
            .prepare(
                "SELECT channel_id, thread_ts, chat_id, permalink FROM inbound_threads
                 ORDER BY rowid",
            )
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("query");
        rows.map(|r| r.expect("row")).collect()
    }

    fn row(channel: &str, ts: &str, chat: &str) -> (String, String, String, String) {
        (
            channel.into(),
            ts.into(),
            chat.into(),
            "https://slack.example/p/1".into(),
        )
    }

    #[tokio::test]
    async fn only_the_first_channel_is_mapped_and_before_its_replies() {
        let _guard = api_base_lock().await;
        let fake = fake(ok_distinct).await;
        let db = mapped_db();
        let mut map = mapping(&db, "chat-a");
        let results = deliver(TOKEN, &channels(&["C1", "C2"]), &run("hi"), &mut map).await;
        assert!(results
            .iter()
            .all(|r| matches!(r.outcome, ChannelOutcome::Sent { .. })));
        assert!(map.is_none(), "the mapping was taken");
        assert_eq!(
            mapped(&db),
            vec![row("C1", "1700000000.000000", "chat-a")],
            "the second channel is posted to and never mapped"
        );
        assert_eq!(
            *fake.methods.lock().expect("lock"),
            vec![
                "chat.postMessage",
                "chat.getPermalink",
                "chat.postMessage",
                "chat.postMessage",
                "chat.postMessage",
            ],
            "mapped between the summary and its first reply, and only once"
        );

        // A second run maps its own chat to its own thread.
        let mut map = mapping(&db, "chat-b");
        deliver(TOKEN, &channels(&["C1"]), &run("hi"), &mut map).await;
        set_api_base(None);
        assert_eq!(
            mapped(&db),
            vec![
                row("C1", "1700000000.000000", "chat-a"),
                row("C1", "1700000000.000004", "chat-b"),
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_first_channel_hands_the_mapping_to_the_next() {
        let _guard = api_base_lock().await;
        let _fake = fake(|n, channel, thread| {
            if channel == "C1" {
                (
                    200,
                    vec![],
                    r#"{"ok":false,"error":"not_in_channel"}"#.into(),
                )
            } else {
                ok_distinct(n, channel, thread)
            }
        })
        .await;
        let db = mapped_db();
        let mut map = mapping(&db, "chat-a");
        let results = deliver(TOKEN, &channels(&["C1", "C2"]), &run("hi"), &mut map).await;
        set_api_base(None);
        assert!(matches!(results[0].outcome, ChannelOutcome::Failed(_)));
        assert_eq!(mapped(&db), vec![row("C2", "1700000000.000001", "chat-a")]);
    }

    #[tokio::test]
    async fn a_failed_mapping_never_fails_the_delivery_or_moves_on() {
        let _guard = api_base_lock().await;
        let fake = fake(ok_distinct).await;
        let db = mapped_db();
        // The chat already holds a thread, so `UNIQUE (chat_id)` refuses.
        rusqlite::Connection::open(db.path())
            .expect("open")
            .execute(
                "INSERT INTO inbound_threads (integration_id, channel_id, thread_ts, chat_id)
                 VALUES ('s1', 'C0', '1.0', 'chat-a')",
                [],
            )
            .expect("seed a thread");
        let mut map = mapping(&db, "chat-a");
        let results = deliver(TOKEN, &channels(&["C1", "C2"]), &run("hi"), &mut map).await;
        set_api_base(None);
        assert!(results
            .iter()
            .all(|r| matches!(r.outcome, ChannelOutcome::Sent { .. })));
        assert_eq!(mapped(&db).len(), 1, "nothing was added");
        assert_eq!(
            fake.methods
                .lock()
                .expect("lock")
                .iter()
                .filter(|m| *m == "chat.getPermalink")
                .count(),
            1,
            "the failed insert is not retried on the second channel"
        );
    }

    #[test]
    fn readable_passes_through_what_it_does_not_know() {
        assert_eq!(
            readable("slack API error (chat.postMessage): is_archived"),
            "the channel is archived (is_archived)"
        );
        assert_eq!(
            readable("slack API error (chat.postMessage): something_new"),
            "slack API error (chat.postMessage): something_new"
        );
        assert_eq!(
            readable("calling Slack chat.postMessage: request failed"),
            "calling Slack chat.postMessage: request failed"
        );
    }

    #[test]
    fn durations_read_like_a_person_wrote_them() {
        assert_eq!(format_duration(-5), "0ms");
        assert_eq!(format_duration(850), "850ms");
        assert_eq!(format_duration(12_400), "12s");
        assert_eq!(format_duration(192_000), "3m 12s");
        assert_eq!(format_duration(3_840_000), "1h 4m");
    }

    #[test]
    fn a_failed_run_summarises_as_failed_and_threads_its_error() {
        let failed = RunSummary {
            succeeded: false,
            body: String::new(),
            ..run("")
        };
        assert!(summary_text(&failed).contains(" · failed · "));
        assert_eq!(thread_body(&failed), "The run failed.");
    }
}

//! What an `app_mention` does: the handler behind [`socket::SocketOptions::handler`]
//! (#568).
//!
//! One mention becomes one agent turn on one chat, and its answer becomes one
//! or more messages in the Slack thread the mention is in. Everything before
//! this module is transport — #567 acknowledges the envelope, claims it against
//! `slack_processed_events` and takes the trigger dispatcher's semaphore, so a
//! handler runs already-deduplicated and already-bounded at ten concurrent runs
//! across every inbound transport.
//!
//! ## The three decisions, in the order they are made
//!
//! 1. **Where the mention is.** A mention with no `thread_ts`, or whose
//!    `thread_ts` equals its own `ts`, is **top-level** and starts a chat under
//!    a new thread. A mention inside a thread is a **resume** when
//!    `(integration, channel, thread_ts)` is in `inbound_threads`, and is
//!    ignored at `debug` when it is not — Agento answers in threads it started
//!    and nowhere else (epic #562, decision 1).
//! 2. **Which rule.** [`select_rule_for_channel`] — most specific first, and a
//!    *disabled* channel-specific rule is that channel's off switch rather than
//!    a fall-through to the workspace default (#565). No rule at all is silence.
//! 3. **Whether there is anything to say.** The bot's own `<@Uxxx>` is removed;
//!    an empty remainder is ignored.
//!
//! The mapping lookup in (1) deliberately happens **inside the per-thread
//! worker**, not when the event arrives. The second mention in a thread whose
//! first mention is still running would otherwise look unmapped — the row is
//! written by the run it is queued behind — and be dropped as a mention in a
//! thread Agento did not start.
//!
//! ## The per-thread FIFO
//!
//! A `thread → queue` map with one worker task per key, torn down when its
//! queue drains, so two mentions in one thread run **in the order they were
//! queued** and never overlap. Queued, not *arrived*: #567 spawns a task per
//! envelope and each awaits its dedup claim before the handler is called at
//! all, so two mentions posted a millisecond apart can reach [`enqueue`] either
//! way round and nothing downstream could put that back. What the queue
//! guarantees — and what the acceptance criterion is about — is that whichever
//! arrives first at the queue is answered first, and that the second never runs
//! while the first is running. It is **in addition to** the chat's busy lock,
//! not instead of it:
//! `agent_run::run_resumed` takes `chat::live::try_lock`, which is what stops a
//! Slack turn and a UI turn colliding on the same chat row (decision 9). The
//! queue is what makes the *ordering* deterministic; the lock is what makes the
//! collision safe.
//!
//! **Teardown is under the same lock that hands out senders.** A worker that
//! found its queue empty, released nothing, and then removed its map entry
//! would lose a job queued in between. So the drain re-checks the channel while
//! holding the map lock and only removes the entry when it is still empty.
//!
//! ## Rules that are not obvious from the code
//!
//! - **Both paths run through [`agent_run::run_resumed`].** A start creates the
//!   chat first and then resumes it — the chat has no `sdk_session_id` yet, so
//!   `resume_spec` passes no `--resume` and the first turn is an ordinary
//!   headless run whose session id is written back. One implementation means
//!   the busy lock, the write-back and the *additive* usage accounting are the
//!   same on the first turn and the fiftieth.
//! - **The two failure sentences are the dispatcher's constants**, not
//!   re-spellings. Telegram and Slack answering differently for the same failure
//!   is the drift those `pub(crate)` consts exist to prevent.
//! - **The bot user id comes from `auth.test` once**, cached for the life of the
//!   handler rather than fetched per event. Without it neither the mention strip
//!   nor the empty-remainder rule can be applied, so a failure to get it is a
//!   failed turn and answers [`ERROR_REPLY`] — a Slack that will not answer
//!   `auth.test` will not accept `chat.postMessage` either. It is fetched
//!   **after** the thread has been established as Agento's, because that reply
//!   would otherwise land in a stranger's thread.
//! - **The dispatcher's ten-slot semaphore is taken around the run**, not around
//!   the handler. It bounds `claude` subprocesses, and a mention waiting for its
//!   thread's turn is not one: taken at the handler, ten queued mentions in one
//!   Slack thread hold every permit while one runs, and Telegram stops too.
//! - **No Slack-derived text reaches an `info` line, a path or a shell.** The
//!   prompt is logged at `debug` and nowhere else; the channel and thread ids
//!   are Slack's own opaque identifiers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::claude::CancellationToken;
use crate::native::agent_run;
use crate::native::db;
use crate::native::trigger::dispatcher::{self, Rule, ERROR_REPLY, NO_RESPONSE_REPLY};
use crate::native::trigger::select_rule::select_rule_for_channel;

use crate::native::gourl::Values;

use super::client::Client;
use super::mrkdwn;
use super::socket::{AppMention, EventHandler};

/// `[Slack] #channel: <first 60 chars>` — the epic's chat title.
const TITLE_PROMPT_CHARS: usize = 60;

/// One thread's queue key. The integration is the handler's, so it is not here.
type ThreadKey = (String, String);

/// One mention, everything decided about it, waiting for its thread's turn.
struct Job {
    mention: AppMention,
    /// The thread this belongs to: the mention's own `ts` when it is top-level.
    thread_ts: String,
    top_level: bool,
    rule: Rule,
    /// Dropped or signalled when this job is finished, however it finished.
    ///
    /// The handler future is the whole turn, not the enqueue: a `debug` line
    /// saying a mention was seen and a reply appearing minutes later are two
    /// different things for anyone reading a log, and #567's `dispatch` has
    /// nothing left to do while this runs. It costs a parked task and no
    /// semaphore permit — the ten-slot bound is taken in [`Inbound::run`],
    /// around the run itself.
    done: tokio::sync::oneshot::Sender<()>,
}

/// The handler's state: one per Slack integration with inbound enabled.
struct Inbound {
    db_path: PathBuf,
    integration_id: String,
    client: Client,
    bot_user_id: tokio::sync::OnceCell<String>,
    queues: Mutex<HashMap<ThreadKey, mpsc::UnboundedSender<Job>>>,
}

/// The `app_mention` handler for one integration.
///
/// `bot_token` is the workspace token every reply is posted with — resolved by
/// the registry through `resolve_slack_token`, so the `auth` column's OAuth arm
/// works here exactly as it does for the hosted tools.
pub fn handler(db_path: &Path, integration_id: &str, bot_token: &str) -> EventHandler {
    let state = Arc::new(Inbound {
        db_path: db_path.to_path_buf(),
        integration_id: integration_id.to_string(),
        client: Client::new(bot_token),
        bot_user_id: tokio::sync::OnceCell::new(),
        queues: Mutex::new(HashMap::new()),
    });
    Arc::new(move |mention: AppMention| {
        let state = Arc::clone(&state);
        Box::pin(async move { state.accept(mention).await })
    })
}

impl Inbound {
    /// Classify, select a rule, and queue — or drop.
    async fn accept(self: Arc<Self>, mention: AppMention) {
        let top_level = mention.thread_ts.is_empty() || mention.thread_ts == mention.ts;
        let thread_ts = if top_level {
            mention.ts.clone()
        } else {
            mention.thread_ts.clone()
        };

        let Some(rule) = self.rule_for(&mention.channel).await else {
            log::debug!(
                "slack mention ignored, no rule for the channel integration_id={:?} channel={:?}",
                self.integration_id,
                mention.channel
            );
            return;
        };

        let (done, finished) = tokio::sync::oneshot::channel();
        enqueue(
            &self,
            Job {
                mention,
                thread_ts,
                top_level,
                rule,
                done,
            },
        );
        // An `Err` is the worker being torn down without answering, which is
        // still the end of this handler's work.
        let _ = finished.await;
    }

    /// The rule this channel runs under, or `None` for silence.
    async fn rule_for(&self, channel: &str) -> Option<Rule> {
        let (db_path, integration_id) = (self.db_path.clone(), self.integration_id.clone());
        let loaded = db::blocking("slack rule match", move || {
            dispatcher::load_rules(&db_path, &integration_id)
        })
        .await?;
        let rules = match loaded {
            Ok(rules) => rules,
            Err(e) => {
                log::warn!("failed to load slack trigger rules: {e}");
                return None;
            }
        };
        select_rule_for_channel(&rules, channel).cloned()
    }

    /// The bot's own user id, fetched from `auth.test` at most once.
    async fn bot_user_id(&self) -> Option<&str> {
        self.bot_user_id
            .get_or_try_init(|| async {
                let ct = CancellationToken::new();
                let body = self
                    .client
                    .call_form(&ct, "auth.test", String::new())
                    .await?;
                let id = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v.get("user_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| "auth.test answered no user_id".to_string())?;
                Ok::<String, String>(id)
            })
            .await
            .map(String::as_str)
            .map_err(|e| log::warn!("slack inbound cannot identify itself: {e}"))
            .ok()
    }

    /// Run this thread's jobs in arrival order, then retire.
    async fn drain(self: Arc<Self>, key: ThreadKey, mut rx: mpsc::UnboundedReceiver<Job>) {
        loop {
            let next = match rx.try_recv() {
                Ok(job) => Some(job),
                Err(_) => {
                    // Re-check while holding the map lock: a job queued between
                    // the check above and the removal below would otherwise be
                    // handed to a worker that has already gone.
                    let mut queues = self.queues.lock().expect("the slack inbound queue lock");
                    let pending = rx.try_recv().ok();
                    if pending.is_none() {
                        queues.remove(&key);
                    }
                    drop(queues);
                    pending
                }
            };
            let Some(job) = next else { return };
            self.run(job).await;
        }
    }

    /// One turn: resolve the chat, run it, answer in the thread.
    async fn run(&self, job: Job) {
        self.turn(&job).await;
        // Whatever happened, the caller waiting on this job is released.
        let _ = job.done.send(());
    }

    async fn turn(&self, job: &Job) {
        // Which thread this is has to be decided before anything is said, so a
        // failure below cannot answer into a thread Agento did not start.
        let mapped = match self.thread_chat(job).await {
            Ok(mapped) => mapped,
            Err(e) => {
                // Not the same as "no row": a database this process could not
                // read is a failed turn, and answering `debug`-and-silence here
                // would drop a resume the moment the scanner held a write lock.
                log::error!("reading the slack thread map: {e}");
                if job.top_level {
                    self.post(&job.mention.channel, &job.thread_ts, ERROR_REPLY)
                        .await;
                }
                return;
            }
        };
        if mapped.is_none() && !job.top_level {
            log::debug!(
                "slack mention ignored, a thread Agento did not start integration_id={:?} \
                 channel={:?} thread={:?}",
                self.integration_id,
                job.mention.channel,
                job.thread_ts
            );
            return;
        }

        // Only now, with the thread established as Agento's, is there anywhere
        // a failure sentence may be posted.
        let Some(bot_user_id) = self.bot_user_id().await else {
            self.post(&job.mention.channel, &job.thread_ts, ERROR_REPLY)
                .await;
            return;
        };
        let prompt = strip_mention(&job.mention.text, bot_user_id);
        if prompt.is_empty() {
            log::debug!(
                "slack mention ignored, nothing said to the bot integration_id={:?} channel={:?}",
                self.integration_id,
                job.mention.channel
            );
            return;
        }

        let (chat_id, kind) = match mapped {
            Some(chat_id) => (chat_id, "resume"),
            None => match self.start_chat(job, &prompt).await {
                Some(chat_id) => (chat_id, "start"),
                None => {
                    self.post(&job.mention.channel, &job.thread_ts, ERROR_REPLY)
                        .await;
                    return;
                }
            },
        };

        log::info!(
            "slack mention matched integration_id={:?} channel={:?} thread={:?} rule_id={:?} \
             chat_id={:?} kind={kind}",
            self.integration_id,
            job.mention.channel,
            job.thread_ts,
            job.rule.id,
            chat_id
        );
        // The one place a Slack message's own words are logged, and it is not
        // `info`: everything above is Slack's opaque identifiers.
        log::debug!("slack mention prompt chat_id={chat_id:?} prompt={prompt:?}");

        // **The ten-slot bound is taken here, around the run.** #567 took it
        // around the handler, which counted a mention *waiting for its thread's
        // turn* against a limit that is about `claude` subprocesses: ten queued
        // mentions in one Slack thread would hold every permit while one ran,
        // stalling Telegram and every other channel for the length of the chain.
        let Ok(_permit) = dispatcher::semaphore().acquire().await else {
            log::warn!("dispatcher stopped, dropping a slack turn chat_id={chat_id:?}");
            return;
        };
        let result = agent_run::run_resumed(
            &self.db_path,
            &chat_id,
            &prompt,
            &job.rule.settings,
            dispatcher::run_timeout(&job.rule),
        )
        .await;

        let reply = reply_for(result, &chat_id);
        self.post(&job.mention.channel, &job.thread_ts, &reply)
            .await;
        self.touch_thread(job).await;
    }

    /// The chat this thread is already mapped to.
    ///
    /// `Ok(None)` is "no such thread" and `Err` is "the map could not be read",
    /// and the two must not be confused: `open_read_write` carries a five-second
    /// `busy_timeout`, so a database busy behind the session scanner would
    /// otherwise make a resume look like a mention in somebody else's thread.
    async fn thread_chat(&self, job: &Job) -> Result<Option<String>, String> {
        let (db_path, integration_id) = (self.db_path.clone(), self.integration_id.clone());
        let (channel, thread_ts) = (job.mention.channel.clone(), job.thread_ts.clone());
        db::blocking("slack thread lookup", move || {
            find_thread(&db_path, &integration_id, &channel, &thread_ts)
        })
        .await
        .unwrap_or_else(|| Err("the slack thread lookup task failed".to_string()))
    }

    /// Create the chat for a top-level mention and map the thread to it.
    async fn start_chat(&self, job: &Job, prompt: &str) -> Option<String> {
        let title = format!(
            "[Slack] #{}: {}",
            self.channel_name(&job.mention.channel).await,
            truncate_chars(prompt, TITLE_PROMPT_CHARS)
        );
        let (db_path, rule) = (self.db_path.clone(), job.rule.clone());
        let created = db::blocking("slack session", move || {
            dispatcher::create_trigger_session(&db_path, &rule, &title)
        })
        .await?;
        let chat_id = match created {
            Ok(chat_id) => chat_id,
            Err(e) => {
                log::error!("failed to create chat session for a slack mention: {e}");
                return None;
            }
        };

        // Best-effort: the permalink is what #570 shows beside the chat, and a
        // Slack that will not produce one is not a reason to refuse the run.
        let permalink = self.permalink(&job.mention.channel, &job.thread_ts).await;
        let (db_path, integration_id) = (self.db_path.clone(), self.integration_id.clone());
        let (channel, thread_ts) = (job.mention.channel.clone(), job.thread_ts.clone());
        let mapped = chat_id.clone();
        if let Some(Err(e)) = db::blocking("slack thread map", move || {
            insert_thread(
                &db_path,
                &integration_id,
                &channel,
                &thread_ts,
                &mapped,
                &permalink,
            )
        })
        .await
        {
            // The run still happens; only the *next* mention in this thread is
            // lost, so this is loud rather than fatal.
            log::warn!("failed to map a slack thread to its chat: {e}");
        }
        Some(chat_id)
    }

    /// `last_event_at`, so a sweep can one day tell a live thread from a dead one.
    async fn touch_thread(&self, job: &Job) {
        let (db_path, integration_id) = (self.db_path.clone(), self.integration_id.clone());
        let (channel, thread_ts) = (job.mention.channel.clone(), job.thread_ts.clone());
        db::blocking("slack thread touch", move || {
            touch_thread(&db_path, &integration_id, &channel, &thread_ts)
        })
        .await;
    }

    /// The channel's name for the chat title, falling back to its id.
    async fn channel_name(&self, channel: &str) -> String {
        let ct = CancellationToken::new();
        let mut values = Values::new();
        values.set("channel", channel);
        let name = self
            .client
            .call_form(&ct, "conversations.info", values.encode())
            .await
            .ok()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
            .and_then(|value| {
                value
                    .get("channel")
                    .and_then(|c| c.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .filter(|name| !name.is_empty());
        name.unwrap_or_else(|| channel.to_string())
    }

    /// `chat.getPermalink` for the thread's first message; `""` on any failure.
    async fn permalink(&self, channel: &str, message_ts: &str) -> String {
        let ct = CancellationToken::new();
        let mut values = Values::new();
        values.set("channel", channel);
        values.set("message_ts", message_ts);
        self.client
            .call_form(&ct, "chat.getPermalink", values.encode())
            .await
            .ok()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
            .and_then(|value| {
                value
                    .get("permalink")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }

    /// The reply, in the thread, as consecutive messages.
    ///
    /// Sent one at a time and in order: Slack orders a thread by arrival, so two
    /// chunks in flight at once can render the answer backwards.
    async fn post(&self, channel: &str, thread_ts: &str, text: &str) {
        let ct = CancellationToken::new();
        for (i, chunk) in mrkdwn::split(text, mrkdwn::MAX_MESSAGE_CHARS)
            .into_iter()
            .enumerate()
        {
            let payload = serde_json::json!({
                "channel": channel,
                "thread_ts": thread_ts,
                "text": chunk,
                "mrkdwn": true,
            });
            let body = match crate::native::gojson::to_vec_marshal(&payload) {
                Ok(body) => body,
                Err(e) => {
                    log::error!("encoding a slack reply: {e}");
                    return;
                }
            };
            if let Err(e) = self.client.call_json(&ct, "chat.postMessage", body).await {
                log::error!(
                    "failed to send slack reply chunk {} channel={channel:?} error={e}",
                    i + 1
                );
                return;
            }
        }
    }
}

/// What a finished run answers with.
///
/// Silence is never an outcome, so every arm produces a sentence: a failed run
/// and a timed-out one are one arm — `agent_run` reports a deadline as
/// `Err(DEADLINE_EXCEEDED)`, so there is nothing to distinguish for a reader in
/// Slack — and an answer with no text is the third.
fn reply_for(result: Result<agent_run::RunResult, String>, chat_id: &str) -> String {
    match result {
        Ok(run) if run.answer.is_empty() => NO_RESPONSE_REPLY.to_string(),
        Ok(run) => mrkdwn::to_mrkdwn(&run.answer),
        Err(e) => {
            log::error!("agent execution failed for slack chat_id={chat_id:?} error={e}");
            ERROR_REPLY.to_string()
        }
    }
}

/// Hand `job` to its thread's worker, starting one when there is none.
///
/// A free function rather than a method because the worker task needs an owned
/// `Arc` and the caller keeps its own.
fn enqueue(state: &Arc<Inbound>, job: Job) {
    let key = (job.mention.channel.clone(), job.thread_ts.clone());
    let mut queues = state.queues.lock().expect("the slack inbound queue lock");
    let job = match queues.get(&key) {
        // A worker removes its entry under this same lock, so an entry that is
        // present has a live receiver.
        Some(tx) => match tx.send(job) {
            Ok(()) => return,
            Err(returned) => returned.0,
        },
        None => job,
    };
    let (tx, rx) = mpsc::unbounded_channel();
    tx.send(job).expect("the receiver is alive on this line");
    queues.insert(key.clone(), tx);
    let worker = Arc::clone(state);
    tokio::spawn(async move { worker.drain(key, rx).await });
}

/// `text` with every `<@bot>` (and `<@bot|label>`) removed, then trimmed.
fn strip_mention(text: &str, bot_user_id: &str) -> String {
    if bot_user_id.is_empty() {
        return text.trim().to_string();
    }
    let open = format!("<@{bot_user_id}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        // `<@U1>` and `<@U1|name>` are the bot; `<@U12>` is somebody else whose
        // id merely starts the same way.
        let Some(close) = after
            .find('>')
            .filter(|_| after.starts_with('>') || after.starts_with('|'))
        else {
            out.push_str(&rest[..at + open.len()]);
            rest = after;
            continue;
        };
        out.push_str(&rest[..at]);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// The first `limit` characters of `text`, never cutting one in half.
fn truncate_chars(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((offset, _)) => &text[..offset],
        None => text,
    }
}

/// The chat this thread is mapped to, distinguishing "no row" from "no answer".
fn find_thread(
    db_path: &Path,
    integration_id: &str,
    channel_id: &str,
    thread_ts: &str,
) -> Result<Option<String>, String> {
    let conn = db::open_read_only(db_path)?;
    match conn.query_row(
        "SELECT chat_id FROM inbound_threads
         WHERE integration_id = ?1 AND channel_id = ?2 AND thread_ts = ?3",
        rusqlite::params![integration_id, channel_id, thread_ts],
        |row| row.get::<_, String>(0),
    ) {
        Ok(chat_id) => Ok(Some(chat_id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("reading the slack thread map: {e}")),
    }
}

fn insert_thread(
    db_path: &Path,
    integration_id: &str,
    channel_id: &str,
    thread_ts: &str,
    chat_id: &str,
    permalink: &str,
) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;
    let now = crate::native::gotime::now_go_text();
    conn.execute(
        "INSERT INTO inbound_threads
            (integration_id, channel_id, thread_ts, chat_id, permalink, created_at, last_event_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        rusqlite::params![
            integration_id,
            channel_id,
            thread_ts,
            chat_id,
            permalink,
            now
        ],
    )
    .map_err(|e| format!("mapping slack thread {thread_ts:?}: {e}"))?;
    Ok(())
}

fn touch_thread(db_path: &Path, integration_id: &str, channel_id: &str, thread_ts: &str) {
    let Ok(conn) = db::open_read_write(db_path) else {
        log::warn!("failed to open the database to touch a slack thread");
        return;
    };
    if let Err(e) = conn.execute(
        "UPDATE inbound_threads SET last_event_at = ?4
         WHERE integration_id = ?1 AND channel_id = ?2 AND thread_ts = ?3",
        rusqlite::params![
            integration_id,
            channel_id,
            thread_ts,
            crate::native::gotime::now_go_text()
        ],
    ) {
        log::warn!("failed to record a slack thread's last event: {e}");
    }
}

#[cfg(test)]
mod tests;

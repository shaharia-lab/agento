//! Slack Socket Mode: one long-lived websocket per enabled Slack integration
//! (#567, epic #562).
//!
//! A desktop app has no public URL, so an Events API webhook is not available to
//! it at all — Socket Mode is the only way a Slack event reaches Agento. This
//! module is the transport half: open the connection, acknowledge, de-duplicate,
//! reconnect, and keep `inbound_status`/`inbound_error` current. What an
//! `app_mention` *does* is #568's, and arrives here as [`SocketOptions::handler`];
//! the default is a no-op that logs at `debug`.
//!
//! # The rules, and why each is a rule
//!
//! - **Acknowledge before anything else.** Slack expects `{"envelope_id": …}`
//!   within seconds and redelivers the envelope otherwise, so the ack is written
//!   to the socket before the claim, before the handler, and before any database
//!   touch. The handler then runs on `tokio::spawn` under the trigger
//!   dispatcher's semaphore — taken by the handler, around the run (#568) —
//!   which is why that semaphore is `pub(crate)` rather
//!   than this module opening a second bound on the same agent runs.
//! - **De-duplicate by `event_id`.** An unacknowledged envelope is redelivered,
//!   and a reconnect can replay one, so "acknowledged" is not "processed".
//!   [`claim_event`] is `trigger::receiver::claim_update`'s shape exactly —
//!   `INSERT OR IGNORE` inside an immediate transaction, the row count deciding
//!   who won, plus the same best-effort 48-hour sweep.
//! - **Back off on the wall clock, not on `tokio::time`.** A capped doubling
//!   schedule re-anchored against `Utc::now()`, for the reason
//!   `schedule::runtime`'s `advance_past_now` re-anchors: a suspended laptop
//!   does not advance a `tokio::time::sleep` on every platform, and a reconnect
//!   that waits out a shut lid is an integration that is silently down long
//!   after the machine is back.
//! - **A fresh `apps.connections.open` on every attempt.** The `wss://` URL it
//!   returns is single-use; reconnecting to the previous one fails.
//! - **A read has a deadline.** A half-open connection delivers no FIN and no
//!   RST, so a read without one parks the worker forever with the row still
//!   reading `connected` — the silent outage the stored status exists to
//!   prevent. [`SocketOptions::idle_timeout`] turns it into a reconnect.
//! - **The status is a stored value, not a log line.** `inbound_status` walks
//!   `connecting` → `connected` → `reconnecting` → `error`, with
//!   `inbound_error` carrying the last reason, so the UI can show an outage
//!   rather than the user discovering it by being ignored. That is the
//!   gateway's `BindFailed` reasoning (`gateway/registry.rs`), applied to a
//!   value the UI already reads.
//! - **`error` still retries.** It is the *reported* state after
//!   [`SocketOptions::failure_threshold`] consecutive failures, not a stop: an
//!   expired token that the user then fixes must reconnect without a restart. A
//!   401 from `apps.connections.open` is reported and **never** clears the
//!   stored credential — that is `token_validate::clear_auth`'s decision to
//!   make, on a route a person asked for.
//! - **Nothing blocking on the runtime, and nothing blocking on the socket
//!   either.** Every database touch goes through [`db::blocking`], which keeps
//!   it off a runtime worker — and no write is ever *awaited* on the task that
//!   reads the socket, which is a second rule with a second reason: the
//!   five-second `busy_timeout` that makes an inline write a stalled runtime
//!   also makes it an unacknowledged envelope. The claim is inside
//!   [`Worker::dispatch`]'s spawn and the status goes through
//!   [`status_writer`]. `a_socket_workers_contended_write_lock_does_not_stall_the_runtime`
//!   and `a_stalled_claim_does_not_hold_up_the_next_envelope` in
//!   `tests/slack_socket.rs` are the two halves.
//! - **Clearing the status belongs to `integrations::registry`, and so does
//!   deciding which worker may report it.** A worker cannot tell a stop from a
//!   replacement, and it cannot know whether the registry accepted it — see
//!   [`clear_status_blocking`] and [`status_writer`].
//! - **The `xapp-` token is read once and captured into the task.** It is never
//!   logged, never formatted into an error, and this module derives no `Debug`
//!   that could carry it.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::SinkExt;
use tokio_stream::StreamExt;
use tokio_tungstenite::tungstenite::Message;

use crate::native::db;

/// `inbound_status` while an attempt is in flight and nothing has succeeded yet.
pub const STATUS_CONNECTING: &str = "connecting";
/// `inbound_status` for a live socket.
pub const STATUS_CONNECTED: &str = "connected";
/// `inbound_status` between a drop and the next successful connect.
pub const STATUS_RECONNECTING: &str = "reconnecting";
/// `inbound_status` after [`SocketOptions::failure_threshold`] consecutive
/// failures. **Still retrying** — see the module header.
pub const STATUS_ERROR: &str = "error";

/// The first backoff wait.
const DEFAULT_BASE_BACKOFF: Duration = Duration::from_secs(1);
/// The ceiling the doubling stops at.
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Consecutive failed attempts before the status is reported as `error`.
const DEFAULT_FAILURE_THRESHOLD: u32 = 5;
/// How long a live socket may deliver nothing before it is treated as dead.
///
/// Slack's gateway pings a Socket Mode connection well inside this, and
/// `tungstenite` answers a ping itself while the frame still arrives here as
/// traffic — so a healthy connection never approaches it. Generous rather than
/// tight because the cost of being wrong is asymmetric: a reconnect too early
/// is a fresh `apps.connections.open`, a reconnect too late is an integration
/// that looks connected and answers nothing.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// How long `apps.connections.open` may take. Shorter than the Slack client's
/// sixty seconds: this call is on the reconnect path, and a request that hangs
/// is a worker that is not backing off.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest chunk of a backoff wait spent inside one `tokio::time::sleep`.
/// See [`sleep_until`] — the point is to re-read the wall clock often.
const SLEEP_CHUNK: Duration = Duration::from_secs(1);
/// How long the sweep keeps a processed `event_id`. `claim_update`'s horizon.
const DEDUP_HORIZON_HOURS: i64 = 48;
/// The epoch a worker holds until the registry accepts it — see
/// [`status_writer`]. Epochs are handed out from one upwards, so this matches
/// nothing and a worker that is built and then refused never writes a status.
pub(crate) const NOT_ACCEPTED: u64 = 0;

/// One `app_mention` event, as much of it as the transport can see.
///
/// Deliberately not the raw payload: #568 decides what an `app_mention` does,
/// and giving it the fields Slack always sends keeps the parse in one place.
/// No `Debug` is derived on the worker's own state, but this carries no
/// credential and a handler will want to log it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMention {
    /// The integration whose socket delivered it.
    pub integration_id: String,
    /// `event.channel`.
    pub channel: String,
    /// `event.user`.
    pub user: String,
    /// `event.text`, with the leading `<@bot>` mention left in.
    pub text: String,
    /// `event.ts` — the message's own timestamp, and its id within the channel.
    pub ts: String,
    /// `event.thread_ts`, empty when the mention is not in a thread.
    pub thread_ts: String,
    /// The envelope's `payload.event_id`, which is what dedup claims.
    pub event_id: String,
}

/// What runs after the ack. Returns a future so a handler may await — #568's
/// does, since it starts an agent run.
pub type EventHandler =
    Arc<dyn Fn(AppMention) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Everything about a worker that a test needs to move and production does not.
///
/// A plain struct rather than a `#[cfg(test)]` override like
/// [`client::API_BASE`](super::client): `tests/slack_socket.rs` is a separate
/// binary, so a `cfg(test)` seam inside the library is invisible to it, and the
/// alternative — an environment variable, as `AGENTO_CLAUDE_EXECUTABLE` is —
/// would be process-wide state for something each worker can simply be handed.
/// Production builds exactly one of these, [`SocketOptions::default`].
#[derive(Clone)]
pub struct SocketOptions {
    /// Where `apps.connections.open` lives. Slack's own base by default.
    pub api_base: String,
    /// The first backoff wait; each consecutive failure doubles it.
    pub base_backoff: Duration,
    /// The ceiling the doubling stops at.
    pub max_backoff: Duration,
    /// Consecutive failures before the status is reported as `error`.
    pub failure_threshold: u32,
    /// How long a live socket may deliver nothing at all before it is treated
    /// as dead. Slack's own pings count as traffic.
    pub idle_timeout: Duration,
    /// What an `app_mention` does. The default logs at `debug` and returns.
    pub handler: EventHandler,
}

impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            api_base: default_api_base(),
            base_backoff: DEFAULT_BASE_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            handler: default_handler(),
        }
    }
}

/// Slack's own base, and in an in-crate test whatever
/// [`client::API_BASE`](super::client) has been pointed at — so a `registry`
/// test can drive the whole start path, which builds its options with
/// [`SocketOptions::default`], without reaching the real `slack.com`.
fn default_api_base() -> String {
    super::client::api_base()
}

/// #568 replaces this. Until then an `app_mention` is observed and dropped, at
/// `debug` so a user who turns inbound on can see the transport working without
/// anything acting on their messages.
fn default_handler() -> EventHandler {
    Arc::new(|mention: AppMention| {
        Box::pin(async move {
            log::debug!(
                "slack socket: app_mention integration_id={:?} channel={:?} event_id={:?}",
                mention.integration_id,
                mention.channel,
                mention.event_id
            );
        })
    })
}

/// A running Socket Mode worker. **Dropping it stops the worker**, which is the
/// discipline the whole `integrations::registry` is built on: the handle *is*
/// the cancel, so there is no second map of cancel functions to keep in step.
///
/// The stop is prompt at every point of the loop — the connect, the read and the
/// backoff wait all select on the same oneshot — so a `stop` landing mid-connect
/// abandons the attempt rather than leaving a socket holding a credential.
pub struct SocketWorker {
    /// `Option` only so `Drop` can take it; always `Some` while alive.
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    /// Set before the shutdown is sent, and read by [`status_writer`] — see
    /// there for why a stopped worker must write nothing more.
    stopped: Arc<std::sync::atomic::AtomicBool>,
    /// [`NOT_ACCEPTED`] until [`SocketWorker::accept`] is called under the
    /// registry's lock. See [`status_writer`] for why it is granted there and
    /// not taken here.
    epoch: Arc<std::sync::atomic::AtomicU64>,
    integration_id: String,
}

impl SocketWorker {
    /// Grant the epoch the registry just took for this id, inside the same
    /// critical section that recorded the handle.
    ///
    /// **`integrations::registry` is the only production caller**, and the
    /// grant must stay where the acceptance is decided — see [`status_writer`].
    /// It is `pub` rather than `pub(crate)` because `tests/slack_socket.rs`
    /// drives a worker without a registry and has to do for itself what the
    /// registry would do for it; a worker that is never accepted reports
    /// nothing, which is the correct production behaviour and a silent test.
    pub fn accept(&self, epoch: u64) {
        self.epoch.store(epoch, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for SocketWorker {
    fn drop(&mut self) {
        // **Before** the oneshot, so there is no instant in which the worker
        // knows it is stopping and the writer does not.
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        log::info!(
            "slack socket worker stopped: integration_id={:?}",
            self.integration_id
        );
    }
}

/// Start a worker. Returns as soon as the task is spawned — the connect happens
/// inside it.
///
/// **Returning before the first connect is the point.** The caller is
/// `registry::start_one`, which has to record the handle under the generation it
/// read; awaiting a connection first would hold that decision open for however
/// long Slack takes, and a `stop` in that window would have nothing to drop.
pub fn start(
    db_path: &Path,
    integration_id: &str,
    app_token: &str,
    options: SocketOptions,
) -> SocketWorker {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (status_tx, status_rx) = tokio::sync::mpsc::unbounded_channel::<StatusMsg>();
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Not claimed here. The registry grants it when it accepts the handle —
    // see [`status_writer`] for why the two cannot be the same moment.
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(NOT_ACCEPTED));
    let task = Worker {
        db_path: db_path.to_path_buf(),
        integration_id: integration_id.to_string(),
        // Read once from `HostingRow` and captured here. It does not leave.
        app_token: app_token.to_string(),
        options,
        status: status_tx,
    };
    let id = integration_id.to_string();
    tokio::spawn(status_writer(
        db_path.to_path_buf(),
        id.clone(),
        Arc::clone(&epoch),
        Arc::clone(&stopped),
        status_rx,
    ));
    tokio::spawn(async move { task.run(shutdown_rx).await });
    log::info!("slack socket worker started: integration_id={id:?}");
    SocketWorker {
        shutdown: Some(shutdown_tx),
        stopped,
        epoch,
        integration_id: id,
    }
}

/// The task's own state. Derives nothing: [`Self::app_token`] is a secret and a
/// `{self:?}` in a log line is the same leak with a longer fuse — the rule
/// `registry::HostingRow` already states for the blob this came out of.
struct Worker {
    db_path: PathBuf,
    integration_id: String,
    app_token: String,
    options: SocketOptions,
    /// Where every status transition is *posted*. See [`status_writer`] — the
    /// worker never awaits a database write, because the thread that would wait
    /// is the one reading the socket.
    status: tokio::sync::mpsc::UnboundedSender<StatusMsg>,
}

/// Serializes the status writes **of one integration**, and the epoch check that
/// guards them.
///
/// The two have to be one critical section or the check proves nothing: a
/// retiring worker's [`StatusMsg`] can pass a `stopped` test and then sit inside
/// a `db::blocking` write for as long as the five-second `busy_timeout` allows,
/// which is ample time for its replacement to write `connected` underneath it.
/// The row would then be stuck on the old worker's last value, because on a
/// socket that connects and stays connected there is no next transition to
/// correct it. Holding this across the write makes the two writes ordered, and
/// re-reading the epoch inside it makes a stale one a no-op whichever order they
/// arrive in.
///
/// **Per integration, not one lock for the process.** What needs ordering is the
/// writes to *one row*; a single global lock would additionally make one
/// integration's slow write — and `busy_timeout` allows five seconds of slow —
/// delay every other integration's status reporting, which is the column being
/// wrong about a socket that is fine. It is also what made
/// `tests/slack_socket.rs` flake: eleven workers share that binary, two of them
/// hold a write lock for a second and a half on purpose, and a global lock
/// propagated those stalls to every other test's status writes.
///
/// The map grows with the number of distinct integration ids the process has
/// ever hosted, which is the same bound the registry's own maps carry. Nothing
/// on the socket's read path waits here.
fn status_lock(integration_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(integration_id.to_string())
        .or_default()
        .clone()
}

/// One thing to record about the connection.
struct StatusMsg {
    status: &'static str,
    error: String,
}

/// The one place `inbound_status` is written, and the reason the worker posts
/// rather than writes.
///
/// **Every database touch this module makes would otherwise be on the task that
/// reads the socket**, and `db::open_read_write` carries a five-second
/// `busy_timeout` — so awaiting one under a lock held by the session scanner's
/// batch writer leaves the next envelope unread and unacknowledged for that
/// whole window, past the seconds Slack waits before redelivering. Posting to a
/// channel takes the wait off that task; `db::blocking` here keeps it off a
/// runtime worker as well.
///
/// One consumer, so the writes stay **ordered**: a `connected` overtaking the
/// `reconnecting` that followed it would leave the row lying about a socket that
/// is down, which is the failure this column exists to prevent. The channel is
/// unbounded because a transition is rare and dropping one is worse than
/// queueing it, and the loop ends by draining when the worker drops its sender.
///
/// **`stopped` is checked per message, not once.** A `reload` is a stop followed
/// immediately by a start, and a transition posted just before the stop is still
/// in this queue; writing it would overwrite the *new* worker's status with the
/// old worker's last words, and nothing would rewrite it until the next
/// transition — which on a socket that connects and stays connected is never.
///
/// **The epoch is granted by the registry, not taken here, and that is the whole
/// of its correctness.** It says *this worker is the one the registry accepted*,
/// and only the registry can know that: a worker is built before the decision —
/// `start_for_type` is `async` and can be slow — and `put_if_current` may refuse
/// it on the generation. A worker that took its own epoch at spawn could
/// therefore take a **later** one than the worker that went on to be accepted,
/// and then be refused; every status the accepted worker posted for the rest of
/// the process would be discarded as stale, freezing the row on whatever it last
/// held. So the epoch is granted inside the same critical section that records
/// the handle, which makes epoch order and acceptance order the same order by
/// construction. A worker that is never accepted keeps [`NOT_ACCEPTED`], which
/// matches nothing.
async fn status_writer(
    db_path: PathBuf,
    integration_id: String,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<StatusMsg>,
) {
    while let Some(StatusMsg { status, error }) = rx.recv().await {
        if stopped.load(std::sync::atomic::Ordering::SeqCst) {
            continue;
        }
        let lock = status_lock(&integration_id);
        let _guard = lock.lock().await;
        // Re-read *inside* the lock. `stopped` above is the cheap early exit for
        // the ordinary case; this is the one that holds when the drop lands
        // between the two.
        if !super::super::registry::registry().socket_epoch_is_current(
            &integration_id,
            epoch.load(std::sync::atomic::Ordering::SeqCst),
        ) {
            continue;
        }
        let (path, id) = (db_path.clone(), integration_id.clone());
        db::blocking("slack inbound status", move || {
            write_status_blocking(&path, &id, status, &error);
        })
        .await;
    }
}

/// Why an inner session ended. Both reconnect; they differ only in what the
/// status says while the next attempt runs.
enum SessionEnd {
    /// Slack asked us to reconnect — a `disconnect` envelope, and only that.
    /// Not a failure — the consecutive counter resets.
    Requested,
    /// The socket died. The reason is user-visible.
    Failed(String),
}

/// One attempt's result: how it ended, and how long it was actually connected.
struct SessionOutcome {
    end: SessionEnd,
    /// `None` when the attempt never reached an open socket at all — an
    /// `apps.connections.open` refusal, or a handshake that failed.
    connected_for: Option<Duration>,
}

impl SessionOutcome {
    fn failed(reason: impl Into<String>) -> Self {
        Self {
            end: SessionEnd::Failed(reason.into()),
            connected_for: None,
        }
    }
}

/// Whether the worker's handle has been dropped, without waiting for it.
///
/// `Err(Empty)` is the only "still running" answer: the sender is dropped only
/// by [`SocketWorker::drop`], which always sends first, so both `Ok` and
/// `Err(Closed)` mean the handle is gone.
fn is_stopped(shutdown: &mut tokio::sync::oneshot::Receiver<()>) -> bool {
    !matches!(
        shutdown.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    )
}

impl Worker {
    async fn run(self, mut shutdown: tokio::sync::oneshot::Receiver<()>) {
        // Consecutive *failed* attempts, and what was last written about them.
        let mut failures: u32 = 0;
        self.post_status(STATUS_CONNECTING, String::new());

        loop {
            let outcome = tokio::select! {
                biased;
                _ = &mut shutdown => return,
                outcome = self.session() => outcome,
            };

            // **A session that stayed up is not evidence of a problem.** The
            // counter is about a gateway that will not have us, not about a
            // laptop lid: without this a socket that runs for hours and dies on
            // a suspend adds one to a counter that never comes back down, so an
            // integration that has worked all week reads `error` on its fifth
            // lifetime drop and reconnects only once a minute thereafter.
            //
            // `max_backoff` is the bar, and it is the one value that cannot
            // produce a hot loop: an attempt that outlived the longest wait this
            // schedule would ever impose costs at most one base wait to retry,
            // whatever it does next. A gateway that accepts and closes
            // immediately keeps escalating, which is the case the counter is
            // for.
            let healthy = outcome
                .connected_for
                .is_some_and(|held| held >= self.options.max_backoff);

            let (status, reason) = match outcome.end {
                SessionEnd::Requested => {
                    failures = 0;
                    (STATUS_RECONNECTING, String::new())
                }
                SessionEnd::Failed(reason) => {
                    failures = if healthy {
                        0
                    } else {
                        failures.saturating_add(1)
                    };
                    let status = if failures >= self.options.failure_threshold {
                        STATUS_ERROR
                    } else {
                        STATUS_RECONNECTING
                    };
                    log::warn!(
                        "slack socket: integration_id={:?} attempt={failures} status={status}: {reason}",
                        self.integration_id
                    );
                    (status, reason)
                }
            };

            // **Do not report anything once stopped.** A `reload` is a stop
            // followed by a start, so a retiring worker writing here would
            // overwrite the new worker's `connecting`/`connected` with its own
            // last words, and nothing would rewrite it until the next
            // transition — which on a healthy socket is never.
            if is_stopped(&mut shutdown) {
                return;
            }
            self.post_status(status, reason);

            let wait = backoff_for(
                failures,
                self.options.base_backoff,
                self.options.max_backoff,
                jitter_ratio(),
            );
            let deadline = Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default();
            if sleep_until(deadline, &mut shutdown).await.is_break() {
                return;
            }
        }
    }

    /// Record a transition. **Never awaited** — see [`status_writer`].
    fn post_status(&self, status: &'static str, error: String) {
        let _ = self.status.send(StatusMsg { status, error });
    }

    /// One attempt: open a connection and pump it until it ends.
    async fn session(&self) -> SessionOutcome {
        let url = match self.open_connection().await {
            Ok(url) => url,
            Err(e) => return SessionOutcome::failed(e),
        };
        // `connect_async` does **no** proxy handling, where `reqwest` reads
        // `HTTPS_PROXY` and the system settings — so behind an explicitly
        // configured proxy the call above succeeds and this does not. Reported
        // on #567's PR rather than widened into it; the `tokio-tungstenite`
        // paragraph in `Cargo.toml` says the same thing.
        let opened = match tokio::time::timeout(
            OPEN_TIMEOUT,
            tokio_tungstenite::connect_async(url.as_str()),
        )
        .await
        {
            Err(_) => {
                return SessionOutcome::failed("opening the socket mode connection timed out")
            }
            Ok(Err(e)) => {
                return SessionOutcome::failed(format!("opening the socket mode connection: {e}"))
            }
            Ok(Ok((socket, _))) => socket,
        };
        let mut socket = opened;
        let opened_at = std::time::Instant::now();

        self.post_status(STATUS_CONNECTED, String::new());

        let end = self.pump(&mut socket).await;
        SessionOutcome {
            end,
            connected_for: Some(opened_at.elapsed()),
        }
    }

    /// Read the socket until it ends, acknowledging and dispatching as it goes.
    async fn pump<S>(&self, socket: &mut S) -> SessionEnd
    where
        S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
            + tokio_stream::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        loop {
            // **A read with no deadline is how an outage becomes invisible.**
            // A half-open connection — a suspended laptop, a NAT rebinding, an
            // address change — delivers no FIN and no RST, so `next()` simply
            // never completes and the worker parks with `inbound_status` still
            // reading `connected`. That is exactly the silent failure the
            // stored status exists to prevent. Slack's own pings are traffic and
            // reset this, so a healthy connection never reaches it.
            let frame = match tokio::time::timeout(self.options.idle_timeout, socket.next()).await {
                Err(_) => {
                    return SessionEnd::Failed(format!(
                        "no traffic from slack for {:?}",
                        self.options.idle_timeout
                    ))
                }
                // **The stream ending without a close frame is a failure, not a
                // polite reconnect.** It is what a dropped TCP connection looks
                // like from here, and calling it `Requested` would reset the
                // consecutive-failure counter — so a gateway that keeps dropping
                // us would be retried every base wait forever, with
                // `inbound_status` never reaching `error`.
                Ok(None) => {
                    return SessionEnd::Failed(
                        "the connection closed without a disconnect".to_string(),
                    )
                }
                Ok(Some(Err(e))) => return SessionEnd::Failed(format!("reading the socket: {e}")),
                Ok(Some(Ok(frame))) => frame,
            };

            let text = match frame {
                Message::Text(text) => text.to_string(),
                // Slack sends no binary frames; one is not a reason to drop the
                // connection, only to say nothing was understood.
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) | Message::Binary(_) => {
                    continue
                }
                // **A close frame with no `disconnect` before it is a
                // failure.** Slack's own reconnect sends `disconnect` first and
                // returns above, so reaching here means the gateway closed on
                // us — and calling that `Requested` would reset the
                // consecutive-failure counter, leaving a gateway that accepts
                // and immediately closes in a one-second loop that never
                // escalates to `error`. Same reasoning as the `Ok(None)` arm.
                Message::Close(_) => {
                    return SessionEnd::Failed("slack closed the connection".to_string())
                }
            };

            let Some(envelope) = Envelope::parse(&text) else {
                log::warn!(
                    "slack socket: integration_id={:?}: an envelope did not parse",
                    self.integration_id
                );
                continue;
            };

            // **The ack comes first, before the claim and before the handler.**
            // Slack redelivers anything unacknowledged within seconds, and the
            // handler starts an agent run — orders of magnitude longer.
            if let Some(envelope_id) = envelope.envelope_id.as_deref() {
                if let Err(e) = socket.send(Message::text(ack_frame(envelope_id))).await {
                    return SessionEnd::Failed(format!("acknowledging an envelope: {e}"));
                }
            }

            if envelope.kind == "disconnect" {
                log::info!(
                    "slack socket: integration_id={:?}: slack asked for a reconnect ({})",
                    self.integration_id,
                    envelope.reason
                );
                return SessionEnd::Requested;
            }

            if let Some(mention) = envelope.app_mention(&self.integration_id) {
                self.dispatch(mention);
            }
        }
    }

    /// Claim the event, then run the handler.
    ///
    /// **Nothing here is awaited by the caller, and the claim is inside the
    /// spawn rather than before it.** The read loop has to go straight back to
    /// `next()`: `db::open_read_write` carries a five-second `busy_timeout`, so
    /// a claim awaited on the loop would, under a lock held by the session
    /// scanner's batch writer, leave the *next* envelope unread and
    /// unacknowledged for that whole window — which is precisely the window the
    /// ack-first rule exists to stay inside. The claim itself is unaffected:
    /// `INSERT OR IGNORE` decides who won whenever it runs.
    fn dispatch(&self, mention: AppMention) {
        let db_path = self.db_path.clone();
        let integration_id = self.integration_id.clone();
        let handler = Arc::clone(&self.options.handler);
        tokio::spawn(async move {
            let (claim_db, claim_id, event_id) = (
                db_path.clone(),
                integration_id.clone(),
                mention.event_id.clone(),
            );
            let claimed = db::blocking("slack event claim", move || {
                claim_event(&claim_db, &claim_id, &event_id)
            })
            .await
            .unwrap_or(false);
            if !claimed {
                log::debug!(
                    "slack socket: integration_id={integration_id:?}: event_id={:?} \
                     already processed",
                    mention.event_id
                );
                return;
            }

            // **The ten-slot bound is the handler's to take, not this task's**
            // (#568). `trigger::dispatcher::semaphore` is still the one bound
            // and it is still where the run happens under it — but it is
            // acquired in `inbound::Inbound::run`, around the run itself. Taken
            // here it would count a mention *waiting for its Slack thread's
            // turn* against a limit that is about `claude` subprocesses, and ten
            // queued mentions in one thread would hold every permit while one
            // ran, stalling Telegram and every other channel for the length of
            // the chain.
            handler(mention).await;
        });
    }

    /// `apps.connections.open` with the app-level token, answering the
    /// single-use `wss://` URL it hands back.
    ///
    /// Slack's own convention, which the six ported tools already follow:
    /// **`ok` decides, not the HTTP status.** No message here interpolates the
    /// token; what a failure names is Slack's `error` code or the transport's.
    async fn open_connection(&self) -> Result<String, String> {
        let client = super::client::http_client()
            .ok_or_else(|| "the slack http client could not be built".to_string())?;
        let url = format!("{}/apps.connections.open", self.options.api_base);
        let response = tokio::time::timeout(
            OPEN_TIMEOUT,
            client
                .post(&url)
                .bearer_auth(&self.app_token)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .send(),
        )
        .await
        .map_err(|_| "apps.connections.open timed out".to_string())?
        .map_err(|e| format!("apps.connections.open: {e}"))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| format!("reading apps.connections.open: {e}"))?;
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|_| format!("apps.connections.open answered {status}, not JSON"))?;

        // **`ok` decides, not the HTTP status** — Slack's own convention, and the
        // one a port gets backwards, as `slack/CLAUDE.md` records for the seven
        // tools. A 500 carrying `{"ok":true}` is a success here too.
        if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            let reason = parsed
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown_error");
            return Err(format!(
                "apps.connections.open refused the app token: {reason}"
            ));
        }
        parsed
            .get("url")
            .and_then(serde_json::Value::as_str)
            .filter(|url| !url.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "apps.connections.open returned no url".to_string())
    }
}

/// The acknowledgement for one envelope.
///
/// **JSON-encoded rather than interpolated**, because the id comes off the
/// network: `format!`ing a quote straight into a document is how a frame Slack
/// cannot parse gets sent, and an unparsed ack is a redelivery. Its own function
/// so the test that pins the escaping calls the same code the socket sends —
/// a test that re-typed the expression would pass against a reverted fix.
fn ack_frame(envelope_id: &str) -> String {
    format!(
        "{{\"envelope_id\":{}}}",
        serde_json::Value::String(envelope_id.to_string())
    )
}

/// The parsed shape of a Socket Mode envelope. Only the fields the transport
/// acts on; the payload is re-read for the event itself.
struct Envelope {
    kind: String,
    envelope_id: Option<String>,
    reason: String,
    payload: serde_json::Value,
}

impl Envelope {
    fn parse(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Self {
            kind: value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            envelope_id: value
                .get("envelope_id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            reason: value
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            payload: value
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        })
    }

    /// The envelope's `app_mention`, or `None` for every other event.
    ///
    /// **Every other Slack event type is out of scope** (#567's *Out of Scope*),
    /// and silently: a workspace sends dozens of event types Agento subscribes
    /// to by accident, and each one is still acknowledged above — dropping it
    /// here rather than at the ack is what keeps Slack from redelivering it.
    fn app_mention(&self, integration_id: &str) -> Option<AppMention> {
        if self.kind != "events_api" {
            return None;
        }
        let event = self.payload.get("event")?;
        if event.get("type").and_then(serde_json::Value::as_str)? != "app_mention" {
            return None;
        }
        let text_field = |value: &serde_json::Value, key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        // A bot's own message is not work, and the app's own reply is a bot's
        // own message: Slack delivers an `app_mention` for a mention inside a
        // message the app itself posted, so without this a reply that quotes the
        // bot answers itself, in the thread, forever. `bot_id` names the poster
        // and `subtype` covers `bot_message` and the joins/leaves/edits that
        // carry no author at all. Dropped here rather than at the ack, like
        // every other event this transport does not act on.
        if !text_field(event, "bot_id").is_empty() || !text_field(event, "subtype").is_empty() {
            log::debug!(
                "slack socket: ignoring an app_mention from a bot integration_id={integration_id:?}"
            );
            return None;
        }
        let event_id = self
            .payload
            .get("event_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())?
            .to_string();
        Some(AppMention {
            integration_id: integration_id.to_string(),
            channel: text_field(event, "channel"),
            user: text_field(event, "user"),
            text: text_field(event, "text"),
            ts: text_field(event, "ts"),
            thread_ts: text_field(event, "thread_ts"),
            event_id,
        })
    }
}

/// `true` when this event is new and has now been claimed.
///
/// `trigger::receiver::claim_update`'s shape, with a TEXT `event_id` instead of
/// an integer `update_id`. The claim is atomic — `INSERT OR IGNORE` inside an
/// immediate transaction, the row count deciding who won — because an envelope
/// redelivered while the first delivery's handler is still running would
/// otherwise start the agent twice, and Socket Mode redelivers anything it does
/// not see acknowledged.
///
/// A failure to record is `false`: do not run the agent against a database that
/// could not record the run.
pub fn claim_event(db_path: &Path, integration_id: &str, event_id: &str) -> bool {
    let claim = || -> Result<bool, String> {
        let mut conn = db::open_read_write(db_path)?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("begin slack event claim: {e}"))?;
        let now = crate::native::gotime::now_go_text();
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO slack_processed_events
                    (integration_id, event_id, processed_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![integration_id, event_id, now],
            )
            .map_err(|e| format!("marking a slack event as processed: {e}"))?;

        // The same best-effort 48-hour sweep `claim_update` does, and ignored
        // the same way: a full table is a slow claim, never a wrong one.
        let cutoff = crate::native::gotime::go_string_from_millis(
            (Utc::now() - chrono::Duration::hours(DEDUP_HORIZON_HOURS)).timestamp_millis(),
        );
        let _ = tx.execute(
            "DELETE FROM slack_processed_events WHERE processed_at < ?1",
            [&cutoff],
        );

        tx.commit()
            .map_err(|e| format!("commit slack event claim: {e}"))?;
        Ok(inserted > 0)
    };
    match claim() {
        Ok(claimed) => claimed,
        Err(e) => {
            log::error!(
                "failed to claim slack event integration_id={integration_id:?} \
                 event_id={event_id:?} error={e}"
            );
            false
        }
    }
}

/// `UPDATE integrations SET inbound_status, inbound_error`, and **nothing else**.
///
/// `updated_at` is deliberately not bumped: a worker rewriting its state on
/// every reconnection would keep moving the record's timestamp for something the
/// user did not do. `integrations/CLAUDE.md` states that as the contract #566
/// left for this worker.
pub fn write_status_blocking(db_path: &Path, integration_id: &str, status: &str, error: &str) {
    run_status_write(
        db_path,
        "UPDATE integrations SET inbound_status = ?1, inbound_error = ?2 WHERE id = ?3",
        rusqlite::params![status, error, integration_id],
        integration_id,
    );
}

/// Clear `inbound_status`/`inbound_error` for one integration.
///
/// **The registry calls this, not the worker, and that is the whole point.** A
/// worker cannot tell a stop from a replacement: a `reload` is a stop followed
/// immediately by a start, so a retiring worker clearing its own row would race
/// the new worker's `connecting`, and a compare-and-swap on the value cannot
/// help because both workers write the identical `"connected"`. The registry is
/// the one place that knows whether a socket is going away or being replaced —
/// it is the code that decides — so it owns the clear, ordered against the start
/// rather than racing it.
/// **Callers other than [`clear_status`] must already hold this row's
/// [`status_lock`].**
/// The one exception is boot, where no worker exists to order against.
pub fn clear_status_blocking(db_path: &Path, integration_id: &str) {
    run_status_write(
        db_path,
        "UPDATE integrations SET inbound_status = '', inbound_error = '' WHERE id = ?1",
        rusqlite::params![integration_id],
        integration_id,
    );
}

/// Clear one row's inbound state, ordered against any status write already in
/// flight.
///
/// **Both halves are load-bearing, and the epoch is the half that is easy to
/// miss.** A `status_writer` that has already passed its `stopped` and
/// `is_current` checks is sitting inside `db::blocking` for as long as the
/// five-second `busy_timeout` allows — and a worker that retires *without a
/// replacement* claims no new epoch, so `is_current` still answers `true` for
/// it. Bumping the epoch here is what retires that writer; holding the lock
/// across the bump and the clear is what makes a writer that got there first
/// finish before the clear rather than after it. Either one alone leaves the row
/// able to end up reading `connected` with no worker running, which nothing
/// would ever correct — a socket that connects and stays connected has no next
/// transition.
pub async fn clear_status(db_path: &Path, integration_id: &str, epoch: u64) {
    let lock = status_lock(integration_id);
    let _guard = lock.lock().await;
    // The registry took `epoch` when it retired the worker, under its own lock.
    // If something has been accepted since, that acceptance took a later one and
    // this clear is about a decision that has been superseded — writing it would
    // blank the row under a worker that is running.
    if !super::super::registry::registry().socket_epoch_is_current(integration_id, epoch) {
        return;
    }
    let (path, id) = (db_path.to_path_buf(), integration_id.to_string());
    db::blocking("slack inbound clear", move || {
        clear_status_blocking(&path, &id);
    })
    .await;
}

/// Clear the inbound state of **every** Slack row. Boot only.
///
/// A fresh process has no workers, so any status left in the database is about a
/// connection that no longer exists — including one a crash left reading
/// `connected`, which nothing else would ever correct. Run once at the top of
/// `start_all`, before any worker exists to race it; each worker then writes
/// `connecting` as it starts.
///
/// This does erase a stored `error` across a restart, where #566's rule is not
/// to erase the reason the user is looking at. The difference is that something
/// is about to re-report it: a row whose token is still bad is answered by its
/// own worker within a second or two, and a row that gets no worker genuinely
/// has nothing to say.
pub fn clear_all_inbound_status_blocking(db_path: &Path) {
    run_status_write(
        db_path,
        "UPDATE integrations SET inbound_status = '', inbound_error = ''
         WHERE type = 'slack' AND (inbound_status != '' OR inbound_error != '')",
        rusqlite::params![],
        "*",
    );
}

fn run_status_write(db_path: &Path, sql: &str, params: impl rusqlite::Params, what: &str) {
    let write = || -> Result<(), String> {
        let conn = db::open_read_write(db_path)?;
        conn.execute(sql, params)
            .map_err(|e| format!("writing the inbound status: {e}"))?;
        Ok(())
    };
    if let Err(e) = write() {
        log::error!("failed to write the slack inbound status integration_id={what:?} error={e}");
    }
}

/// The wait before attempt `failures + 1`: `base * 2^failures`, capped, plus up
/// to a quarter of itself as jitter.
///
/// `failures == 0` is the reconnect after a *successful* session and gets the
/// base wait; every consecutive failure doubles it. The jitter is additive and
/// bounded above by `ratio < 0.25 * step`, which is what keeps the schedule
/// **strictly increasing** below the cap — two workers reconnecting in lockstep
/// is what jitter is for, and a jitter wide enough to reorder two steps would
/// make "the second attempt waits longer than the first" untrue.
fn backoff_for(failures: u32, base: Duration, max: Duration, ratio: f64) -> Duration {
    let step = base
        .checked_mul(1u32.checked_shl(failures.min(31)).unwrap_or(u32::MAX))
        .unwrap_or(max)
        .min(max);
    let jitter = step.mul_f64(ratio.clamp(0.0, 1.0) * 0.25);
    step.saturating_add(jitter)
}

/// A cheap, dependency-free source of jitter in `[0, 1)`.
///
/// `rand` is in the lockfile (via `rmcp`) but not a direct dependency, and this
/// needs no distribution guarantees at all — only that two processes waking at
/// the same instant do not pick the same wait. The nanosecond field of the wall
/// clock is exactly that.
fn jitter_ratio() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

/// Wait until a **wall-clock** instant, or until the worker is stopped.
///
/// `tokio::time::sleep` measures elapsed process time, and on a suspended
/// machine that is not elapsed wall-clock time — the same difference
/// `schedule::runtime` re-anchors against with `advance_past_now`, quoting
/// gocron's own "the machine went to sleep, and woke up some time later". A
/// single long sleep would therefore hold the socket down for the length of a
/// shut lid *after* the lid opens. Sleeping in [`SLEEP_CHUNK`] slices and
/// re-reading `Utc::now()` each time bounds that error at one chunk.
///
/// `Break` means the worker was stopped and must return.
async fn sleep_until(
    deadline: DateTime<Utc>,
    shutdown: &mut tokio::sync::oneshot::Receiver<()>,
) -> std::ops::ControlFlow<()> {
    loop {
        let remaining = deadline - Utc::now();
        if remaining <= chrono::Duration::zero() {
            return std::ops::ControlFlow::Continue(());
        }
        let chunk = remaining.to_std().unwrap_or(SLEEP_CHUNK).min(SLEEP_CHUNK);
        tokio::select! {
            biased;
            _ = &mut *shutdown => return std::ops::ControlFlow::Break(()),
            () = tokio::time::sleep(chunk) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        file
    }

    /// The property the reconnect test in `tests/slack_socket.rs` observes over
    /// a socket, asserted here over the schedule itself — deterministically, at
    /// every jitter value rather than the one the clock happened to produce.
    ///
    /// A wider jitter would break it: at `± step` two adjacent steps overlap,
    /// and "the second attempt waits longer than the first" becomes true only
    /// most of the time, which is the shape of a test that passes in CI and
    /// fails on someone's laptop.
    #[test]
    fn the_backoff_doubles_to_a_cap_and_never_goes_backwards() {
        let base = Duration::from_secs(1);
        let max = Duration::from_secs(60);
        for ratio in [0.0, 0.25, 0.5, 0.999] {
            let waits: Vec<Duration> = (0..10).map(|n| backoff_for(n, base, max, ratio)).collect();
            for pair in waits.windows(2) {
                assert!(
                    pair[1] >= pair[0],
                    "the schedule went backwards at ratio {ratio}: {waits:?}"
                );
            }
            assert!(
                waits[1] > waits[0],
                "the second wait must be strictly longer than the first at ratio {ratio}: {waits:?}"
            );
            assert!(
                *waits.last().expect("ten waits") <= max.mul_f64(1.25),
                "the cap plus its jitter is the ceiling: {waits:?}"
            );
        }
    }

    /// A boot clears every Slack row's inbound state and nothing else's.
    ///
    /// A fresh process hosts no sockets, so a `connected` left by a crash is
    /// about a connection that does not exist and nothing else would ever
    /// correct it. The `type` scope is the part worth pinning: an `app_token`
    /// can be stored on any row (no validator rejects an unknown key), so a
    /// clear that forgot to scope by type would blank a column another
    /// integration's own inbound half might one day own.
    #[test]
    fn a_boot_clears_every_slack_rows_inbound_state_and_no_others() {
        let file = db();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        for (id, kind) in [("s1", "slack"), ("s2", "slack"), ("t1", "telegram")] {
            conn.execute(
                "INSERT INTO integrations
                    (id, name, type, enabled, credentials, services, created_at, updated_at,
                     inbound_enabled, inbound_status, inbound_error)
                 VALUES (?1, ?1, ?2, 1, '{}', '{}', 'then', 'then', 1, 'connected', 'boom')",
                rusqlite::params![id, kind],
            )
            .expect("seed");
        }
        drop(conn);

        clear_all_inbound_status_blocking(file.path());

        let conn = rusqlite::Connection::open(file.path()).expect("reopen");
        let read = |id: &str| -> (String, String, String) {
            conn.query_row(
                "SELECT inbound_status, inbound_error, updated_at FROM integrations WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read back")
        };
        for id in ["s1", "s2"] {
            assert_eq!(
                read(id),
                (String::new(), String::new(), "then".to_string()),
                "{id} must be cleared, and its updated_at left alone"
            );
        }
        assert_eq!(
            read("t1"),
            (
                "connected".to_string(),
                "boom".to_string(),
                "then".to_string()
            ),
            "a non-slack row is not this clear's business"
        );
    }

    /// The cap is a cap: a very large failure count must neither overflow nor
    /// wrap back to a short wait, which is how a shift-based doubling fails.
    #[test]
    fn a_runaway_failure_count_still_waits_the_cap_and_no_more() {
        let max = Duration::from_secs(60);
        for failures in [31u32, 32, 64, u32::MAX] {
            let wait = backoff_for(failures, Duration::from_secs(1), max, 0.0);
            assert_eq!(wait, max, "failures={failures} left the cap");
        }
    }

    /// `INSERT OR IGNORE` claims once. The second call is the redelivery Socket
    /// Mode makes when it does not see an ack, and it must not reach a handler.
    #[test]
    fn an_event_is_claimed_once_per_integration() {
        let file = db();
        assert!(
            claim_event(file.path(), "int-1", "Ev1"),
            "the first delivery"
        );
        assert!(!claim_event(file.path(), "int-1", "Ev1"), "a redelivery");
        assert!(
            claim_event(file.path(), "int-2", "Ev1"),
            "another integration is a different event"
        );
    }

    /// The status write touches two columns and no others. `updated_at` is the
    /// one that matters: a worker that reconnects hourly would otherwise keep
    /// moving the record's timestamp for something the user did not do.
    #[test]
    fn a_status_write_moves_neither_updated_at_nor_the_switch() {
        let file = db();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute(
            "INSERT INTO integrations
                (id, name, type, enabled, credentials, services, created_at, updated_at,
                 inbound_enabled, inbound_status, inbound_error)
             VALUES ('s1', 'S', 'slack', 1, '{}', '{}', 'then', 'then', 1, '', '')",
            [],
        )
        .expect("seed");
        drop(conn);

        write_status_blocking(file.path(), "s1", STATUS_ERROR, "invalid_auth");

        let conn = rusqlite::Connection::open(file.path()).expect("reopen");
        let (status, error, updated, enabled): (String, String, String, i64) = conn
            .query_row(
                "SELECT inbound_status, inbound_error, updated_at, inbound_enabled
                 FROM integrations WHERE id = 's1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read back");
        assert_eq!(status, STATUS_ERROR);
        assert_eq!(error, "invalid_auth");
        assert_eq!(updated, "then", "the status write must not bump updated_at");
        assert_eq!(enabled, 1, "the status write must not touch the switch");
    }

    /// Only `app_mention` inside an `events_api` envelope becomes work, and an
    /// event with no `event_id` is not dispatched at all — dedup has nothing to
    /// claim for it, so it would run again on every redelivery.
    #[test]
    fn only_an_app_mention_carrying_an_event_id_becomes_work() {
        let mention = Envelope::parse(
            r#"{"type":"events_api","envelope_id":"e1","payload":{"event_id":"Ev1",
                "event":{"type":"app_mention","channel":"C1","user":"U1","text":"hi",
                         "ts":"1.1","thread_ts":"1.0"}}}"#,
        )
        .expect("parse")
        .app_mention("int-1")
        .expect("an app_mention");
        assert_eq!(mention.event_id, "Ev1");
        assert_eq!(mention.channel, "C1");
        assert_eq!(mention.thread_ts, "1.0");

        let not_work = [
            // A different event type.
            r#"{"type":"events_api","payload":{"event_id":"Ev2","event":{"type":"message"}}}"#,
            // An app_mention with no event_id to claim.
            r#"{"type":"events_api","payload":{"event":{"type":"app_mention"}}}"#,
            // A control envelope.
            r#"{"type":"hello"}"#,
            r#"{"type":"disconnect","reason":"refresh_requested"}"#,
        ];
        for text in not_work {
            assert!(
                Envelope::parse(text)
                    .expect("parse")
                    .app_mention("int-1")
                    .is_none(),
                "{text} must not become work"
            );
        }
    }

    /// The app's own reply is a bot's own message, and a bot's own message is
    /// not work (#568).
    ///
    /// This is the loop guard: Slack delivers an `app_mention` for a mention
    /// inside a message the app itself posted, so an answer that quotes the
    /// question would otherwise answer itself in the same thread, forever, one
    /// `claude` subprocess at a time.
    #[test]
    fn a_bot_authored_app_mention_is_not_work() {
        for text in [
            r#"{"type":"events_api","envelope_id":"e1","payload":{"event_id":"Ev1",
                "event":{"type":"app_mention","channel":"C1","bot_id":"B1",
                         "text":"<@B1> hi","ts":"1.1"}}}"#,
            r#"{"type":"events_api","envelope_id":"e1","payload":{"event_id":"Ev1",
                "event":{"type":"app_mention","channel":"C1","subtype":"bot_message",
                         "text":"<@B1> hi","ts":"1.1"}}}"#,
        ] {
            assert!(
                Envelope::parse(text)
                    .expect("parse")
                    .app_mention("int-1")
                    .is_none(),
                "a bot's own app_mention must not become work: {text}"
            );
        }
    }

    /// A `thread_ts` that Slack omits is the empty string, not a missing field
    /// that drops the whole mention — a top-level mention is the common case.
    #[test]
    fn a_mention_outside_a_thread_carries_an_empty_thread_ts() {
        let mention = Envelope::parse(
            r#"{"type":"events_api","envelope_id":"e1","payload":{"event_id":"Ev1",
                "event":{"type":"app_mention","channel":"C1","user":"U1","text":"hi","ts":"1.1"}}}"#,
        )
        .expect("parse")
        .app_mention("int-1")
        .expect("an app_mention");
        assert_eq!(mention.thread_ts, "");
    }

    /// An envelope id is JSON-encoded rather than interpolated. Slack's ids are
    /// UUIDs, but the ack is a document and building it with `format!` on a
    /// value from the network is how a quote in it becomes a broken frame — and
    /// a frame Slack cannot parse is an unacknowledged envelope, so it comes
    /// back.
    ///
    /// Calls [`ack_frame`] rather than re-typing it, which is the difference
    /// between a regression guard and a copy that agrees with itself.
    #[test]
    fn an_envelope_id_is_json_encoded_into_the_ack() {
        let ack = ack_frame("a\"b");
        assert_eq!(ack, r#"{"envelope_id":"a\"b"}"#);
        let parsed: serde_json::Value = serde_json::from_str(&ack).expect("valid json");
        assert_eq!(parsed["envelope_id"], "a\"b");

        // And the ordinary case is still exactly what Slack expects.
        assert_eq!(ack_frame("Env0001"), r#"{"envelope_id":"Env0001"}"#);
    }

    /// The default handler is what ships until #568, and the thing it must not
    /// do is anything: no database touch, no reply, no panic.
    #[tokio::test]
    async fn the_default_handler_does_nothing_at_all() {
        let handler = default_handler();
        handler(AppMention {
            integration_id: "int-1".into(),
            channel: "C1".into(),
            user: "U1".into(),
            text: "hi".into(),
            ts: "1.1".into(),
            thread_ts: String::new(),
            event_id: "Ev1".into(),
        })
        .await;
    }
}

//! Telegram inbound by long-polling: one `getUpdates` loop per enabled Telegram
//! integration (#676, a prerequisite of epic #679).
//!
//! A desktop app has no public URL, so a webhook only ever worked behind a
//! tunnel the user set up and kept alive. A long poll is outbound-only — the
//! shape Slack's Socket Mode worker (`slack/socket.rs`, #567) already has — and
//! this module is that worker's twin, down to sharing its status writer.
//! Everything after "an update arrived" is unchanged: each update is handed to
//! `trigger::dispatcher::handle_update`, which claims it and runs the rule.
//!
//! # The rules, and why each is a rule
//!
//! - **`deleteWebhook` before the first poll.** Telegram answers `getUpdates`
//!   with a 409 while a webhook is set, so a row upgraded from the webhook
//!   transport would otherwise never receive anything. The row's
//!   `webhook_secret`/`webhook_status` are cleared in the same step, so the
//!   stored state never claims a webhook that is gone. A failure of either half
//!   is a failed attempt and is retried on the backoff; it is not skipped.
//! - **The offset lives in memory.** `last update_id + 1`, sent with every poll,
//!   is what confirms a batch to Telegram. After a restart Telegram redelivers
//!   whatever was unconfirmed and `trigger::receiver::claim_update` drops what
//!   already ran, so nothing is stored for it.
//! - **A bad update is skipped, and the offset still moves past it.** One
//!   element that does not decode would otherwise be redelivered forever and
//!   wedge every update behind it. A batch that cannot move the offset at all is
//!   reported as a failure rather than polled again at once.
//! - **A poll has a deadline of its own.** The long poll's server-side timeout
//!   plus [`PollOptions::request_grace`], which stays inside the sixty seconds
//!   `client::http_client` allows. A half-open connection is then a failed
//!   attempt, not a worker parked behind a status that still reads `connected`.
//! - **Back off on the wall clock**, for `slack/socket.rs`'s reason: a suspended
//!   laptop does not advance a `tokio::time::sleep` on every platform.
//! - **The first poll after a start or a failure does not wait.** It asks with
//!   `timeout: 0`, so `connected` is reported — or the failure is — in one round
//!   trip instead of after a thirty-second quiet poll.
//! - **`error` still retries.** It is the reported state after
//!   [`PollOptions::failure_threshold`] consecutive failures, not a stop.
//! - **A 409 is named.** Two programs polling one bot token take turns being
//!   refused, and Telegram's own sentence does not say what to do about it —
//!   see [`describe_failure`].
//! - **The status goes through `slack::socket::status_writer`**, so the epoch
//!   the registry grants decides which worker may write the row, exactly as it
//!   does for Slack. The claim is the dispatcher's, inside its own spawn; the
//!   only write this task awaits is the one-time webhook clear, and that goes
//!   through [`db::blocking`] like every other.
//! - **The bot token is read once and captured into the task.** It is in the
//!   request *path* (`client.rs`), so no error here interpolates a transport
//!   cause, and nothing holding it derives `Debug`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::client::Client;
use crate::claude::CancellationToken;
use crate::native::db;
use crate::native::gojson::GoStruct;
use crate::native::integrations::slack::socket::{
    backoff_for, jitter_ratio, status_writer, StatusMsg, NOT_ACCEPTED, STATUS_CONNECTED,
    STATUS_CONNECTING, STATUS_ERROR, STATUS_RECONNECTING,
};
use crate::native::trigger::receiver::TelegramUpdate;

/// The first backoff wait.
const DEFAULT_BASE_BACKOFF: Duration = Duration::from_secs(1);
/// The ceiling the doubling stops at.
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Consecutive failed attempts before the status is reported as `error`.
const DEFAULT_FAILURE_THRESHOLD: u32 = 5;
/// `getUpdates`' server-side `timeout`: how long Telegram holds a quiet poll.
const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long past the server-side timeout a request may run. Thirty plus twenty
/// stays below the sixty-second timeout on `client::http_client`, so this
/// deadline is the one that fires and its sentence is the one reported.
const DEFAULT_REQUEST_GRACE: Duration = Duration::from_secs(20);
/// The largest chunk of a backoff wait spent inside one `tokio::time::sleep`.
const SLEEP_CHUNK: Duration = Duration::from_secs(1);

/// Everything about a worker that a test needs to move and production does not.
/// Production builds exactly one of these, [`PollOptions::default`].
#[derive(Debug, Clone)]
pub struct PollOptions {
    /// The first backoff wait; each consecutive failure doubles it.
    pub base_backoff: Duration,
    /// The ceiling the doubling stops at.
    pub max_backoff: Duration,
    /// Consecutive failures before the status is reported as `error`.
    pub failure_threshold: u32,
    /// `getUpdates`' `timeout`, sent in whole seconds.
    pub poll_timeout: Duration,
    /// How long past [`Self::poll_timeout`] a request may run before it is
    /// treated as dead.
    pub request_grace: Duration,
}

impl Default for PollOptions {
    fn default() -> Self {
        Self {
            base_backoff: DEFAULT_BASE_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
            poll_timeout: DEFAULT_POLL_TIMEOUT,
            request_grace: DEFAULT_REQUEST_GRACE,
        }
    }
}

/// A running long-poll worker. **Dropping it stops the worker** and cancels the
/// poll in flight, which is the discipline `integrations::registry` is built
/// on: the handle *is* the cancel.
pub struct PollWorker {
    cancel: CancellationToken,
    /// Set before the cancel, and read by `slack::socket::status_writer` — a
    /// stopped worker must write nothing more.
    stopped: Arc<AtomicBool>,
    /// [`NOT_ACCEPTED`] until [`PollWorker::accept`] is called under the
    /// registry's lock.
    epoch: Arc<AtomicU64>,
    integration_id: String,
}

impl PollWorker {
    /// Grant the epoch the registry just took for this id, inside the same
    /// critical section that recorded the handle. `integrations::registry` is
    /// the only production caller; a worker that is never accepted reports
    /// nothing.
    pub fn accept(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::SeqCst);
    }
}

impl Drop for PollWorker {
    fn drop(&mut self) {
        // **Before** the cancel, so there is no instant in which the worker
        // knows it is stopping and the writer does not.
        self.stopped.store(true, Ordering::SeqCst);
        self.cancel.cancel();
        log::info!(
            "telegram poll worker stopped: integration_id={:?}",
            self.integration_id
        );
    }
}

/// Start a worker. Returns as soon as the task is spawned — the first request
/// happens inside it, for the reason `slack::socket::start` gives: the registry
/// has to record the handle under the generation it read.
pub fn start(
    db_path: &Path,
    integration_id: &str,
    bot_token: &str,
    options: PollOptions,
) -> PollWorker {
    let cancel = CancellationToken::new();
    let (status_tx, status_rx) = tokio::sync::mpsc::unbounded_channel::<StatusMsg>();
    let stopped = Arc::new(AtomicBool::new(false));
    let epoch = Arc::new(AtomicU64::new(NOT_ACCEPTED));
    let task = Worker {
        db_path: db_path.to_path_buf(),
        integration_id: integration_id.to_string(),
        // Read once from `HostingRow` and captured here. It does not leave.
        bot_token: bot_token.to_string(),
        options,
        status: status_tx,
        cancel: cancel.clone(),
    };
    let id = integration_id.to_string();
    tokio::spawn(status_writer(
        db_path.to_path_buf(),
        id.clone(),
        Arc::clone(&epoch),
        Arc::clone(&stopped),
        status_rx,
    ));
    tokio::spawn(task.run());
    log::info!("telegram poll worker started: integration_id={id:?}");
    PollWorker {
        cancel,
        stopped,
        epoch,
        integration_id: id,
    }
}

/// The task's own state. Derives nothing: [`Self::bot_token`] is a secret.
struct Worker {
    db_path: PathBuf,
    integration_id: String,
    bot_token: String,
    options: PollOptions,
    /// Where every status transition is *posted*; `status_writer` writes it.
    status: tokio::sync::mpsc::UnboundedSender<StatusMsg>,
    cancel: CancellationToken,
}

/// `getUpdates`' payload. Keys in sorted order, as `SetWebhook`'s are, because
/// Go would build it as a map and `json.Marshal` sorts those.
#[derive(Serialize)]
struct GetUpdates {
    allowed_updates: [&'static str; 1],
    offset: i64,
    timeout: u64,
}

/// `deleteWebhook`'s payload. Pending updates are kept: they are the messages
/// sent while the webhook was the transport, and the first poll collects them.
#[derive(Serialize)]
struct DeleteWebhook {
    drop_pending_updates: bool,
}

impl Worker {
    async fn run(self) {
        let client = Client::new(&self.bot_token);
        // Consecutive failed attempts.
        let mut failures: u32 = 0;
        let mut webhook_cleared = false;
        // Whether the row currently reads `connected`. A poll made while it
        // does not is the probe: it asks Telegram not to wait.
        let mut connected = false;
        let mut offset: i64 = 0;
        self.post_status(STATUS_CONNECTING, String::new());

        loop {
            let attempt = if webhook_cleared {
                let timeout = if connected {
                    self.options.poll_timeout
                } else {
                    Duration::ZERO
                };
                self.poll(&client, &mut offset, timeout)
                    .await
                    .map(|()| Step::Polled)
            } else {
                let cleared = self.clear_webhook(&client).await;
                webhook_cleared = cleared.is_ok();
                cleared.map(|()| Step::WebhookCleared)
            };

            // **Report nothing once stopped.** A cancelled request comes back
            // as an `Err`, and a `reload` is a stop followed by a start, so a
            // retiring worker writing here would put its last words over its
            // replacement's `connecting`.
            if self.cancel.is_cancelled() {
                return;
            }

            let reason = match attempt {
                // Not a poll: only a `getUpdates` that Telegram answered says
                // the bot can be polled, so the status stays where it is.
                Ok(Step::WebhookCleared) => continue,
                Ok(Step::Polled) => {
                    failures = 0;
                    if !connected {
                        connected = true;
                        self.post_status(STATUS_CONNECTED, String::new());
                    }
                    continue;
                }
                Err(reason) => reason,
            };

            connected = false;
            failures = failures.saturating_add(1);
            let status = if failures >= self.options.failure_threshold {
                STATUS_ERROR
            } else {
                STATUS_RECONNECTING
            };
            log::warn!(
                "telegram poll: integration_id={:?} attempt={failures} status={status}: {reason}",
                self.integration_id
            );
            self.post_status(status, reason);

            let wait = wait_after(failures, &self.options, jitter_ratio());
            let deadline = Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default();
            if sleep_until(deadline, &self.cancel).await.is_break() {
                return;
            }
        }
    }

    /// Record a transition. **Never awaited** — see `slack::socket::status_writer`.
    fn post_status(&self, status: &'static str, error: String) {
        let _ = self.status.send(StatusMsg { status, error });
    }

    /// Remove any webhook at Telegram, then clear what the row says about it.
    async fn clear_webhook(&self, client: &Client) -> Result<(), String> {
        let body = crate::native::gojson::to_vec_marshal(&DeleteWebhook {
            drop_pending_updates: false,
        })
        .map_err(|e| format!("encoding deleteWebhook: {e}"))?;
        let call = client.call(&self.cancel, "deleteWebhook", body);
        match tokio::time::timeout(self.options.request_grace, call).await {
            Err(_) => return Err("removing the webhook got no answer from Telegram in time".into()),
            Ok(Err(e)) => return Err(format!("removing the webhook before polling: {e}")),
            Ok(Ok(_)) => {}
        }

        let (path, id) = (self.db_path.clone(), self.integration_id.clone());
        let stored = db::blocking("telegram webhook clear", move || {
            clear_webhook_columns_blocking(&path, &id)
        })
        .await;
        match stored {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(format!("recording that the webhook was removed: {e}")),
            None => Err("recording that the webhook was removed: the write did not finish".into()),
        }
    }

    /// One `getUpdates`: dispatch what it returned and move the offset.
    async fn poll(
        &self,
        client: &Client,
        offset: &mut i64,
        timeout: Duration,
    ) -> Result<(), String> {
        let body = crate::native::gojson::to_vec_marshal(&GetUpdates {
            allowed_updates: ["message"],
            offset: *offset,
            timeout: timeout.as_secs(),
        })
        .map_err(|e| format!("encoding getUpdates: {e}"))?;

        let deadline = timeout.saturating_add(self.options.request_grace);
        let call = client.call(&self.cancel, "getUpdates", body);
        let response = match tokio::time::timeout(deadline, call).await {
            Err(_) => return Err("the poll got no answer from Telegram in time".into()),
            Ok(Err(e)) => return Err(describe_failure(&e)),
            Ok(Ok(response)) => response,
        };

        let batch = read_batch(response.result(), *offset)?;
        *offset = batch.next_offset;
        if batch.skipped > 0 {
            log::debug!(
                "telegram poll: integration_id={:?} skipped {} undecodable update(s)",
                self.integration_id,
                batch.skipped
            );
        }
        for update in batch.updates {
            // Spawned by the dispatcher, which claims the update through
            // `db::blocking` inside that spawn — nothing here waits on SQLite.
            crate::native::trigger::dispatcher::handle_update(
                &self.db_path,
                &self.integration_id,
                &self.bot_token,
                update,
            );
        }
        Ok(())
    }
}

/// What a successful attempt was.
enum Step {
    WebhookCleared,
    Polled,
}

/// What one `getUpdates` answer holds. Chat content, not a credential.
#[derive(Debug)]
struct Batch {
    updates: Vec<TelegramUpdate>,
    /// The offset to send next: one past the highest `update_id` seen, and
    /// never lower than the one this poll was made with.
    next_offset: i64,
    /// Elements that did not decode as an update.
    skipped: usize,
}

/// Decode `result` — the raw bytes of the envelope's `result` field.
///
/// Each element is decoded on its own through [`GoStruct`], the wrapper the
/// webhook's receiver uses, so the two transports accept the same shapes. An
/// element that fails is skipped, and its `update_id` is still read loosely so
/// the offset moves past it.
fn read_batch(result: &str, offset: i64) -> Result<Batch, String> {
    let elements: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(result)
        .map_err(|_| "getUpdates answered a result that is not a list of updates".to_string())?;

    let mut batch = Batch {
        updates: Vec::with_capacity(elements.len()),
        next_offset: offset,
        skipped: 0,
    };
    for element in &elements {
        match serde_json::from_str::<GoStruct<TelegramUpdate>>(element.get()) {
            Ok(GoStruct(update)) => {
                batch.next_offset = batch.next_offset.max(update.update_id.saturating_add(1));
                batch.updates.push(update);
            }
            Err(_) => {
                batch.skipped += 1;
                if let Some(id) = loose_update_id(element.get()) {
                    batch.next_offset = batch.next_offset.max(id.saturating_add(1));
                }
            }
        }
    }

    // Telegram will hand the same batch back at once, so polling again without
    // a wait would be a hot loop. Reported instead, and retried on the backoff.
    if !elements.is_empty() && batch.next_offset == offset {
        return Err(
            "getUpdates returned updates without a usable update_id, so the queue cannot advance"
                .to_string(),
        );
    }
    Ok(batch)
}

/// The `update_id` of an element that did not decode as an update, when it is
/// an object that carries an integer one.
fn loose_update_id(element: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(element)
        .ok()?
        .get("update_id")?
        .as_i64()
}

/// The sentence stored in `inbound_error` for a failed `getUpdates`.
///
/// `client::Client::call` never interpolates the URL, so none of its sentences
/// can carry the bot token and they pass through as they are. The one rewritten
/// is the 409: Telegram's `Conflict: …` does not tell a user that the cause is a
/// second Agento, or that turning inbound off and on removes a webhook.
fn describe_failure(error: &str) -> String {
    let Some(description) = error.strip_prefix("telegram API error: ") else {
        return error.to_string();
    };
    if !description.starts_with("Conflict") {
        return error.to_string();
    }
    if description.contains("webhook") {
        format!(
            "A webhook is set for this bot somewhere else, so Telegram refuses to be polled. \
             Turn inbound off and on again to remove it. Telegram said: {description}"
        )
    } else {
        format!(
            "Something else is polling this bot token: a second Agento, or another program \
             using the same bot. Only one can receive at a time. Telegram said: {description}"
        )
    }
}

/// The wait after the `failures`-th consecutive failure: the base wait after
/// the first, doubling to the cap, plus `slack::socket::backoff_for`'s jitter.
fn wait_after(failures: u32, options: &PollOptions, ratio: f64) -> Duration {
    backoff_for(
        failures.saturating_sub(1),
        options.base_backoff,
        options.max_backoff,
        ratio,
    )
}

/// `UPDATE integrations SET webhook_secret, webhook_status, webhook_error`, and
/// nothing else — `updated_at` tracks what the user wrote, as it does for the
/// inbound status columns.
fn clear_webhook_columns_blocking(db_path: &Path, integration_id: &str) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;
    conn.execute(
        "UPDATE integrations
            SET webhook_secret = '', webhook_status = 'inactive', webhook_error = ''
          WHERE id = ?1",
        [integration_id],
    )
    .map_err(|e| format!("clearing webhook info for {integration_id:?}: {e}"))?;
    Ok(())
}

/// Wait until a **wall-clock** instant, or until the worker is stopped.
/// `slack::socket::sleep_until`, over a `CancellationToken`. `Break` means the
/// worker was stopped and must return.
async fn sleep_until(
    deadline: DateTime<Utc>,
    cancel: &CancellationToken,
) -> std::ops::ControlFlow<()> {
    loop {
        let remaining = deadline - Utc::now();
        if remaining <= chrono::Duration::zero() {
            return std::ops::ControlFlow::Continue(());
        }
        let chunk = remaining.to_std().unwrap_or(SLEEP_CHUNK).min(SLEEP_CHUNK);
        tokio::select! {
            biased;
            () = cancel.cancelled() => return std::ops::ControlFlow::Break(()),
            () = tokio::time::sleep(chunk) => {}
        }
    }
}

#[cfg(test)]
#[path = "polling_tests.rs"]
mod tests;

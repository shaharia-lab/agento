//! Where a run's output leaves Agento: the post-run delivery step (#636, epic
//! #626).
//!
//! # The three rules
//!
//! **Never awaited under the permit.** [`dispatch`] `tokio::spawn`s and returns
//! `()` — not a future, not a handle — so the run that calls it cannot wait on a
//! slow or dead destination while it still holds the scheduler's semaphore
//! permit and its `RunGuard`. That is [`super::executor`]'s `publish` rule, and
//! the reason is the same: a hung Slack post must not throttle the scheduler.
//! It is an async task rather than a `spawn_blocking` one because a post is
//! async `reqwest`; every database touch inside it goes through
//! [`db::blocking`] (#366), and a future *blocking* destination wraps its own
//! send in `spawn_blocking` inside its arm.
//!
//! **Never on the run's status.** A delivery's outcome is written to
//! `job_deliveries` through `tasks::insert_pending_delivery` and
//! `tasks::finish_delivery` alone, so a failed post leaves `job_history.status`
//! and `error_message` exactly as the run wrote them.
//!
//! **Every target ends in a row.** One row per channel, chat or email
//! destination (an email is one message to all its recipients), written
//! `pending` before the attempt and finished after it. A destination whose
//! `when` is not met is finished `skipped` with the reason rather than left
//! out, so the Jobs view lists every configured destination; a post lost to the
//! app quitting keeps its `pending` row, which the next startup's
//! `Scheduler::reap_pending_deliveries` fails as interrupted (#635).
//!
//! # Adding a destination type
//!
//! One [`Destination`] variant, one arm in [`Destination::from_config`],
//! [`Destination::targets`] and [`Destination::deliver_one`]. The executor does
//! not change: it only builds a [`DeliveryReport`] and calls [`dispatch`].
//!
//! # Reply to sender (#682)
//!
//! A `reply` entry has no sub-object: its target is the run's, not the
//! configuration's — [`DeliveryReport::reply_to`], the Telegram chat or Slack
//! thread the triggering event came from. A run nothing sent (a schedule, a
//! manual run) has no origin, and its row finishes `skipped` with
//! [`SKIPPED_NO_ORIGIN`]. The sender is outside Agento, so a failed run answers
//! the fixed `ERROR_REPLY` and never the run's error text; an empty answer is
//! `NO_RESPONSE_REPLY` — the two sentences the inline replies send today.

use std::path::{Path, PathBuf};

use crate::native::db;
use crate::native::gotime::GoTime;
use crate::native::tasks::{
    self, EmailDestination, JobDelivery, SlackDestination, TaskDestination, TelegramDestination,
    DELIVERY_FAILED, DELIVERY_PENDING, DELIVERY_SENT, DELIVERY_SKIPPED,
};

/// Everything a destination may say about one run, owned, because it outlives
/// the run that built it.
///
/// `answer` is the agent's reply itself, not `job_history.response_text`: the
/// answer is delivered even when `save_output` is off and the job row stores
/// `""`. `save_output` decides what Agento *keeps*; a destination is where the
/// user asked the answer to go.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryReport {
    pub task_id: String,
    pub task_name: String,
    pub job_id: String,
    pub chat_session_id: String,
    /// `success` or `failed` — the run's, never a delivery's.
    pub status: String,
    pub duration_ms: i64,
    pub model: String,
    /// Empty on a failed run.
    pub answer: String,
    /// The run's error on a failed run; `None` on a successful one.
    pub error: Option<String>,
    /// Where the event that started the run came from, for a `reply`
    /// destination; `None` for a schedule or manual run.
    pub reply_to: Option<ReplyTarget>,
}

/// The chat or thread a run's triggering message came from (#682).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyTarget {
    /// The first chunk of the reply quotes `message_id`.
    Telegram {
        integration_id: String,
        chat_id: i64,
        message_id: i64,
    },
    Slack {
        integration_id: String,
        channel: String,
        thread_ts: String,
    },
}

impl DeliveryReport {
    fn run_ok(&self) -> bool {
        self.status == "success"
    }
}

/// How one target's delivery ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    Failed(String),
    Skipped(String),
}

impl Outcome {
    fn status_and_error(&self) -> (&'static str, &str) {
        match self {
            Self::Sent => (DELIVERY_SENT, ""),
            Self::Failed(e) => (DELIVERY_FAILED, e),
            Self::Skipped(reason) => (DELIVERY_SKIPPED, reason),
        }
    }
}

/// The reason a `success`-only destination records on a failed run.
pub const SKIPPED_RUN_FAILED: &str = "run failed; this destination delivers on success only";

/// What a `reply` destination records on a run no message started.
pub const SKIPPED_NO_ORIGIN: &str = "this run was not started by a message";

/// What a stored entry whose `type` this build does not know records.
const SKIPPED_UNKNOWN_TYPE: &str = "unknown destination type";

/// A stored [`TaskDestination`], resolved to the type that delivers it.
#[derive(Debug, Clone)]
enum Destination {
    Slack(SlackDestination),
    Telegram(TelegramDestination),
    Email(EmailDestination),
    Reply,
    /// Test-only. `type` `fake` sends, `fake-fail` fails, `fake-hang` never
    /// finishes; each records the report it was handed (see [`fake_received`]).
    #[cfg(any(test, feature = "test-hooks"))]
    Fake(FakeMode),
}

#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, Copy)]
enum FakeMode {
    Send,
    Fail,
    Hang,
}

impl Destination {
    /// `None` for a `type` this build does not know — a row written by a newer
    /// build, or a hand edit — which is recorded as skipped rather than dropped.
    fn from_config(config: &TaskDestination) -> Option<Self> {
        match config.r#type.as_str() {
            "slack" => Some(Self::Slack(
                config.slack.as_deref().cloned().unwrap_or_default(),
            )),
            "telegram" => Some(Self::Telegram(
                config.telegram.as_deref().cloned().unwrap_or_default(),
            )),
            "email" => Some(Self::Email(
                config.email.as_deref().cloned().unwrap_or_default(),
            )),
            "reply" => Some(Self::Reply),
            #[cfg(any(test, feature = "test-hooks"))]
            "fake" => Some(Self::Fake(FakeMode::Send)),
            #[cfg(any(test, feature = "test-hooks"))]
            "fake-fail" => Some(Self::Fake(FakeMode::Fail)),
            #[cfg(any(test, feature = "test-hooks"))]
            "fake-hang" => Some(Self::Fake(FakeMode::Hang)),
            _ => None,
        }
    }

    /// One entry per channel or recipient, each of which gets its own row.
    ///
    /// The Slack target is the bare channel id; the Telegram one the chat id.
    /// An email destination is **one** target, its recipients joined by `, `:
    /// it sends one message to all of them (#640), so it gets one row. A reply
    /// is one row for the run's origin — the chat id, or `<channel> · <ts>` —
    /// and one row with an empty target when there is none.
    fn targets(&self, report: &DeliveryReport) -> Vec<String> {
        match self {
            Self::Slack(slack) => slack.channel_ids.iter().cloned().collect(),
            Self::Telegram(telegram) => telegram.chat_ids.iter().cloned().collect(),
            Self::Email(email) => vec![email.recipients.join(", ")],
            Self::Reply => vec![match &report.reply_to {
                Some(ReplyTarget::Telegram { chat_id, .. }) => chat_id.to_string(),
                Some(ReplyTarget::Slack {
                    channel, thread_ts, ..
                }) => format!("{channel} · {thread_ts}"),
                None => String::new(),
            }],
            #[cfg(any(test, feature = "test-hooks"))]
            Self::Fake(_) => vec!["fake".to_string()],
        }
    }

    /// `slack_thread_mapped` is shared by every target of one run: the first
    /// Slack summary that posts maps its thread to the run's chat, and nothing
    /// after it tries (#642).
    async fn deliver_one(
        &self,
        db_path: &Path,
        target: &str,
        report: &DeliveryReport,
        slack_thread_mapped: &mut bool,
    ) -> Outcome {
        match self {
            Self::Slack(slack) => {
                deliver_slack(
                    db_path,
                    &slack.integration_id,
                    target,
                    report,
                    slack_thread_mapped,
                )
                .await
            }
            Self::Telegram(telegram) => {
                deliver_telegram(db_path, &telegram.integration_id, target, report).await
            }
            Self::Email(email) => deliver_email(db_path, &email.recipients, report).await,
            Self::Reply => deliver_reply(db_path, report).await,
            #[cfg(any(test, feature = "test-hooks"))]
            Self::Fake(mode) => {
                fake_record(report);
                match mode {
                    FakeMode::Send => Outcome::Sent,
                    FakeMode::Fail => Outcome::Failed("fake destination failed".to_string()),
                    FakeMode::Hang => {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        Outcome::Sent
                    }
                }
            }
        }
    }
}

/// One Slack channel (#637): the token through `registry::slack_delivery_token`,
/// then `slack::delivery`'s summary and thread. An integration that cannot post
/// — deleted, not Slack, disabled, not connected, no token — is `skipped` with
/// the reason, before any network call.
///
/// Until `thread_mapped` is set, the channel is offered the mapping of its
/// summary's thread to the run's chat, so an `@app` reply there continues the
/// session (#642). It is set once a summary has posted and the insert was
/// attempted, whether or not the insert succeeded — `inbound_threads` holds one
/// thread per chat. A run with no chat has nothing to continue and maps nothing.
async fn deliver_slack(
    db_path: &Path,
    integration_id: &str,
    channel: &str,
    report: &DeliveryReport,
    thread_mapped: &mut bool,
) -> Outcome {
    use crate::native::integrations::{registry, slack::delivery};

    let path = db_path.to_path_buf();
    let id = integration_id.to_string();
    let token = match db::blocking("slack delivery token", move || {
        registry::slack_delivery_token(&path, &id)
    })
    .await
    {
        Some(Ok(token)) => token,
        Some(Err(reason)) => return Outcome::Skipped(reason.to_string()),
        None => return Outcome::Failed("could not read the Slack integration".to_string()),
    };
    let run = delivery::RunSummary {
        task_name: report.task_name.clone(),
        succeeded: report.run_ok(),
        duration_ms: report.duration_ms,
        job_id: report.job_id.clone(),
        body: if report.run_ok() {
            report.answer.clone()
        } else {
            report.error.clone().unwrap_or_default()
        },
    };
    let mut mapping =
        (!*thread_mapped && !report.chat_session_id.is_empty()).then(|| delivery::ThreadMapping {
            db_path: db_path.to_path_buf(),
            integration_id: integration_id.to_string(),
            chat_id: report.chat_session_id.clone(),
        });
    let offered = mapping.is_some();
    let outcome = delivery::deliver_channel(&token, channel, &run, &mut mapping).await;
    if offered && mapping.is_none() {
        *thread_mapped = true;
    }
    match outcome {
        delivery::ChannelOutcome::Sent { .. } => Outcome::Sent,
        delivery::ChannelOutcome::Failed(e) => Outcome::Failed(e),
    }
}

/// One Telegram chat (#639): the token through
/// `receiver::telegram_delivery_token`, then `telegram::delivery`'s header and
/// output. An integration that cannot send — deleted, not Telegram, disabled,
/// no bot token — is `skipped` with the reason, before any network call.
async fn deliver_telegram(
    db_path: &Path,
    integration_id: &str,
    chat: &str,
    report: &DeliveryReport,
) -> Outcome {
    use crate::native::integrations::telegram::delivery;
    use crate::native::trigger::receiver;

    // Validation admits numeric ids only, so this refuses a hand-edited row.
    let Ok(chat_id) = chat.parse::<i64>() else {
        return Outcome::Failed(format!(
            "chat id {chat:?} is not a numeric Telegram chat id"
        ));
    };
    let path = db_path.to_path_buf();
    let id = integration_id.to_string();
    let token = match db::blocking("telegram delivery token", move || {
        receiver::telegram_delivery_token(&path, &id)
    })
    .await
    {
        Some(Ok(token)) => token,
        Some(Err(reason)) => return Outcome::Skipped(reason.to_string()),
        None => return Outcome::Failed("could not read the Telegram integration".to_string()),
    };
    let run = delivery::RunSummary {
        task_name: report.task_name.clone(),
        succeeded: report.run_ok(),
        duration_ms: report.duration_ms,
        body: if report.run_ok() {
            report.answer.clone()
        } else {
            report.error.clone().unwrap_or_default()
        },
    };
    match delivery::deliver_chat(&token, chat_id, &run).await {
        Ok(()) => Outcome::Sent,
        Err(e) => Outcome::Failed(e),
    }
}

/// One email destination (#640): every recipient on one message, through the
/// SMTP provider from Settings → Notifications. No provider is `skipped` with a
/// pointer to it. The send is lettre's blocking transport, so the whole arm runs
/// on the blocking pool.
async fn deliver_email(db_path: &Path, recipients: &[String], report: &DeliveryReport) -> Outcome {
    use crate::native::notifications::delivery::{self, EmailOutcome};

    let path = db_path.to_path_buf();
    let recipients = recipients.to_vec();
    let run = delivery::RunSummary {
        task_name: report.task_name.clone(),
        succeeded: report.run_ok(),
        duration_ms: report.duration_ms,
        model: report.model.clone(),
        chat_session_id: report.chat_session_id.clone(),
        body: if report.run_ok() {
            report.answer.clone()
        } else {
            report.error.clone().unwrap_or_default()
        },
    };
    match db::blocking("email delivery", move || {
        delivery::deliver(&path, &recipients, &run)
    })
    .await
    {
        Some(EmailOutcome::Sent) => Outcome::Sent,
        Some(EmailOutcome::Skipped(reason)) => Outcome::Skipped(reason),
        Some(EmailOutcome::Failed(e)) => Outcome::Failed(e),
        None => Outcome::Failed("the email send did not finish".to_string()),
    }
}

/// The answer to the sender who started the run (#682), on the integration the
/// event arrived through. An integration that cannot send is `skipped` with the
/// reason before any network call, as the configured arms are.
async fn deliver_reply(db_path: &Path, report: &DeliveryReport) -> Outcome {
    use crate::native::integrations::{registry, slack, telegram};
    use crate::native::trigger::{receiver, telegram_api};

    let text = reply_text(report);
    match &report.reply_to {
        None => Outcome::Skipped(SKIPPED_NO_ORIGIN.to_string()),
        Some(ReplyTarget::Telegram {
            integration_id,
            chat_id,
            message_id,
        }) => {
            let (path, id) = (db_path.to_path_buf(), integration_id.clone());
            let token = match db::blocking("telegram reply token", move || {
                receiver::telegram_delivery_token(&path, &id)
            })
            .await
            {
                Some(Ok(token)) => token,
                Some(Err(reason)) => return Outcome::Skipped(reason.to_string()),
                None => {
                    return Outcome::Failed("could not read the Telegram integration".to_string())
                }
            };
            match telegram_api::send_reply(&token, *chat_id, *message_id, text).await {
                Ok(()) => Outcome::Sent,
                Err(e) => Outcome::Failed(telegram::delivery::readable(&e, *chat_id)),
            }
        }
        Some(ReplyTarget::Slack {
            integration_id,
            channel,
            thread_ts,
        }) => {
            let (path, id) = (db_path.to_path_buf(), integration_id.clone());
            let token = match db::blocking("slack reply token", move || {
                registry::slack_delivery_token(&path, &id)
            })
            .await
            {
                Some(Ok(token)) => token,
                Some(Err(reason)) => return Outcome::Skipped(reason.to_string()),
                None => return Outcome::Failed("could not read the Slack integration".to_string()),
            };
            match slack::delivery::deliver_thread(&token, channel, thread_ts, text).await {
                Ok(()) => Outcome::Sent,
                Err(e) => Outcome::Failed(e),
            }
        }
    }
}

/// What the sender reads: the answer, `NO_RESPONSE_REPLY` for an empty one,
/// and `ERROR_REPLY` for a failed run — never `report.error`, which is
/// written for the task's owner, not for whoever sent the message.
fn reply_text(report: &DeliveryReport) -> &str {
    use crate::native::trigger::dispatcher::{ERROR_REPLY, NO_RESPONSE_REPLY};

    if !report.run_ok() {
        ERROR_REPLY
    } else if report.answer.is_empty() {
        NO_RESPONSE_REPLY
    } else {
        &report.answer
    }
}

/// Whether a destination with this `when` delivers for a run that ended
/// `run_ok`. `always` always does; `success` — and an empty value, which
/// validation stores as `success` — only for a successful run.
fn should_deliver(when: &str, run_ok: bool) -> bool {
    when == "always" || run_ok
}

/// Deliver `report` to each of `destinations`, off the caller's task.
///
/// Returns `()` so it cannot be awaited: see the module header. A task with no
/// destinations spawns nothing and writes nothing. Targets are delivered
/// sequentially — a run has a handful, each bounded by its client's timeout.
pub fn dispatch(db_path: &Path, destinations: Vec<TaskDestination>, report: DeliveryReport) {
    if destinations.is_empty() {
        return;
    }
    let db_path = db_path.to_path_buf();
    tokio::spawn(async move {
        deliver_all(&db_path, &destinations, &report).await;
    });
}

async fn deliver_all(db_path: &Path, destinations: &[TaskDestination], report: &DeliveryReport) {
    // In destination order, then channel order: the first Slack summary that
    // posts is the run's one continuable thread.
    let mut slack_thread_mapped = false;
    for (position, config) in destinations.iter().enumerate() {
        let destination = Destination::from_config(config);
        let targets = destination
            .as_ref()
            .map_or_else(|| vec![String::new()], |d| d.targets(report));
        for target in targets {
            let Some(id) = record_pending(db_path, position, config, &target, report).await else {
                // The row could not be written, so there is nothing to finish —
                // and a post nobody can see the outcome of is not attempted.
                continue;
            };
            let outcome = match &destination {
                None => Outcome::Skipped(SKIPPED_UNKNOWN_TYPE.to_string()),
                Some(_) if !should_deliver(&config.when, report.run_ok()) => {
                    Outcome::Skipped(SKIPPED_RUN_FAILED.to_string())
                }
                Some(destination) => {
                    destination
                        .deliver_one(db_path, &target, report, &mut slack_thread_mapped)
                        .await
                }
            };
            record_outcome(db_path, id, outcome).await;
        }
    }
}

async fn record_pending(
    db_path: &Path,
    position: usize,
    config: &TaskDestination,
    target: &str,
    report: &DeliveryReport,
) -> Option<String> {
    let delivery = JobDelivery {
        id: uuid::Uuid::new_v4().to_string(),
        job_id: report.job_id.clone(),
        position: i64::try_from(position).unwrap_or(i64::MAX),
        r#type: config.r#type.clone(),
        target: target.to_string(),
        status: DELIVERY_PENDING.to_string(),
        error: String::new(),
        created_at: GoTime::from_utc(chrono::Utc::now()),
        finished_at: None,
    };
    let db_path: PathBuf = db_path.to_path_buf();
    db::blocking(
        "delivery record",
        move || match tasks::insert_pending_delivery(&db_path, &delivery) {
            Ok(()) => Some(delivery.id),
            Err(e) => {
                log::error!(
                    "failed to record delivery job_id={:?} error={e}",
                    delivery.job_id
                );
                None
            }
        },
    )
    .await
    .flatten()
}

async fn record_outcome(db_path: &Path, id: String, outcome: Outcome) {
    if let Outcome::Failed(e) = &outcome {
        log::warn!("delivery failed delivery_id={id:?} error={e}");
    }
    let db_path = db_path.to_path_buf();
    db::blocking("delivery result", move || {
        let (status, error) = outcome.status_and_error();
        if let Err(e) = tasks::finish_delivery(&db_path, &id, status, error) {
            log::error!("failed to finish delivery delivery_id={id:?} error={e}");
        }
    })
    .await;
}

#[cfg(any(test, feature = "test-hooks"))]
fn fake_log() -> &'static std::sync::Mutex<Vec<DeliveryReport>> {
    static LOG: std::sync::OnceLock<std::sync::Mutex<Vec<DeliveryReport>>> =
        std::sync::OnceLock::new();
    LOG.get_or_init(Default::default)
}

#[cfg(any(test, feature = "test-hooks"))]
fn fake_record(report: &DeliveryReport) {
    fake_log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(report.clone());
}

/// Every report a Fake destination was handed for `job_id`. Process-wide, so
/// callers key on a job id no other test uses.
#[cfg(any(test, feature = "test-hooks"))]
pub fn fake_received(job_id: &str) -> Vec<DeliveryReport> {
    fake_log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|r| r.job_id == job_id)
        .cloned()
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn fake(r#type: &str, when: &str) -> TaskDestination {
        TaskDestination {
            r#type: r#type.to_string(),
            when: when.to_string(),
            slack: None,
            telegram: None,
            email: None,
        }
    }

    /// A migrated database with task `t1` and one finished job row, `job_id`.
    fn with_job(job_id: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute_batch(&format!(
            "INSERT INTO scheduled_tasks (id, name, prompt, created_at, updated_at)
             VALUES ('t1', 'T', 'p', '2026-01-01 00:00:00 +0000 UTC',
                     '2026-01-01 00:00:00 +0000 UTC');
             INSERT INTO job_history (id, task_id, task_name, status, started_at)
             VALUES ('{job_id}', 't1', 'T', 'success', '2026-01-01 00:00:00 +0000 UTC');"
        ))
        .expect("seed");
        file
    }

    /// `(position, type, target, status, error)` in read order.
    pub(crate) fn rows(path: &Path, job_id: &str) -> Vec<(i64, String, String, String, String)> {
        let conn = rusqlite::Connection::open(path).expect("open");
        let mut stmt = conn
            .prepare(
                "SELECT position, type, target, status, error FROM job_deliveries
                 WHERE job_id = ?1 ORDER BY position, created_at, id",
            )
            .expect("prepare");
        let rows = stmt
            .query_map([job_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .expect("query");
        rows.map(|r| r.expect("row")).collect()
    }

    fn report(job_id: &str, status: &str) -> DeliveryReport {
        DeliveryReport {
            task_id: "t1".into(),
            job_id: job_id.into(),
            status: status.into(),
            answer: "the answer".into(),
            ..Default::default()
        }
    }

    #[test]
    fn should_deliver_truth_table() {
        assert!(should_deliver("always", true));
        assert!(should_deliver("always", false));
        assert!(should_deliver("success", true));
        assert!(!should_deliver("success", false));
        assert!(should_deliver("", true));
        assert!(!should_deliver("", false));
    }

    #[tokio::test]
    async fn every_target_gets_one_finished_row() {
        let file = with_job("j-targets");
        let slack = TaskDestination {
            r#type: "slack".into(),
            when: "success".into(),
            slack: Some(crate::native::gojson::GoStruct(SlackDestination {
                integration_id: "i1".into(),
                channel_ids: crate::native::gojson::GoList(vec![
                    "C0123ABCD".into(),
                    "C0456EFGH".into(),
                ]),
            })),
            telegram: None,
            email: None,
        };
        deliver_all(
            file.path(),
            &[fake("fake", "always"), slack],
            &report("j-targets", "success"),
        )
        .await;

        assert_eq!(
            rows(file.path(), "j-targets"),
            vec![
                (
                    0,
                    "fake".into(),
                    "fake".into(),
                    "sent".into(),
                    String::new()
                ),
                (
                    1,
                    "slack".into(),
                    "C0123ABCD".into(),
                    "skipped".into(),
                    "the Slack integration was deleted".into()
                ),
                (
                    1,
                    "slack".into(),
                    "C0456EFGH".into(),
                    "skipped".into(),
                    "the Slack integration was deleted".into()
                ),
            ]
        );
    }

    /// A Slack integration that cannot post is `skipped` with its reason before
    /// any request: the API base is never redirected here, so a post would
    /// reach the real Slack and fail rather than skip (#637).
    #[tokio::test]
    async fn a_disabled_slack_integration_is_skipped_with_its_reason() {
        let file = with_job("j-slack-off");
        rusqlite::Connection::open(file.path())
            .expect("open")
            .execute_batch(
                r#"INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                             created_at, updated_at)
                   VALUES ('s-off', 's-off', 'slack', 0,
                           '{"auth_mode":"bot_token","bot_token":"xoxb-off"}', '{}', '{}',
                           '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC');"#,
            )
            .expect("seed");
        let slack = TaskDestination {
            r#type: "slack".into(),
            when: "always".into(),
            slack: Some(crate::native::gojson::GoStruct(SlackDestination {
                integration_id: "s-off".into(),
                channel_ids: crate::native::gojson::GoList(vec!["C0123ABCD".into()]),
            })),
            telegram: None,
            email: None,
        };
        deliver_all(file.path(), &[slack], &report("j-slack-off", "success")).await;
        assert_eq!(
            rows(file.path(), "j-slack-off"),
            vec![(
                0,
                "slack".into(),
                "C0123ABCD".into(),
                "skipped".into(),
                "the Slack integration is disabled".into()
            )]
        );
    }

    #[tokio::test]
    async fn an_unmet_when_is_recorded_as_skipped_and_never_delivered() {
        let file = with_job("j-unmet");
        deliver_all(
            file.path(),
            &[fake("fake", "success"), fake("fake", "always")],
            &report("j-unmet", "failed"),
        )
        .await;

        let rows = rows(file.path(), "j-unmet");
        assert_eq!(rows[0].3, "skipped");
        assert_eq!(rows[0].4, SKIPPED_RUN_FAILED);
        assert_eq!(rows[1].3, "sent");
        assert_eq!(
            fake_received("j-unmet").len(),
            1,
            "only the `always` destination was handed the report"
        );
    }

    #[tokio::test]
    async fn a_failed_delivery_is_recorded_with_its_text() {
        let file = with_job("j-fail");
        deliver_all(
            file.path(),
            &[fake("fake-fail", "success")],
            &report("j-fail", "success"),
        )
        .await;
        assert_eq!(
            rows(file.path(), "j-fail"),
            vec![(
                0,
                "fake-fail".into(),
                "fake".into(),
                "failed".into(),
                "fake destination failed".into()
            )]
        );
    }

    #[tokio::test]
    async fn an_unknown_type_is_skipped_rather_than_dropped() {
        let file = with_job("j-unknown");
        deliver_all(
            file.path(),
            &[fake("pigeon", "always")],
            &report("j-unknown", "success"),
        )
        .await;
        assert_eq!(
            rows(file.path(), "j-unknown"),
            vec![(
                0,
                "pigeon".into(),
                String::new(),
                "skipped".into(),
                SKIPPED_UNKNOWN_TYPE.into()
            )]
        );
    }

    /// One row per email destination, not per recipient — it is one message —
    /// and no SMTP provider is `skipped` with the pointer to set one up (#640).
    #[tokio::test]
    async fn an_email_destination_without_smtp_is_one_skipped_row() {
        let file = with_job("j-email");
        let email = TaskDestination {
            r#type: "email".into(),
            when: "always".into(),
            slack: None,
            telegram: None,
            email: Some(crate::native::gojson::GoStruct(EmailDestination {
                recipients: crate::native::gojson::GoList(vec![
                    "a@example.com".into(),
                    "b@example.com".into(),
                ]),
            })),
        };
        deliver_all(file.path(), &[email], &report("j-email", "success")).await;
        assert_eq!(
            rows(file.path(), "j-email"),
            vec![(
                0,
                "email".into(),
                "a@example.com, b@example.com".into(),
                "skipped".into(),
                crate::native::notifications::delivery::SKIPPED_NO_SMTP.into()
            )]
        );
    }

    fn telegram(integration_id: &str, chats: &[&str]) -> TaskDestination {
        TaskDestination {
            r#type: "telegram".into(),
            when: "always".into(),
            slack: None,
            telegram: Some(crate::native::gojson::GoStruct(TelegramDestination {
                integration_id: integration_id.into(),
                chat_ids: crate::native::gojson::GoList(
                    chats.iter().map(ToString::to_string).collect(),
                ),
            })),
            email: None,
        }
    }

    fn seed_telegram(path: &Path, id: &str, enabled: bool) {
        rusqlite::Connection::open(path)
            .expect("open")
            .execute(
                r#"INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                             created_at, updated_at)
                   VALUES (?1, ?1, 'telegram', ?2, '{"bot_token":"123:TG-DELIVERY-SECRET"}', '{}',
                           '{}', '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')"#,
                rusqlite::params![id, enabled],
            )
            .expect("seed");
    }

    /// A Telegram integration that cannot send is `skipped` with its reason
    /// before any request, one row per chat (#639).
    #[tokio::test]
    async fn a_deleted_or_disabled_telegram_integration_is_skipped() {
        let file = with_job("j-tg-off");
        seed_telegram(file.path(), "tg-off", false);
        deliver_all(
            file.path(),
            &[
                telegram("tg-off", &["42"]),
                telegram("tg-gone", &["-100", "7"]),
            ],
            &report("j-tg-off", "success"),
        )
        .await;
        let skipped = |position, chat: &str, why: &str| {
            (
                position,
                "telegram".to_string(),
                chat.to_string(),
                "skipped".to_string(),
                why.to_string(),
            )
        };
        assert_eq!(
            rows(file.path(), "j-tg-off"),
            vec![
                skipped(0, "42", "the Telegram integration is disabled"),
                skipped(1, "-100", "the Telegram integration was deleted"),
                skipped(1, "7", "the Telegram integration was deleted"),
            ]
        );
    }

    /// End to end through the dispatcher: each chat gets its row, a refusal
    /// names its chat, the token is in no row, and the run's own row is not
    /// touched (#639).
    #[tokio::test]
    async fn a_telegram_destination_records_one_row_per_chat() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};

        let _guard = api_base_lock().await;
        let app = axum::Router::new().fallback(|body: String| async move {
            if body.contains(r#""chat_id":-404"#) {
                r#"{"ok":false,"description":"Bad Request: chat not found"}"#
            } else {
                r#"{"ok":true}"#
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));

        let file = with_job("j-tg");
        seed_telegram(file.path(), "tg-1", true);
        deliver_all(
            file.path(),
            &[telegram("tg-1", &["42", "-404"])],
            &report("j-tg", "success"),
        )
        .await;
        set_api_base(None);

        let rows = rows(file.path(), "j-tg");
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].2.as_str(), rows[0].3.as_str()), ("42", "sent"));
        assert_eq!((rows[1].2.as_str(), rows[1].3.as_str()), ("-404", "failed"));
        assert!(
            rows[1].4.starts_with("chat -404 not found"),
            "{}",
            rows[1].4
        );
        assert!(rows.iter().all(|r| !r.4.contains("TG-DELIVERY-SECRET")));
        let status: String = rusqlite::Connection::open(file.path())
            .expect("open")
            .query_row(
                "SELECT status FROM job_history WHERE id = 'j-tg'",
                [],
                |r| r.get(0),
            )
            .expect("job row");
        assert_eq!(status, "success", "a failed delivery never touches the run");
    }

    // ─── Reply to sender (#682) ─────────────────────────────────────────────

    fn reply(when: &str) -> TaskDestination {
        fake("reply", when)
    }

    fn from_telegram(job_id: &str, status: &str, chat_id: i64) -> DeliveryReport {
        DeliveryReport {
            reply_to: Some(ReplyTarget::Telegram {
                integration_id: "tg-1".into(),
                chat_id,
                message_id: 7,
            }),
            ..report(job_id, status)
        }
    }

    fn job_status(path: &Path, job_id: &str) -> (String, String) {
        rusqlite::Connection::open(path)
            .expect("open")
            .query_row(
                "SELECT status, COALESCE(error_message, '') FROM job_history WHERE id = ?1",
                [job_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("job row")
    }

    /// A fake Telegram that answers `chat not found` for chat -404 and `ok`
    /// otherwise, keeping every request body.
    async fn fake_telegram() -> std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> {
        use crate::native::integrations::telegram::client::set_api_base;

        let seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> = Default::default();
        let log = seen.clone();
        let app = axum::Router::new().fallback(move |body: String| {
            let log = log.clone();
            async move {
                let payload: serde_json::Value = serde_json::from_str(&body).expect("JSON");
                let missing = payload["chat_id"] == -404;
                log.lock().expect("lock").push(payload);
                if missing {
                    r#"{"ok":false,"description":"Bad Request: chat not found"}"#
                } else {
                    r#"{"ok":true}"#
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));
        seen
    }

    #[test]
    fn the_sender_reads_the_answer_or_one_fixed_sentence() {
        use crate::native::trigger::dispatcher::{ERROR_REPLY, NO_RESPONSE_REPLY};

        assert_eq!(reply_text(&report("j", "success")), "the answer");
        let empty = DeliveryReport {
            answer: String::new(),
            ..report("j", "success")
        };
        assert_eq!(reply_text(&empty), NO_RESPONSE_REPLY);
        let failed = DeliveryReport {
            answer: String::new(),
            error: Some("claude exited 1: /home/u/secret".into()),
            ..report("j", "failed")
        };
        assert_eq!(reply_text(&failed), ERROR_REPLY);
    }

    #[tokio::test]
    async fn a_reply_without_an_origin_is_skipped_with_the_reason() {
        let file = with_job("j-reply-none");
        deliver_all(
            file.path(),
            &[reply("always")],
            &report("j-reply-none", "success"),
        )
        .await;
        assert_eq!(
            rows(file.path(), "j-reply-none"),
            vec![(
                0,
                "reply".into(),
                String::new(),
                "skipped".into(),
                SKIPPED_NO_ORIGIN.into()
            )]
        );
    }

    /// The reply quotes the message the run came from, carries the answer,
    /// and records the chat as its target.
    #[tokio::test]
    async fn a_telegram_reply_answers_the_message_it_came_from() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};

        let _guard = api_base_lock().await;
        let seen = fake_telegram().await;
        let file = with_job("j-reply-tg");
        seed_telegram(file.path(), "tg-1", true);
        deliver_all(
            file.path(),
            &[reply("success")],
            &from_telegram("j-reply-tg", "success", 42),
        )
        .await;
        set_api_base(None);

        assert_eq!(
            rows(file.path(), "j-reply-tg"),
            vec![(0, "reply".into(), "42".into(), "sent".into(), String::new())]
        );
        assert_eq!(
            *seen.lock().expect("lock"),
            vec![serde_json::json!({
                "chat_id": 42, "reply_to_message_id": 7, "text": "the answer"
            })]
        );
    }

    /// The run's error is written for the task's owner; the sender is outside
    /// Agento and gets `ERROR_REPLY`, the sentence the inline reply sends.
    #[tokio::test]
    async fn a_failed_run_replies_with_the_fixed_sentence_never_its_error() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};
        use crate::native::trigger::dispatcher::ERROR_REPLY;

        let _guard = api_base_lock().await;
        let seen = fake_telegram().await;
        let file = with_job("j-reply-err");
        seed_telegram(file.path(), "tg-1", true);
        let failed = DeliveryReport {
            answer: String::new(),
            error: Some("claude exited 1: /home/u/INTERNAL-DETAIL".into()),
            ..from_telegram("j-reply-err", "failed", 42)
        };
        // Two entries only to show both `when`s; validation admits one.
        deliver_all(file.path(), &[reply("success"), reply("always")], &failed).await;
        set_api_base(None);

        let rows = rows(file.path(), "j-reply-err");
        assert_eq!(
            (rows[0].3.as_str(), rows[0].4.as_str()),
            ("skipped", SKIPPED_RUN_FAILED),
            "`success` is not met by a failed run"
        );
        assert_eq!(rows[1].3, "sent");
        let seen = seen.lock().expect("lock");
        assert_eq!(seen.len(), 1, "only the `always` reply was sent");
        assert_eq!(seen[0]["text"], ERROR_REPLY);
        assert!(!seen[0].to_string().contains("INTERNAL-DETAIL"));
    }

    /// A refused reply is the delivery's failure, in a sentence naming the
    /// chat, and the run's own row keeps what the run wrote.
    #[tokio::test]
    async fn a_failed_reply_is_recorded_and_never_fails_the_run() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};

        let _guard = api_base_lock().await;
        let _seen = fake_telegram().await;
        let file = with_job("j-reply-fail");
        seed_telegram(file.path(), "tg-1", true);
        deliver_all(
            file.path(),
            &[reply("always")],
            &from_telegram("j-reply-fail", "success", -404),
        )
        .await;
        set_api_base(None);

        let rows = rows(file.path(), "j-reply-fail");
        assert_eq!((rows[0].2.as_str(), rows[0].3.as_str()), ("-404", "failed"));
        assert!(
            rows[0].4.starts_with("chat -404 not found"),
            "{}",
            rows[0].4
        );
        assert!(!rows[0].4.contains("TG-DELIVERY-SECRET"));
        assert_eq!(
            job_status(file.path(), "j-reply-fail"),
            ("success".into(), String::new())
        );
    }

    /// A Slack origin is answered in its thread, with no summary, and the row
    /// names the channel and the thread; an integration that cannot post is
    /// `skipped` before any request.
    #[tokio::test]
    async fn a_slack_reply_posts_into_the_origin_thread() {
        use crate::native::integrations::slack::client::{api_base_lock, set_api_base};

        let _guard = api_base_lock().await;
        let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>> =
            Default::default();
        let log = seen.clone();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri, body: String| {
            let log = log.clone();
            async move {
                let payload: serde_json::Value = serde_json::from_str(&body).expect("JSON");
                log.lock()
                    .expect("lock")
                    .push((uri.path().to_string(), payload));
                r#"{"ok":true,"ts":"1700000000.000900"}"#
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));

        let file = with_job("j-reply-slack");
        rusqlite::Connection::open(file.path())
            .expect("open")
            .execute_batch(
                r#"INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                             created_at, updated_at)
                   VALUES ('s-1', 's-1', 'slack', 1,
                           '{"auth_mode":"bot_token","bot_token":"xoxb-reply"}', '{}', '{}',
                           '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC');"#,
            )
            .expect("seed");
        let origin = |integration_id: &str| {
            Some(ReplyTarget::Slack {
                integration_id: integration_id.into(),
                channel: "C0123ABCD".into(),
                thread_ts: "1700000000.000100".into(),
            })
        };
        let sent = DeliveryReport {
            reply_to: origin("s-1"),
            ..report("j-reply-slack", "success")
        };
        deliver_all(file.path(), &[reply("always")], &sent).await;
        let gone = DeliveryReport {
            reply_to: origin("s-gone"),
            ..report("j-reply-slack", "success")
        };
        deliver_all(file.path(), &[reply("always")], &gone).await;
        set_api_base(None);

        let target = "C0123ABCD · 1700000000.000100".to_string();
        let rows = rows(file.path(), "j-reply-slack");
        assert_eq!(rows.len(), 2);
        let mut statuses: Vec<_> = rows
            .iter()
            .map(|r| (r.1.clone(), r.2.clone(), r.3.clone(), r.4.clone()))
            .collect();
        statuses.sort();
        assert_eq!(
            statuses,
            vec![
                ("reply".into(), target.clone(), "sent".into(), String::new()),
                (
                    "reply".into(),
                    target,
                    "skipped".into(),
                    "the Slack integration was deleted".into()
                ),
            ]
        );
        assert_eq!(
            *seen.lock().expect("lock"),
            vec![(
                "/chat.postMessage".to_string(),
                serde_json::json!({
                    "channel": "C0123ABCD",
                    "mrkdwn": true,
                    "text": "the answer",
                    "thread_ts": "1700000000.000100"
                })
            )],
            "one post, into the thread, and nothing for the deleted integration"
        );
    }
}

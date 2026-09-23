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
//! **Every target ends in a row.** One row per channel or recipient, written
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

use std::path::{Path, PathBuf};

use crate::native::db;
use crate::native::gotime::GoTime;
use crate::native::tasks::{
    self, JobDelivery, SlackDestination, TaskDestination, TelegramDestination, DELIVERY_FAILED,
    DELIVERY_PENDING, DELIVERY_SENT, DELIVERY_SKIPPED,
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

/// What a stored entry whose `type` this build does not know records.
const SKIPPED_UNKNOWN_TYPE: &str = "unknown destination type";

/// A stored [`TaskDestination`], resolved to the type that delivers it.
#[derive(Debug, Clone)]
enum Destination {
    Slack(SlackDestination),
    Telegram(TelegramDestination),
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
    fn targets(&self) -> Vec<String> {
        match self {
            Self::Slack(slack) => slack.channel_ids.iter().cloned().collect(),
            Self::Telegram(telegram) => telegram.chat_ids.iter().cloned().collect(),
            #[cfg(any(test, feature = "test-hooks"))]
            Self::Fake(_) => vec!["fake".to_string()],
        }
    }

    async fn deliver_one(&self, db_path: &Path, target: &str, report: &DeliveryReport) -> Outcome {
        match self {
            Self::Slack(slack) => {
                deliver_slack(db_path, &slack.integration_id, target, report).await
            }
            Self::Telegram(telegram) => {
                deliver_telegram(db_path, &telegram.integration_id, target, report).await
            }
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
async fn deliver_slack(
    db_path: &Path,
    integration_id: &str,
    channel: &str,
    report: &DeliveryReport,
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
    match delivery::deliver_channel(&token, channel, &run).await {
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
    for (position, config) in destinations.iter().enumerate() {
        let destination = Destination::from_config(config);
        let targets = destination
            .as_ref()
            .map_or_else(|| vec![String::new()], Destination::targets);
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
                Some(destination) => destination.deliver_one(db_path, &target, report).await,
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
}

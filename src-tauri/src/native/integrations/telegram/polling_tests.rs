//! `polling.rs`'s tests: the pure parts directly, and the worker against a fake
//! Bot API through `client::set_api_base`.
//!
//! Library tests rather than a `tests/telegram_polling.rs` binary, which is
//! where `tests/slack_socket.rs` lives: the Telegram base URL is a `cfg(test)`
//! seam (`client.rs` says why — the token is in the path), and a separate
//! binary cannot reach it. Every async test therefore holds `api_base_lock`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize};
use std::sync::Mutex;
use std::time::Instant;

use super::super::client::{api_base_lock, set_api_base};
use super::*;
use crate::native::integrations::registry::registry;

/// A distinctive token, so a leak into anything stored is unmistakable.
const SECRET: &str = "123456:AAF-POLLING-SECRET-TOKEN";

// ─── The pure parts ──────────────────────────────────────────────────────────

#[test]
fn the_payloads_are_gos_sorted_maps() {
    let poll = crate::native::gojson::to_vec_marshal(&GetUpdates {
        allowed_updates: ["message"],
        offset: 5,
        timeout: 30,
    })
    .expect("encode");
    assert_eq!(
        String::from_utf8(poll).expect("utf-8"),
        r#"{"allowed_updates":["message"],"offset":5,"timeout":30}"#
    );
    let delete = crate::native::gojson::to_vec_marshal(&DeleteWebhook {
        drop_pending_updates: false,
    })
    .expect("encode");
    assert_eq!(
        String::from_utf8(delete).expect("utf-8"),
        r#"{"drop_pending_updates":false}"#
    );
}

#[test]
fn the_default_poll_stays_inside_the_clients_timeout() {
    let options = PollOptions::default();
    assert_eq!(options.poll_timeout, Duration::from_secs(30));
    assert!(
        options.poll_timeout + options.request_grace < Duration::from_secs(60),
        "the worker's own deadline must fire before `client::http_client`'s"
    );
}

#[test]
fn the_offset_moves_one_past_the_highest_update() {
    let batch = read_batch(
        r#"[{"update_id":7,"message":{"message_id":1,"chat":{"id":5},"text":"a"}},
            {"update_id":9,"message":{"message_id":2,"chat":{"id":5},"text":"b"}}]"#,
        3,
    )
    .expect("a batch");
    assert_eq!(batch.next_offset, 10);
    assert_eq!(batch.skipped, 0);
    assert_eq!(
        batch
            .updates
            .iter()
            .map(|u| u.update_id)
            .collect::<Vec<_>>(),
        [7, 9]
    );

    // An empty answer is a quiet poll: nothing to do, and the offset stays.
    let quiet = read_batch("[]", 10).expect("a batch");
    assert_eq!(quiet.next_offset, 10);
    assert!(quiet.updates.is_empty());

    // A redelivery below the offset never moves it backwards.
    let stale = read_batch(r#"[{"update_id":2}]"#, 10);
    assert!(stale.is_err(), "a batch that cannot advance is a failure");
}

#[test]
fn a_bad_element_is_skipped_and_the_offset_still_passes_it() {
    // The middle element is the positional-array shape `GoStruct` refuses, and
    // the last has a `message` of the wrong type. Neither may wedge the queue.
    let batch = read_batch(
        r#"[{"update_id":7,"message":{"message_id":1,"chat":{"id":5},"text":"a"}},
            [8,{"message_id":1}],
            {"update_id":9,"message":"not an object"}]"#,
        0,
    )
    .expect("a batch");
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(batch.skipped, 2);
    assert_eq!(batch.next_offset, 10, "9 is read loosely and passed");
}

#[test]
fn a_result_that_is_not_a_list_or_cannot_advance_is_a_failure() {
    for result in ["", "null", r#"{"update_id":1}"#, "7"] {
        assert!(read_batch(result, 0).is_err(), "{result:?}");
    }
    // Elements with no usable id would be handed back at once, forever.
    let stuck = read_batch(r#"[[1,2],"x"]"#, 4).expect_err("stuck");
    assert!(stuck.contains("cannot advance"), "{stuck}");
}

#[test]
fn a_conflict_is_named_and_everything_else_passes_through() {
    let other_poller = describe_failure(
        "telegram API error: Conflict: terminated by other getUpdates request; \
         make sure that only one bot instance is running",
    );
    assert!(
        other_poller.starts_with("Something else is polling this bot token"),
        "{other_poller}"
    );
    assert!(other_poller.contains("terminated by other getUpdates request"));

    let webhook = describe_failure(
        "telegram API error: Conflict: can't use getUpdates method while webhook is active; \
         use deleteWebhook to delete the webhook first",
    );
    assert!(
        webhook.starts_with("A webhook is set for this bot somewhere else"),
        "{webhook}"
    );

    for plain in [
        "telegram API error: Unauthorized",
        "calling Telegram getUpdates: request failed",
        "parsing response: expected value at line 1 column 1",
    ] {
        assert_eq!(describe_failure(plain), plain);
    }
}

#[test]
fn the_wait_starts_at_the_base_and_doubles_to_the_cap() {
    let options = PollOptions::default();
    let waits: Vec<u64> = (1..=9)
        .map(|failures| wait_after(failures, &options, 0.0).as_secs())
        .collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
}

// ─── The worker, against a fake Bot API ──────────────────────────────────────

/// What the fake answers one request with.
struct Reply {
    body: String,
    delay: Duration,
}

impl Reply {
    fn ok(result: &str) -> Self {
        Self {
            body: format!(r#"{{"ok":true,"result":{result}}}"#),
            delay: Duration::ZERO,
        }
    }

    fn refused(description: &str) -> Self {
        Self {
            body: format!(r#"{{"ok":false,"description":"{description}"}}"#),
            delay: Duration::ZERO,
        }
    }

    fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// `(method, decoded body, how many calls of this method came before)`.
type Script = Arc<dyn Fn(&str, &serde_json::Value, usize) -> Reply + Send + Sync>;

struct Fake {
    /// Every request, in arrival order: the method and its body.
    calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    /// Requests the client abandoned before the fake answered.
    abandoned: Arc<AtomicUsize>,
}

impl Fake {
    fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }

    fn bodies(&self, method: &str) -> Vec<serde_json::Value> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, body)| body.clone())
            .collect()
    }
}

/// Counts a request the client walked away from: axum drops the handler future
/// when the connection closes, and this is still armed when it does.
struct Abandoned {
    counter: Arc<AtomicUsize>,
    armed: bool,
}

impl Drop for Abandoned {
    fn drop(&mut self) {
        if self.armed {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Start a fake Bot API and point the Telegram client at it. The caller holds
/// `api_base_lock`.
async fn fake_telegram(script: Script) -> Fake {
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::default();
    let abandoned = Arc::new(AtomicUsize::new(0));
    let (seen, gone) = (Arc::clone(&calls), Arc::clone(&abandoned));
    let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let (seen, gone, script) = (Arc::clone(&seen), Arc::clone(&gone), Arc::clone(&script));
        async move {
            let method = request
                .uri()
                .path()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            let bytes = axum::body::to_bytes(request.into_body(), 1 << 20)
                .await
                .unwrap_or_default();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let nth = {
                let mut calls = seen.lock().expect("calls");
                let nth = calls.iter().filter(|(m, _)| *m == method).count();
                calls.push((method.clone(), body.clone()));
                nth
            };
            let reply = script(&method, &body, nth);
            let mut guard = Abandoned {
                counter: gone,
                armed: true,
            };
            tokio::time::sleep(reply.delay).await;
            guard.armed = false;
            reply.body
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
    Fake { calls, abandoned }
}

/// A quiet bot: the webhook delete succeeds and every poll is empty. A long
/// poll is held briefly, as Telegram holds one, so the loop does not spin.
fn quiet(body: &serde_json::Value) -> Reply {
    let held = body["timeout"].as_u64().unwrap_or(0) > 0;
    Reply::ok("[]").after(if held {
        Duration::from_millis(30)
    } else {
        Duration::ZERO
    })
}

/// A migrated database holding one Telegram row whose webhook is registered.
fn fixture(dir: &Path, id: &str) -> PathBuf {
    let path = dir.join("agento.db");
    let mut conn = rusqlite::Connection::open(&path).expect("open");
    crate::native::migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO integrations
            (id, name, type, enabled, credentials, services, inbound_enabled,
             webhook_secret, webhook_status, created_at, updated_at)
         VALUES (?1, ?1, 'telegram', 1, ?2, '{}', 1, 'old-secret', 'active',
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        rusqlite::params![id, format!(r#"{{"bot_token":"{SECRET}"}}"#)],
    )
    .expect("seed");
    path
}

/// Short waits, so a failure schedule plays out in milliseconds.
fn fast() -> PollOptions {
    PollOptions {
        base_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(40),
        failure_threshold: 3,
        poll_timeout: Duration::from_secs(1),
        request_grace: Duration::from_secs(5),
    }
}

/// A worker the registry has accepted — what `registry::host_poll_worker` does
/// for a real one. Without the grant it would report nothing.
fn accepted(db: &Path, id: &str, options: PollOptions) -> PollWorker {
    let worker = start(db, id, SECRET, options);
    worker.accept(registry().retire_socket(id));
    worker
}

fn inbound(db: &Path, id: &str) -> (String, String) {
    rusqlite::Connection::open(db)
        .expect("open")
        .query_row(
            "SELECT inbound_status, inbound_error FROM integrations WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read the inbound state")
}

async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn the_webhook_is_deleted_before_the_first_poll_and_the_row_says_so() {
    let _guard = api_base_lock().await;
    let fake = fake_telegram(Arc::new(|_, body, _| quiet(body))).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), "tg-webhook");

    let worker = accepted(&db, "tg-webhook", fast());
    eventually("the row to read connected", || {
        inbound(&db, "tg-webhook").0 == STATUS_CONNECTED
    })
    .await;
    drop(worker);
    set_api_base(None);

    let methods = fake.methods();
    assert_eq!(methods[0], "deleteWebhook", "{methods:?}");
    assert_eq!(methods[1], "getUpdates", "{methods:?}");
    assert_eq!(
        fake.count("deleteWebhook"),
        1,
        "once, not before every poll"
    );
    assert_eq!(
        fake.bodies("deleteWebhook")[0]["drop_pending_updates"],
        false,
        "what arrived while the webhook was the transport must not be dropped"
    );
    assert_eq!(
        fake.bodies("getUpdates")[0]["timeout"],
        0,
        "the first poll does not wait, so `connected` is reported at once"
    );

    let (secret, status): (String, String) = rusqlite::Connection::open(&db)
        .expect("open")
        .query_row(
            "SELECT webhook_secret, webhook_status FROM integrations WHERE id = 'tg-webhook'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read");
    assert_eq!((secret.as_str(), status.as_str()), ("", "inactive"));
}

/// A webhook Telegram will not let go of is a failed attempt like any other:
/// nothing polls behind it, the row says why, and the worker keeps trying until
/// the delete goes through. Polling regardless would only collect 409s.
#[tokio::test]
async fn a_failed_webhook_delete_is_retried_and_nothing_polls_behind_it() {
    let _guard = api_base_lock().await;
    // 1 = Telegram refusing the delete.
    let refusing = Arc::new(AtomicU8::new(1));
    let mode = Arc::clone(&refusing);
    let fake = fake_telegram(Arc::new(move |method, body, _| {
        if method == "deleteWebhook" && mode.load(Ordering::SeqCst) == 1 {
            Reply::refused("Bad Gateway")
        } else {
            quiet(body)
        }
    }))
    .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), "tg-stuck-webhook");
    let options = PollOptions {
        base_backoff: Duration::from_millis(150),
        max_backoff: Duration::from_millis(300),
        ..fast()
    };

    let worker = accepted(&db, "tg-stuck-webhook", options);
    eventually("reconnecting", || {
        inbound(&db, "tg-stuck-webhook").0 == STATUS_RECONNECTING
    })
    .await;
    eventually("error after the third failure", || {
        inbound(&db, "tg-stuck-webhook").0 == STATUS_ERROR
    })
    .await;
    assert_eq!(
        inbound(&db, "tg-stuck-webhook").1,
        "removing the webhook before polling: telegram API error: Bad Gateway"
    );
    assert!(fake.count("deleteWebhook") >= 3, "each attempt asks again");
    assert_eq!(
        fake.count("getUpdates"),
        0,
        "no poll may be sent while the webhook is still set"
    );
    let status: String = rusqlite::Connection::open(&db)
        .expect("open")
        .query_row(
            "SELECT webhook_status FROM integrations WHERE id = 'tg-stuck-webhook'",
            [],
            |row| row.get(0),
        )
        .expect("read");
    assert_eq!(
        status, "active",
        "the row is cleared only once the delete worked"
    );

    refusing.store(0, Ordering::SeqCst);
    eventually("connected once the delete goes through", || {
        inbound(&db, "tg-stuck-webhook") == (STATUS_CONNECTED.to_string(), String::new())
    })
    .await;
    drop(worker);
    set_api_base(None);
}

/// A redelivery — a restart, or a poll whose confirmation never reached
/// Telegram — must not run the rule twice. The rule names an agent that does
/// not exist, so a run is exactly one error reply: the cheapest thing that
/// proves the dispatcher was reached, and how many times.
#[tokio::test]
async fn an_update_delivered_twice_runs_once() {
    let _guard = api_base_lock().await;
    const UPDATE: &str =
        r#"[{"update_id":7,"message":{"message_id":3,"chat":{"id":42},"text":"hello"}}]"#;
    let fake = fake_telegram(Arc::new(|method, body, nth| match method {
        // Twice, whatever offset is asked for.
        "getUpdates" if nth < 2 => Reply::ok(UPDATE),
        "getUpdates" => quiet(body),
        _ => Reply::ok("true"),
    }))
    .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), "tg-twice");
    rusqlite::Connection::open(&db)
        .expect("open")
        .execute(
            "INSERT INTO trigger_rules
                (id, integration_id, name, agent_slug, enabled, filter_prefix,
                 filter_keywords, filter_chat_ids, created_at, updated_at)
             VALUES ('r', 'tg-twice', 'r', 'no-such-agent', 1, '', '[]', '[]',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed rule");

    let worker = accepted(&db, "tg-twice", fast());
    eventually("the error reply", || fake.count("sendMessage") >= 1).await;
    eventually("polls past both deliveries", || {
        fake.count("getUpdates") >= 5
    })
    .await;
    // Long enough for a second run's reply to have been sent, had there been one.
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(worker);
    set_api_base(None);

    assert_eq!(fake.count("sendMessage"), 1, "one run, so one reply");
    assert_eq!(
        fake.bodies("getUpdates")[1]["offset"],
        8,
        "the next poll confirms the update it was just given"
    );
    let claimed: i64 = rusqlite::Connection::open(&db)
        .expect("open")
        .query_row(
            "SELECT COUNT(*) FROM telegram_processed_updates WHERE integration_id = 'tg-twice'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(claimed, 1);
}

#[tokio::test]
async fn the_status_walks_out_to_error_and_back_without_a_restart() {
    let _guard = api_base_lock().await;
    // 0 = healthy, 1 = Telegram refusing every poll.
    let failing = Arc::new(AtomicU8::new(0));
    let mode = Arc::clone(&failing);
    let fake = fake_telegram(Arc::new(move |method, body, _| {
        if method == "getUpdates" && mode.load(Ordering::SeqCst) == 1 {
            Reply::refused("Bad Gateway")
        } else {
            quiet(body)
        }
    }))
    .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), "tg-walk");
    // Slow enough that `reconnecting` is still on the row when it is read.
    let options = PollOptions {
        base_backoff: Duration::from_millis(150),
        max_backoff: Duration::from_millis(300),
        ..fast()
    };

    let worker = accepted(&db, "tg-walk", options);
    eventually("connected", || {
        inbound(&db, "tg-walk") == (STATUS_CONNECTED.to_string(), String::new())
    })
    .await;

    failing.store(1, Ordering::SeqCst);
    eventually("reconnecting", || {
        inbound(&db, "tg-walk").0 == STATUS_RECONNECTING
    })
    .await;
    eventually("error after the third failure", || {
        inbound(&db, "tg-walk").0 == STATUS_ERROR
    })
    .await;
    let (_, reason) = inbound(&db, "tg-walk");
    assert_eq!(reason, "telegram API error: Bad Gateway");

    failing.store(0, Ordering::SeqCst);
    eventually("connected again", || {
        inbound(&db, "tg-walk") == (STATUS_CONNECTED.to_string(), String::new())
    })
    .await;
    drop(worker);
    set_api_base(None);

    assert_eq!(
        fake.count("deleteWebhook"),
        1,
        "a failed poll retries the poll, not the webhook delete"
    );
}

#[tokio::test]
async fn a_conflict_is_reported_by_cause_and_never_names_the_token() {
    let _guard = api_base_lock().await;
    crate::native::writes::testlog::install();
    let _fake = fake_telegram(Arc::new(|method, body, _| {
        if method == "getUpdates" {
            Reply::refused(
                "Conflict: terminated by other getUpdates request; \
                 make sure that only one bot instance is running",
            )
        } else {
            quiet(body)
        }
    }))
    .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), "tg-conflict");

    let worker = accepted(&db, "tg-conflict", fast());
    eventually("the conflict to reach the row", || {
        inbound(&db, "tg-conflict").0 == STATUS_ERROR
    })
    .await;
    drop(worker);
    set_api_base(None);

    let (_, reason) = inbound(&db, "tg-conflict");
    assert!(
        reason.starts_with("Something else is polling this bot token"),
        "{reason}"
    );
    assert!(!reason.contains(SECRET), "the token is in the request path");
    assert!(
        crate::native::writes::testlog::matching(SECRET).is_empty(),
        "no log line may carry the bot token"
    );
}

/// The registry retires a worker by taking the next epoch. A status the old
/// worker posts afterwards must not land, or a reload would leave the row
/// holding the previous worker's last words.
#[tokio::test]
async fn a_retired_workers_late_status_is_refused() {
    let _guard = api_base_lock().await;
    let failing = Arc::new(AtomicU8::new(0));
    let mode = Arc::clone(&failing);
    let fake = fake_telegram(Arc::new(move |method, body, _| {
        if method == "getUpdates" && mode.load(Ordering::SeqCst) == 1 {
            Reply::refused("Bad Gateway")
        } else {
            quiet(body)
        }
    }))
    .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), "tg-retired");

    let worker = accepted(&db, "tg-retired", fast());
    eventually("connected", || {
        inbound(&db, "tg-retired").0 == STATUS_CONNECTED
    })
    .await;

    // Retired, but still running: the handle is kept so the worker goes on to
    // fail and post `reconnecting`, which is the write under test.
    registry().retire_socket("tg-retired");
    let polls = fake.count("getUpdates");
    failing.store(1, Ordering::SeqCst);
    eventually("several refused polls", || {
        fake.count("getUpdates") >= polls + 4
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(worker);
    set_api_base(None);

    assert_eq!(
        inbound(&db, "tg-retired"),
        (STATUS_CONNECTED.to_string(), String::new()),
        "only the worker holding the current epoch may write the row"
    );
}

#[tokio::test]
async fn a_stop_cancels_the_poll_in_flight() {
    let _guard = api_base_lock().await;
    let fake = fake_telegram(Arc::new(|method, body, _| {
        if method == "getUpdates" && body["timeout"].as_u64().unwrap_or(0) > 0 {
            // A long poll Telegram is holding open.
            Reply::ok("[]").after(Duration::from_secs(30))
        } else {
            Reply::ok("[]")
        }
    }))
    .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), "tg-stop");
    let options = PollOptions {
        poll_timeout: Duration::from_secs(30),
        ..fast()
    };

    let worker = accepted(&db, "tg-stop", options);
    eventually("the long poll to be in flight", || {
        fake.count("getUpdates") >= 2
    })
    .await;
    let stopped_at = Instant::now();
    drop(worker);
    eventually("the request to be abandoned", || {
        fake.abandoned.load(Ordering::SeqCst) >= 1
    })
    .await;
    set_api_base(None);

    assert!(
        stopped_at.elapsed() < Duration::from_secs(5),
        "the stop waited out the poll instead of cancelling it"
    );
    assert_eq!(fake.count("getUpdates"), 2, "and nothing polled afterwards");
}

/// A half-open connection delivers neither an answer nor an error. The worker's
/// own deadline turns it into a failed attempt instead of a parked worker.
#[tokio::test]
async fn a_poll_that_is_never_answered_is_a_failed_attempt() {
    let _guard = api_base_lock().await;
    let _fake = fake_telegram(Arc::new(|method, _, _| {
        if method == "getUpdates" {
            Reply::ok("[]").after(Duration::from_secs(30))
        } else {
            Reply::ok("true")
        }
    }))
    .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), "tg-silent");
    let options = PollOptions {
        request_grace: Duration::from_millis(150),
        ..fast()
    };

    let worker = accepted(&db, "tg-silent", options);
    eventually("the silence to be reported", || {
        inbound(&db, "tg-silent").0 == STATUS_ERROR
    })
    .await;
    drop(worker);
    set_api_base(None);

    assert_eq!(
        inbound(&db, "tg-silent").1,
        "the poll got no answer from Telegram in time"
    );
}

/// The copy of `a_contended_write_lock_does_not_stall_the_runtime` for this
/// worker. Its writes — the webhook clear and every status — sit behind
/// `db.rs`'s five-second `busy_timeout`, and the task is not on `proxy.rs`'s
/// blocking pool, so an inline write would park a runtime worker.
///
/// The shape is the established one: **one worker thread**, so a single parked
/// worker is the whole runtime; a **plain OS thread** holds the lock; and
/// `last` is seeded before the spawn, because a starved ticker is never polled.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_poll_workers_contended_write_lock_does_not_stall_the_runtime() {
    let _guard = api_base_lock().await;
    let fake = fake_telegram(Arc::new(|_, body, _| quiet(body))).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture(dir.path(), "tg-contended");

    /// Long enough that a parked worker is unmistakable, short enough to stay
    /// well inside the 5 s `busy_timeout` so the writes still succeed.
    const HOLD: Duration = Duration::from_millis(1_500);

    // Already WAL, or `open_read_write`'s own mode change fails at once
    // instead of waiting — see the Slack copy.
    db::open_read_write(&db_path).expect("convert to WAL");

    let (holding_tx, holding_rx) = std::sync::mpsc::channel();
    let lock_db = db_path.clone();
    let holder = std::thread::spawn(move || {
        let mut conn = rusqlite::Connection::open(&lock_db).expect("open");
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("begin immediate");
        holding_tx.send(()).expect("signal");
        std::thread::sleep(HOLD);
        tx.rollback().expect("rollback");
    });
    holding_rx.recv().expect("the writer took the lock");

    let worst_gap_ms = Arc::new(AtomicU64::new(0));
    let ticks = Arc::new(AtomicU64::new(0));
    let ticker = {
        let (worst_gap_ms, ticks, mut last) = (
            Arc::clone(&worst_gap_ms),
            Arc::clone(&ticks),
            Instant::now(),
        );
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let now = Instant::now();
                let gap = u64::try_from(now.duration_since(last).as_millis()).unwrap_or(u64::MAX);
                worst_gap_ms.fetch_max(gap, Ordering::Relaxed);
                ticks.fetch_add(1, Ordering::Relaxed);
                last = now;
            }
        })
    };

    let worker = accepted(&db_path, "tg-contended", fast());
    // Both of the worker's writes are now waiting on the held lock: the
    // `connecting` status, and the webhook clear behind the delete.
    eventually("the webhook delete", || fake.count("deleteWebhook") >= 1).await;
    tokio::time::sleep(HOLD).await;
    holder.join().expect("the writer finished");
    ticker.abort();

    let worst = worst_gap_ms.load(Ordering::Relaxed);
    assert!(
        worst < 500,
        "the runtime stalled for {worst} ms while the write lock was held \
         (the hold is {} ms; anything near it means the worker wrote inline)",
        HOLD.as_millis()
    );
    let ticks = ticks.load(Ordering::Relaxed);
    assert!(
        ticks > 50,
        "the ticker only advanced {ticks} times across a {} ms hold",
        HOLD.as_millis()
    );

    // And the writes it was waiting on did land once the lock was released.
    eventually("connected once the lock is released", || {
        inbound(&db_path, "tg-contended").0 == STATUS_CONNECTED
    })
    .await;
    drop(worker);
    set_api_base(None);
}

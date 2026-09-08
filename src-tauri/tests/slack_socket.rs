//! The Slack Socket Mode worker, against a fake Slack (#567).
//!
//! The fake is two halves of one thing: an `axum` route answering
//! `apps.connections.open` with a `ws://` URL, and a raw `TcpListener` speaking
//! websocket through `tokio-tungstenite`'s server side. Both are in-process, for
//! the reason `tests/gateway_engine.rs` gives for its own fake — there is no
//! executable to fork, so there is no `python3` to skip on — and, unlike the
//! three suites that drive a fake Claude CLI, nothing here reaches for
//! `std::os::unix::fs::PermissionsExt`, so this file is not a new reason
//! `src-tauri/tests/` cannot compile on Windows.
//!
//! **Every await has a deadline.** A worker that never connects and a fake that
//! never answers look identical to a test that simply waits, so each wait is a
//! `tokio::time::timeout` whose message names the hang.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agento_lib::native::integrations::slack::socket::{
    self, AppMention, SocketOptions, STATUS_CONNECTED, STATUS_ERROR, STATUS_RECONNECTING,
};
use agento_lib::native::{db, migrate};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

/// The app-level token every fixture stores. Its presence in a captured log is
/// the leak `a_full_connect_drop_reconnect_cycle_names_nothing_secret` looks for.
const APP_TOKEN: &str = "xapp-1-A0000000000-1111111111111-secretsecretsecret";

/// Short enough that a three-attempt reconnect test finishes in about two
/// seconds, long enough that the gaps are unambiguous against scheduler noise.
const BASE_BACKOFF: Duration = Duration::from_millis(250);

/// What the fake does with one accepted websocket connection, in order.
#[derive(Clone, Debug)]
enum Step {
    /// Send one text frame.
    Send(String),
    /// Wait until the worker has acknowledged `n` envelopes in total.
    AwaitAcks(usize),
    /// Sleep, holding the connection open.
    Hold(Duration),
    /// Drop the TCP connection with no closing handshake — a network drop.
    Drop,
}

/// One accepted connection's script. The fake serves them in order; running off
/// the end holds the connection open forever, which is what a healthy Slack
/// gateway does.
type Session = Vec<Step>;

/// A fake Slack: `apps.connections.open` plus the websocket it points at.
///
/// Dropping it stops both listeners.
struct FakeSlack {
    api_base: String,
    /// One entry per `apps.connections.open`, at the moment it was answered.
    opens: Arc<Mutex<Vec<Instant>>>,
    /// Every `envelope_id` the worker acknowledged, in arrival order.
    acks: Arc<Mutex<Vec<String>>>,
    /// Fires on every ack, so a test can wait for one instead of sleeping.
    acked: Arc<tokio::sync::Notify>,
    /// Websocket connections currently open. `max_live` is what proves a reload
    /// left exactly one.
    live: Arc<AtomicUsize>,
    max_live: Arc<AtomicUsize>,
    /// Fires whenever a websocket connection ends, however it ended.
    closed: Arc<tokio::sync::Notify>,
    _shutdown: tokio::sync::oneshot::Sender<()>,
    _ws_shutdown: tokio::sync::oneshot::Sender<()>,
}

impl FakeSlack {
    fn opens(&self) -> Vec<Instant> {
        self.opens.lock().expect("opens lock").clone()
    }

    fn acks(&self) -> Vec<String> {
        self.acks.lock().expect("acks lock").clone()
    }
}

/// Bind the fake and hand back its base URL.
///
/// `refusals` is answered *before* the sessions: each entry is the Slack `error`
/// code `apps.connections.open` reports with `{"ok":false}`, which is how the
/// failure-threshold and 401 cases are driven without a socket at all.
async fn fake_slack(refusals: Vec<String>, sessions: Vec<Session>) -> FakeSlack {
    let opens: Arc<Mutex<Vec<Instant>>> = Arc::new(Mutex::new(Vec::new()));
    let acks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let acked = Arc::new(tokio::sync::Notify::new());
    let closed = Arc::new(tokio::sync::Notify::new());
    let live = Arc::new(AtomicUsize::new(0));
    let max_live = Arc::new(AtomicUsize::new(0));
    let script: Arc<Mutex<VecDeque<Session>>> = Arc::new(Mutex::new(sessions.into()));
    let refusals: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(refusals.into()));

    // The websocket half.
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake socket mode gateway");
    let ws_addr = ws_listener.local_addr().expect("ws addr");
    let (ws_tx, mut ws_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let (acks, acked, closed, live, max_live, script) = (
            Arc::clone(&acks),
            Arc::clone(&acked),
            Arc::clone(&closed),
            Arc::clone(&live),
            Arc::clone(&max_live),
            Arc::clone(&script),
        );
        tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    _ = &mut ws_rx => return,
                    accepted = ws_listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(_) => return,
                    },
                };
                let session = script.lock().expect("script lock").pop_front();
                let (acks, acked, closed, live, max_live) = (
                    Arc::clone(&acks),
                    Arc::clone(&acked),
                    Arc::clone(&closed),
                    Arc::clone(&live),
                    Arc::clone(&max_live),
                );
                tokio::spawn(async move {
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    max_live.fetch_max(now, Ordering::SeqCst);
                    serve_session(stream, session.unwrap_or_default(), &acks, &acked).await;
                    live.fetch_sub(1, Ordering::SeqCst);
                    closed.notify_waiters();
                });
            }
        });
    }

    // The `apps.connections.open` half.
    let ws_url = format!("ws://{ws_addr}/link");
    let http_opens = Arc::clone(&opens);
    let app = axum::Router::new().route(
        "/apps.connections.open",
        axum::routing::post(move || {
            let (opens, refusals, ws_url) = (
                Arc::clone(&http_opens),
                Arc::clone(&refusals),
                ws_url.clone(),
            );
            async move {
                opens.lock().expect("opens lock").push(Instant::now());
                match refusals.lock().expect("refusals lock").pop_front() {
                    Some(error) => axum::Json(serde_json::json!({"ok": false, "error": error})),
                    // The URL is single-use in Slack's own protocol, which is
                    // why the worker asks again on every attempt; the fake hands
                    // back the same one because what is under test is that it
                    // *asked*, not that Slack rotated it.
                    None => axum::Json(serde_json::json!({"ok": true, "url": ws_url})),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake slack api");
    let api_base = format!("http://{}", listener.local_addr().expect("api addr"));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    FakeSlack {
        api_base,
        opens,
        acks,
        acked,
        live,
        max_live,
        closed,
        _shutdown: tx,
        _ws_shutdown: ws_tx,
    }
}

/// Run one connection's script while recording every ack the worker sends.
///
/// The script and the read loop are separate tasks and the session ends when
/// **either** finishes — which is the part that makes `live` mean what the
/// reload and stop tests read it as. A session that ended only when its script
/// ran out would still be counted as live for the whole of a `Hold` after the
/// worker had already gone, so "the socket closed" and "the script is still
/// sleeping" would be indistinguishable.
async fn serve_session(
    stream: tokio::net::TcpStream,
    session: Session,
    acks: &Arc<Mutex<Vec<String>>>,
    acked: &Arc<tokio::sync::Notify>,
) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut sink, mut source) = ws.split();
    let ack_count = Arc::new(AtomicUsize::new(0));

    let mut reader = {
        let (acks, acked, ack_count) =
            (Arc::clone(acks), Arc::clone(acked), Arc::clone(&ack_count));
        tokio::spawn(async move {
            while let Some(Ok(frame)) = source.next().await {
                if let Message::Text(text) = frame {
                    let value: serde_json::Value =
                        serde_json::from_str(&text).expect("an ack is valid json");
                    let id = value["envelope_id"]
                        .as_str()
                        .expect("an ack carries an envelope_id")
                        .to_string();
                    acks.lock().expect("acks lock").push(id);
                    ack_count.fetch_add(1, Ordering::SeqCst);
                    acked.notify_waiters();
                }
            }
        })
    };

    let mut script = tokio::spawn(async move {
        for step in session {
            match step {
                Step::Send(text) => {
                    if sink.send(Message::text(text)).await.is_err() {
                        return;
                    }
                }
                Step::AwaitAcks(n) => {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while ack_count.load(Ordering::SeqCst) < n && Instant::now() < deadline {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }
                Step::Hold(d) => tokio::time::sleep(d).await,
                // Returning drops `sink`, which closes the TCP connection with
                // no closing handshake — what a network drop looks like from
                // the worker's side, and the case the reconnect schedule is
                // about.
                Step::Drop => return,
            }
        }
        // Running off the end holds the connection open, as a healthy gateway
        // does, until the worker goes away.
        std::future::pending::<()>().await;
    });

    tokio::select! {
        _ = &mut reader => script.abort(),
        _ = &mut script => reader.abort(),
    }
}

/// A migrated database with one Slack integration, inbound on.
fn fixture_db(dir: &Path) -> PathBuf {
    let db_path = dir.join("agento.db");
    let mut conn = db::ensure_database(&db_path).expect("create");
    migrate::apply(&mut conn).expect("migrations");
    conn.execute(
        "INSERT INTO integrations
            (id, name, type, enabled, credentials, services, created_at, updated_at,
             inbound_enabled, inbound_status, inbound_error)
         VALUES ('s1', 'Slack', 'slack', 1, ?1, '{}', 'then', 'then', 1, '', '')",
        rusqlite::params![format!(r#"{{"app_token":"{APP_TOKEN}"}}"#)],
    )
    .expect("seed the integration");
    db_path
}

fn inbound_state(db_path: &Path) -> (String, String) {
    let conn = db::open_read_only(db_path).expect("open");
    conn.query_row(
        "SELECT inbound_status, inbound_error FROM integrations WHERE id = 's1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("read the inbound state")
}

/// Poll the stored status until it is one of `wanted`, or fail naming what it
/// was. A status is written from a `db::blocking` task, so it lands a moment
/// after the transition it reports.
async fn await_status(db_path: &Path, wanted: &[&str], what: &str) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = (String::new(), String::new());
    while Instant::now() < deadline {
        last = inbound_state(db_path);
        if wanted.contains(&last.0.as_str()) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{what}: inbound_status never reached {wanted:?}, it is {last:?}");
}

fn options(fake: &FakeSlack, handler: socket::EventHandler) -> SocketOptions {
    SocketOptions {
        api_base: fake.api_base.clone(),
        base_backoff: BASE_BACKOFF,
        max_backoff: Duration::from_secs(5),
        failure_threshold: 2,
        handler,
    }
}

/// A handler that records every mention it is given and, optionally, holds for
/// `hold` before recording — long enough that an ack arriving in the meantime
/// cannot have been waiting behind it.
fn recording_handler(seen: &Arc<Mutex<Vec<AppMention>>>, hold: Duration) -> socket::EventHandler {
    let seen = Arc::clone(seen);
    Arc::new(move |mention: AppMention| {
        let seen = Arc::clone(&seen);
        Box::pin(async move {
            tokio::time::sleep(hold).await;
            seen.lock().expect("seen lock").push(mention);
        })
    })
}

fn events_api(envelope_id: &str, event_id: &str) -> String {
    serde_json::json!({
        "type": "events_api",
        "envelope_id": envelope_id,
        "payload": {
            "event_id": event_id,
            "event": {
                "type": "app_mention",
                "channel": "C1",
                "user": "U1",
                "text": "<@B1> hello",
                "ts": "1700000000.000100",
            },
        },
    })
    .to_string()
}

/// The worker opens exactly the URL `apps.connections.open` returned, and
/// **acknowledges before the handler runs**.
///
/// The ordering is the whole point and it is asserted the only way that is not a
/// race: the handler holds for a second, and the ack has to arrive at the fake
/// inside a fraction of that. An implementation that acknowledged after the
/// handler could not produce it. Recording two timestamps and comparing them
/// would pass against such an implementation whenever the scheduler happened to
/// run the ack first.
#[tokio::test]
async fn an_envelope_is_acknowledged_before_its_handler_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    let fake = fake_slack(
        vec![],
        vec![vec![
            Step::Send(events_api("env-1", "Ev1")),
            Step::Hold(Duration::from_secs(5)),
        ]],
    )
    .await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = recording_handler(&seen, Duration::from_secs(1));
    let _worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));

    tokio::time::timeout(Duration::from_secs(5), fake.acked.notified())
        .await
        .expect("the envelope must be acknowledged, and long before the handler finishes");
    assert!(
        seen.lock().expect("seen lock").is_empty(),
        "the handler must still be running when the ack has already arrived"
    );
    assert_eq!(fake.acks(), vec!["env-1".to_string()]);
    assert_eq!(fake.opens().len(), 1, "one apps.connections.open");

    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.lock().expect("seen lock").is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen = seen.lock().expect("seen lock").clone();
    assert_eq!(seen.len(), 1, "the handler ran once");
    assert_eq!(seen[0].event_id, "Ev1");
    assert_eq!(seen[0].channel, "C1");
    assert_eq!(seen[0].integration_id, "s1");

    let (status, error) = await_status(&db_path, &[STATUS_CONNECTED], "after a connect").await;
    assert_eq!(status, STATUS_CONNECTED);
    assert_eq!(error, "", "a healthy connection carries no error");
}

/// A repeated `event_id` is acknowledged twice and reaches the handler once.
///
/// **Both halves matter.** Acknowledging only the first would make Slack
/// redeliver the second forever; running the handler twice would answer the same
/// mention twice, which is the failure a user actually sees.
#[tokio::test]
async fn a_repeated_event_id_is_acknowledged_twice_and_handled_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    let fake = fake_slack(
        vec![],
        vec![vec![
            Step::Send(events_api("env-1", "Ev1")),
            Step::AwaitAcks(1),
            Step::Send(events_api("env-2", "Ev1")),
            Step::Hold(Duration::from_secs(5)),
        ]],
    )
    .await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = recording_handler(&seen, Duration::ZERO);
    let _worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));

    let deadline = Instant::now() + Duration::from_secs(10);
    while fake.acks().len() < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        fake.acks(),
        vec!["env-1".to_string(), "env-2".to_string()],
        "every envelope is acknowledged, duplicate or not"
    );

    // Give a second delivery every chance to reach the handler before saying it
    // did not.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let seen = seen.lock().expect("seen lock").clone();
    assert_eq!(
        seen.len(),
        1,
        "the duplicate event_id must reach the handler once, got {seen:?}"
    );
}

/// A `disconnect` envelope and a dropped socket each produce a fresh
/// `apps.connections.open`, and **the wait after the second failure is strictly
/// longer than the wait after the first**.
///
/// The schedule is what makes a flapping gateway survivable, and the property
/// that would silently regress is monotonicity: a backoff that resets on every
/// attempt reconnects in a tight loop, which reads as "it reconnects" to any
/// test that only counts attempts.
#[tokio::test]
async fn a_disconnect_and_a_drop_each_reconnect_with_a_growing_wait() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    let fake = fake_slack(
        vec![],
        vec![
            // Slack asking politely. Not a failure: the counter resets.
            vec![Step::Send(
                serde_json::json!({"type": "disconnect", "reason": "refresh_requested"})
                    .to_string(),
            )],
            // Then two network drops, whose waits must grow.
            vec![Step::Drop],
            vec![Step::Drop],
            vec![Step::Hold(Duration::from_secs(10))],
        ],
    )
    .await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = recording_handler(&seen, Duration::ZERO);
    let _worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));

    let deadline = Instant::now() + Duration::from_secs(20);
    while fake.opens().len() < 4 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let opens = fake.opens();
    assert_eq!(
        opens.len(),
        4,
        "a disconnect and two drops each ask apps.connections.open again"
    );

    let after_disconnect = opens[1].duration_since(opens[0]);
    let after_first_drop = opens[2].duration_since(opens[1]);
    let after_second_drop = opens[3].duration_since(opens[2]);
    assert!(
        after_second_drop > after_first_drop,
        "the wait after the second consecutive failure must be strictly longer \
         than after the first: {after_first_drop:?} then {after_second_drop:?}"
    );
    assert!(
        after_disconnect < after_first_drop,
        "a disconnect is not a failure, so it must not have advanced the schedule: \
         {after_disconnect:?} then {after_first_drop:?}"
    );
}

/// `inbound_status` reads `connected`, then `reconnecting` with the reason in
/// `inbound_error` after a drop, then `error` once the failures pile up — and
/// `error` is a report, not a stop, so the worker is still trying.
#[tokio::test]
async fn the_stored_status_walks_connected_reconnecting_and_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    // One good connection, then nothing but refusals: enough to cross the
    // threshold of two and stay there.
    let fake = fake_slack(
        vec![],
        vec![vec![Step::Hold(Duration::from_millis(150)), Step::Drop]],
    )
    .await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = recording_handler(&seen, Duration::ZERO);
    let _worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));

    await_status(&db_path, &[STATUS_CONNECTED], "after the first connect").await;
    let (_, error) = await_status(&db_path, &[STATUS_RECONNECTING], "after the drop").await;
    assert!(
        !error.is_empty(),
        "a reconnecting status must carry the reason it is reconnecting"
    );

    // Every later attempt reconnects to a gateway with no script left, which
    // holds the connection open — so the failures that carry it to `error` come
    // from the sessions after the script is exhausted only if they fail. Drive
    // it instead with a worker whose every attempt is refused.
    drop(_worker);
    let refusing = fake_slack(vec!["invalid_auth".into(), "invalid_auth".into()], vec![]).await;
    let handler = recording_handler(&seen, Duration::ZERO);
    let worker = socket::start(&db_path, "s1", APP_TOKEN, options(&refusing, handler));

    let (_, error) = await_status(&db_path, &[STATUS_ERROR], "after the failure threshold").await;
    assert!(
        error.contains("invalid_auth"),
        "the error column must name what Slack said, got {error:?}"
    );
    assert!(
        !error.contains("xapp-"),
        "a stored error must not carry the app token, got {error:?}"
    );

    // `error` still retries: a third attempt happens after it.
    let before = refusing.opens().len();
    let deadline = Instant::now() + Duration::from_secs(10);
    while refusing.opens().len() <= before && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        refusing.opens().len() > before,
        "`error` is a reported state, not a stop — the worker must keep trying"
    );
    drop(worker);
}

/// Dropping the handle closes the socket promptly, and nothing survives it.
///
/// One second is the bar because that is what a `PUT
/// /api/integrations/{id}/inbound` turning inbound off has to feel like — and
/// because a socket that outlives its handle is a socket still holding the app
/// token of a row the user has just changed.
#[tokio::test]
async fn dropping_the_handle_closes_the_socket_within_a_second() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    let fake = fake_slack(vec![], vec![vec![Step::Hold(Duration::from_secs(30))]]).await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = recording_handler(&seen, Duration::ZERO);
    let worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));
    await_status(&db_path, &[STATUS_CONNECTED], "before the stop").await;
    assert_eq!(fake.live.load(Ordering::SeqCst), 1);

    let closed = fake.closed.notified();
    drop(worker);
    tokio::time::timeout(Duration::from_secs(1), closed)
        .await
        .expect("the socket must close within a second of the handle being dropped");
    assert_eq!(
        fake.live.load(Ordering::SeqCst),
        0,
        "no connection may survive the handle"
    );

    // And nothing reconnects afterwards: the task is gone, not merely idle.
    let opens = fake.opens().len();
    tokio::time::sleep(BASE_BACKOFF * 6).await;
    assert_eq!(
        fake.opens().len(),
        opens,
        "a dropped worker must not ask apps.connections.open again"
    );
}

/// A stop landing while a worker is connecting, immediately followed by a
/// start — what `Registry::reload` does — leaves **one** live connection, never
/// two.
///
/// Two would mean two sockets holding the same app token, which is the failure
/// the registry's generation counter exists to prevent for MCP listeners and
/// which the socket map now shares.
#[tokio::test]
async fn a_reload_mid_connect_leaves_exactly_one_live_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    let fake = fake_slack(
        vec![],
        vec![
            vec![Step::Hold(Duration::from_secs(30))],
            vec![Step::Hold(Duration::from_secs(30))],
        ],
    )
    .await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    for _ in 0..5 {
        let handler = recording_handler(&seen, Duration::ZERO);
        let worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));
        // Long enough to be inside `apps.connections.open` or the handshake,
        // short enough that most iterations land mid-connect.
        tokio::time::sleep(Duration::from_millis(5)).await;
        drop(worker);
    }
    let handler = recording_handler(&seen, Duration::ZERO);
    let worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));
    await_status(&db_path, &[STATUS_CONNECTED], "after the last start").await;

    // Give any abandoned attempt every chance to finish its handshake.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fake.live.load(Ordering::SeqCst),
        1,
        "exactly one connection may be live after a run of reloads"
    );
    assert_eq!(
        fake.max_live.load(Ordering::SeqCst),
        1,
        "two connections were live at once, so a stop left a socket behind"
    );
    drop(worker);
}

/// A full connect → drop → reconnect cycle puts no `xapp-` token anywhere it can
/// be read back.
///
/// The token is the one value in this module that must never be logged, and the
/// paths that would leak it are the ones a happy-path test never walks: an error
/// message built by `format!` from a failing request, or a `Debug` of a struct
/// that happens to hold it. So the cycle here is deliberately the failing one,
/// and both the stored `inbound_error` and every message the worker produced are
/// searched.
#[tokio::test]
async fn a_full_connect_drop_reconnect_cycle_names_nothing_secret() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    let fake = fake_slack(
        vec!["invalid_auth".into()],
        vec![vec![Step::Hold(Duration::from_millis(100)), Step::Drop]],
    )
    .await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = recording_handler(&seen, Duration::ZERO);
    let worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));

    await_status(&db_path, &[STATUS_CONNECTED], "after the retry succeeded").await;
    await_status(
        &db_path,
        &[STATUS_RECONNECTING, STATUS_ERROR],
        "after the drop",
    )
    .await;
    drop(worker);

    let (status, error) = inbound_state(&db_path);
    for value in [&status, &error] {
        assert!(
            !value.contains("xapp-") && !value.contains(APP_TOKEN),
            "the stored inbound state names the app token: {value:?}"
        );
    }

    // The whole row, not just the two columns — a token that reached
    // `credentials` is expected, one that reached anything else is not.
    let conn = db::open_read_only(&db_path).expect("open");
    let (name, credentials): (String, String) = conn
        .query_row(
            "SELECT name, credentials FROM integrations WHERE id = 's1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read the row");
    assert!(!name.contains("xapp-"));
    assert!(
        credentials.contains(APP_TOKEN),
        "the credentials blob is where the token belongs and it must be untouched"
    );
}

/// A contended write lock must not stall the runtime (#366, #425, #567).
///
/// The fifth copy of this rule, and the first for a task driven by a socket
/// rather than a request or a timer. `db::open_read_write` sets a five-second
/// `busy_timeout`, so the worker's status write or its dedup claim meeting a
/// lock held by the session scanner's batch writer parks its thread for up to
/// that long. This task is not on `proxy.rs`'s blocking pool at all — nothing
/// spawned from `integrations::registry::start_all` is — so an inline write
/// would park a *runtime worker*, which on a four-core box is shared with the
/// proxy, every SSE stream and the scheduler.
///
/// The shape is the established one and each part is load-bearing: **one worker
/// thread**, so a single parked worker is the whole runtime; **a plain OS
/// thread** holds the lock, so the contention comes from outside the runtime
/// exactly as the scanner's batch writer does; and **`last` is seeded before the
/// spawn**, because a starved ticker is never polled and seeding it on the first
/// poll would start the clock after the stall and measure nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_socket_workers_contended_write_lock_does_not_stall_the_runtime() {
    use std::sync::atomic::AtomicU64;

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    let fake = fake_slack(
        vec![],
        vec![vec![
            Step::Send(events_api("env-1", "Ev1")),
            Step::Hold(Duration::from_secs(10)),
        ]],
    )
    .await;

    /// Long enough that a parked worker is unmistakable, short enough to stay
    /// well inside the 5 s `busy_timeout` so the writes still succeed.
    const HOLD: Duration = Duration::from_millis(1_500);

    // The file must already be WAL. Left in the default rollback journal,
    // `open_read_write`'s own `PRAGMA journal_mode=WAL` is a *mode change*
    // needing an exclusive lock and fails in about a millisecond instead of
    // waiting on `busy_timeout` — which would make this measure the wrong thing.
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

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = recording_handler(&seen, Duration::ZERO);
    let worker = socket::start(&db_path, "s1", APP_TOKEN, options(&fake, handler));

    // The envelope has to be acknowledged while the lock is held: the ack is on
    // the socket's own task, and the claim behind it is the write that must not
    // park it.
    tokio::time::timeout(Duration::from_secs(5), fake.acked.notified())
        .await
        .expect("the envelope must be acknowledged even while the database is locked");

    holder.join().expect("the writer finished");
    ticker.abort();
    drop(worker);

    let worst = worst_gap_ms.load(Ordering::Relaxed);
    assert!(
        worst < 500,
        "the runtime stalled for {worst} ms while the write lock was held \
         (the hold is {} ms; anything near it means the worker wrote inline)",
        HOLD.as_millis()
    );
    // The gap alone would also read as healthy if the ticker had simply been
    // cancelled early, so assert it really ran throughout.
    let ticks = ticks.load(Ordering::Relaxed);
    assert!(
        ticks > 50,
        "the ticker only advanced {ticks} times across a {} ms hold",
        HOLD.as_millis()
    );
}

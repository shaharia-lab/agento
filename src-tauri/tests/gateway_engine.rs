//! The LLM gateway's engine, driven end to end over real HTTP (#424).
//!
//! # What a fake upstream buys that a unit test cannot
//!
//! Everything this suite asserts is a property of a *sequence* or of a *byte
//! stream*: the order Anthropic's SSE events arrive in, that exactly one
//! `data: [DONE]` terminates an OpenAI stream, that a 429 costs two upstream
//! calls and a 400 costs one, that a disconnect stops the upstream request
//! rather than leaving it running. None of those is visible from inside a
//! function — they are visible from the socket, and from what the upstream was
//! asked for.
//!
//! So the shape is `tests/claude_sdk.rs`'s, one layer up: instead of a scripted
//! fake `claude` CLI behind a subprocess, a scripted fake **provider** behind an
//! HTTP port, which records every request it receives and replies to order. The
//! gateway is pointed at it through an ordinary `gateway_providers` row, so
//! nothing under test is stubbed — the real router, the real auth layer, the
//! real `ferrox-providers` adapters and the real translation all run.
//!
//! It is in-process `axum` rather than a Python script for one reason the CLI
//! tests could not use: there is no executable to fork, so there is no
//! `python3` to skip on and no `ETXTBSY` race between writing the script and
//! running it.
//!
//! # The process-wide keypair
//!
//! `security::keys` is a process-global `RwLock`, and `cargo test` runs a
//! binary's tests in parallel. Every test here that needs a credential goes
//! through [`installed_keypair`], which installs exactly once — the shape
//! `tests/jwks_external_verify.rs` records, and the hazard the port has been
//! bitten by twice.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use agento_lib::gateway::config::{
    self, GatewaySettings, ModelAlias, ProviderInput, ProviderType, RouteTarget, Routing, Timeouts,
};
use agento_lib::gateway::{dispatch::Dispatcher, registry, server};
use agento_lib::native::security::keys::Keypair;
use agento_lib::native::security::token::{self, Scope};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use tempfile::NamedTempFile;

// ── The scripted fake upstream ───────────────────────────────────────────────

/// One reply the fake provider will give, in order.
#[derive(Clone)]
enum Reply {
    /// A non-streaming answer with this status and body.
    Json { status: u16, body: String },
    /// An OpenAI SSE stream: each entry becomes one `data:` line, terminated by
    /// the upstream's own `data: [DONE]`.
    Sse { chunks: Vec<String> },
    /// An SSE stream that emits its chunks and then never ends. Used to park a
    /// stream so a client can disconnect while it is open.
    SseThenHang { chunks: Vec<String> },
}

impl Reply {
    fn ok(body: &str) -> Self {
        Self::Json {
            status: 200,
            body: body.to_string(),
        }
    }
    fn status(status: u16) -> Self {
        Self::Json {
            status,
            body: format!(r#"{{"error":{{"message":"upstream said {status}"}}}}"#),
        }
    }
}

#[derive(Clone)]
struct FakeState {
    script: Arc<Mutex<VecDeque<Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Set when a [`Reply::SseThenHang`] body's receiver is dropped — i.e. when
    /// the upstream connection this fake is holding open is finally torn down.
    ///
    /// This is the *only* observable proof that a client disconnect propagated
    /// all the way through the gateway to the provider; everything closer to
    /// the client is true whether or not the teardown happened.
    hung_up: Arc<tokio::sync::Notify>,
}

/// What the upstream was actually asked for, which is half of every assertion.
#[derive(Clone, Debug)]
struct Seen {
    authorization: Option<String>,
    body: String,
}

/// A fake provider on a loopback port. Dropping it stops the listener.
struct Fake {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    hung_up: Arc<tokio::sync::Notify>,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl Fake {
    /// The `base_url` a `gateway_providers` row points at it with.
    ///
    /// The version segment is the caller's, not the adapter's: ferrox's OpenAI
    /// adapter appends only `/chat/completions`.
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    fn calls(&self) -> Vec<Seen> {
        self.seen.lock().expect("fake lock").clone()
    }
}

async fn fake_upstream(script: Vec<Reply>) -> Fake {
    let state = FakeState {
        script: Arc::new(Mutex::new(script.into())),
        seen: Arc::new(Mutex::new(Vec::new())),
        hung_up: Arc::new(tokio::sync::Notify::new()),
    };
    let seen = Arc::clone(&state.seen);
    let hung_up = Arc::clone(&state.hung_up);

    let app = axum::Router::new()
        .route("/v1/chat/completions", post(fake_handler))
        // A real provider accepts bodies far larger than `axum`'s 2 MiB
        // default, and this fixture has to as well — otherwise the fake
        // refuses a large request the gateway correctly forwarded, and the
        // resulting `413` reads exactly like the gateway's own limit. It is
        // distinguishable only by the error body's `"type":"provider_error"`,
        // which is how this was diagnosed.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind fake upstream");
    let port = listener.local_addr().expect("fake addr").port();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    Fake {
        port,
        seen,
        hung_up,
        _shutdown: tx,
    }
}

async fn fake_handler(
    State(state): State<FakeState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    state.seen.lock().expect("fake lock").push(Seen {
        authorization: headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body: String::from_utf8_lossy(&body).into_owned(),
    });

    let reply = state.script.lock().expect("fake lock").pop_front();
    match reply {
        Some(Reply::Json { status, body }) => (
            axum::http::StatusCode::from_u16(status).expect("valid status"),
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Some(Reply::Sse { chunks }) => sse(chunks, None),
        Some(Reply::SseThenHang { chunks }) => sse(chunks, Some(state.hung_up)),
        // Running off the end of the script is a test bug, and a 500 would be
        // retried — which turns "one call too many" into a hang. 400 is not
        // retryable, so it surfaces immediately.
        None => (
            axum::http::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"fake upstream: script exhausted"}}"#,
        )
            .into_response(),
    }
}

fn sse(chunks: Vec<String>, hang_until_dropped: Option<Arc<tokio::sync::Notify>>) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::convert::Infallible>>(16);
    tokio::spawn(async move {
        for chunk in chunks {
            if tx.send(Ok(format!("data: {chunk}\n\n"))).await.is_err() {
                return;
            }
        }
        if let Some(hung_up) = hang_until_dropped {
            // Held open until the receiver is dropped, which is what a client
            // disconnect must cause all the way up the chain — and the notify
            // is the signal that it did.
            tx.closed().await;
            // `notify_one`, not `notify_waiters`: the latter wakes only waiters
            // already registered, so a test that has not yet reached its await
            // loses the notification and times out. `notify_one` stores a
            // permit instead, which removes the race rather than narrowing it.
            hung_up.notify_one();
            return;
        }
        let _ = tx.send(Ok("data: [DONE]\n\n".to_string())).await;
    });

    Response::builder()
        .status(200)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .body(axum::body::Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
        .expect("fake sse response")
}

// ── The gateway under test ───────────────────────────────────────────────────

/// A migrated database with the given providers and one alias over them.
///
/// `targets` are `(provider name, upstream model id)` in preference order; the
/// first is the primary and the rest are the fallback chain, which is exactly
/// how [`Routing`] stores them.
fn seeded_db(providers: &[(&str, &str)], alias: &str, targets: &[(&str, &str)]) -> NamedTempFile {
    let file = NamedTempFile::new().expect("tempfile");
    {
        let mut conn =
            agento_lib::native::db::ensure_database(file.path()).expect("create database");
        agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    }

    for (name, base_url) in providers {
        config::store_provider(
            file.path(),
            &ProviderInput {
                id: name,
                name,
                provider_type: ProviderType::Openai,
                api_key: "upstream-key",
                base_url,
                timeouts: Timeouts::default(),
                enabled: true,
            },
        )
        .expect("store provider");
    }

    let mut rows = targets.iter().map(|(provider, model_id)| RouteTarget {
        provider: (*provider).to_string(),
        model_id: (*model_id).to_string(),
    });
    let first = rows.next().expect("at least one target");
    config::store_alias(
        file.path(),
        &ModelAlias {
            id: alias.to_string(),
            alias: alias.to_string(),
            routing: Routing {
                targets: vec![first],
                fallbacks: rows.collect(),
            },
            enabled: true,
        },
    )
    .expect("store alias");

    file
}

/// The gateway's own listener, serving the real router on a loopback port.
struct Gateway {
    port: u16,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl Gateway {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

/// Serve [`server::router`] directly, on an OS-assigned port.
///
/// `registry::start_if_enabled` is exercised by its own two tests below; every
/// other test wants a port it cannot collide on, and `gateway_settings.port`
/// deliberately refuses `0` (a gateway on a port nobody configured is one every
/// tool is pointed away from).
async fn gateway_for(db: &NamedTempFile) -> Gateway {
    let dispatcher = Arc::new(
        Dispatcher::build(db.path())
            .await
            .expect("build the dispatcher"),
    );
    let app = server::router(server::GatewayState {
        db_path: db.path().to_path_buf(),
        dispatcher,
    });

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind gateway");
    let port = listener.local_addr().expect("gateway addr").port();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    Gateway {
        port,
        _shutdown: tx,
    }
}

/// The process-wide keypair, installed exactly once for this test binary.
fn installed_keypair() -> &'static Arc<Keypair> {
    static KEYPAIR: OnceLock<Arc<Keypair>> = OnceLock::new();
    KEYPAIR.get_or_init(|| {
        let keypair = Keypair::generate().expect("generate a keypair");
        agento_lib::native::security::keys::install(keypair)
    })
}

fn token_with(scope: Scope) -> String {
    token::mint(installed_keypair(), "test-token", scope, 3600)
        .expect("mint")
        .token
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("client")
}

/// The whole SSE body as text.
async fn body_text(response: reqwest::Response) -> String {
    response.text().await.expect("read body")
}

/// The `event:` names of an Anthropic SSE body, in order.
fn event_names(body: &str) -> Vec<&str> {
    body.lines()
        .filter_map(|line| line.strip_prefix("event: "))
        .collect()
}

/// One minimal OpenAI streaming chunk carrying `text`.
fn chunk(text: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1735000000,"model":"m","choices":[{{"index":0,"delta":{{"content":"{text}"}},"finish_reason":null}}]}}"#
    )
}

/// The final chunk: a stop, with usage attached, as ferrox's own fixture shows.
fn final_chunk() -> String {
    r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1735000000,"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":15,"completion_tokens":2,"total_tokens":17}}"#.to_string()
}

/// A non-streaming OpenAI response body, from `ferrox/docs/user/api-reference.md`.
fn completion_body() -> String {
    r#"{"id":"chatcmpl-abc123","object":"chat.completion","created":1735000000,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"Hello! How can I help you today?"},"finish_reason":"stop"}],"usage":{"prompt_tokens":15,"completion_tokens":12,"total_tokens":27}}"#.to_string()
}

// ── Streaming ────────────────────────────────────────────────────────────────

/// The OpenAI surface's terminator, and that there is exactly one of it.
///
/// "Ends with `[DONE]`" is not the assertion — a stream that emitted the
/// terminator after every chunk would pass that, and every SDK would report the
/// first token as the whole answer. The count is the property.
#[tokio::test]
async fn an_openai_stream_ends_with_exactly_one_done() {
    let upstream = fake_upstream(vec![Reply::Sse {
        chunks: vec![chunk("Hello"), chunk("!"), final_chunk()],
    }])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );

    let body = body_text(response).await;
    assert_eq!(
        body.matches("data: [DONE]\n\n").count(),
        1,
        "exactly one terminator, not one per chunk; got:\n{body}"
    );
    assert!(
        body.ends_with("data: [DONE]\n\n"),
        "and it is last; got:\n{body}"
    );
    assert!(body.contains(r#""content":"Hello""#), "got:\n{body}");

    // The alias is what the client sent; `upstream-model` is what the upstream
    // was asked for. Sending the alias upstream is the failure this catches.
    let call = &upstream.calls()[0];
    assert!(
        call.body.contains(r#""model":"upstream-model""#),
        "the target's model_id goes upstream, not the alias; got: {}",
        call.body
    );
    assert_eq!(
        call.authorization.as_deref(),
        Some("Bearer upstream-key"),
        "the provider row's key reaches the provider, not the client's token"
    );
}

/// The Anthropic surface's event sequence.
///
/// The state machine belongs to `ferrox-providers`; what is asserted here is
/// that this gateway drives it and writes its frames out in order, with the
/// `event:` name on every one — a `data:`-only stream parses as nothing to an
/// Anthropic SDK.
#[tokio::test]
async fn the_anthropic_surface_emits_its_frames_in_protocol_order() {
    let upstream = fake_upstream(vec![Reply::Sse {
        chunks: vec![chunk("Hello"), chunk(" there"), final_chunk()],
    }])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/anthropic/v1/messages"))
        // The Anthropic SDK's header spelling, deliberately: this is the one
        // path where using only `Authorization` would still pass every other
        // test here and fail against Claude Code.
        .header("x-api-key", token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200);

    let body = body_text(response).await;
    let names = event_names(&body);

    assert_eq!(names.first(), Some(&"message_start"), "got:\n{body}");
    assert_eq!(names.last(), Some(&"message_stop"), "got:\n{body}");

    // The order that matters, with the deltas collapsed: a stop before its
    // start, or a message_delta after message_stop, is a stream an SDK
    // abandons.
    let skeleton: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| *n != "content_block_delta" && *n != "ping")
        .collect();
    assert_eq!(
        skeleton,
        vec![
            "message_start",
            "content_block_start",
            "content_block_stop",
            "message_delta",
            "message_stop"
        ],
        "got the full sequence {names:?} from:\n{body}"
    );
    assert!(
        names
            .iter()
            .filter(|n| **n == "content_block_delta")
            .count()
            >= 2,
        "both text chunks should have produced a delta; got {names:?}"
    );

    // Every frame carries both lines. An `event:` with no `data:` is not a
    // frame the Anthropic SDK will deliver.
    assert_eq!(
        body.matches("event: ").count(),
        body.matches("\ndata: ").count(),
        "every event line needs its data line; got:\n{body}"
    );
    assert!(
        !body.contains("[DONE]"),
        "`[DONE]` is the OpenAI surface's terminator and has no meaning here"
    );
}

/// A non-streaming completion on each surface, translated back into the
/// caller's own shape.
#[tokio::test]
async fn a_non_streaming_completion_answers_in_the_callers_shape() {
    let upstream = fake_upstream(vec![
        Reply::ok(&completion_body()),
        Reply::ok(&completion_body()),
    ])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let openai: serde_json::Value = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(openai["object"], "chat.completion");
    assert_eq!(openai["choices"][0]["message"]["role"], "assistant");
    assert_eq!(openai["usage"]["total_tokens"], 27);

    let anthropic: serde_json::Value = client()
        .post(gateway.url("/anthropic/v1/messages"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(anthropic["type"], "message");
    assert_eq!(anthropic["content"][0]["type"], "text");
    // OpenAI's `stop` is Anthropic's `end_turn`; a passthrough here is a value
    // the Anthropic SDK does not recognise.
    assert_eq!(anthropic["stop_reason"], "end_turn");
    assert_eq!(anthropic["usage"]["input_tokens"], 15);
}

// ── Retry and failover ───────────────────────────────────────────────────────

/// A 429 is retried on the same target, and then the chain is walked.
///
/// Both halves are asserted by *counting upstream calls*, which is the only
/// place the difference is observable: a gateway that failed over without
/// retrying, and one that retried without failing over, both answer 200 here.
#[tokio::test]
async fn a_429_retries_the_same_target_and_then_walks_to_the_next() {
    let primary = fake_upstream(vec![
        Reply::status(429),
        Reply::status(429),
        Reply::status(429),
    ])
    .await;
    let fallback = fake_upstream(vec![Reply::ok(&completion_body())]).await;

    let db = seeded_db(
        &[("p1", &primary.base_url()), ("p2", &fallback.base_url())],
        "my-alias",
        &[("p1", "primary-model"), ("p2", "fallback-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200, "the fallback served it");

    assert_eq!(
        primary.calls().len(),
        3,
        "a 429 is retryable, so the primary is asked `max_attempts` times"
    );
    assert_eq!(
        fallback.calls().len(),
        1,
        "and then the chain is walked exactly once"
    );
    assert!(
        fallback.calls()[0]
            .body
            .contains(r#""model":"fallback-model""#),
        "the fallback is asked for *its* model id, not the primary's"
    );
}

/// A 400 does neither — one call, and the client's own mistake comes straight
/// back.
///
/// This is the arm a "retry everything" implementation gets wrong for free: it
/// would triple every malformed request against the primary and then triple it
/// again against the fallback, spending six upstream calls on a body no
/// provider will ever accept.
#[tokio::test]
async fn a_400_is_neither_retried_nor_failed_over() {
    let primary = fake_upstream(vec![Reply::status(400); 4]).await;
    let fallback = fake_upstream(vec![Reply::ok(&completion_body())]).await;

    let db = seeded_db(
        &[("p1", &primary.base_url()), ("p2", &fallback.base_url())],
        "my-alias",
        &[("p1", "primary-model"), ("p2", "fallback-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 400, "the upstream's status is preserved");

    assert_eq!(primary.calls().len(), 1, "asked exactly once");
    assert_eq!(
        fallback.calls().len(),
        0,
        "and the fallback's credentials are never spent on it"
    );
}

/// Retry and failover work on the **streaming** path too, and that is not
/// implied by the non-streaming tests.
///
/// It holds only because `chat_stream` checks the upstream status *before*
/// handing back a stream (`ferrox-providers`' adapters all
/// `return Err(ProviderError { status })` on a 4xx/5xx), so a failed handshake
/// is an ordinary `Err` the retry loop can see. An adapter that instead
/// returned `Ok` with a stream that errored on first poll would make every
/// assertion below silently false — the head would already be committed, and a
/// rate-limited streaming request would reach the client as a broken stream
/// rather than being served by the fallback. Streaming is also where a 429 is
/// *most* likely, so this is the path that matters.
#[tokio::test]
async fn a_streaming_request_retries_and_fails_over_the_same_way() {
    let primary = fake_upstream(vec![Reply::status(429); 3]).await;
    let fallback = fake_upstream(vec![Reply::Sse {
        chunks: vec![chunk("Hello"), final_chunk()],
    }])
    .await;

    let db = seeded_db(
        &[("p1", &primary.base_url()), ("p2", &fallback.base_url())],
        "my-alias",
        &[("p1", "primary-model"), ("p2", "fallback-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(
        response.status(),
        200,
        "the fallback served it, and the status is still open to be set"
    );

    let body = body_text(response).await;
    assert!(body.contains(r#""content":"Hello""#), "got:\n{body}");
    assert_eq!(
        body.matches("data: [DONE]\n\n").count(),
        1,
        "the failed-over stream is still terminated exactly once; got:\n{body}"
    );

    assert_eq!(primary.calls().len(), 3, "the handshake is retried");
    assert_eq!(fallback.calls().len(), 1, "then the chain is walked");
}

/// An upstream **403** is the asymmetric case, and the reason two predicates
/// exist: quota exhaustion reported as a 403 must not be retried against the
/// provider that is out of quota, and must still be served from the next one.
#[tokio::test]
async fn an_upstream_403_fails_over_without_retrying() {
    let primary = fake_upstream(vec![Reply::status(403); 4]).await;
    let fallback = fake_upstream(vec![Reply::ok(&completion_body())]).await;

    let db = seeded_db(
        &[("p1", &primary.base_url()), ("p2", &fallback.base_url())],
        "my-alias",
        &[("p1", "primary-model"), ("p2", "fallback-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200, "the fallback served it");

    assert_eq!(
        primary.calls().len(),
        1,
        "no backoff is spent on a provider known to be out of quota"
    );
    assert_eq!(fallback.calls().len(), 1);
}

// ── Auth ─────────────────────────────────────────────────────────────────────

/// The scope gate, on both surfaces and both header spellings.
///
/// The `read`/`write` rows are the disjointness #423 built: an `/api`
/// credential must not reach a surface that spends provider credits, and the
/// *false* cells are the assertion — a lattice with a wildcard arm passes every
/// positive test while granting exactly this.
#[tokio::test]
async fn only_an_llm_scoped_token_opens_either_surface() {
    let upstream = fake_upstream(vec![Reply::ok(&completion_body()); 4]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;
    let client = client();

    for path in ["/v1/models", "/anthropic/v1/models"] {
        // No credential at all.
        let response = client.get(gateway.url(path)).send().await.expect("send");
        assert_eq!(response.status(), 401, "{path} with no credential");

        // A credential that verifies, with a scope that does not cover this.
        for scope in [Scope::Read, Scope::Write] {
            let response = client
                .get(gateway.url(path))
                .bearer_auth(token_with(scope))
                .send()
                .await
                .expect("send");
            assert_eq!(
                response.status(),
                403,
                "{path} with a {scope:?} token must be 403, not 401 — it verified"
            );
        }

        // Garbage that does not verify.
        let response = client
            .get(gateway.url(path))
            .bearer_auth("not-a-jwt")
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 401, "{path} with a malformed credential");

        // Both spellings of the one that works.
        for response in [
            client
                .get(gateway.url(path))
                .bearer_auth(token_with(Scope::Llm))
                .send()
                .await
                .expect("send"),
            client
                .get(gateway.url(path))
                .header("x-api-key", token_with(Scope::Llm))
                .send()
                .await
                .expect("send"),
        ] {
            assert_eq!(response.status(), 200, "{path} with an llm token");
        }
    }

    // The two completion routes, which are the ones that actually spend money —
    // the layer covers all four, but asserting only on the `GET`s would leave
    // the expensive pair resting on that being true rather than checked.
    for (path, body) in [
        (
            "/v1/chat/completions",
            serde_json::json!({"model": "my-alias", "messages": [{"role": "user", "content": "hi"}]}),
        ),
        (
            "/anthropic/v1/messages",
            serde_json::json!({"model": "my-alias", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}),
        ),
    ] {
        let response = client
            .post(gateway.url(path))
            .json(&body)
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 401, "{path} with no credential");

        let response = client
            .post(gateway.url(path))
            .bearer_auth(token_with(Scope::Write))
            .json(&body)
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 403, "{path} with a write token");
    }

    assert_eq!(
        upstream.calls().len(),
        0,
        "a refused request must never reach the provider"
    );
}

/// A denial is written in the dialect the client's SDK parses.
///
/// Agento's own `{"error":"..."}` is a shape neither SDK can read, and an
/// Anthropic client branches on `error.type` to decide whether to retry — so
/// `authentication_error` versus `permission_error` is behaviour, not wording.
#[tokio::test]
async fn a_denial_is_shaped_for_the_surface_it_was_refused_on() {
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;
    let client = client();

    let openai: serde_json::Value = client
        .get(gateway.url("/v1/models"))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(openai["error"]["type"], "unauthorized");
    assert_eq!(openai["error"]["code"], 401);

    let anthropic: serde_json::Value = client
        .get(gateway.url("/anthropic/v1/models"))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(anthropic["type"], "error");
    assert_eq!(anthropic["error"]["type"], "authentication_error");

    let anthropic_403: serde_json::Value = client
        .get(gateway.url("/anthropic/v1/models"))
        .bearer_auth(token_with(Scope::Write))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(anthropic_403["error"]["type"], "permission_error");
}

/// `/healthz` answers with no credential, and is the only route that does.
#[tokio::test]
async fn healthz_needs_no_credential() {
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .get(gateway.url("/healthz"))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200);
    assert_eq!(body_text(response).await, "ok");
}

/// A foreign `Host` is refused before anything else looks at the request.
///
/// This is the half of the no-CORS decision that does the work: the browser is
/// already shut out of a `POST` carrying `Authorization` by preflight, but a
/// DNS-rebinding page reaches a loopback port with a *simple* request and its
/// own `Host`. Refusing that is what makes the missing `CorsLayer` sufficient
/// rather than merely absent — and `/healthz`, which needs no credential, is
/// inside this layer for exactly that reason.
#[tokio::test]
async fn a_foreign_host_header_is_refused_on_every_route() {
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;
    let client = client();

    for path in ["/healthz", "/v1/models", "/anthropic/v1/models"] {
        let response = client
            .get(gateway.url(path))
            .header(reqwest::header::HOST, "evil.example.com")
            .bearer_auth(token_with(Scope::Llm))
            .send()
            .await
            .expect("send");
        assert_eq!(
            response.status(),
            403,
            "{path} must refuse a Host this listener is not reachable at"
        );
    }

    // ...and `localhost`, which is what a browser on the dev origin sends, is
    // still allowed — the allowlist is loopback, not a single literal.
    let response = client
        .get(gateway.url("/healthz"))
        .header(reqwest::header::HOST, format!("localhost:{}", gateway.port))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200);
}

// ── Configuration failures ───────────────────────────────────────────────────

/// An unknown alias, and an alias whose providers are all disabled, are
/// different answers — and neither is an empty 200.
#[tokio::test]
async fn an_unroutable_request_gets_a_typed_error_rather_than_an_empty_answer() {
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "no-such-alias",
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(
        response.status(),
        404,
        "the client named a model we do not have"
    );
    let body: serde_json::Value = response.json().await.expect("json");
    assert_eq!(body["error"]["type"], "model_not_found");
    assert_eq!(upstream.calls().len(), 0);
}

/// A gateway with no provider configured says so, rather than answering an
/// empty models list a client cannot tell from "you have no models".
#[tokio::test]
async fn the_models_routes_refuse_a_gateway_with_no_provider() {
    let file = NamedTempFile::new().expect("tempfile");
    {
        let mut conn =
            agento_lib::native::db::ensure_database(file.path()).expect("create database");
        agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    }
    let gateway = gateway_for(&file).await;

    // The two dialects spell the same `ProxyError::ConfigError` differently —
    // OpenAI has a `config_error` type and Anthropic's vocabulary does not, so
    // it lands on `api_error`. Both are 500, and both are something a client
    // can surface; `{"data":[]}` is neither.
    for (path, expected_type) in [
        ("/v1/models", "config_error"),
        ("/anthropic/v1/models", "api_error"),
    ] {
        let response = client()
            .get(gateway.url(path))
            .bearer_auth(token_with(Scope::Llm))
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 500, "{path}");
        let body: serde_json::Value = response.json().await.expect("json");
        assert_eq!(
            body["error"]["type"], expected_type,
            "{path} answered {body}"
        );
    }
}

/// The aliases a configured gateway lists, on both surfaces.
#[tokio::test]
async fn the_models_routes_list_the_configured_aliases() {
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let openai: serde_json::Value = client()
        .get(gateway.url("/v1/models"))
        .bearer_auth(token_with(Scope::Llm))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(openai["object"], "list");
    assert_eq!(openai["data"][0]["id"], "my-alias");
    assert_eq!(openai["data"][0]["object"], "model");

    let anthropic: serde_json::Value = client()
        .get(gateway.url("/anthropic/v1/models"))
        .bearer_auth(token_with(Scope::Llm))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(anthropic["data"][0]["type"], "model");
    assert_eq!(anthropic["data"][0]["id"], "my-alias");
    assert_eq!(anthropic["has_more"], false);
    assert_eq!(anthropic["first_id"], "my-alias");
}

// ── Disconnect ───────────────────────────────────────────────────────────────

/// A client that disconnects mid-stream must take the upstream request with it.
///
/// The failure this catches is silent and unbounded: the frame loop lives in a
/// spawned task holding the upstream response, so a client that goes away
/// leaves that task blocked on `tx.send` forever, with the provider connection
/// open. One abandoned tool invocation would be invisible; a tool that
/// reconnects on every timeout leaks one per attempt.
///
/// The assertion is on the **upstream** side, because that is the only place
/// the leak is observable — a test that only checked the gateway's own response
/// passes with the whole thing reverted. `tx.closed()` in the fake resolves
/// exactly when its receiver is dropped, which happens only if the chain from
/// the disconnected client all the way to the upstream body was torn down.
#[tokio::test]
async fn a_disconnect_mid_stream_tears_down_the_upstream_request() {
    let upstream = fake_upstream(vec![Reply::SseThenHang {
        chunks: vec![chunk("Hello")],
    }])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let hung_up = Arc::clone(&upstream.hung_up);

    let mut response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200);

    // Read until a frame has actually arrived, *then* disconnect. Dropping
    // immediately would tear down before the stream was ever established, which
    // passes whether or not the disconnect is handled — the same trap
    // `chat_turn.rs`'s disconnect tests document.
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), response.chunk())
        .await
        .expect("a first frame within 10s")
        .expect("chunk")
        .expect("some bytes");
    assert!(String::from_utf8_lossy(&first).contains("data: "));

    drop(response);

    // The upstream is parked on `tx.closed()`, which resolves only when its own
    // body's receiver is dropped — and that happens only if the disconnect
    // propagated from the client, through the gateway's frame loop, into the
    // upstream response it was holding. If any link in that chain leaks, this
    // never fires and the timeout reports it as a failure rather than a hang.
    //
    // No arming is needed before the drop: the fake signals with `notify_one`,
    // which stores a permit when nobody is waiting yet.
    tokio::time::timeout(std::time::Duration::from_secs(10), hung_up.notified())
        .await
        .expect("the client disconnect must reach the upstream request");

    assert_eq!(
        upstream.calls().len(),
        1,
        "and the abandoned stream must not have been retried"
    );
}

// ── Lifecycle ────────────────────────────────────────────────────────────────

/// Serialises the three tests below.
///
/// `gateway::registry` is a process-wide `OnceLock` — one listener, one status
/// — and `cargo test` runs a binary's tests in parallel, so two of these
/// running at once would each assert against the other's status. That is the
/// hazard `security::keys` has already cost this port twice, and it fails
/// intermittently in whichever test lost, which is worse than not asserting at
/// all. Every test that touches the global takes this lock and leaves the
/// registry stopped.
fn registry_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// A disabled gateway starts nothing and says so.
#[tokio::test]
async fn a_disabled_gateway_binds_nothing() {
    let _guard = registry_lock().lock().await;
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    // `enabled: false` is the default, so this only makes it explicit.
    config::store_settings(
        db.path(),
        &GatewaySettings {
            enabled: false,
            port: free_port().await,
            start_with_app: true,
            ..Default::default()
        },
    )
    .expect("store settings");

    registry::start_if_enabled(db.path())
        .await
        .expect("start_if_enabled");
    assert_eq!(registry::status(), registry::Status::Stopped);
    registry::stop();
}

/// A port already in use leaves a **readable** status, not a log line.
///
/// This is the collision a developer hits every day — an installed Agento and a
/// `~/.agento-desktop-dev` one configured for the same port — and #426's status
/// route is what turns it into something the UI can explain. Without a stored
/// status the second instance reports "not running" and offers a Start button
/// that silently does nothing.
#[tokio::test]
async fn a_port_already_in_use_is_a_readable_status_rather_than_silence() {
    let _guard = registry_lock().lock().await;
    let db = seeded_db(&[("p1", "http://127.0.0.1:1/v1")], "a", &[("p1", "m")]);

    // Hold the port for the whole test, so the bind cannot succeed.
    let squatter = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind squatter");
    let port = squatter.local_addr().expect("addr").port();

    config::store_settings(
        db.path(),
        &GatewaySettings {
            enabled: true,
            port,
            start_with_app: true,
            ..Default::default()
        },
    )
    .expect("store settings");

    registry::start_if_enabled(db.path())
        .await
        .expect("a bind failure is an answer, not an Err");

    match registry::status() {
        registry::Status::BindFailed {
            port: reported,
            error,
        } => {
            assert_eq!(reported, port);
            assert!(
                !error.is_empty(),
                "the status has to say *why*, or it is no better than Stopped"
            );
        }
        other => panic!("expected BindFailed, got {other:?}"),
    }

    registry::stop();
    drop(squatter);
}

/// A provider this build cannot turn into an adapter is `StartFailed`, not
/// `BindFailed`.
///
/// Reachable from a typo, which is why the two are separate variants: ferrox's
/// OpenAI adapter refuses a `base_url` ending in `/chat/completions` (it would
/// build `…/chat/completions/chat/completions`), and reporting that as a bind
/// failure would send the user to look at a port nothing tried to bind.
#[tokio::test]
async fn a_provider_that_cannot_be_built_is_not_reported_as_a_port_collision() {
    let _guard = registry_lock().lock().await;
    let db = seeded_db(
        &[("p1", "http://127.0.0.1:9/v1/chat/completions")],
        "a",
        &[("p1", "m")],
    );
    config::store_settings(
        db.path(),
        &GatewaySettings {
            enabled: true,
            port: free_port().await,
            start_with_app: true,
            ..Default::default()
        },
    )
    .expect("store settings");

    registry::start_if_enabled(db.path())
        .await
        .expect("a misconfigured provider is an answer, not an Err");

    match registry::status() {
        registry::Status::StartFailed { error } => assert!(
            error.contains("p1"),
            "the status must name the provider to fix; got {error:?}"
        ),
        other => panic!("expected StartFailed, got {other:?}"),
    }

    registry::stop();
}

/// The whole lifecycle: start from settings, serve, reload, stop.
///
/// The reload half is what makes a provider edit take effect — the adapter
/// registry is built once per start, so a gateway that never rebuilt it would
/// go on using the API key the user just rotated away.
#[tokio::test]
async fn the_listener_starts_reloads_and_stops() {
    let _guard = registry_lock().lock().await;
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let port = free_port().await;
    config::store_settings(
        db.path(),
        &GatewaySettings {
            enabled: true,
            port,
            start_with_app: true,
            ..Default::default()
        },
    )
    .expect("store settings");

    registry::start_if_enabled(db.path())
        .await
        .expect("start_if_enabled");
    assert_eq!(registry::status(), registry::Status::Running { port });

    let health = format!("http://127.0.0.1:{port}/healthz");
    assert_eq!(
        client().get(&health).send().await.expect("send").status(),
        200
    );

    // A reload is stop-then-start on the same port. It must come back.
    registry::reload(db.path()).await.expect("reload");
    assert_eq!(registry::status(), registry::Status::Running { port });
    assert_eq!(
        client().get(&health).send().await.expect("send").status(),
        200
    );

    registry::stop();
    assert_eq!(registry::status(), registry::Status::Stopped);
    // Poll rather than sleep a fixed amount: the graceful shutdown is
    // asynchronous, and a fixed wait is either flaky or slow.
    let mut gone = false;
    for _ in 0..200 {
        if client().get(&health).send().await.is_err() {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(gone, "a stopped gateway must stop answering");
}

/// An OS-assigned port, released before it is configured.
///
/// `gateway_settings.port` refuses `0`, so a test that wants a port it cannot
/// collide on has to pick one. There is a race here in principle — nothing
/// stops another process taking it in between — and it is accepted because the
/// alternative is a hardcoded port that collides with the developer's own
/// running Agento every time.
async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind for a free port");
    listener.local_addr().expect("addr").port()
}

// ── Startup ordering ─────────────────────────────────────────────────────────

/// The gateway may not start before the credential system it verifies against.
///
/// **This is a source assertion, and deliberately so.** The property is about
/// the order of three calls in one `setup` closure that also creates a database,
/// binds the proxy and opens a window — there is no seam to drive it through,
/// and the runtime consequence is a *window* rather than a state: a listener
/// bound before `tokens::load_revoked` accepts a revoked token for however long
/// the load takes, and then behaves correctly forever. A test that asked the
/// running app would have to win that race to see anything.
///
/// What it costs to get wrong is why it is asserted at all. Bound before
/// `keys::install`, every client gets a 401 until the key lands — visible, and
/// merely broken. Bound before `load_revoked`, a token the user revoked is
/// **honoured**, and nothing anywhere reports it.
#[test]
fn a_gateway_start_requires_an_installed_keypair() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("read lib.rs");

    let install = source
        .find("security::keys::install")
        .expect("lib.rs must install the signing keypair");
    let revoked = source
        .find("security::tokens::load_revoked")
        .expect("lib.rs must load the revoked token set");
    let gateway = source
        .find("gateway::registry::start_if_enabled")
        .expect("lib.rs must start the gateway");

    assert!(
        install < gateway,
        "the gateway must not be started before the signing keypair is installed"
    );
    assert!(
        revoked < gateway,
        "the gateway must not be started before the revoked-token set is loaded, \
         or it accepts a revoked token for the length of that window"
    );
}

/// A large request body must reach the upstream rather than being refused.
///
/// `axum` applies a **2 MiB default body limit** to `Bytes` and `Json`
/// extractors. That is a sensible default for an API whose bodies are forms;
/// it is the wrong one for the endpoint Claude Code and the OpenAI SDKs are
/// pointed at, where the body is an entire conversation resent on every turn.
/// A long coding session, or one pasted screenshot at base64's 4/3 expansion,
/// crosses it — and the refusal is `axum`'s own bare `413`, in neither
/// surface's dialect, so the client reports nothing useful.
#[tokio::test]
async fn a_large_request_body_is_not_refused() {
    let upstream = fake_upstream(vec![Reply::ok(&completion_body())]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    // ~3 MiB of conversation: over axum's default, well under anything a real
    // client would consider unusual.
    let big = "x".repeat(3 * 1024 * 1024);
    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": big}],
        }))
        .send()
        .await
        .expect("send");

    // The body is printed on failure because the two limits that could produce
    // a 413 here — this gateway's and the fake upstream's — are otherwise
    // indistinguishable, and only the error body's `type` tells them apart.
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert_eq!(
        status, 200,
        "a 3 MiB conversation is an ordinary request, not an oversized one; body={body}"
    );
    assert_eq!(upstream.calls().len(), 1, "and it reached the provider");
}

/// A reload while a stream is in flight must still come back on the same port.
///
/// This is the interaction between the two properties this module wants at
/// once, and they pull against each other. Shutdown is **graceful**, so a
/// client mid-completion when the user saves the settings form keeps its
/// stream — that is the whole reason the oneshot exists. But `reload` then
/// rebinds the **same, user-configured** port, and if the draining listener
/// still owns that socket the new bind gets `EADDRINUSE`.
///
/// The failure that would cause is the worst one this module has: the status
/// reads `BindFailed` while the *old* listener — holding the API key the user
/// just rotated away — goes on serving until its stream ends. A reload that
/// cannot rebind is a reload that did not revoke anything.
///
/// A quiet reload cannot catch it: with nothing in flight the drain completes
/// in microseconds and the rebind always wins the race. The stream has to be
/// open across the reload.
#[tokio::test]
async fn a_reload_during_a_stream_rebinds_the_same_port() {
    let _guard = registry_lock().lock().await;
    let upstream = fake_upstream(vec![Reply::SseThenHang {
        chunks: vec![chunk("Hello")],
    }])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let port = free_port().await;
    config::store_settings(
        db.path(),
        &GatewaySettings {
            enabled: true,
            port,
            start_with_app: true,
            ..Default::default()
        },
    )
    .expect("store settings");

    registry::start_if_enabled(db.path())
        .await
        .expect("start_if_enabled");
    assert_eq!(registry::status(), registry::Status::Running { port });

    // Open a stream and read a frame, so the listener is genuinely draining
    // rather than idle when the reload lands.
    let mut streaming = client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(streaming.status(), 200);
    tokio::time::timeout(std::time::Duration::from_secs(10), streaming.chunk())
        .await
        .expect("a first frame")
        .expect("chunk")
        .expect("some bytes");

    registry::reload(db.path()).await.expect("reload");

    assert_eq!(
        registry::status(),
        registry::Status::Running { port },
        "a reload across an open stream must rebind, or the old listener keeps \
         serving with the credential the reload was meant to replace"
    );
    assert_eq!(
        client()
            .get(format!("http://127.0.0.1:{port}/healthz"))
            .send()
            .await
            .expect("send")
            .status(),
        200,
        "and the new listener must actually be answering"
    );

    // The other half, and the reason the shutdown is graceful at all: the
    // in-flight stream must have *survived* the reload rather than being cut.
    // The fake is still parked holding its connection open, so its hang-up
    // signal firing would mean the teardown reached through and killed a
    // client's completion mid-answer — which is what firing the cancellation
    // token *as* the shutdown signal does, the bug `claude/mcp.rs` records.
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            upstream.hung_up.notified()
        )
        .await
        .is_err(),
        "a reload must not tear down a stream that was already in flight"
    );

    drop(streaming);
    registry::stop();
}

// ── Usage recording (#425) ───────────────────────────────────────────────────

/// Every row in the log, oldest first, as the columns a test cares about.
#[derive(Debug, Clone, PartialEq)]
struct UsageRow {
    alias: String,
    provider: String,
    model_id: String,
    prompt: i64,
    completion: i64,
    cache_read: i64,
    cache_write: i64,
    status: String,
    streamed: bool,
    surface: String,
    token_sub: String,
    cost_usd: Option<f64>,
    unpriced: bool,
}

fn usage_rows(db: &NamedTempFile) -> Vec<UsageRow> {
    let conn = agento_lib::native::db::open_read_only(db.path()).expect("open");
    let mut stmt = conn
        .prepare(
            "SELECT alias, provider, model_id, prompt_tokens, completion_tokens, \
             cache_read_tokens, cache_write_tokens, status, streamed, surface, token_sub, \
             cost_usd, unpriced FROM gateway_usage_log ORDER BY created_at, rowid",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok(UsageRow {
                alias: r.get(0)?,
                provider: r.get(1)?,
                model_id: r.get(2)?,
                prompt: r.get(3)?,
                completion: r.get(4)?,
                cache_read: r.get(5)?,
                cache_write: r.get(6)?,
                status: r.get(7)?,
                streamed: r.get(8)?,
                surface: r.get(9)?,
                token_sub: r.get(10)?,
                cost_usd: r.get(11)?,
                unpriced: r.get(12)?,
            })
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    rows
}

/// Wait for the log to reach `want` rows, then return them.
///
/// The write is spawned and deliberately not awaited by the request, so a test
/// that read immediately would race it. Polling rather than sleeping a fixed
/// amount keeps it fast when it is fast and non-flaky when the box is loaded —
/// and it settles on `want` rather than "at least one", so a **doubled** write
/// is caught by the assertion after it rather than hidden by an early return.
async fn wait_for_rows(db: &NamedTempFile, want: usize) -> Vec<UsageRow> {
    for _ in 0..200 {
        if usage_rows(db).len() >= want {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    // A moment for a second, wrong write to land if one is coming — otherwise
    // "exactly one" would only ever be testing "at least one".
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    usage_rows(db)
}

/// The four ordinary outcomes, each producing **exactly one** row.
///
/// Asserted by count rather than by "a row exists", because both failure modes
/// are silent: a missed terminal arm writes none, and a doubled one writes two,
/// and a usage log that sometimes double-counts is worse than one that
/// sometimes misses — nothing in the numbers says which happened.
#[tokio::test]
async fn each_outcome_produces_exactly_one_row() {
    // Four requests over one gateway: streamed ok, non-streamed ok, streamed
    // upstream failure, non-streamed upstream failure. The 400s are terminal,
    // so each costs exactly one upstream call.
    let upstream = fake_upstream(vec![
        Reply::Sse {
            chunks: vec![chunk("Hi"), final_chunk()],
        },
        Reply::ok(&completion_body()),
        Reply::status(400),
        Reply::status(400),
    ])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;
    let token = token_with(Scope::Llm);

    for (streaming, path) in [
        (true, "/v1/chat/completions"),
        (false, "/v1/chat/completions"),
        (true, "/v1/chat/completions"),
        (false, "/v1/chat/completions"),
    ] {
        let response = client()
            .post(gateway.url(path))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "model": "my-alias",
                "messages": [{"role": "user", "content": "Hello"}],
                "stream": streaming,
            }))
            .send()
            .await
            .expect("send");
        // Drain the body so a streamed response actually finishes before the
        // next request goes out — otherwise the terminal arm has not run.
        let _ = body_text(response).await;
    }

    let rows = wait_for_rows(&db, 4).await;
    assert_eq!(rows.len(), 4, "four requests, four rows — got {rows:#?}");

    let statuses: Vec<&str> = rows.iter().map(|r| r.status.as_str()).collect();
    assert_eq!(
        statuses.iter().filter(|s| **s == "ok").count(),
        2,
        "the two successes; got {statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == "upstream_error").count(),
        2,
        "a 400 from every target is an upstream error, not a refusal; got {statuses:?}"
    );

    for row in &rows {
        assert_eq!(row.alias, "my-alias");
        assert_eq!(row.surface, "openai");
        assert_eq!(
            row.provider, "p1",
            "the target that answered is named, even on a failure"
        );
        assert_eq!(row.model_id, "upstream-model");
    }
    assert_eq!(
        rows.iter().filter(|r| r.streamed).count(),
        2,
        "the streamed flag has to distinguish the two shapes"
    );

    // The successful ones carry the provider's own numbers.
    let ok: Vec<&UsageRow> = rows.iter().filter(|r| r.status == "ok").collect();
    assert!(
        ok.iter().all(|r| r.prompt == 15),
        "prompt tokens come from the provider's usage field; got {ok:#?}"
    );
}

/// An unknown alias is `refused`, not an upstream error, and never reaches a
/// provider.
///
/// The distinction is what stops a misconfigured alias from reading as a
/// failing provider in every aggregate built on this table.
#[tokio::test]
async fn an_unroutable_request_is_recorded_as_refused() {
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "no-such-alias",
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 404);

    let rows = wait_for_rows(&db, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "refused");
    assert_eq!(rows[0].alias, "no-such-alias");
    assert_eq!(
        (rows[0].prompt, rows[0].completion),
        (0, 0),
        "nothing was sent, so nothing was spent"
    );
    assert_eq!(
        rows[0].provider, "",
        "no target was chosen, so none is named"
    );
    assert_eq!(upstream.calls().len(), 0);
}

/// A body that will not decode records nothing.
///
/// There is no alias, no provider and no model to attribute it to, and nothing
/// was spent — a row here would be four empty columns diluting every average.
/// This is the deliberate companion to the `refused` case above, which *does*
/// get a row because it names an alias the user configured.
#[tokio::test]
async fn a_malformed_body_records_nothing() {
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{not json")
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 400);

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(usage_rows(&db).is_empty(), "got {:#?}", usage_rows(&db));
}

/// A refused *credential* records nothing either — there is no token to
/// attribute it to, and logging unauthenticated attempts is a different feature
/// from recording spend.
#[tokio::test]
async fn an_unauthenticated_request_records_nothing() {
    let upstream = fake_upstream(vec![]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    for auth in [None, Some(token_with(Scope::Write))] {
        let mut request =
            client()
                .post(gateway.url("/v1/chat/completions"))
                .json(&serde_json::json!({
                    "model": "my-alias",
                    "messages": [{"role": "user", "content": "Hello"}],
                }));
        if let Some(token) = auth {
            request = request.bearer_auth(token);
        }
        let status = request.send().await.expect("send").status();
        assert!(status == 401 || status == 403, "got {status}");
    }

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(usage_rows(&db).is_empty(), "got {:#?}", usage_rows(&db));
}

/// A stream the client abandons still records what it saw.
///
/// This is the arm most easily missed — it is not the protocol's ending, it is
/// the absence of one — and missing it loses exactly the requests a user is
/// most likely to ask about, since an abandoned stream still spent tokens.
#[tokio::test]
async fn an_interrupted_stream_records_the_tokens_it_saw() {
    // A chunk carrying usage, then silence: the client disconnects with
    // counters populated but no terminal frame.
    let upstream = fake_upstream(vec![Reply::SseThenHang {
        chunks: vec![final_chunk()],
    }])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let mut response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200);

    // Read the usage-bearing frame, *then* disconnect — otherwise the counters
    // would legitimately be zero and the test would pass without proving the
    // accumulator survives the interruption.
    tokio::time::timeout(std::time::Duration::from_secs(10), response.chunk())
        .await
        .expect("a first frame")
        .expect("chunk")
        .expect("some bytes");
    drop(response);

    let rows = wait_for_rows(&db, 1).await;
    assert_eq!(rows.len(), 1, "an abandoned stream still owes a row");
    assert_eq!(rows[0].status, "interrupted");
    assert!(rows[0].streamed);
    assert_eq!(
        (rows[0].prompt, rows[0].completion),
        (15, 2),
        "the tokens observed before the client left, not zeros"
    );
}

/// The row names the token that served the request, and two tokens are
/// distinguishable.
///
/// This is what makes revoking a credential an informed decision rather than a
/// guess — without it the log says money was spent but not by which tool.
#[tokio::test]
async fn each_row_names_the_token_that_served_it() {
    let upstream = fake_upstream(vec![Reply::ok(&completion_body()); 2]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    // Two distinct tokens, minted with different subjects.
    let first =
        agento_lib::native::security::token::mint(installed_keypair(), "tool-a", Scope::Llm, 3600)
            .expect("mint")
            .token;
    let second =
        agento_lib::native::security::token::mint(installed_keypair(), "tool-b", Scope::Llm, 3600)
            .expect("mint")
            .token;

    for token in [&first, &second] {
        let response = client()
            .post(gateway.url("/v1/chat/completions"))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "model": "my-alias",
                "messages": [{"role": "user", "content": "Hello"}],
            }))
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 200);
    }

    let rows = wait_for_rows(&db, 2).await;
    assert_eq!(rows.len(), 2);
    let subs: std::collections::BTreeSet<&str> =
        rows.iter().map(|r| r.token_sub.as_str()).collect();
    assert_eq!(
        subs,
        ["tool-a", "tool-b"].into_iter().collect(),
        "each row must name its own token; got {subs:?}"
    );
}

/// The Anthropic surface records too, and its tokens survive the translation.
///
/// Its frames are produced by consuming the provider stream, so a naive reading
/// of `usage` from the emitted frames would find none — the metering has to
/// happen a layer down, and this is what says it does.
#[tokio::test]
async fn the_anthropic_surface_records_its_own_rows() {
    let upstream = fake_upstream(vec![Reply::Sse {
        chunks: vec![chunk("Hi"), final_chunk()],
    }])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/anthropic/v1/messages"))
        .header("x-api-key", token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200);
    let _ = body_text(response).await;

    let rows = wait_for_rows(&db, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].surface, "anthropic");
    assert_eq!(rows[0].status, "ok");
    assert!(rows[0].streamed);
    assert_eq!(
        (rows[0].prompt, rows[0].completion),
        (15, 2),
        "metering happens before the translation, or these are zero"
    );
}

/// A model the pricing catalog does not know is disclosed, never zeroed.
///
/// The catalog is seeded for Claude models, so an OpenAI or Gemini alias missing
/// is the *ordinary* case here rather than an edge one — and `$0.00` would read
/// as "this request was free" on every dashboard built over this table.
#[tokio::test]
async fn an_unpriced_model_is_recorded_as_unpriced_rather_than_free() {
    let upstream = fake_upstream(vec![Reply::ok(&completion_body())]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "definitely-not-a-real-model-xyz")],
    );
    let gateway = gateway_for(&db).await;

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200);

    let rows = wait_for_rows(&db, 1).await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].unpriced, "got {:#?}", rows[0]);
    assert_eq!(
        rows[0].cost_usd, None,
        "NULL, not 0.0 — a total has to read as a floor"
    );
}

/// A contended write lock must not stall the runtime (#366, #425).
///
/// The defect this guards is the one `chat_turn.rs` and `scheduled_run.rs`
/// already carry a copy of, reaching a third caller: `db::open_read_write` sets
/// a five-second `busy_timeout`, so a usage insert meeting a lock held by the
/// session scanner's batch writer parks its thread for up to that long. This
/// listener is not on `proxy.rs`'s blocking pool at all, so an inline insert
/// would park a *runtime worker* — and on a four-core box the gateway shares
/// that runtime with the proxy, every SSE stream and the scheduler.
///
/// The shape is the established one, and each part is load-bearing:
///
/// - **one worker thread**, so a single parked worker is the whole runtime;
/// - **a plain OS thread** holds the lock, so the contention comes from outside
///   the runtime exactly as the scanner's batch writer does;
/// - **`last` is seeded before the spawn**, because a starved ticker is never
///   *polled* — seeding it on the first poll would start the clock after the
///   stall and measure nothing, which is how the first version of the sibling
///   test passed against the unfixed code.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_contended_write_lock_does_not_stall_the_runtime() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    let upstream = fake_upstream(vec![Reply::ok(&completion_body())]).await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let gateway = gateway_for(&db).await;

    /// Long enough that a parked worker is unmistakable, short enough to stay
    /// well inside the 5 s `busy_timeout` so the insert itself still succeeds.
    const HOLD: Duration = Duration::from_millis(1_500);

    // The file must already be WAL. Left in the default rollback journal,
    // `open_read_write`'s own `PRAGMA journal_mode=WAL` is a *mode change*
    // needing an exclusive lock and fails in about a millisecond instead of
    // waiting on `busy_timeout` — which would make this measure the wrong thing.
    agento_lib::native::db::open_read_write(db.path()).expect("convert to WAL");

    let (holding_tx, holding_rx) = std::sync::mpsc::channel();
    let lock_db = db.path().to_path_buf();
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

    let response = client()
        .post(gateway.url("/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 200, "the request is answered regardless");

    holder.join().expect("the writer finished");
    ticker.abort();

    let worst = worst_gap_ms.load(Ordering::Relaxed);
    assert!(
        worst < 500,
        "the runtime stalled for {worst} ms while the write lock was held \
         (the hold is {} ms; anything near it means the usage insert blocked a worker)",
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

    // ...and the row still landed once the lock was released, so this is not
    // passing because nothing happened.
    let rows = wait_for_rows(&db, 1).await;
    assert_eq!(rows.len(), 1, "the insert still completed: {rows:#?}");
}

// ── The control API's reload (#426) ──────────────────────────────────────────

/// Saving a provider through `PUT /api/gateway/providers/{id}` reloads the
/// listener **without breaking a stream that is already in flight**.
///
/// This is where reloads originate, so it is where the property is worth
/// pinning. The two halves pull against each other: an unconditional reload is
/// what makes a rotated API key stop being used — the whole point — while
/// graceful shutdown is what stops it cutting a completion the user is
/// currently reading. Getting either wrong is silent.
///
/// It is driven through the real handler rather than through
/// `config::update_provider` + `registry::reload`, because "the write and its
/// effect are in one place" is precisely the thing under test: calling the two
/// halves by hand would pass even if the handler had forgotten the second.
#[tokio::test]
async fn a_provider_save_reloads_without_cutting_an_in_flight_stream() {
    let _guard = registry_lock().lock().await;
    let upstream = fake_upstream(vec![Reply::SseThenHang {
        chunks: vec![chunk("Hello")],
    }])
    .await;
    let db = seeded_db(
        &[("p1", &upstream.base_url())],
        "my-alias",
        &[("p1", "upstream-model")],
    );
    let port = free_port().await;
    config::store_settings(
        db.path(),
        &GatewaySettings {
            enabled: true,
            port,
            start_with_app: true,
            ..Default::default()
        },
    )
    .expect("store settings");

    registry::start_if_enabled(db.path())
        .await
        .expect("start_if_enabled");
    assert_eq!(registry::status(), registry::Status::Running { port });

    let mut streaming = client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth(token_with(Scope::Llm))
        .json(&serde_json::json!({
            "model": "my-alias",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("send");
    assert_eq!(streaming.status(), 200);
    tokio::time::timeout(std::time::Duration::from_secs(10), streaming.chunk())
        .await
        .expect("a first frame")
        .expect("chunk")
        .expect("some bytes");

    // The save, through the endpoint the UI calls. `api_key` is omitted, which
    // is the round trip a scrubbed read produces — so this also exercises the
    // credential-preserving path on a live gateway.
    let method = axum::http::Method::PUT;
    let body =
        br#"{"name":"p1","type":"openai","base_url":"http://127.0.0.1:1/v1","enabled":true}"#;
    let ctx = agento_lib::native::Ctx {
        db_path: db.path().to_path_buf(),
    };
    let answer = (agento_lib::native::gateway_api::ENDPOINT.serve)(
        &ctx,
        &agento_lib::native::Request {
            method: &method,
            path: "/api/gateway/providers/p1",
            query: "",
            content_type: "application/json",
            secret_token: "",
            body,
        },
    )
    .expect("the save answered");
    assert_eq!(answer.status.as_u16(), 200, "the save succeeded");

    // The reload is spawned, so poll for the listener to come back rather than
    // sleeping a fixed amount.
    let health = format!("http://127.0.0.1:{port}/healthz");
    let mut back = false;
    for _ in 0..200 {
        if registry::status() == (registry::Status::Running { port })
            && client()
                .get(&health)
                .send()
                .await
                .is_ok_and(|r| r.status() == 200)
        {
            back = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        back,
        "the listener must come back after a config save, or every later \
         request goes nowhere; status was {:?}",
        registry::status()
    );

    // And the stream that was already running was not cut. The fake is still
    // parked holding its connection, so its hang-up signal firing would mean
    // the reload reached through and killed a completion mid-answer.
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            upstream.hung_up.notified()
        )
        .await
        .is_err(),
        "a config save must not cut a stream that was already in flight"
    );

    drop(streaming);
    registry::stop();
}

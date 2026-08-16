//! Local reverse proxy — the single origin the webview talks to.
//!
//! Why this exists rather than calling the Go server directly: the Go server
//! answers `/api` only for same-origin requests (its CORS middleware is a no-op
//! in production builds, by design), and the webview's origin is not the Go
//! server's. Putting one origin in front of both the UI assets and the API
//! sidesteps CORS entirely and keeps SSE working, which a Rust-side `fetch`
//! shim would not.
//!
//! It is also the migration seam. `route_is_native` decides, per request,
//! whether Rust answers or the Go sidecar does — so a subsystem can be ported
//! one endpoint at a time with both implementations running side by side.
//! `AGENTO_DESKTOP_NATIVE` steers that: `on` (the default), `off` to forward
//! everything, and `diff` to let Go answer while the Rust result is computed
//! alongside and compared byte for byte. See `native/`.
//!
//! Being that seam is also why the request log lives here (#301): one place
//! sees every `/api` request whichever side answers it, so the record cannot go
//! selectively sparse as routes move. See [`Served`].

use std::net::SocketAddr;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, Method, Request, Response, StatusCode, Uri};
use axum::routing::any;
use axum::Router;

use crate::native;

#[derive(Clone)]
pub struct ProxyState {
    /// Where the Go sidecar is listening.
    pub upstream: String,
    pub client: reqwest::Client,
}

/// Start the proxy and return the port it bound.
pub async fn serve(upstream_port: u16, port: u16) -> Result<u16, String> {
    let state = ProxyState {
        upstream: format!("http://127.0.0.1:{upstream_port}"),
        client: reqwest::Client::builder()
            // No request timeout at all — reqwest's default. SSE chat turns
            // stay open for minutes and can be quiet between tokens, so any
            // finite deadline would sever them mid-answer. (Passing
            // Duration::ZERO would not mean "unlimited"; it expires at once.)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .build()
            .map_err(|e| format!("building proxy http client: {e}"))?,
    };

    let app = Router::new()
        .route("/", any(handle))
        .route("/{*path}", any(handle))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("binding proxy on {addr}: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("reading proxy addr: {e}"))?
        .port();

    tauri::async_runtime::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            log::error!("proxy server stopped: {e}");
        }
    });

    log::info!("proxy listening on 127.0.0.1:{bound} -> 127.0.0.1:{upstream_port}");
    Ok(bound)
}

/// Whether this request is served by ported Rust code instead of the Go server.
///
/// The route table lives in `native::claims` next to the handlers it selects,
/// so claiming a route and implementing it are one edit rather than two files
/// that can disagree.
fn route_is_native(method: &axum::http::Method, path: &str) -> bool {
    native::may_serve(native::mode(), method)
        && native::claims(method, path)
        // The streaming routes are handled above; `claims` is the union of both
        // registries, so the buffered path has to exclude them or it would try
        // to answer a chat turn with a `Vec<u8>`.
        && !native::claims_stream(method, path)
}

/// Forward, turning a proxy failure into the 502 the caller would have written.
async fn forward_or_bad_gateway(
    state: &ProxyState,
    req: Request<Body>,
    path: &str,
) -> Response<Body> {
    match forward(state, req).await {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("proxy error for {path}: {e}");
            error_response(StatusCode::BAD_GATEWAY, &e)
        }
    }
}

/// How much request body a native handler will accept.
///
/// **Over this the request is answered 400, not forwarded** — and it cannot be
/// forwarded, because `to_bytes` has already consumed the body by the time the
/// limit is hit. That is the one place the seam's "a native failure just falls
/// back" rule does not hold, so the cap is set well above anything a claimed
/// route legitimately carries: the biggest is an agent's `system_prompt`, and
/// 8 MiB of it would be about a million words.
const MAX_NATIVE_BODY: usize = 8 << 20;

/// What `POST /api/uploads` is allowed to carry, since #308 claimed it.
///
/// Go caps the request at `maxUploadSize` (100 MiB) with a `MaxBytesReader` and
/// answers 400 over it. This has to be **at least** that, or the shell would
/// refuse a file the server accepts — and it has to be more, because the cap
/// here applies to the whole multipart envelope while Go's applies to the same
/// bytes but the *file* inside it is what the 100 MiB is about. The slack is
/// the part headers and the boundaries.
///
/// It is a second constant rather than a raised `MAX_NATIVE_BODY` because the
/// cost is real: this is a buffer held in memory for the length of the request,
/// where Go's `ParseMultipartForm` keeps 10 MiB and spills the rest to temp
/// files. Paying that on an upload is a deliberate trade; paying it on every
/// claimed route would be a memory regression on a route that never needs it.
const MAX_UPLOAD_BODY: usize = (100 << 20) + (1 << 20);

/// The body cap for one route.
fn max_body_for(path: &str) -> usize {
    if path == native::uploads::PATH {
        MAX_UPLOAD_BODY
    } else {
        MAX_NATIVE_BODY
    }
}

/// Which implementation answered a request — the part of the access line that
/// makes it honest about who did the work.
///
/// The port's whole hazard is that the same user action is served by different
/// code depending on whether its route has moved yet (#301). A log line that
/// did not say which would be worse than none, because it reads as coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Served {
    /// A native handler answered in full, body buffered.
    Native,
    /// A native handler answered with a stream still arriving.
    NativeStream,
    /// The Go sidecar answered; Rust does not claim this route.
    Forwarded,
    /// A native handler was tried, failed, and the request fell through to Go —
    /// the seam's fallback rule. Worth distinguishing from a plain forward,
    /// because it means a ported route is broken while the app looks healthy.
    NativeFailedForwarded,
    /// Shadow-diff mode: Go answered and Rust was computed alongside it.
    Diff,
}

impl Served {
    fn label(self) -> &'static str {
        match self {
            Served::Native => "native",
            Served::NativeStream => "native-stream",
            Served::Forwarded => "forwarded",
            Served::NativeFailedForwarded => "native-failed-forwarded",
            Served::Diff => "diff",
        }
    }
}

/// What the access line is logged at.
///
/// `tauri_plugin_log` is built at `LevelFilter::Info` in `lib.rs`, so this split
/// is what the log actually contains by default: every state-changing request —
/// the ones Go's service layer wrote its `logger.Info` lines for — and none of
/// the reads. The UI polls `GET /api/claude-sessions/status` on a timer for the
/// whole length of a scan; an info line per poll would bury everything else in
/// the file, which is how a log stops being read.
fn access_level(method: &Method) -> log::Level {
    if method == Method::GET {
        log::Level::Debug
    } else {
        log::Level::Info
    }
}

/// The path to log, which is deliberately **not** the URI.
///
/// The query string carries search terms, project paths and date ranges — the
/// user's own data. It is dropped here rather than at the call site so there is
/// one place to check that it always is.
fn log_path(uri: &Uri) -> String {
    uri.path().to_string()
}

async fn handle(State(state): State<ProxyState>, req: Request<Body>) -> Response<Body> {
    let path = log_path(req.uri());

    // Anything that isn't the API is a frontend route. In release the assets
    // are embedded here; in debug the webview loads Vite directly and only
    // ever reaches the proxy for /api, so this arm is release-only.
    //
    // It returns before the access log on purpose: handing out an embedded file
    // is static serving, not an application operation, and one page load is
    // dozens of them.
    if !is_api_path(&path) {
        #[cfg(not(debug_assertions))]
        {
            return assets::serve(&path);
        }
        #[cfg(debug_assertions)]
        {
            return error_response(
                StatusCode::NOT_FOUND,
                "frontend is served by Vite in development",
            );
        }
    }

    let method = req.method().clone();
    let started = Instant::now();
    let (response, served) = dispatch(state, req, path.clone()).await;

    // The one record the desktop build emits for a request (#301). No OTel span
    // accompanies it and none will — see "Do not port" in `desktop/CLAUDE.md`.
    //
    // **Method, path, status, duration and who answered. Never a body, never a
    // header, never the query string.** This is a desktop app: the bodies
    // passing through here are chat prompts, agent system prompts and
    // integration credentials, and this file is written to disk unencrypted in
    // the app's log directory. Anyone tempted to add "just the request body for
    // debugging" is proposing to write the user's API tokens to it.
    //
    // For a `native-stream` line the elapsed figure is time-to-headers, not the
    // turn's duration — an SSE body is still arriving when this runs, so a
    // 12 ms chat turn means the stream opened in 12 ms and says nothing about
    // how long the answer took.
    log::log!(
        access_level(&method),
        "{} {} {} {}ms {}",
        method,
        path,
        response.status().as_u16(),
        started.elapsed().as_millis(),
        served.label(),
    );

    response
}

/// Answer one `/api` request, reporting which implementation did it.
///
/// Split out of [`handle`] so the access line has exactly one call site. There
/// are eight-odd ways out of here, and logging at each of them is the shape
/// that drifts: the next port adds a ninth and quietly stops being recorded.
async fn dispatch(state: ProxyState, req: Request<Body>, path: String) -> (Response<Body>, Served) {
    // The streaming half of the seam (#276). Checked before the buffered one
    // because a chat turn must never be collected into a `Vec<u8>`: it is the
    // one response whose whole point is arriving in pieces.
    if native::may_serve(native::mode(), req.method()) && native::claims_stream(req.method(), &path)
    {
        let (parts, body) = req.into_parts();
        let body_bytes = match axum::body::to_bytes(body, MAX_NATIVE_BODY).await {
            Ok(bytes) => bytes,
            Err(e) => {
                log::warn!("native stream for {path}: reading body failed: {e}");
                return (
                    error_response(StatusCode::BAD_REQUEST, "could not read request body"),
                    Served::Native,
                );
            }
        };
        let req = Request::from_parts(parts, Body::from(body_bytes.clone()));

        let stream_req = native::StreamRequest {
            method: req.method().clone(),
            path: path.clone(),
            body: body_bytes.to_vec(),
            db_path: match crate::paths::database_path() {
                Some(path) => path,
                None => {
                    log::warn!("native stream for {path}: no data dir, forwarding");
                    return (
                        forward_or_bad_gateway(&state, req, &path).await,
                        Served::NativeFailedForwarded,
                    );
                }
            },
        };

        match native::serve_stream(stream_req).await {
            Ok(response) => return (response, Served::NativeStream),
            // Same rule as the buffered path, and the same obligation: a
            // streaming handler must fail *before* it has any effect, or the
            // forward spawns a second subprocess. Every check in `turn::run`
            // happens before the CLI is started.
            //
            // This is also how `/input`, `/permission` and `/stop` hand a chat
            // back when Rust holds no live session for it — Go may, and its
            // answer is then the right one. See `native::chat`'s header.
            Err(e) => {
                log::warn!("native stream for {path} failed, forwarding to Go: {e}");
                return (
                    forward_or_bad_gateway(&state, req, &path).await,
                    Served::NativeFailedForwarded,
                );
            }
        }
    }

    if route_is_native(req.method(), &path) {
        // A native handler needs the body, so it has to be read here — and
        // reading it consumes the request, which is why the buffered bytes are
        // put back before any forward below. Claimed routes are never the SSE
        // ones, so nothing streaming is buffered by this.
        let (parts, body) = req.into_parts();
        let body_bytes = match axum::body::to_bytes(body, max_body_for(&path)).await {
            Ok(bytes) => bytes,
            // Not forwarded: the body is gone. See `MAX_NATIVE_BODY`.
            Err(e) => {
                log::warn!("native handler for {path}: reading body failed: {e}");
                return (
                    error_response(StatusCode::BAD_REQUEST, "could not read request body"),
                    Served::Native,
                );
            }
        };
        // `req` keeps a complete copy of the body, so both forwards below can
        // use it as-is; the handler gets the other copy.
        let req = Request::from_parts(parts, Body::from(body_bytes.clone()));

        // Reading SQLite is blocking work; keeping it off the axum worker means
        // one slow read cannot stall an SSE stream sharing the runtime.
        let method = req.method().clone();
        let query = req.uri().query().unwrap_or_default().to_string();
        // Carried because one claimed route needs it: a multipart body is
        // unparseable without the boundary, and the boundary is only in the
        // header. Everything else ignores it.
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let native_answer = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || {
                native::serve(&native::Request {
                    method: &method,
                    path: &path,
                    query: &query,
                    content_type: &content_type,
                    body: &body_bytes,
                })
            })
            .await
            .unwrap_or_else(|e| Err(format!("native handler panicked: {e}")))
        };

        match (native::mode(), native_answer) {
            // Go stays authoritative in shadow mode; Rust is only compared.
            //
            // A write never reaches here — `route_is_native` refuses to run one
            // natively in this mode, because computing it "alongside" means
            // applying it twice. See `native::may_serve`.
            (native::Mode::Diff, native) => {
                let (go_response, go_body) = match forward_buffered(&state, req).await {
                    Ok(buffered) => buffered,
                    Err(e) => {
                        log::error!("proxy error for {path}: {e}");
                        return (error_response(StatusCode::BAD_GATEWAY, &e), Served::Diff);
                    }
                };
                match native {
                    // A route the two sides cannot agree on by construction is
                    // skipped rather than reported — see `native::diff_exempt`.
                    // A permanent false difference is worse than no comparison,
                    // because it teaches the reader to ignore the output.
                    Ok(_) if native::diff_exempt(&path) => {
                        log::debug!("native diff {path}: exempt, not compared")
                    }
                    Ok(answer) => native::diff::report(
                        &path,
                        &native::diff::compare(&go_body, answer.body.as_deref().unwrap_or(&[])),
                    ),
                    Err(e) => log::error!("native diff {path}: native handler failed: {e}"),
                }
                return (go_response, Served::Diff);
            }
            (_, Ok(answer)) => return (native::response(answer), Served::Native),
            // A native failure is never surfaced to the UI: the request falls
            // through to the Go sidecar, which is still running and still
            // correct. A ported route can only be as broken as an unported one.
            //
            // **A write handler must therefore fail before it mutates**, or the
            // fallback re-applies what already happened. Every write below runs
            // its validation and its schema check first and does the whole
            // mutation in one transaction, so an `Err` means nothing was
            // written. That invariant is the write path's half of the seam's
            // safety, and it lives in the handlers because only they know it.
            (_, Err(e)) => {
                log::warn!("native handler for {path} failed, forwarding to Go: {e}");
                return (
                    match forward(&state, req).await {
                        Ok(resp) => resp,
                        Err(e) => {
                            log::error!("proxy error for {path}: {e}");
                            error_response(StatusCode::BAD_GATEWAY, &e)
                        }
                    },
                    Served::NativeFailedForwarded,
                );
            }
        }
    }

    (
        match forward(&state, req).await {
            Ok(resp) => resp,
            Err(e) => {
                log::error!("proxy error for {path}: {e}");
                error_response(StatusCode::BAD_GATEWAY, &e)
            }
        },
        Served::Forwarded,
    )
}

/// Forward a request upstream, streaming both directions.
async fn forward(state: &ProxyState, req: Request<Body>) -> Result<Response<Body>, String> {
    let (parts, body) = req.into_parts();

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let url = format!("{}{}", state.upstream, path_and_query);

    let mut headers = parts.headers.clone();

    // The Go server validates the Host header against what it is served under.
    // Rewriting it to the upstream authority is what makes the proxied request
    // indistinguishable from a direct same-origin one.
    let upstream_authority = state
        .upstream
        .strip_prefix("http://")
        .unwrap_or(&state.upstream);
    headers.insert(
        header::HOST,
        HeaderValue::from_str(upstream_authority)
            .map_err(|e| format!("invalid upstream host: {e}"))?,
    );

    // Hop-by-hop headers must not be forwarded.
    for h in [
        header::CONNECTION,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
    ] {
        headers.remove(h);
    }
    // Let reqwest negotiate its own encoding; a forwarded Accept-Encoding would
    // hand us a compressed body we then fail to re-frame for SSE.
    headers.remove(header::ACCEPT_ENCODING);

    let body_bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|e| format!("reading request body: {e}"))?;

    let upstream_req = state
        .client
        .request(parts.method.clone(), &url)
        .headers(headers)
        .body(body_bytes)
        .build()
        .map_err(|e| format!("building upstream request: {e}"))?;

    let upstream_resp = state
        .client
        .execute(upstream_req)
        .await
        .map_err(|e| format!("upstream request failed: {e}"))?;

    let status = upstream_resp.status();
    let mut resp_headers = upstream_resp.headers().clone();
    for h in [
        header::CONNECTION,
        header::TRANSFER_ENCODING,
        header::CONTENT_LENGTH,
    ] {
        resp_headers.remove(h);
    }

    // Stream the body through rather than buffering it — this is what keeps SSE
    // chat turns arriving token by token instead of all at once at the end.
    let stream = upstream_resp.bytes_stream();
    let body = Body::from_stream(stream);

    let mut builder = Response::builder().status(status);
    if let Some(h) = builder.headers_mut() {
        *h = resp_headers;
    }
    builder
        .body(body)
        .map_err(|e| format!("building response: {e}"))
}

/// Forward, and also hand back the response body as bytes.
///
/// Only shadow-diff mode uses this. Buffering would break SSE, but a claimed
/// route is by definition one Rust can answer in full, so there is nothing to
/// stream — and comparing two bodies requires having both of them.
async fn forward_buffered(
    state: &ProxyState,
    req: Request<Body>,
) -> Result<(Response<Body>, Vec<u8>), String> {
    let (parts, body) = forward(state, req).await?.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|e| format!("buffering upstream body: {e}"))?;
    let replayed = Response::from_parts(parts, Body::from(bytes.clone()));
    Ok((replayed, bytes.to_vec()))
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    let payload = serde_json::json!({ "error": message }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Serve the built frontend for release builds. In development the webview
/// loads Vite directly, so nothing is embedded.
#[cfg(not(debug_assertions))]
mod assets {
    use super::*;
    use rust_embed::RustEmbed;

    #[derive(RustEmbed)]
    #[folder = "../dist"]
    pub struct Assets;

    pub fn serve(path: &str) -> Response<Body> {
        let candidate = path.trim_start_matches('/');
        let candidate = if candidate.is_empty() {
            "index.html"
        } else {
            candidate
        };

        let file = Assets::get(candidate).or_else(|| Assets::get("index.html"));

        match file {
            Some(content) => {
                let mime = mime_guess::from_path(candidate).first_or_octet_stream();
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, mime.as_ref())
                    .body(Body::from(content.data.into_owned()))
                    .unwrap_or_else(|_| Response::new(Body::empty()))
            }
            None => error_response(StatusCode::NOT_FOUND, "not found"),
        }
    }
}

/// Paths the API owns; everything else is a frontend route.
pub fn is_api_path(path: &str) -> bool {
    path.starts_with("/api")
        || path.starts_with("/webhooks")
        || path == "/health"
        || path == "/metrics"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The labels are the whole point of the line — they are what says which
    /// implementation answered while both are running (#301). Pinned because a
    /// rename would silently reword every historical log's meaning.
    #[test]
    fn served_labels_name_each_implementation() {
        assert_eq!(Served::Native.label(), "native");
        assert_eq!(Served::NativeStream.label(), "native-stream");
        assert_eq!(Served::Forwarded.label(), "forwarded");
        assert_eq!(
            Served::NativeFailedForwarded.label(),
            "native-failed-forwarded"
        );
        assert_eq!(Served::Diff.label(), "diff");
    }

    #[test]
    fn writes_log_at_info_and_reads_at_debug() {
        for method in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ] {
            assert_eq!(
                access_level(&method),
                log::Level::Info,
                "{method} should be logged at info"
            );
        }
        assert_eq!(access_level(&Method::GET), log::Level::Debug);
    }

    /// A logged path must never carry the query string: it holds search terms,
    /// project paths and date ranges.
    #[test]
    fn logged_path_drops_the_query_string() {
        let uri: Uri = "/api/claude-sessions?search=my+secret+project&project=%2Fhome%2Fme%2Fwork"
            .parse()
            .expect("uri");
        assert_eq!(log_path(&uri), "/api/claude-sessions");

        let uri: Uri = "/api/agents".parse().expect("uri");
        assert_eq!(log_path(&uri), "/api/agents");
    }

    /// `handle` returns before the access line for anything that is not an API
    /// path, so this predicate is what decides whether a request is logged at
    /// all. Serving an embedded asset is not an application operation, and one
    /// page load is dozens of them.
    #[test]
    fn frontend_assets_are_not_logged() {
        for path in [
            "/",
            "/index.html",
            "/assets/index-abc123.js",
            "/favicon.ico",
        ] {
            assert!(!is_api_path(path), "{path} should not be logged");
        }
        for path in ["/api/agents", "/webhooks/telegram/1", "/health", "/metrics"] {
            assert!(is_api_path(path), "{path} should be logged");
        }
    }
}

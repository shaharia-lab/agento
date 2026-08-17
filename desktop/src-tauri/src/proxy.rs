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
///
/// Each variant names **which half of the seam the request was routed to**, not
/// which function produced the bytes. The seam can reject a request before
/// either handler runs — an over-cap body is answered 400 right here — and such
/// a line still belongs to the half that claimed the route, because that is
/// what the reader is trying to establish. The adjacent `warn!` says where it
/// actually failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Served {
    /// The buffered native half took it: a handler answered in full, or the
    /// seam rejected the request before one ran.
    Native,
    /// The streaming native half took it: a handler answered with a stream
    /// still arriving, or the seam rejected the request before one ran.
    NativeStream,
    /// The Go sidecar answered; Rust does not claim this route.
    Forwarded,
    /// A native handler was tried, failed, and the request fell through to Go —
    /// the seam's fallback rule. Worth distinguishing from a plain forward,
    /// because it means a ported route is broken while the app looks healthy.
    NativeFailedForwarded,
    /// Shadow-diff mode: Go answered and Rust was computed alongside it.
    Diff,
    /// Neither half was reached: `guards.rs` refused the request (#329). Its own
    /// variant because it is the one line here that does *not* name a half of
    /// the seam — the check runs before routing is decided, deliberately, so
    /// that a claimed route and a forwarded one are guarded identically.
    Rejected,
}

impl Served {
    fn label(self) -> &'static str {
        match self {
            Served::Native => "native",
            Served::NativeStream => "native-stream",
            Served::Forwarded => "forwarded",
            Served::NativeFailedForwarded => "native-failed-forwarded",
            Served::Diff => "diff",
            Served::Rejected => "rejected",
        }
    }
}

/// What the access line is logged at.
///
/// `tauri_plugin_log` is built at `LevelFilter::Info` in `lib.rs`, so this split
/// is what the log actually contains by default: every state-changing request,
/// every request that failed, and none of the successful reads. It is the same
/// record Go's `requestLogger` (`internal/server/server.go`) writes, promoted
/// out of debug for the half worth keeping — not Go's service-layer `Info`
/// lines, which the seam cannot see. See `desktop/CLAUDE.md`.
///
/// Three arms rather than two, and the status arm is why:
///
/// - The argument for putting reads at debug is **volume**. The UI polls
///   `GET /api/claude-sessions/status` on a timer for the whole length of a
///   scan, and an info line per poll buries everything else, which is how a log
///   stops being read.
/// - A read that *failed* is not that volume. It is the one read anybody wants
///   in the file, so a 4xx or 5xx outranks the method entirely. Without this
///   arm the first native `GET` handler that answers 404 as `Ok(Answer)` — the
///   natural shape once a read stops wanting the Go fallback — would be
///   invisible, as would a Go 5xx forwarded through on a `GET`.
/// - `HEAD` and `OPTIONS` reach here because the router is `any(handle)`, and
///   neither changes anything. The split is reads against writes, not `GET`
///   against everything else.
fn access_level(method: &Method, status: StatusCode) -> log::Level {
    if status.is_client_error() || status.is_server_error() {
        log::Level::Warn
    } else if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
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
    //
    // `path` here is the **raw** request target, and stays raw for both halves
    // of this `if` and for the access line below. The asset lookup is a file
    // this binary embeds, so nothing about Go's router is involved in finding
    // one; the API gate is raw because reaching a divergence would need a
    // percent-escape inside the `/api` prefix itself (`/%61pi/agents`), which no
    // client sends and which `dispatch` would answer identically anyway. The log
    // is raw because it should record what arrived, not what matched.
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
    // The one exception is the path itself, and it is an accepted one rather
    // than an oversight: two route families put user-authored text in a path
    // segment — the agent slug (`routeAgentBySlug`, derived from the name the
    // user typed) and the settings-profile id (`routeProfileByID`). A name is
    // not a body, and dropping the segment would leave the line unable to say
    // which agent was written, which is most of what it is for. Nothing else in
    // the route table carries user data in a path segment; a route that wanted
    // to would need to be argued here first.
    //
    // The elapsed figure is time-to-headers whenever the body is still
    // arriving, which is more lines than it looks: `native-stream` always, and
    // any `forwarded` / `native-failed-forwarded` line for an SSE route, since
    // `forward` builds its body with `Body::from_stream` too — under
    // `AGENTO_DESKTOP_NATIVE=off` a three-minute chat turn logs `200 9ms
    // forwarded`. Only `native` and `diff` buffer the whole response before
    // this runs, and only they report a duration the request actually took.
    log::log!(
        access_level(&method, response.status()),
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
async fn dispatch(
    state: ProxyState,
    req: Request<Body>,
    raw_path: String,
) -> (Response<Body>, Served) {
    // `internal/server/guards.go`, applied **before** the seam decides who
    // answers (#329) — so a claimed route and a forwarded one are guarded
    // identically, and the guards' coverage stops shrinking with every endpoint
    // the port claims.
    //
    // Before the `gourl::route_path` resolution below, not after, and that
    // ordering is the security-relevant one: `forward` rewrites the `Host` to
    // the upstream authority, so anything that forwards has already lost the
    // browser's `Host` by the time Go's own `validateHost` sees it. Leaving the
    // two "no route path" cases unguarded would hand a rebinding request
    // straight through. The cost is that a *malformed escape* is answered 415 or
    // 403 here where Go answers 400 from inside `net/http` — a refusal either
    // way, with no handler reached on either side.
    if let Some((status, message)) = crate::guards::reject(&req) {
        return (error_response(status, message), Served::Rejected);
    }

    // From here on, the path every claim function sees is the one **chi** would
    // route on, not the raw request target (#294). They are not the same string:
    // `net/http` decodes the target into `url.URL` before any handler runs, and
    // chi routes on `RawPath` when the escaping is non-canonical and on the
    // decoded `Path` when it is not — so `/api/agents/a%2Db` is `a%2Db` to Go
    // and `/api/agents/a%20b` is `a b`. Matching on the raw target got the
    // second class wrong, which was invisible while every claimed route was a
    // read (a miss forwarded and Go answered) and is not now that #274 and #276
    // claim writes: `agents::update` would *answer* 404 for a row Go updates.
    //
    // Doing it once here rather than in each module's `slug_of`/`id_of` is what
    // stops the five of them drifting apart — and it is also the only place
    // that can see the whole path, which is what the rule is about: one
    // non-canonical escape anywhere leaves every segment raw.
    let path = match native::gourl::route_path(&raw_path) {
        Some(path) => path,
        // No route path at all: either the escaping is malformed, in which case
        // `url.ParseRequestURI` fails and Go answers 400 from inside
        // `net/http`, or it is canonical and the decoded path is not UTF-8, so
        // Rust cannot carry the string chi routes on. Forwarding is how both get
        // the answer Go would have given — `Forwarded` rather than
        // `NativeFailedForwarded` because no native handler was tried.
        //
        // Logged because every other fallback here says why it fell back, and
        // these two conditions are the hardest of the lot to reproduce from a
        // bug report.
        None => {
            log::debug!(
                "no route path for {raw_path}: malformed escape or a non-UTF-8 decoded path, forwarding"
            );
            return (
                forward_or_bad_gateway(&state, req, &raw_path).await,
                Served::Forwarded,
            );
        }
    };

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
                    Served::NativeStream,
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

    // The seam runs in this direction too, for exactly one route: a forwarded
    // request can have an effect the *shell* owns, because the sidecar has been
    // told not to host the integration types this process hosts. See
    // `native::after_forward`.
    let method = req.method().clone();
    let response = match forward(&state, req).await {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("proxy error for {path}: {e}");
            error_response(StatusCode::BAD_GATEWAY, &e)
        }
    };
    native::after_forward(&method, &path, response.status());
    (response, Served::Forwarded)
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
        assert_eq!(Served::Rejected.label(), "rejected");
    }

    /// The guards run before the seam decides who answers, so a claimed route
    /// and a forwarded one are refused identically (#329). Reachable without a
    /// live sidecar for the same reason the over-cap case above is: the arm
    /// returns before any forward.
    #[tokio::test]
    async fn a_guard_rejection_precedes_both_halves_of_the_seam() {
        let state = ProxyState {
            // Never dialled. If it ever is, the test fails on the status rather
            // than hanging, because nothing is listening on port 1.
            upstream: "http://127.0.0.1:1".to_string(),
            client: reqwest::Client::new(),
        };

        // A claimed write route, and the shape that made #329 exploitable: a
        // cross-origin `POST` carrying `text/plain` is a CORS simple request, so
        // the browser sends it with no preflight.
        let path = "/api/agents".to_string();
        let req = Request::builder()
            .method(Method::POST)
            .uri(&path)
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(r#"{"name":"pwned","slug":"pwned"}"#))
            .expect("request");

        let (response, served) = dispatch(state.clone(), req, path.clone()).await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(served, Served::Rejected);

        // And a route nothing claims, which would otherwise have been forwarded
        // — port 1 is what proves it was not.
        let path = "/api/settings".to_string();
        let req = Request::builder()
            .method(Method::PUT)
            .uri(&path)
            .header(header::HOST, "attacker.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("request");

        let (response, served) = dispatch(state, req, path).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(served, Served::Rejected);
    }

    #[test]
    fn writes_and_failures_log_at_info_or_above_and_successful_reads_at_debug() {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(
                access_level(&method, StatusCode::OK),
                log::Level::Info,
                "{method} should be logged at info"
            );
        }
        // Reads. `HEAD` and `OPTIONS` reach the seam because the router is
        // `any(handle)`, and neither changes anything, so they belong with
        // `GET` rather than with the writes.
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(
                access_level(&method, StatusCode::OK),
                log::Level::Debug,
                "{method} should be logged at debug"
            );
        }
        // A failed read is not polling volume, and is the one read a reader
        // wants in the default-level file.
        assert_eq!(
            access_level(&Method::GET, StatusCode::NOT_FOUND),
            log::Level::Warn
        );
        assert_eq!(
            access_level(&Method::GET, StatusCode::BAD_GATEWAY),
            log::Level::Warn
        );
        assert_eq!(
            access_level(&Method::POST, StatusCode::CONFLICT),
            log::Level::Warn
        );
    }

    /// The label→path mapping, which is the half that rots as ports land: a
    /// wrong label is not a crash, it is a log that quietly misattributes the
    /// work. Only one `dispatch` arm can be reached without a live sidecar —
    /// a claimed route whose body is over [`max_body_for`] is answered 400 by
    /// the seam itself, before any forward — so that is the arm pinned here,
    /// and it is also the one where a wrong label is least visible, since the
    /// UI renders the 400 as an ordinary error.
    ///
    /// `native::mode()` reads the environment once per process, so this asserts
    /// the shipped default rather than forcing it. Under
    /// `AGENTO_DESKTOP_NATIVE=off` nothing is claimed and the arm does not
    /// exist; skipping is honest, where forcing the env would make the test
    /// claim to have exercised a path it did not.
    #[tokio::test]
    async fn oversized_body_on_a_claimed_route_is_answered_natively() {
        if native::mode() != native::Mode::On {
            return;
        }

        let state = ProxyState {
            // Never dialled: the arm under test returns before any forward. If
            // this ever is dialled the test fails on the status rather than
            // hanging, because nothing is listening on port 1.
            upstream: "http://127.0.0.1:1".to_string(),
            client: reqwest::Client::new(),
        };
        let path = "/api/agents".to_string();
        let req = Request::builder()
            .method(Method::POST)
            .uri(&path)
            // Both headers are required since #329: the guards run ahead of the
            // seam, so a request without them never reaches the arm under test.
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(vec![0u8; MAX_NATIVE_BODY + 1]))
            .expect("request");

        let (response, served) = dispatch(state, req, path).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(served, Served::Native);
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

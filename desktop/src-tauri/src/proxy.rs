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

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, Request, Response, StatusCode};
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
/// 8 MiB of it would be about a million words. Uploads are the only genuinely
/// large body in the API and they are not claimed.
const MAX_NATIVE_BODY: usize = 8 << 20;

async fn handle(State(state): State<ProxyState>, req: Request<Body>) -> Response<Body> {
    let path = req.uri().path().to_string();

    // Anything that isn't the API is a frontend route. In release the assets
    // are embedded here; in debug the webview loads Vite directly and only
    // ever reaches the proxy for /api, so this arm is release-only.
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
                return error_response(StatusCode::BAD_REQUEST, "could not read request body");
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
                    return forward_or_bad_gateway(&state, req, &path).await;
                }
            },
        };

        match native::serve_stream(stream_req).await {
            Ok(response) => return response,
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
                return forward_or_bad_gateway(&state, req, &path).await;
            }
        }
    }

    if route_is_native(req.method(), &path) {
        // A native handler needs the body, so it has to be read here — and
        // reading it consumes the request, which is why the buffered bytes are
        // put back before any forward below. Claimed routes are never the SSE
        // ones, so nothing streaming is buffered by this.
        let (parts, body) = req.into_parts();
        let body_bytes = match axum::body::to_bytes(body, MAX_NATIVE_BODY).await {
            Ok(bytes) => bytes,
            // Not forwarded: the body is gone. See `MAX_NATIVE_BODY`.
            Err(e) => {
                log::warn!("native handler for {path}: reading body failed: {e}");
                return error_response(StatusCode::BAD_REQUEST, "could not read request body");
            }
        };
        // `req` keeps a complete copy of the body, so both forwards below can
        // use it as-is; the handler gets the other copy.
        let req = Request::from_parts(parts, Body::from(body_bytes.clone()));

        // Reading SQLite is blocking work; keeping it off the axum worker means
        // one slow read cannot stall an SSE stream sharing the runtime.
        let method = req.method().clone();
        let query = req.uri().query().unwrap_or_default().to_string();
        let native_answer = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || {
                native::serve(&native::Request {
                    method: &method,
                    path: &path,
                    query: &query,
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
                        return error_response(StatusCode::BAD_GATEWAY, &e);
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
                return go_response;
            }
            (_, Ok(answer)) => return native::response(answer),
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
                return match forward(&state, req).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        log::error!("proxy error for {path}: {e}");
                        error_response(StatusCode::BAD_GATEWAY, &e)
                    }
                };
            }
        }
    }

    match forward(&state, req).await {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("proxy error for {path}: {e}");
            error_response(StatusCode::BAD_GATEWAY, &e)
        }
    }
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

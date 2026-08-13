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
    native::mode() != native::Mode::Off && native::claims(method, path)
}

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

    if route_is_native(req.method(), &path) {
        // Reading SQLite is blocking work; keeping it off the axum worker means
        // one slow read cannot stall an SSE stream sharing the runtime.
        let method = req.method().clone();
        let native_body = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || native::serve(&method, &path))
                .await
                .unwrap_or_else(|e| Err(format!("native handler panicked: {e}")))
        };

        match (native::mode(), native_body) {
            // Go stays authoritative in shadow mode; Rust is only compared.
            (native::Mode::Diff, native) => {
                let (go_response, go_body) = match forward_buffered(&state, req).await {
                    Ok(buffered) => buffered,
                    Err(e) => {
                        log::error!("proxy error for {path}: {e}");
                        return error_response(StatusCode::BAD_GATEWAY, &e);
                    }
                };
                match native {
                    Ok(body) => {
                        native::diff::report(&path, &native::diff::compare(&go_body, &body))
                    }
                    Err(e) => log::error!("native diff {path}: native handler failed: {e}"),
                }
                return go_response;
            }
            (_, Ok(body)) => return native::response(body),
            // A native failure is never surfaced to the UI: the request falls
            // through to the Go sidecar, which is still running and still
            // correct. A ported route can only be as broken as an unported one.
            (_, Err(e)) => log::warn!("native handler for {path} failed, forwarding to Go: {e}"),
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

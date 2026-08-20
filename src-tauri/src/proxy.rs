//! Local HTTP server — the single origin the webview talks to.
//!
//! This began life as a reverse proxy in front of the bundled Go server, and
//! doubled as the migration seam: `route_is_native` decided per request whether
//! ported Rust answered or the request forwarded to the sidecar. #278 removed
//! the sidecar, so there is nothing to forward to and no upstream to hold —
//! every request is answered by `native/`'s registry, and the remaining routing
//! decision is buffered handler vs streaming handler vs "no such route".
//!
//! What deliberately survives from the seam era:
//!
//! - **One origin for UI and API.** The webview loads the frontend from here
//!   and fetches `/api` from the same origin, so CORS never applies and SSE
//!   works without a shim.
//! - **The request log** (#301): one place sees every `/api` request, whoever
//!   answers it. See [`Served`].
//! - **The guards** (#329): `guards.rs` runs before routing, so every route is
//!   refused identically — including the ones nothing claims.
//! - **Go's router shape at the edges.** An unclaimed request gets chi's own
//!   404 (`404 page not found`, `text/plain`, nosniff), and a native handler's
//!   `Err` gets `httpErr`'s default 500 — so a route this build genuinely does
//!   not have answers exactly as the Go server answered a route *it* did not
//!   have.

use std::net::SocketAddr;
use std::time::Instant;

use axum::body::Body;
use axum::http::{header, Method, Request, Response, StatusCode, Uri};
use axum::routing::any;
use axum::Router;

use crate::native;

/// Start the server and return the port it bound.
pub async fn serve(port: u16) -> Result<u16, String> {
    let app = Router::new()
        .route("/", any(handle))
        .route("/{*path}", any(handle));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("binding server on {addr}: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("reading server addr: {e}"))?
        .port();

    tauri::async_runtime::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            log::error!("api server stopped: {e}");
        }
    });

    log::info!("api server listening on 127.0.0.1:{bound}");
    Ok(bound)
}

/// How much request body a native handler will accept. Over this the request
/// is answered 400 — the cap is set well above anything a claimed route
/// legitimately carries: the biggest is an agent's `system_prompt`, and 8 MiB
/// of it would be about a million words.
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
/// cost is real: this is a buffer held in memory for the length of the request.
/// Paying that on an upload is a deliberate trade; paying it on every route
/// would be a memory regression on a route that never needs it.
const MAX_UPLOAD_BODY: usize = (100 << 20) + (1 << 20);

/// The body cap for one route.
fn max_body_for(path: &str) -> usize {
    if path == native::uploads::PATH {
        MAX_UPLOAD_BODY
    } else {
        MAX_NATIVE_BODY
    }
}

/// Which half of the router answered a request.
///
/// Until #278 this distinguished native answers from sidecar forwards, which
/// was the whole point of the access line while two implementations ran at
/// once. The forwarding variants died with the sidecar; what remains still
/// says *how* a request was handled, which is what a reader debugging a log
/// needs first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Served {
    /// A buffered native handler answered — or the router rejected the request
    /// before one ran (over-cap body, handler `Err` rendered as a 500).
    Native,
    /// The streaming native half answered, or rejected before streaming began.
    NativeStream,
    /// No module claims this route; chi's own 404 was answered.
    Unrouted,
    /// `guards.rs` refused the request before routing was decided (#329).
    Rejected,
}

impl Served {
    fn label(self) -> &'static str {
        match self {
            Served::Native => "native",
            Served::NativeStream => "native-stream",
            Served::Unrouted => "unrouted",
            Served::Rejected => "rejected",
        }
    }
}

/// What the access line is logged at.
///
/// `tauri_plugin_log` is built at `LevelFilter::Info` in `lib.rs`, so this split
/// is what the log actually contains by default: every state-changing request,
/// every request that failed, and none of the successful reads.
///
/// Three arms rather than two, and the status arm is why:
///
/// - The argument for putting reads at debug is **volume**. The UI polls
///   `GET /api/claude-sessions/status` on a timer for the whole length of a
///   scan, and an info line per poll buries everything else, which is how a log
///   stops being read.
/// - A read that *failed* is not that volume. It is the one read anybody wants
///   in the file, so a 4xx or 5xx outranks the method entirely.
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

async fn handle(req: Request<Body>) -> Response<Body> {
    let path = log_path(req.uri());

    // Anything that isn't the API is a frontend route. In release the assets
    // are embedded here; in debug the webview loads Vite directly and only
    // ever reaches this server for /api, so this arm is release-only.
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
    // client sends. The log is raw because it should record what arrived, not
    // what matched.
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
    let (response, served) = dispatch(req, path.clone()).await;

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
    // segment — the agent slug (derived from the name the user typed) and the
    // settings-profile id. A name is not a body, and dropping the segment would
    // leave the line unable to say which agent was written, which is most of
    // what it is for.
    //
    // The elapsed figure is time-to-headers whenever the body is still
    // arriving — `native-stream` always. Only buffered answers report a
    // duration the request actually took.
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

/// Answer one `/api` request, reporting which half of the router did it.
///
/// Split out of [`handle`] so the access line has exactly one call site. There
/// are several ways out of here, and logging at each of them is the shape that
/// drifts: the next route adds another and quietly stops being recorded.
async fn dispatch(req: Request<Body>, raw_path: String) -> (Response<Body>, Served) {
    // `internal/server/guards.go`, applied **before** routing (#329) — so every
    // route, claimed or not, is refused identically. The proxy used to be the
    // only place the browser's `Host` could be checked because `forward`
    // rewrote it; the forward is gone, but the ordering stays: the guards need
    // no route to say no.
    if let Some((status, message)) = crate::guards::reject(&req) {
        return (error_response(status, message), Served::Rejected);
    }

    // From here on, the path is the one **chi** would route on, not the raw
    // request target (#294). They are not the same string: `net/http` decodes
    // the target into `url.URL` before any handler runs, and chi routes on
    // `RawPath` when the escaping is non-canonical and on the decoded `Path`
    // when it is not — so `/api/agents/a%2Db` is `a%2Db` to Go and
    // `/api/agents/a%20b` is `a b`.
    let path = match native::gourl::route_path(&raw_path) {
        Some(path) => path,
        // No route path at all: either the escaping is malformed — Go answered
        // 400 from inside `net/http` before any handler — or it is canonical
        // and the decoded path is not UTF-8, a string Rust cannot carry, which
        // no real client produces. Both used to forward so Go could answer;
        // now the first is the same 400 and the second is the router's 404.
        None => {
            log::debug!("no route path for {raw_path}: malformed escape or non-UTF-8 decoded path");
            if raw_path.as_bytes().contains(&b'%') && !percent_escapes_are_well_formed(&raw_path) {
                return (
                    text_response(StatusCode::BAD_REQUEST, "400 Bad Request"),
                    Served::Unrouted,
                );
            }
            return (not_found(), Served::Unrouted);
        }
    };

    // The streaming half (#276). Checked before the buffered one because a
    // chat turn must never be collected into a `Vec<u8>`: it is the one
    // response whose whole point is arriving in pieces.
    if native::claims_stream(req.method(), &path) {
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

        let stream_req = native::StreamRequest {
            method: parts.method.clone(),
            path: path.clone(),
            body: body_bytes.to_vec(),
            db_path: match crate::paths::database_path() {
                Some(path) => path,
                None => {
                    log::error!("native stream for {path}: no data dir");
                    return (
                        error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal server error"),
                        Served::NativeStream,
                    );
                }
            },
        };

        return match native::serve_stream(stream_req).await {
            Ok(response) => (response, Served::NativeStream),
            // A streaming handler answers its own error cases before the
            // stream begins; an `Err` that reaches here is a machinery
            // failure, rendered as `httpErr`'s default 500.
            Err(e) => {
                log::warn!("native stream for {path} failed: {e}");
                (
                    error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal server error"),
                    Served::NativeStream,
                )
            }
        };
    }

    if !native::claims(req.method(), &path) {
        // chi's own answer for a route it does not know. Includes the routes
        // deliberately dropped at the cut-over — the WhatsApp reads (#273),
        // the session journey and per-session insights, which no desktop view
        // calls — recorded in `desktop/parity/read_routes.json`.
        return (not_found(), Served::Unrouted);
    }

    // A native handler needs the body, so it has to be read here.
    let (parts, body) = req.into_parts();
    let body_bytes = match axum::body::to_bytes(body, max_body_for(&path)).await {
        Ok(bytes) => bytes,
        Err(e) => {
            log::warn!("native handler for {path}: reading body failed: {e}");
            return (
                error_response(StatusCode::BAD_REQUEST, "could not read request body"),
                Served::Native,
            );
        }
    };

    // Reading SQLite is blocking work; keeping it off the axum worker means
    // one slow read cannot stall an SSE stream sharing the runtime.
    let method = parts.method.clone();
    let query = parts.uri.query().unwrap_or_default().to_string();
    // Carried because one claimed route needs it: a multipart body is
    // unparseable without the boundary, and the boundary is only in the
    // header. Everything else ignores it.
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    // Likewise carried for one route: the Telegram webhook authenticates on
    // this header alone, being mounted outside `/api` and so outside both
    // guards. See `native::Request::secret_token`.
    let secret_token = parts
        .headers
        .get("x-telegram-bot-api-secret-token")
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
                secret_token: &secret_token,
                body: &body_bytes,
            })
        })
        .await
        .unwrap_or_else(|e| Err(format!("native handler panicked: {e}")))
    };

    match native_answer {
        Ok(answer) => (native::response(answer), Served::Native),
        // A handler answers its deliberate 4xx cases itself; an `Err` that
        // reaches here is a machinery failure — a driver error, a panic —
        // rendered exactly as Go's `httpErr` default rendered one, with the
        // reason in the log where Go put it too.
        Err(e) => {
            log::warn!("native handler for {path} failed: {e}");
            (
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal server error"),
                Served::Native,
            )
        }
    }
}

/// Whether every `%` in the raw target is followed by two hex digits — the
/// well-formedness half of `gourl::route_path`'s two "no route path" cases,
/// used only to pick between Go's 400 (malformed escape) and the router's 404
/// (unrepresentable path).
fn percent_escapes_are_well_formed(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            match (bytes.get(i + 1), bytes.get(i + 2)) {
                (Some(a), Some(b)) if a.is_ascii_hexdigit() && b.is_ascii_hexdigit() => i += 3,
                _ => return false,
            }
        } else {
            i += 1;
        }
    }
    true
}

/// chi's `NotFound` — `http.NotFound(w, r)`: `404 page not found` under
/// `text/plain; charset=utf-8` with nosniff, trailing newline included.
fn not_found() -> Response<Body> {
    text_response(StatusCode::NOT_FOUND, "404 page not found")
}

/// `http.Error`'s shape: plain text, one trailing newline, nosniff.
fn text_response(status: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(format!("{message}\n")))
        .unwrap_or_else(|_| Response::new(Body::empty()))
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

    /// The labels are the whole point of the line — they say how a request was
    /// handled (#301). Pinned because a rename would silently reword every
    /// historical log's meaning.
    #[test]
    fn served_labels_name_each_half_of_the_router() {
        assert_eq!(Served::Native.label(), "native");
        assert_eq!(Served::NativeStream.label(), "native-stream");
        assert_eq!(Served::Unrouted.label(), "unrouted");
        assert_eq!(Served::Rejected.label(), "rejected");
    }

    /// The guards run before routing is decided, so a claimed route and an
    /// unclaimed one are refused identically (#329).
    #[tokio::test]
    async fn a_guard_rejection_precedes_routing() {
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

        let (response, served) = dispatch(req, path.clone()).await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(served, Served::Rejected);

        // And a foreign Host on a route nothing claims: still refused by the
        // guard, never reaching the 404 arm.
        let path = "/api/no-such-route".to_string();
        let req = Request::builder()
            .method(Method::PUT)
            .uri(&path)
            .header(header::HOST, "attacker.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("request");

        let (response, served) = dispatch(req, path).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(served, Served::Rejected);
    }

    /// A route nothing claims answers chi's own 404 — status, content type,
    /// nosniff and the exact body Go's router wrote.
    #[tokio::test]
    async fn an_unclaimed_route_answers_chis_404() {
        let path = "/api/claude-sessions/abc/journey".to_string();
        let req = Request::builder()
            .method(Method::GET)
            .uri(&path)
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .expect("request");

        let (response, served) = dispatch(req, path).await;
        assert_eq!(served, Served::Unrouted);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            response
                .headers()
                .get("X-Content-Type-Options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], b"404 page not found\n");
    }

    /// A malformed percent escape is Go's own 400 from inside `net/http`,
    /// answered before any handler on either side.
    #[tokio::test]
    async fn a_malformed_escape_is_a_400_not_a_404() {
        let path = "/api/agents/a%2".to_string();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/agents/a%252") // uri parsing needs a valid target; dispatch takes the raw path separately
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .expect("request");

        let (response, served) = dispatch(req, path).await;
        assert_eq!(served, Served::Unrouted);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
        // Reads. `HEAD` and `OPTIONS` reach the router because it is
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

    /// A claimed route whose body is over [`max_body_for`] is answered 400 by
    /// the router itself, before any handler runs.
    #[tokio::test]
    async fn oversized_body_on_a_claimed_route_is_answered_with_a_400() {
        let path = "/api/agents".to_string();
        let req = Request::builder()
            .method(Method::POST)
            .uri(&path)
            // Both headers are required since #329: the guards run ahead of
            // routing, so a request without them never reaches the arm under
            // test.
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(vec![0u8; MAX_NATIVE_BODY + 1]))
            .expect("request");

        let (response, served) = dispatch(req, path).await;

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

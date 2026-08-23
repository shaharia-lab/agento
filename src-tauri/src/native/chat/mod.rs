//! Chat execution: the SSE turn, and the three routes that steer it.
//!
//! Mirrors `handleSendMessage`, `handleProvideInput`, `handlePermissionResponse`
//! and `handleStopSession` (`internal/api/chats.go`).
//!
//! # Why all four are here, and how the split stays safe
//!
//! They share the process-local [`live`] registry: `/messages` puts a session
//! in, the other three look one up. Splitting them across two implementations
//! would leave `/stop` searching a registry that never saw the session — the
//! button would silently do nothing.
//!
//! Since #278 removed the Go sidecar, this registry is the **only** place a
//! live session can exist, so "no live session here" is no longer a reason to
//! forward — it is the answer. The three steering routes answer Go's own 409s
//! (`handleProvideInput`, `handlePermissionResponse`, `handleStopSession` each
//! had a distinct string; they are reproduced verbatim), and a chat this
//! runtime cannot execute — `whatsapp` tools are dropped (#273), `mcps.yaml`
//! is not read — is a 500 carrying the reason, produced *before* the
//! subprocess is spawned. See `runner::build_options`.

pub mod live;
pub mod persist;
pub mod runner;
pub mod sse;
pub mod turn;

use axum::body::Body;
use axum::http::{header, Method, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::native::{BoxFuture, StreamEndpoint, StreamRequest};

/// This module's entry in `native::STREAM_ENDPOINTS`.
pub const ENDPOINT: StreamEndpoint = StreamEndpoint {
    name: "chat",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    *method == Method::POST && route_of(path).is_some()
}

enum Route<'a> {
    Messages(&'a str),
    Input(&'a str),
    Permission(&'a str),
    Stop(&'a str),
}

/// `/api/chats/{id}/<action>` and nothing else. The id is one segment, so a
/// nested path cannot be swallowed.
fn route_of(path: &str) -> Option<Route<'_>> {
    let rest = path.strip_prefix("/api/chats/")?;
    let (id, action) = rest.split_once('/')?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    match action {
        "messages" => Some(Route::Messages(id)),
        "input" => Some(Route::Input(id)),
        "permission" => Some(Route::Permission(id)),
        "stop" => Some(Route::Stop(id)),
        _ => None,
    }
}

fn serve(req: StreamRequest) -> BoxFuture<'static, Result<Response<Body>, String>> {
    Box::pin(async move {
        match route_of(&req.path) {
            Some(Route::Messages(id)) => {
                let content = match decode_content(&req.body) {
                    Ok(content) => content,
                    Err(response) => return Ok(*response),
                };
                turn::run(req.db_path.clone(), id.to_string(), content).await
            }
            Some(Route::Input(id)) => Ok(provide_input(id, &req.body)),
            Some(Route::Permission(id)) => Ok(permission_response(id, &req.body)),
            Some(Route::Stop(id)) => Ok(stop(id).await),
            None => Err(format!("{} is not a chat action", req.path)),
        }
    })
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SendMessageRequest {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    content: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ProvideInputRequest {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    answer: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PermissionRequestBody {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    allow: bool,
}

/// Boxed error: a `Response<Body>` is a large variant to return by value, and
/// this is the error path.
fn decode_content(body: &[u8]) -> Result<String, Box<Response<Body>>> {
    let req: SendMessageRequest = match crate::native::writes::decode_body(body) {
        Ok(req) => req,
        Err(_) => {
            return Err(Box::new(error_json(
                StatusCode::BAD_REQUEST,
                "invalid JSON body",
            )))
        }
    };
    if req.content.is_empty() {
        return Err(Box::new(error_json(
            StatusCode::BAD_REQUEST,
            "content is required",
        )));
    }
    Ok(req.content)
}

/// `handleProvideInput`. The answer reaches whichever half of the turn is
/// waiting — the permission handler or the post-result continuation.
fn provide_input(id: &str, body: &[u8]) -> Response<Body> {
    let req: ProvideInputRequest = match crate::native::writes::decode_body(body) {
        Ok(req) => req,
        Err(_) => return error_json(StatusCode::BAD_REQUEST, "invalid JSON body"),
    };
    if req.answer.is_empty() {
        return error_json(StatusCode::BAD_REQUEST, "answer is required");
    }
    let Some((_, input_tx, _)) = live::registry().get(id) else {
        // This registry is the only holder of live sessions since #278, so
        // Go's own 409 — its exact string — is the answer, not a forward.
        return error_json(
            StatusCode::CONFLICT,
            "no active session awaiting input for this chat",
        );
    };
    // Capacity 1, so a successful send is the approximation of "awaiting
    // input" — the same one Go makes.
    match input_tx.try_send(req.answer) {
        Ok(()) => no_content(),
        Err(_) => error_json(
            StatusCode::CONFLICT,
            "session is not currently awaiting input",
        ),
    }
}

/// `handlePermissionResponse`.
fn permission_response(id: &str, body: &[u8]) -> Response<Body> {
    let req: PermissionRequestBody = match crate::native::writes::decode_body(body) {
        Ok(req) => req,
        Err(_) => return error_json(StatusCode::BAD_REQUEST, "invalid JSON body"),
    };
    let Some((_, _, perm_tx)) = live::registry().get(id) else {
        return error_json(StatusCode::CONFLICT, "no active session for this chat");
    };
    match perm_tx.try_send(req.allow) {
        Ok(()) => no_content(),
        Err(_) => error_json(
            StatusCode::CONFLICT,
            "session is not currently awaiting a permission response",
        ),
    }
}

/// `handleStopSession`. Returns **204 even when the interrupt fails** — Go only
/// logs it — and never closes the session; the stream's own teardown owns that.
async fn stop(id: &str) -> Response<Body> {
    let Some((control, _, _)) = live::registry().get(id) else {
        return error_json(StatusCode::CONFLICT, "no active session for this chat");
    };
    if let Err(e) = control.interrupt().await {
        log::warn!("interrupt session failed for chat {id}: {e}");
    }
    no_content()
}

fn no_content() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn error_json(status: StatusCode, message: &str) -> Response<Body> {
    #[derive(Serialize)]
    struct ErrorBody<'a> {
        error: &'a str,
    }
    let body = crate::native::gojson::to_vec(&ErrorBody { error: message }).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_chat_actions_are_claimed_and_nothing_else_is() {
        for action in ["messages", "input", "permission", "stop"] {
            let path = format!("/api/chats/abc/{action}");
            assert!(claims(&Method::POST, &path), "{path}");
            // Only POST: the CRUD verbs on the same prefix belong to #274.
            assert!(!claims(&Method::GET, &path));
            assert!(!claims(&Method::DELETE, &path));
        }

        // The CRUD routes, which `native::chats` owns.
        assert!(!claims(&Method::POST, "/api/chats"));
        assert!(!claims(&Method::POST, "/api/chats/abc"));
        // Unknown actions and nested paths are unrouted.
        assert!(!claims(&Method::POST, "/api/chats/abc/unknown"));
        assert!(!claims(&Method::POST, "/api/chats//messages"));
        assert!(!claims(&Method::POST, "/api/chats/abc/messages/extra"));
    }

    /// The registry is empty in a unit test, so every steering route takes the
    /// "no live session" path. With the sidecar gone (#278) this registry is
    /// the only holder, so the answer is Go's own 409 — each handler had its
    /// own string, reproduced verbatim.
    #[tokio::test]
    async fn a_chat_with_no_live_session_answers_gos_own_409() {
        let cases: [(Response<Body>, &str); 3] = [
            (
                provide_input("no-such-chat", br#"{"answer":"yes"}"#),
                "no active session awaiting input for this chat",
            ),
            (
                permission_response("no-such-chat", br#"{"allow":true}"#),
                "no active session for this chat",
            ),
            (
                stop("no-such-chat").await,
                "no active session for this chat",
            ),
        ];
        for (response, want) in cases {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            assert_eq!(
                String::from_utf8_lossy(&body),
                format!("{{\"error\":\"{want}\"}}\n"),
            );
        }
    }

    /// Body validation happens *before* the registry lookup, exactly as Go's
    /// handlers decode before they look up the session.
    #[test]
    fn the_body_is_validated_before_the_session_is_looked_up() {
        for body in [&b"{}"[..], b"not json"] {
            let response = provide_input("any", body);
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    /// `allow` defaults to false rather than failing, matching Go's decoder:
    /// an absent key is the zero value, and a permission body with no `allow`
    /// is a deny.
    #[test]
    fn an_absent_allow_is_a_deny_not_an_error() {
        // A 409 rather than a 400 proves the body parsed and the registry was
        // consulted.
        let response = permission_response("no-such-chat", b"{}");
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}

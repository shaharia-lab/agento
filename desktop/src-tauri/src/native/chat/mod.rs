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
//! But not every chat *can* run here: an agent whose tools come from an
//! integration needs one MCP server per provider, and this port has four of the
//! six still to write (#313–#316), so `runner::build_options` refuses those and
//! they keep running on the sidecar. Parts of that refusal have gone — the
//! **local** in-process server (#310), then any agent whose `capabilities.mcp`
//! names only hosted integrations (**github** since #311, **confluence** since
//! #317) — but each only shrinks the set of chats Go still holds; it does not
//! empty it, and the argument here turns on the set being non-empty.
//! So the three steering routes are answered natively **only when Rust holds a
//! live session for that chat**, and forward otherwise. Go then answers —
//! correctly, because it is the side that has the session — and a chat with no
//! live session anywhere gets Go's own 409 rather than a second copy of it.
//!
//! That is what lets the four move together without requiring *every* chat to
//! move at once.

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
            // `Forward` becomes the seam's `Err`, which is what makes Go answer.
            // The reason travels with it so the log says which chat and why.
            Some(Route::Input(id)) => steer(provide_input(id, &req.body), id),
            Some(Route::Permission(id)) => steer(permission_response(id, &req.body), id),
            Some(Route::Stop(id)) => steer(stop(id).await, id),
            None => Err(format!("{} is not a chat action", req.path)),
        }
    })
}

/// What one of the three steering routes decided.
///
/// A distinct type rather than a sentinel status: "forward" and "answered with
/// a status" are different kinds of outcome, and encoding the first as a
/// reserved status code would mean a real upstream response could impersonate
/// it. There is no number here to collide with.
enum Steer {
    Answered(Response<Body>),
    Forward,
}

fn steer(outcome: Steer, id: &str) -> Result<Response<Body>, String> {
    match outcome {
        Steer::Answered(response) => Ok(response),
        Steer::Forward => Err(format!(
            "chat {id:?} has no live session here; the sidecar may have one"
        )),
    }
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
fn provide_input(id: &str, body: &[u8]) -> Steer {
    let req: ProvideInputRequest = match crate::native::writes::decode_body(body) {
        Ok(req) => req,
        Err(_) => return Steer::Answered(error_json(StatusCode::BAD_REQUEST, "invalid JSON body")),
    };
    if req.answer.is_empty() {
        return Steer::Answered(error_json(StatusCode::BAD_REQUEST, "answer is required"));
    }
    let Some((_, input_tx, _)) = live::registry().get(id) else {
        // Not ours: Go may still have this session, and its 409 is the right
        // answer if neither does.
        return Steer::Forward;
    };
    // Capacity 1, so a successful send is the approximation of "awaiting
    // input" — the same one Go makes.
    match input_tx.try_send(req.answer) {
        Ok(()) => Steer::Answered(no_content()),
        Err(_) => Steer::Answered(error_json(
            StatusCode::CONFLICT,
            "session is not currently awaiting input",
        )),
    }
}

/// `handlePermissionResponse`.
fn permission_response(id: &str, body: &[u8]) -> Steer {
    let req: PermissionRequestBody = match crate::native::writes::decode_body(body) {
        Ok(req) => req,
        Err(_) => return Steer::Answered(error_json(StatusCode::BAD_REQUEST, "invalid JSON body")),
    };
    let Some((_, _, perm_tx)) = live::registry().get(id) else {
        return Steer::Forward;
    };
    match perm_tx.try_send(req.allow) {
        Ok(()) => Steer::Answered(no_content()),
        Err(_) => Steer::Answered(error_json(
            StatusCode::CONFLICT,
            "session is not currently awaiting a permission response",
        )),
    }
}

/// `handleStopSession`. Returns **204 even when the interrupt fails** — Go only
/// logs it — and never closes the session; the stream's own teardown owns that.
async fn stop(id: &str) -> Steer {
    let Some((control, _, _)) = live::registry().get(id) else {
        return Steer::Forward;
    };
    if let Err(e) = control.interrupt().await {
        log::warn!("interrupt session failed for chat {id}: {e}");
    }
    Steer::Answered(no_content())
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
        // Unknown actions and nested paths forward.
        assert!(!claims(&Method::POST, "/api/chats/abc/unknown"));
        assert!(!claims(&Method::POST, "/api/chats//messages"));
        assert!(!claims(&Method::POST, "/api/chats/abc/messages/extra"));
    }

    /// The registry is empty in a unit test, so every steering route takes the
    /// "not ours" path — which is the behaviour that keeps a chat running on Go
    /// working.
    #[test]
    fn a_chat_with_no_live_session_here_forwards_rather_than_409ing() {
        assert!(matches!(
            provide_input("no-such-chat", br#"{"answer":"yes"}"#),
            Steer::Forward
        ));
        assert!(matches!(
            permission_response("no-such-chat", br#"{"allow":true}"#),
            Steer::Forward
        ));
    }

    /// Body validation happens *before* the registry lookup, so a malformed
    /// request is rejected here rather than being forwarded for Go to reject
    /// identically.
    #[test]
    fn the_body_is_validated_before_the_session_is_looked_up() {
        for body in [&b"{}"[..], b"not json"] {
            match provide_input("any", body) {
                Steer::Answered(response) => {
                    assert_eq!(response.status(), StatusCode::BAD_REQUEST)
                }
                Steer::Forward => panic!("a malformed body must be rejected, not forwarded"),
            }
        }
    }

    /// `allow` defaults to false rather than failing, matching Go's decoder:
    /// an absent key is the zero value, and a permission body with no `allow`
    /// is a deny.
    #[test]
    fn an_absent_allow_is_a_deny_not_an_error() {
        // Reaching the registry lookup at all proves the body parsed.
        assert!(matches!(
            permission_response("no-such-chat", b"{}"),
            Steer::Forward
        ));
    }
}

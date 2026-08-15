//! One chat turn: spawn the CLI, stream its events as SSE, and persist what it
//! produced. Mirrors `handleSendMessage` → `streamAgentSession` →
//! `consumeAgentEvents` → `commitMessage` (`internal/api/chats.go`).
//!
//! # The shape of the loop
//!
//! Four things can happen and the loop selects over all of them: an event
//! arrives from the CLI, the permission handler asks a question, the permission
//! handler asks for an allow/deny, or the client disconnects. There is **no
//! timeout** and **no heartbeat** — a long tool call sends nothing for minutes,
//! by design.
//!
//! # Rules that are silent when broken
//!
//! - **There is no terminal event.** `result` means "turn done", not "stream
//!   done": an `AskUserQuestion` keeps the same subprocess alive across several
//!   turns in one HTTP request. The loop ends on stream close, on an error
//!   result, or on a final result with nothing pending.
//! - **A mid-stream failure is a `result` with `is_error: true`**, never an
//!   `error` event, because the 200 was committed before the first frame.
//! - **An event with no raw line emits nothing.** The SDK synthesizes process
//!   failures as a `system` event with no `raw`, and Go's `len(event.Raw) > 0`
//!   guard means the client is told *nothing* when the subprocess dies — the
//!   stream simply ends. Reproduced.
//! - **A turn with no final text persists no messages** — not even the user's —
//!   so an interrupted stream cannot leave an orphan the CLI's own transcript
//!   does not have. The session *row* is still updated.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Response, StatusCode};
use serde::Serialize;
use serde_json::value::RawValue;
use tokio::sync::mpsc;

use crate::claude::messages::Event;
use crate::claude::permissions::{PermissionContext, PermissionResult};
use crate::claude::session::Session;

use super::live::{registry, LiveSession};
use super::runner::{self, RunSpec};
use super::sse;

/// How many notifications may queue before one is dropped.
///
/// Go's `questionCh`/`permissionReqCh` are capacity 4 with a non-blocking send,
/// so a full buffer silently drops the notification — and the handler then waits
/// forever for an answer the user was never asked for. Matching the capacity
/// matters more than it looks: a different one changes when that happens.
const NOTIFY_CAPACITY: usize = 4;

/// The synthetic event announcing an `AskUserQuestion`.
#[derive(Serialize)]
struct UserInputRequired<'a> {
    input: &'a RawValue,
}

/// The synthetic event announcing a tool permission prompt.
#[derive(Serialize)]
struct PermissionRequest {
    tool_name: String,
    /// `omitempty` on Go's `json.RawMessage` tests byte length, so an absent
    /// input drops the key entirely.
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Box<RawValue>>,
}

/// What the handler goroutine sends the stream.
enum Notify {
    Question(Box<RawValue>),
    Permission(PermissionRequest),
}

/// What one turn accumulated, and what `commit` writes.
#[derive(Default)]
pub struct TurnState {
    pub assistant_text: String,
    pub sdk_session_id: String,
    pub blocks: Vec<crate::native::chats::MessageBlock>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
}

/// Run a turn and stream it.
///
/// Everything that can fail *before* the subprocess exists does so, so an `Err`
/// here means nothing happened and the proxy may safely forward to Go.
pub async fn run(
    db_path: std::path::PathBuf,
    chat_id: String,
    content: String,
) -> Result<Response<Body>, String> {
    // Reject a concurrent send first, exactly as Go does: a second request
    // would read a stale sdk_session_id and start a new CLI session instead of
    // resuming the right one.
    if !registry().try_lock(&chat_id) {
        return Ok(error_json(
            StatusCode::CONFLICT,
            "session is busy, wait for the current message to complete",
        ));
    }
    // From here every early return must release the lock, or the chat is
    // wedged until the process restarts.
    let guard = LockGuard {
        id: chat_id.clone(),
    };

    let loaded = match runner::load(&db_path, &chat_id) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => {
            return Ok(error_json(
                StatusCode::NOT_FOUND,
                &format!("chat {chat_id:?} not found"),
            ))
        }
        Err(e) => return Err(e),
    };
    let (row, agent) = loaded;

    let fallback_model = if row.model.is_empty() {
        default_model(&db_path)
    } else {
        row.model.clone()
    };

    let (notify_tx, notify_rx) = mpsc::channel::<Notify>(NOTIFY_CAPACITY);
    let (input_tx, input_rx) = mpsc::channel::<String>(1);
    let (perm_tx, perm_rx) = mpsc::channel::<bool>(1);

    let answers = Arc::new(Answers {
        input: tokio::sync::Mutex::new(input_rx),
        permission: tokio::sync::Mutex::new(perm_rx),
    });
    let handler = build_permission_handler(notify_tx.clone(), Arc::clone(&answers));

    let spec = RunSpec {
        agent,
        fallback_model,
        working_dir: row.working_dir.clone(),
        settings_profile_id: row.settings_profile_id.clone(),
        resume_session_id: Some(row.sdk_session_id.clone()).filter(|s| !s.is_empty()),
        chat_id: chat_id.clone(),
    };
    // Refuses for an agent whose tools this port cannot supply — before any
    // subprocess exists, so forwarding is safe.
    let options = runner::build_options(&spec, handler)?;

    // The subprocess starts here. Past this point a failure can no longer
    // forward, because Go would spawn a second one.
    let mut session = Session::new(options)
        .await
        .map_err(|e| format!("starting agent session: {e}"))?;
    if let Err(e) = session.send(&content).await {
        session.close();
        return Err(format!("sending first message: {e}"));
    }

    registry().put(
        &chat_id,
        LiveSession {
            control: session.control(),
            input_tx,
            permission_resp_tx: perm_tx,
        },
    );

    let is_first_message = row.title == "New Chat";
    let (body_tx, body_rx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);

    tokio::spawn(async move {
        let state = stream_events(&mut session, notify_rx, &body_tx, &answers).await;

        // Go's defer order: forget the live session (which also releases the
        // busy lock), then close the subprocess.
        drop(guard);
        registry().release(&chat_id);
        session.close();

        // The commit is detached from the request on purpose, so a client that
        // disconnected mid-stream still has its turn persisted. Errors are
        // logged and never surfaced — the stream has already ended.
        if let Err(e) = super::persist::commit(&db_path, &row, &content, &state, is_first_message) {
            log::error!("commit message failed for chat {chat_id}: {e}");
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(body_rx);
    Ok(sse_response(Body::from_stream(stream)))
}

/// Releases the busy lock if an early return happens before the stream task
/// takes ownership of it.
struct LockGuard {
    id: String,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // The stream task calls `release` itself after dropping this, so a
        // double release is possible and harmless — both are idempotent
        // removals from a set.
        registry().release(&self.id);
    }
}

/// The receivers the permission handler waits on.
///
/// Behind mutexes because the handler is an `Arc<dyn Fn>` called from the SDK's
/// reader task and must be `Send + Sync`, while an `mpsc::Receiver` is not
/// shareable. Only one permission round trip is ever in flight — the CLI blocks
/// on the answer — so the mutex is never contended.
struct Answers {
    input: tokio::sync::Mutex<mpsc::Receiver<String>>,
    permission: tokio::sync::Mutex<mpsc::Receiver<bool>>,
}

/// `buildPermissionHandler`: `AskUserQuestion` is answered by *denying* the tool
/// with the user's text as the message; everything else is a genuine
/// allow/deny prompt.
fn build_permission_handler(
    notify: mpsc::Sender<Notify>,
    answers: Arc<Answers>,
) -> crate::claude::permissions::PermissionHandler {
    Arc::new(
        move |tool_name: String, input: Option<Box<RawValue>>, _ctx: PermissionContext| {
            let notify = notify.clone();
            let answers = Arc::clone(&answers);
            Box::pin(async move {
                if tool_name == "AskUserQuestion" {
                    let raw = input.unwrap_or_else(empty_object);
                    // Non-blocking: a full buffer drops the notification, matching
                    // Go's `default:` arm. The wait below is then unbounded, which
                    // is how a dropped notification wedges the turn — reproduced
                    // rather than fixed, so the two implementations agree.
                    let _ = notify.try_send(Notify::Question(raw));
                    let mut rx = answers.input.lock().await;
                    return match rx.recv().await {
                        // The answer travels as the *denial message*. That is how
                        // it reaches the model without the tool ever executing.
                        Some(answer) => PermissionResult::deny(answer),
                        None => PermissionResult::deny("request canceled"),
                    };
                }

                let _ = notify.try_send(Notify::Permission(PermissionRequest { tool_name, input }));
                let mut rx = answers.permission.lock().await;
                match rx.recv().await {
                    Some(true) => PermissionResult::allow(),
                    Some(false) => PermissionResult::deny("Permission denied by user"),
                    None => PermissionResult::deny("request canceled"),
                }
            })
        },
    )
}

fn empty_object() -> Box<RawValue> {
    RawValue::from_string("{}".to_string()).expect("`{}` is valid JSON")
}

/// The event loop. Returns what the turn accumulated.
async fn stream_events(
    session: &mut Session,
    mut notify_rx: mpsc::Receiver<Notify>,
    out: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    answers: &Arc<Answers>,
) -> TurnState {
    let mut state = TurnState::default();
    let mut pending_input: Option<Box<RawValue>> = None;

    loop {
        tokio::select! {
            event = session.next_event() => {
                let Some(event) = event else {
                    // The subprocess ended. This is the only "the turn is over"
                    // signal there is.
                    return state;
                };
                if !forward_event(&event, out).await {
                    // The client hung up; the commit still runs.
                    return state;
                }
                match handle_event(session, &event, &mut state, &mut pending_input, out, answers).await {
                    Flow::Continue => {}
                    Flow::Stop => return state,
                }
            }
            Some(notification) = notify_rx.recv() => {
                let frame = match &notification {
                    Notify::Question(input) => {
                        // A prompt from the handler supersedes one noticed in
                        // the assistant event, so the user is not asked twice.
                        pending_input = None;
                        sse::json_frame("user_input_required", &UserInputRequired { input })
                    }
                    Notify::Permission(req) => sse::json_frame("permission_request", req),
                };
                match frame {
                    Ok(bytes) => {
                        if out.send(Ok(bytes)).await.is_err() {
                            return state;
                        }
                    }
                    Err(e) => log::error!("encoding synthetic event: {e}"),
                }
            }
            else => return state,
        }
    }
}

enum Flow {
    Continue,
    Stop,
}

/// Forward the CLI's own line verbatim. Returns false when the client is gone.
///
/// **An event with no raw line emits nothing** — the SDK synthesizes process
/// failures that way, so a crashed subprocess tells the client nothing and the
/// stream just ends.
async fn forward_event(event: &Event, out: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>) -> bool {
    let Some(raw) = event.raw.as_ref() else {
        return true;
    };
    out.send(Ok(sse::raw_frame(&event.event_type, raw.get().as_bytes())))
        .await
        .is_ok()
}

async fn handle_event(
    session: &Session,
    event: &Event,
    state: &mut TurnState,
    pending_input: &mut Option<Box<RawValue>>,
    out: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    answers: &Arc<Answers>,
) -> Flow {
    match event.event_type.as_str() {
        "assistant" => {
            if let Some(raw) = event.raw.as_ref() {
                super::persist::append_assistant_blocks(&mut state.blocks, raw.get().as_bytes());
                if let Some(input) = extract_ask_user_question(raw.get().as_bytes()) {
                    *pending_input = Some(input);
                }
            }
            Flow::Continue
        }
        "result" => {
            let Some(result) = event.result.as_ref() else {
                // A result line the SDK could not decode: forwarded raw above,
                // but it ends nothing.
                return Flow::Continue;
            };
            add_usage(state, result);

            if result.is_error {
                // Keep a previously good session id rather than blanking it, so
                // the next attempt still resumes the same CLI session.
                if !result.session_id.is_empty() {
                    state.sdk_session_id = result.session_id.clone();
                }
                return Flow::Stop;
            }

            state.sdk_session_id = result.session_id.clone();
            state.assistant_text = result.result.clone();

            let Some(input) = pending_input.take() else {
                return Flow::Stop; // the final result
            };

            // An `AskUserQuestion` was seen, so this `result` is *not* the end:
            // ask, wait, and continue the **same** subprocess into another turn.
            // This is why one HTTP request can span several turns, and why the
            // loop must never treat `result` as terminal.
            match sse::json_frame("user_input_required", &UserInputRequired { input: &input }) {
                Ok(bytes) => {
                    if out.send(Ok(bytes)).await.is_err() {
                        return Flow::Stop;
                    }
                }
                Err(e) => {
                    log::error!("encoding user_input_required: {e}");
                    return Flow::Stop;
                }
            }

            // The answer arrives on the same channel the permission handler
            // waits on — whichever is listening consumes it, exactly as Go's
            // shared `inputCh` does.
            let answer = {
                let mut rx = answers.input.lock().await;
                rx.recv().await
            };
            let Some(answer) = answer else {
                return Flow::Stop;
            };
            if let Err(e) = session.send(&answer).await {
                log::error!("injecting the answer failed: {e}");
                return Flow::Stop;
            }
            // Reset so the *next* turn's result is what gets persisted; the
            // blocks and token totals keep accumulating across turns.
            state.assistant_text.clear();
            Flow::Continue
        }
        _ => Flow::Continue,
    }
}

/// Token totals accumulate across **every** result in the request, including
/// error ones and every turn of an AskUserQuestion exchange.
fn add_usage(state: &mut TurnState, result: &crate::claude::messages::Result) {
    let usage = &result.usage;
    state.input_tokens += usage.input_tokens;
    state.output_tokens += usage.output_tokens;
    state.cache_creation_tokens += usage.cache_creation_input_tokens;
    state.cache_read_tokens += usage.cache_read_input_tokens;
}

/// The input of the first `tool_use` named `AskUserQuestion`, if any.
fn extract_ask_user_question(raw: &[u8]) -> Option<Box<RawValue>> {
    let value: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let content = value.get("message")?.get("content")?.as_array()?;
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
            && block.get("name").and_then(|n| n.as_str()) == Some("AskUserQuestion")
        {
            let input = block.get("input")?;
            return RawValue::from_string(input.to_string()).ok();
        }
    }
    None
}

/// The four headers Go sets, and the 200 written before any event.
fn sse_response(body: Body) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// A JSON error answered *before* the SSE headers — which is the only window in
/// which an HTTP status can still say anything.
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

fn default_model(db_path: &std::path::Path) -> String {
    let Ok(conn) = crate::native::db::open_read_only(db_path) else {
        return String::new();
    };
    crate::native::settings::load_stored(&conn).default_model
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ask_user_question_tool_use_is_found() {
        let raw = br#"{"message":{"content":[
            {"type":"text","text":"thinking"},
            {"type":"tool_use","id":"t1","name":"AskUserQuestion","input":{"q":"which?"}}
        ]}}"#;
        let found = extract_ask_user_question(raw).expect("found");
        assert_eq!(found.get(), r#"{"q":"which?"}"#);
    }

    #[test]
    fn another_tool_use_is_not_mistaken_for_one() {
        let raw = br#"{"message":{"content":[
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}
        ]}}"#;
        assert!(extract_ask_user_question(raw).is_none());
    }

    #[test]
    fn a_malformed_assistant_event_yields_nothing_rather_than_failing() {
        assert!(extract_ask_user_question(b"not json").is_none());
        assert!(extract_ask_user_question(b"{}").is_none());
    }

    /// `input` is omitted when absent, matching Go's `omitempty` on a
    /// `json.RawMessage` — which tests byte length, so an absent input drops
    /// the key rather than sending `null`.
    #[test]
    fn a_permission_request_omits_an_absent_input() {
        let req = PermissionRequest {
            tool_name: "Bash".into(),
            input: None,
        };
        let frame = sse::json_frame("permission_request", &req).expect("encode");
        let text = String::from_utf8(frame).unwrap();
        assert_eq!(
            text,
            "event: permission_request\ndata: {\"tool_name\":\"Bash\"}\n\n"
        );

        let req = PermissionRequest {
            tool_name: "Bash".into(),
            input: Some(RawValue::from_string(r#"{"command":"ls"}"#.into()).unwrap()),
        };
        let frame = sse::json_frame("permission_request", &req).expect("encode");
        let text = String::from_utf8(frame).unwrap();
        assert!(text.contains(r#""input":{"command":"ls"}"#), "{text}");
    }
}

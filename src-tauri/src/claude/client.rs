//! The public entry points and the `Stream` type, ported from
//! `claude/client.go`.
//!
//! Go hands the caller a `*Stream` carrying both an events channel and the
//! control methods, and documents that the control methods are safe to call
//! from any goroutine while another ranges the channel. Rust cannot express
//! that on one type — reading the channel needs `&mut self` — so the two halves
//! are split: [`Stream`] owns the receiver, and [`StreamControl`] is a cheap
//! clonable handle carrying every control method. `Stream` delegates all of
//! them, so single-task code never notices the split, and
//! [`Stream::control`] is how a second task gets the ability to interrupt.
//!
//! Dropping a [`Stream`] shuts the subprocess down. That is the Rust stand-in
//! for Go's context cancellation, and it is what stops a cancelled chat from
//! leaving an orphaned `claude` process holding a session open.

use std::sync::Arc;

use serde_json::value::RawValue;
use tokio::sync::{mpsc, oneshot};

use super::errors::{Error, Result};
use super::init_types::{AccountInfo, AgentInfo, InitializeResponse, ModelInfo, SlashCommand};
use super::messages::{message_type, system_subtype, Event, Result as RunResult};
use super::options::Options;
use super::process::{self, ControlResponse, Shared};

/// The result of an interrupt.
///
/// The CLI advertises this through the `interrupt_receipt_v1` capability on
/// `system`/`init`; CLIs without it reply to an interrupt with an empty success
/// and no receipt at all, which is reported as `None` rather than an error.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InterruptReceipt {
    /// The uuids of async user messages that survived the interrupt and remain
    /// queued for a subsequent turn. Empty when nothing survived.
    pub still_queued: Vec<String>,
}

/// Extracts the receipt from an interrupt control_response body.
///
/// Decoding is deliberately lenient: an interrupt the CLI acknowledged has
/// succeeded regardless of what the payload looks like, so anything absent,
/// null, unparseable, or of an unexpected type yields no receipt instead of an
/// error. Only a well-formed `still_queued` array produces one, and only its
/// string elements are kept — matching the reference implementation's
/// `typeof r === "string"` filter, which exists because a JSON null would
/// otherwise smuggle in an empty id.
fn decode_interrupt_receipt(body: Option<&RawValue>) -> Option<InterruptReceipt> {
    let body = body?;

    #[derive(serde::Deserialize)]
    struct Payload {
        still_queued: Option<Vec<serde_json::Value>>,
    }

    let payload: Payload = serde_json::from_str(body.get()).ok()?;
    let queued = payload.still_queued?;

    Some(InterruptReceipt {
        still_queued: queued
            .into_iter()
            .filter_map(|v| match v {
                serde_json::Value::String(s) => Some(s),
                _ => None,
            })
            .collect(),
    })
}

/// The control half of a stream: every method that talks to the CLI rather than
/// reading from it.
///
/// Cheap to clone and safe to use from any task while another reads events.
#[derive(Clone)]
pub struct StreamControl {
    shared: Arc<Shared>,
    /// The cached initialize response. Written once before the stream is handed
    /// to the caller, so reads need no synchronisation.
    init: Arc<InitializeResponse>,
}

impl StreamControl {
    pub(crate) fn new(shared: Arc<Shared>, init: Arc<InitializeResponse>) -> Self {
        StreamControl { shared, init }
    }

    /// Writes a `control_request` and blocks until its matching
    /// `control_response` arrives, returning the raw body on success.
    async fn send_control_request(
        &self,
        subtype: &str,
        extras: serde_json::Value,
    ) -> Result<Option<Box<RawValue>>> {
        let request_id = process::new_uuid();
        let (tx, rx) = oneshot::channel::<ControlResponse>();

        if let Ok(mut pending) = self.shared.pending.lock() {
            pending.insert(request_id.clone(), tx);
        }

        let mut request = serde_json::json!({ "subtype": subtype });
        if let serde_json::Value::Object(extras) = extras {
            for (key, value) in extras {
                request[key] = value;
            }
        }

        let envelope = serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        });

        if let Err(e) = self.shared.write(&envelope).await {
            if let Ok(mut pending) = self.shared.pending.lock() {
                pending.remove(&request_id);
            }
            return Err(Error::wrap(subtype, e));
        }

        match rx.await {
            Ok(response) => {
                if !response.success {
                    return Err(Error::Other(format!(
                        "claude: {subtype}: {}",
                        response.error
                    )));
                }
                Ok(response.body)
            }
            // The reader task ended without replying: the subprocess is gone.
            Err(_) => {
                if let Ok(mut pending) = self.shared.pending.lock() {
                    pending.remove(&request_id);
                }
                Err(Error::Cancelled)
            }
        }
    }

    /// Asks the CLI to switch to a different model mid-session. Blocks until
    /// the CLI acknowledges the change.
    pub async fn set_model(&self, model: &str) -> Result<()> {
        self.send_control_request("set_model", serde_json::json!({ "model": model }))
            .await
            .map(|_| ())
    }

    /// Asks the CLI to change the permission mode mid-session.
    ///
    /// Note the field is `mode`, not `permission_mode` — the outbound request
    /// and the inbound notification do not share a spelling.
    pub async fn set_permission_mode(&self, mode: &str) -> Result<()> {
        self.send_control_request("set_permission_mode", serde_json::json!({ "mode": mode }))
            .await
            .map(|_| ())
    }

    /// Asks the CLI to update the max thinking token budget.
    pub async fn set_max_thinking_tokens(&self, n: i64) -> Result<()> {
        self.send_control_request(
            "set_max_thinking_tokens",
            serde_json::json!({ "max_thinking_tokens": n }),
        )
        .await
        .map(|_| ())
    }

    /// Aborts the turn currently in progress and leaves the session alive.
    ///
    /// The subprocess keeps running, the event stream stays open, and the next
    /// user message starts a new turn on the same conversation. To terminate
    /// the session instead, call [`Stream::close`].
    ///
    /// The returned receipt lists async user messages that survived and are
    /// still queued. It is `None` when the CLI sends no receipt — older CLIs
    /// reply with an empty success, and only those advertising
    /// `interrupt_receipt_v1` populate it. A missing receipt is not an error.
    pub async fn interrupt(&self) -> Result<Option<InterruptReceipt>> {
        // The request object carries exactly one key; interrupt takes no extras.
        let body = self
            .send_control_request("interrupt", serde_json::json!({}))
            .await?;
        Ok(decode_interrupt_receipt(body.as_deref()))
    }

    /// Injects an additional user message into the running subprocess.
    ///
    /// In single-turn usage this can be called mid-stream, before the result
    /// arrives, to inject extra context. For persistent multi-turn usage prefer
    /// [`super::Session::send`], which wraps this.
    pub async fn send_user_message(&self, message: &str) -> Result<()> {
        self.shared.write(&process::user_msg(message)).await
    }

    /// Asks the CLI to rewind files to the state at the given user message id.
    pub async fn rewind_files(&self, user_message_id: &str) -> Result<()> {
        self.send_control_request(
            "rewind_files",
            serde_json::json!({ "user_message_id": user_message_id }),
        )
        .await
        .map(|_| ())
    }

    /// Asks the CLI to reconnect a named MCP server.
    pub async fn reconnect_mcp_server(&self, server_name: &str) -> Result<()> {
        self.send_control_request(
            "reconnect_mcp_server",
            serde_json::json!({ "server_name": server_name }),
        )
        .await
        .map(|_| ())
    }

    /// Asks the CLI to enable or disable a named MCP server.
    pub async fn toggle_mcp_server(&self, server_name: &str, enabled: bool) -> Result<()> {
        self.send_control_request(
            "toggle_mcp_server",
            serde_json::json!({ "server_name": server_name, "enabled": enabled }),
        )
        .await
        .map(|_| ())
    }

    /// Asks the CLI to replace the current MCP server configuration.
    pub async fn set_mcp_servers(&self, servers: serde_json::Value) -> Result<()> {
        self.send_control_request(
            "set_mcp_servers",
            serde_json::json!({ "mcp_servers": servers }),
        )
        .await
        .map(|_| ())
    }

    /// Asks the CLI to stop a running background task.
    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.send_control_request("stop_task", serde_json::json!({ "task_id": task_id }))
            .await
            .map(|_| ())
    }

    /// The models the connected CLI offers.
    ///
    /// Read from the initialize handshake completed when the session started:
    /// this performs no I/O and never blocks.
    pub fn supported_models(&self) -> &[ModelInfo] {
        &self.init.models
    }

    /// The slash commands available in this session, from the handshake.
    pub fn supported_commands(&self) -> &[SlashCommand] {
        &self.init.commands
    }

    /// The subagent types this session can dispatch to, from the handshake.
    pub fn supported_agents(&self) -> &[AgentInfo] {
        &self.init.agents
    }

    /// The account the CLI is authenticated as, from the handshake.
    pub fn account_info(&self) -> &AccountInfo {
        &self.init.account
    }

    /// The session's output style, and the styles available.
    pub fn output_style(&self) -> (&str, &[String]) {
        (&self.init.output_style, &self.init.available_output_styles)
    }

    /// The protocol capabilities the connected CLI advertises, for feature
    /// detection.
    ///
    /// Unlike the other accessors this is **not** part of the initialize
    /// response — the CLI advertises it on the `system`/`init` event instead,
    /// which arrives with the first turn. It therefore returns `None` until a
    /// turn has started. Treat that as "not yet known", never as "the CLI
    /// supports nothing".
    pub fn capabilities(&self) -> Option<Vec<String>> {
        self.shared.capabilities.read().ok()?.clone()
    }

    /// Terminates the session: stdin is closed and the subprocess is signalled.
    /// Idempotent.
    pub fn close(&self) {
        self.shared.shutdown();
    }
}

/// An active `claude` subprocess streaming session.
///
/// Call [`Stream::next_event`] until it returns `None`; the stream ends when the
/// agent finishes, the subprocess exits, or the stream is closed.
pub struct Stream {
    events: mpsc::Receiver<Event>,
    control: StreamControl,
}

impl Stream {
    pub(crate) fn new(events: mpsc::Receiver<Event>, control: StreamControl) -> Self {
        Stream { events, control }
    }

    /// The next event, or `None` once the session has ended.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// A clonable handle carrying the control methods, for use from another
    /// task while this one reads events. This is how a caller interrupts a turn
    /// it is in the middle of streaming.
    pub fn control(&self) -> StreamControl {
        self.control.clone()
    }

    /// Terminates the session. Idempotent.
    ///
    /// To abort the current turn while keeping the session usable, call
    /// [`StreamControl::interrupt`].
    pub fn close(&self) {
        self.control.close();
    }
}

// Hand-written rather than derived: the interesting state is the handshake the
// session negotiated, not the channel or the pipe handles behind it.
impl std::fmt::Debug for StreamControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamControl")
            .field("models", &self.init.models.len())
            .field("commands", &self.init.commands.len())
            .field("agents", &self.init.agents.len())
            .field("account", &self.init.account.api_provider)
            .field("capabilities", &self.capabilities())
            .finish()
    }
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("control", &self.control)
            .finish()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // Go ties the subprocess lifetime to a context; Rust ties it to the
        // handle. Without this a dropped stream leaves `claude` running.
        self.control.close();
    }
}

/// Delegations so single-task callers never have to reach for
/// [`Stream::control`].
impl Stream {
    pub async fn set_model(&self, model: &str) -> Result<()> {
        self.control.set_model(model).await
    }
    pub async fn set_permission_mode(&self, mode: &str) -> Result<()> {
        self.control.set_permission_mode(mode).await
    }
    pub async fn set_max_thinking_tokens(&self, n: i64) -> Result<()> {
        self.control.set_max_thinking_tokens(n).await
    }
    pub async fn interrupt(&self) -> Result<Option<InterruptReceipt>> {
        self.control.interrupt().await
    }
    pub async fn send_user_message(&self, message: &str) -> Result<()> {
        self.control.send_user_message(message).await
    }
    pub async fn rewind_files(&self, user_message_id: &str) -> Result<()> {
        self.control.rewind_files(user_message_id).await
    }
    pub async fn reconnect_mcp_server(&self, server_name: &str) -> Result<()> {
        self.control.reconnect_mcp_server(server_name).await
    }
    pub async fn toggle_mcp_server(&self, server_name: &str, enabled: bool) -> Result<()> {
        self.control.toggle_mcp_server(server_name, enabled).await
    }
    pub async fn set_mcp_servers(&self, servers: serde_json::Value) -> Result<()> {
        self.control.set_mcp_servers(servers).await
    }
    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.control.stop_task(task_id).await
    }
    pub fn supported_models(&self) -> &[ModelInfo] {
        self.control.supported_models()
    }
    pub fn supported_commands(&self) -> &[SlashCommand] {
        self.control.supported_commands()
    }
    pub fn supported_agents(&self) -> &[AgentInfo] {
        self.control.supported_agents()
    }
    pub fn account_info(&self) -> &AccountInfo {
        self.control.account_info()
    }
    pub fn output_style(&self) -> (&str, &[String]) {
        self.control.output_style()
    }
    pub fn capabilities(&self) -> Option<Vec<String>> {
        self.control.capabilities()
    }
}

// ─── Entry points ────────────────────────────────────────────────────────────

/// Runs the agent with the given prompt and returns a [`Stream`] for real-time
/// event processing.
///
/// The stream ends when the agent emits a result, the subprocess exits, or the
/// stream is dropped. Control methods may be called at any time while it is
/// active.
pub async fn query(prompt: &str, opts: Options) -> Result<Stream> {
    process::spawn_and_stream(opts, prompt).await
}

/// Blocks until the agent finishes and returns only the final result.
///
/// Intermediate events (streaming deltas, system messages, rate-limit events)
/// are discarded; use [`query`] directly to process them. Errors from the
/// subprocess itself — bad flags, auth failures, crashes — are surfaced as
/// errors so callers always get a meaningful message.
pub async fn run(prompt: &str, opts: Options) -> Result<RunResult> {
    let stream = query(prompt, opts).await?;
    result_from_stream(stream).await
}

/// Renders a failed result as an error, surfacing the fields a caller would
/// otherwise have to use [`query`] to see: why the loop ended, and the HTTP
/// status when an upstream call was what failed.
fn result_error(r: &RunResult) -> Error {
    let mut detail = r.subtype.clone();
    if !r.terminal_reason.is_empty() {
        detail.push_str(&format!(", {}", r.terminal_reason.as_str()));
    }
    if let Some(status) = r.api_error_status {
        detail.push_str(&format!(", HTTP {status}"));
    }

    // The CLI does not always send an errors list; repeating the subtype as the
    // message when it doesn't adds nothing the detail above hasn't said.
    if r.errors.is_empty() {
        Error::Other(format!("claude: agent error ({detail})"))
    } else {
        Error::Other(format!(
            "claude: agent error ({detail}): {}",
            r.errors.join("; ")
        ))
    }
}

/// Drains a stream and returns its final result, converting error results and
/// process-level failures into errors.
async fn result_from_stream(mut stream: Stream) -> Result<RunResult> {
    while let Some(event) = stream.next_event().await {
        match event.event_type.as_str() {
            message_type::RESULT => {
                // parse_line leaves the result absent when the payload does not
                // decode at all. Report that rather than inventing a value — a
                // library must not claim success because the CLI sent a shape we
                // do not model yet.
                let Some(result) = event.result else {
                    let raw = event
                        .raw
                        .as_ref()
                        .map(|r| truncate(r.get(), 512))
                        .unwrap_or_default();
                    return Err(Error::Other(format!(
                        "claude: could not decode the result message: {raw}"
                    )));
                };
                if result.is_error {
                    return Err(result_error(&result));
                }
                return Ok(result);
            }
            message_type::SYSTEM => {
                // Surface process-level errors (bad flag, auth failure, crash)
                // synthesised because no result message arrived.
                if let Some(system) = &event.system {
                    if system.subtype == system_subtype::ERROR {
                        return Err(Error::Other(format!("claude: {}", system.error)));
                    }
                }
            }
            _ => {}
        }
    }

    Err(Error::Other(
        "claude: agent finished without a result message".to_string(),
    ))
}

/// Shortens a payload for inclusion in an error message. A result runs to
/// several kilobytes; the head is enough to identify the offending shape.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Slicing must land on a character boundary; the payload is arbitrary UTF-8.
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(s: &str) -> Box<RawValue> {
        RawValue::from_string(s.to_owned()).unwrap()
    }

    #[test]
    fn an_absent_receipt_is_not_an_error() {
        // Older CLIs reply to an interrupt with an empty success.
        assert_eq!(decode_interrupt_receipt(None), None);
    }

    #[test]
    fn an_unparseable_or_wrongly_typed_receipt_yields_none() {
        assert_eq!(decode_interrupt_receipt(Some(&raw("null"))), None);
        assert_eq!(
            decode_interrupt_receipt(Some(&raw(r#"{"still_queued":7}"#))),
            None
        );
        assert_eq!(decode_interrupt_receipt(Some(&raw(r#"{"other":1}"#))), None);
    }

    #[test]
    fn a_receipt_keeps_only_its_string_elements() {
        // A JSON null would otherwise smuggle in an empty id.
        let receipt =
            decode_interrupt_receipt(Some(&raw(r#"{"still_queued":["a",null,3,"b"]}"#))).unwrap();
        assert_eq!(receipt.still_queued, vec!["a", "b"]);
    }

    #[test]
    fn an_empty_queue_is_still_a_receipt() {
        let receipt = decode_interrupt_receipt(Some(&raw(r#"{"still_queued":[]}"#))).unwrap();
        assert!(receipt.still_queued.is_empty());
    }

    #[test]
    fn a_failed_result_names_the_reason_and_the_status() {
        let r = RunResult {
            subtype: "error_during_execution".into(),
            terminal_reason: crate::claude::TerminalReason("aborted_streaming".into()),
            api_error_status: Some(529),
            ..Default::default()
        };
        assert_eq!(
            result_error(&r).to_string(),
            "claude: agent error (error_during_execution, aborted_streaming, HTTP 529)"
        );
    }

    #[test]
    fn an_errors_list_is_appended_when_the_cli_sends_one() {
        let r = RunResult {
            subtype: "error_max_turns".into(),
            terminal_reason: crate::claude::TerminalReason("max_turns".into()),
            errors: vec!["first".into(), "second".into()],
            ..Default::default()
        };
        assert_eq!(
            result_error(&r).to_string(),
            "claude: agent error (error_max_turns, max_turns): first; second"
        );
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        let s = "é".repeat(400); // 800 bytes
        let out = truncate(&s, 511);
        assert!(out.ends_with("… (truncated)"));
        // The point: slicing mid-character would panic rather than truncate.
        assert!(out.len() <= 511 + "… (truncated)".len());
    }
}

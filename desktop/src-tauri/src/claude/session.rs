//! Persistent multi-turn sessions, ported from `claude/session.go`.
//!
//! A [`Session`] keeps the `claude` subprocess alive between turns, where
//! [`super::query`] spawns one per call. The difference is entirely in what
//! happens when a result arrives: single-turn mode closes stdin and lets the
//! process exit, session mode leaves both open so the next
//! [`Session::send`] starts a new turn on the same conversation.
//!
//! Consume one turn by reading events until a `result`, then send again.

use super::client::{InterruptReceipt, Stream, StreamControl};
use super::errors::Result;
use super::init_types::{AccountInfo, AgentInfo, ModelInfo, SlashCommand};
use super::messages::Event;
use super::options::Options;
use super::process;

/// A persistent Claude session.
///
/// The subprocess starts immediately; the first turn begins when
/// [`Session::send`] is called.
pub struct Session {
    stream: Stream,
}

impl Session {
    /// Creates a new persistent session.
    pub async fn new(opts: Options) -> Result<Self> {
        let stream = process::spawn_session(opts).await?;
        Ok(Session { stream })
    }

    /// Sends a user message and starts a new turn. Call this before reading
    /// events for each turn.
    pub async fn send(&self, message: &str) -> Result<()> {
        self.stream.send_user_message(message).await
    }

    /// The next event. Read until a `result` to consume one turn, then
    /// [`Session::send`] again. Returns `None` when the session has ended.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.stream.next_event().await
    }

    /// A clonable handle carrying the control methods, for use from another
    /// task — notably to interrupt a turn this one is streaming.
    pub fn control(&self) -> StreamControl {
        self.stream.control()
    }

    /// Terminates the session and its subprocess. Idempotent.
    ///
    /// To abort the current turn while keeping the session usable, call
    /// [`Session::interrupt`].
    pub fn close(&self) {
        self.stream.close();
    }

    /// Aborts the turn currently in progress; the session stays open, so call
    /// [`Session::send`] to start the next turn.
    ///
    /// The returned receipt lists async user messages that survived the
    /// interrupt, and is `None` when the CLI sends none.
    pub async fn interrupt(&self) -> Result<Option<InterruptReceipt>> {
        self.stream.interrupt().await
    }

    pub async fn set_model(&self, model: &str) -> Result<()> {
        self.stream.set_model(model).await
    }

    pub async fn set_permission_mode(&self, mode: &str) -> Result<()> {
        self.stream.set_permission_mode(mode).await
    }

    pub async fn set_max_thinking_tokens(&self, n: i64) -> Result<()> {
        self.stream.set_max_thinking_tokens(n).await
    }

    pub async fn rewind_files(&self, user_message_id: &str) -> Result<()> {
        self.stream.rewind_files(user_message_id).await
    }

    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.stream.stop_task(task_id).await
    }

    pub async fn reconnect_mcp_server(&self, server_name: &str) -> Result<()> {
        self.stream.reconnect_mcp_server(server_name).await
    }

    pub async fn toggle_mcp_server(&self, server_name: &str, enabled: bool) -> Result<()> {
        self.stream.toggle_mcp_server(server_name, enabled).await
    }

    pub async fn set_mcp_servers(&self, servers: serde_json::Value) -> Result<()> {
        self.stream.set_mcp_servers(servers).await
    }

    /// The models the connected CLI offers, from the initialize handshake. No
    /// I/O; never blocks.
    pub fn supported_models(&self) -> &[ModelInfo] {
        self.stream.supported_models()
    }

    /// The slash commands available in this session, from the handshake.
    pub fn supported_commands(&self) -> &[SlashCommand] {
        self.stream.supported_commands()
    }

    /// The subagent types this session can dispatch to, from the handshake.
    pub fn supported_agents(&self) -> &[AgentInfo] {
        self.stream.supported_agents()
    }

    /// The account the CLI is authenticated as, from the handshake.
    pub fn account_info(&self) -> &AccountInfo {
        self.stream.account_info()
    }

    /// The session's output style and the styles available.
    pub fn output_style(&self) -> (&str, &[String]) {
        self.stream.output_style()
    }

    /// The protocol capabilities the CLI advertises. These come from the
    /// `system`/`init` event rather than the handshake, so the list is `None`
    /// until the first turn has started.
    pub fn capabilities(&self) -> Option<Vec<String>> {
        self.stream.capabilities()
    }
}

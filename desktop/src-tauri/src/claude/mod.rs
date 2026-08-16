//! A Rust port of `github.com/shaharia-lab/claude-agent-sdk-go` — the SDK
//! Agento runs every agent through.
//!
//! This is **not an API client**. It spawns the `claude` CLI as a subprocess
//! and speaks the same stream-json protocol the official TypeScript and Python
//! SDKs use, so there is no model inference to reimplement and no API key to
//! hold: the CLI's own sign-in is the credential.
//!
//! ```text
//!   Options ──build_args──> claude --output-format stream-json …
//!                              │  stdin: initialize, user messages,
//!                              │         control_requests + responses
//!                              │  stdout: assistant/user/result/system events
//!                              │          interleaved with control traffic
//!   Stream <──events(32)────── reader task
//!   StreamControl ──control_request/response by request_id──> the same pipes
//! ```
//!
//! ## Where to start
//!
//! * [`query`] for one prompt with a live event stream; [`run`] when only the
//!   final [`Result`] matters.
//! * [`Session`] for a persistent multi-turn conversation on one subprocess.
//! * [`Options`] for configuration — note it splits across two channels, CLI
//!   flags and the initialize message, and [`options`] documents which is
//!   which.
//! * [`new_tool`] and [`tool_server`] to expose local functions to the CLI as
//!   tools; [`start_in_process_mcp_server`] when the server is not just a bag
//!   of tools.
//!
//! ## Ported deliberately, not mechanically
//!
//! Four places where Rust and Go genuinely differ, each documented at its site:
//!
//! * **Decode tolerance** ([`lenient`]) — Go keeps the fields that decoded and
//!   reports the error; `serde_json` aborts. The port reproduces Go's
//!   behaviour, because a CLI that reshapes one field must not blank a message.
//! * **Stream vs control** ([`client`]) — Go's single `*Stream` is split into
//!   [`Stream`] (owns the events) and [`StreamControl`] (clonable, carries the
//!   control methods), because reading a channel needs `&mut self`.
//! * **Callbacks are async** ([`permissions`]) — Go blocks a goroutine inline
//!   on the reader; blocking a runtime worker instead would be a bug, so the
//!   handlers return futures and are awaited in the same position.
//! * **Lifetimes replace contexts** — Go cancels via `context.Context`;
//!   dropping a [`Stream`] or an [`InProcessMcpServer`] is what stops the
//!   subprocess or the listener here.
//!
//! ## Scope
//!
//! This module is the SDK only. Nothing in the desktop app calls it yet:
//! wiring agent execution and the chat SSE onto it is the next step of the
//! port, and Agento's integrations — every one of which is an in-process MCP
//! server — come with it.

pub mod client;
pub mod errors;
pub mod hooks;
pub mod init_types;
mod lenient;
pub mod mcp;
pub mod messages;
pub mod options;
pub mod permissions;
pub mod process;
pub mod session;
pub mod sessions;
pub mod tool;

/// The MCP protocol implementation this SDK hosts tools with, re-exported so a
/// caller building a server need not name the dependency. See [`mcp`] for why
/// it is `rmcp` rather than the hand-rolled trait #281 shipped.
pub use rmcp;

/// The version reported to the CLI via `CLAUDE_AGENT_SDK_VERSION`, tracking the
/// Go SDK release this was ported from.
pub const SDK_VERSION: &str = "0.3.0";

pub use client::{query, run, InterruptReceipt, Stream, StreamControl};
pub use errors::{Error, Result};
pub use hooks::{hook_event, HookFunc, HookMatcher, HookOutput};
pub use init_types::{AccountInfo, AgentInfo, ModelInfo, SlashCommand};
pub use mcp::{
    self_as_stdio_mcp_server, serve_stdio_mcp, start_in_process_mcp_server, InProcessMcpServer,
};
pub use messages::{
    block, message_type, result_subtype, system_subtype, AssistantMessage, ContentBlock,
    ContentBlocks, Event, ModelUsage, Result as RunResult, SystemMessage, TaskStatus,
    TerminalReason, ToolProgressMessage, Usage, UserMessage,
};
pub use options::{
    effort, permission_mode, setting_source, thinking, AgentDefinition, McpHttpServer,
    McpSseServer, McpStdioServer, Options, OutputFormat, SandboxSettings, SdkPluginConfig,
    SystemPromptPreset, ToolsPreset,
};
pub use permissions::{
    ElicitationHandler, PermissionContext, PermissionHandler, PermissionResult, PermissionUpdate,
};
pub use session::Session;
pub use sessions::{get_session_messages, list_sessions, SessionSummary, SessionTranscript};
pub use tool::{new_tool, tool_server, ToolDef, ToolServer};

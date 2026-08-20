//! Event and message types, ported from `claude/messages.go`.
//!
//! Every type here mirrors a shape captured from a real `claude` CLI rather
//! than any published schema, so the field names and the casing are the
//! contract. Two casing quirks are real and deliberate: `modelUsage` and
//! `rate_limit_info`'s contents are camelCase *inside* an otherwise snake_case
//! message, because the CLI passes those values through verbatim from its
//! TypeScript shape.
//!
//! Three rules run through the whole file:
//!
//! * **`raw` is always authoritative.** Every message keeps the bytes it was
//!   built from, because this SDK is expected to run against CLI versions both
//!   older and newer than itself and a field we do not model must still reach
//!   the caller. Agento's chat SSE relies on exactly this: it forwards the raw
//!   line rather than re-encoding a typed view.
//! * **Decoding is best-effort.** A field the CLI reshapes degrades that field
//!   and sets `decode_err`; it never blanks the message. See `lenient`.
//! * **Open string types, not closed enums.** `terminal_reason`, task statuses
//!   and rate-limit statuses are named strings so an unrecognised value decodes
//!   through unchanged instead of being dropped.

use serde::Deserialize;
use serde_json::value::RawValue;

use super::lenient::{decode, lenient};

/// The `type` discriminant present on every message.
///
/// Kept as an open string rather than an enum, matching Go's
/// `type MessageType string`: an unknown type must still reach the caller with
/// its `raw` intact.
pub mod message_type {
    /// A complete assistant turn (`SDKAssistantMessage`).
    pub const ASSISTANT: &str = "assistant";
    /// A turn on the user side (`SDKUserMessage`) — most often the CLI
    /// delivering a tool's result, not something a human typed.
    pub const USER: &str = "user";
    /// Incremental streaming deltas (`SDKPartialAssistantMessage`).
    pub const STREAM_EVENT: &str = "stream_event";
    /// The final message emitted when the agent finishes (`SDKResultMessage`).
    pub const RESULT: &str = "result";
    /// Status/info messages from the CLI (`SDKStatusMessage`).
    pub const SYSTEM: &str = "system";
    /// Emitted when rate-limit information is available.
    pub const RATE_LIMIT_EVENT: &str = "rate_limit_event";
    /// Incremental tool execution progress updates.
    pub const TOOL_PROGRESS: &str = "tool_progress";
    /// A summary of a tool use after completion.
    pub const TOOL_USE_SUMMARY: &str = "tool_use_summary";
    /// Authentication status updates.
    pub const AUTH_STATUS: &str = "auth_status";
    /// Prompt suggestions from the agent.
    pub const PROMPT_SUGGESTION: &str = "prompt_suggestion";
}

/// Subtypes of a `system` message.
///
/// These are **not** top-level message types: nothing on the wire ever carries
/// `type:"task_started"`. Treating them as top-level is what made eight parse
/// branches permanently dead in the Go SDK before #23.
pub mod system_subtype {
    pub const INIT: &str = "init";
    pub const STATUS: &str = "status";

    // Task lifecycle.
    pub const TASK_STARTED: &str = "task_started";
    pub const TASK_PROGRESS: &str = "task_progress";
    pub const TASK_NOTIFICATION: &str = "task_notification";
    pub const TASK_UPDATED: &str = "task_updated";

    // Hook lifecycle.
    pub const HOOK_STARTED: &str = "hook_started";
    pub const HOOK_PROGRESS: &str = "hook_progress";
    pub const HOOK_RESPONSE: &str = "hook_response";

    // Session lifecycle.
    pub const COMPACT_BOUNDARY: &str = "compact_boundary";
    pub const FILES_PERSISTED: &str = "files_persisted";

    /// Observed on the wire but not typed — read `Event::raw`.
    pub const THINKING_TOKENS: &str = "thinking_tokens";
    /// Observed on the wire but not typed — read `Event::raw`.
    pub const BACKGROUND_TASKS_CHANGED: &str = "background_tasks_changed";

    /// Synthesised by this SDK for a process-level failure. The CLI never
    /// sends `system`/`error`.
    pub const ERROR: &str = "error";
}

/// Values of [`ContentBlock::block_type`].
pub mod block {
    pub const TEXT: &str = "text";
    pub const THINKING: &str = "thinking";
    pub const TOOL_USE: &str = "tool_use";
    pub const TOOL_RESULT: &str = "tool_result";
    pub const SERVER_TOOL_USE: &str = "server_tool_use";
    pub const ADVISOR_TOOL_RESULT: &str = "advisor_tool_result";

    // Server-side tool results are per-tool block types rather than one
    // generic shape. All carry tool_use_id + content, so ContentBlock decodes
    // them uniformly.
    pub const WEB_SEARCH_TOOL_RESULT: &str = "web_search_tool_result";
    pub const CODE_EXECUTION_TOOL_RESULT: &str = "code_execution_tool_result";
}

// ─── Content blocks ──────────────────────────────────────────────────────────

/// The decodable half of [`ContentBlock`], kept separate so the block's custom
/// `Deserialize` can capture the verbatim bytes before decoding fields.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ContentBlockFields {
    #[serde(rename = "type", deserialize_with = "lenient")]
    block_type: String,

    #[serde(deserialize_with = "lenient")]
    text: String,

    #[serde(deserialize_with = "lenient")]
    thinking: String,
    #[serde(deserialize_with = "lenient")]
    signature: String,

    #[serde(deserialize_with = "lenient")]
    id: String,
    #[serde(deserialize_with = "lenient")]
    name: String,
    input: Option<Box<RawValue>>,

    caller: Option<Box<RawValue>>,

    #[serde(rename = "tool_use_id", deserialize_with = "lenient")]
    tool_use_id: String,
    content: Option<Box<RawValue>>,
    #[serde(deserialize_with = "lenient")]
    is_error: Option<bool>,
}

/// One element of a message's content array.
///
/// The wire carries a discriminated union: `text`, `thinking`, `tool_use`,
/// `tool_result`, the server-side tool blocks, and whatever the API adds next.
/// This is one struct with a discriminator and the superset of fields rather
/// than an enum, so a content array decodes in a single pass and an unknown
/// block degrades to its type plus `raw` instead of being dropped.
///
/// Read only the fields that belong to `block_type`. `raw` is always the
/// authoritative payload.
#[derive(Debug, Clone, Default)]
pub struct ContentBlock {
    pub block_type: String,

    /// Set when `block_type` is [`block::TEXT`].
    pub text: String,

    /// Set when `block_type` is [`block::THINKING`]. `signature` arrives whole
    /// here, and incrementally as a `signature_delta` while streaming.
    pub thinking: String,
    pub signature: String,

    /// Set when `block_type` is [`block::TOOL_USE`] or
    /// [`block::SERVER_TOOL_USE`]. `input` is the tool's arguments object, kept
    /// raw because its schema is the tool's own.
    pub id: String,
    pub name: String,
    pub input: Option<Box<RawValue>>,

    /// How the tool was invoked, e.g. `{"type":"direct"}`. Sent alongside every
    /// `tool_use` block by CLI 2.1.224 — and described by no type definition,
    /// which is why it is kept raw.
    pub caller: Option<Box<RawValue>>,

    /// Set on the result blocks. `tool_use_id` pairs the result back to the
    /// `id` of the `tool_use` block that requested it.
    ///
    /// `content` is raw because the CLI sends either a plain string or an array
    /// of blocks depending on the tool — both shapes were observed in one
    /// session.
    pub tool_use_id: String,
    pub content: Option<Box<RawValue>>,
    /// A pointer in Go, an `Option` here, for the same reason: absent and
    /// `false` are different answers.
    pub is_error: Option<bool>,

    /// The complete block as received, including fields this struct does not
    /// model. Always populated when the block came off the wire.
    pub raw: Option<Box<RawValue>>,
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Capture the verbatim block first, exactly as Go's UnmarshalJSON does,
        // so an unknown or partially-decodable block still carries its payload.
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let fields: ContentBlockFields = serde_json::from_str(raw.get()).unwrap_or_default();

        Ok(ContentBlock {
            block_type: fields.block_type,
            text: fields.text,
            thinking: fields.thinking,
            signature: fields.signature,
            id: fields.id,
            name: fields.name,
            input: fields.input,
            caller: fields.caller,
            tool_use_id: fields.tool_use_id,
            content: fields.content,
            is_error: fields.is_error,
            raw: Some(raw),
        })
    }
}

impl ContentBlock {
    /// The block's `content` as a plain string, when the CLI sent one.
    ///
    /// Result blocks carry either a string or an array of blocks; this returns
    /// `None` for the array form, which the caller should decode itself.
    pub fn content_text(&self) -> Option<String> {
        let raw = self.content.as_ref()?;
        serde_json::from_str::<String>(raw.get()).ok()
    }

    /// Whether this is a result block the tool reported as an error.
    pub fn failed(&self) -> bool {
        self.is_error == Some(true)
    }
}

/// A message's content array.
///
/// The CLI sends content either as an array of blocks (every message it
/// originates) or as a bare string (the shape this SDK itself sends for a user
/// turn, and what stored transcripts replay). A string decodes to a single text
/// block so callers have one shape to handle.
#[derive(Debug, Clone, Default)]
pub struct ContentBlocks(pub Vec<ContentBlock>);

impl std::ops::Deref for ContentBlocks {
    type Target = [ContentBlock];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContentBlocks {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let text = raw.get();

        // Check null first. Go unmarshals JSON null into a string as a no-op,
        // so the string branch below would otherwise turn absent content into
        // one empty text block rather than no content at all.
        if text.trim() == "null" {
            return Ok(ContentBlocks(Vec::new()));
        }

        if let Ok(s) = serde_json::from_str::<String>(text) {
            return Ok(ContentBlocks(vec![ContentBlock {
                block_type: block::TEXT.to_string(),
                text: s,
                raw: RawValue::from_string(text.to_owned()).ok(),
                ..Default::default()
            }]));
        }

        Ok(ContentBlocks(
            serde_json::from_str::<Vec<ContentBlock>>(text).unwrap_or_default(),
        ))
    }
}

// ─── Assistant and user messages ─────────────────────────────────────────────

/// The inner `message` object of an assistant or user message.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MessagePayload {
    #[serde(deserialize_with = "lenient")]
    pub role: String,
    pub content: ContentBlocks,

    /// The API message id, e.g. `msg_011Cdz…`. Assistant messages only.
    #[serde(deserialize_with = "lenient")]
    pub id: String,
    /// The API object type; the CLI sends `message`.
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub payload_type: String,
    /// The model that produced this turn, e.g. `claude-opus-5`.
    #[serde(deserialize_with = "lenient")]
    pub model: String,

    /// `None` until the turn ends; `tool_use` and `end_turn` are the common
    /// terminal values. `stop_sequence` is set only when a stop sequence
    /// triggered the stop.
    #[serde(deserialize_with = "lenient")]
    pub stop_reason: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub stop_sequence: Option<String>,
    pub stop_details: Option<Box<RawValue>>,

    /// Classifies a failed turn; empty on success. One of
    /// `authentication_failed`, `billing_error`, `rate_limit`,
    /// `invalid_request`, `server_error`, `unknown`.
    #[serde(deserialize_with = "lenient")]
    pub error: String,

    /// This turn's token counts, kept raw because the CLI nests
    /// provider-specific detail under it (`cache_creation`, `inference_geo`, …)
    /// that changes independently of this SDK.
    ///
    /// This is **not** the cost-accounting field: it reports a single turn of
    /// the main loop only. Cumulative per-model usage and cost live in
    /// [`Result::model_usages`].
    pub usage: Option<Box<RawValue>>,
}

/// Emitted when Claude produces a complete response turn.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AssistantMessage {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub message_type: String,
    pub message: MessagePayload,
    #[serde(deserialize_with = "lenient")]
    pub parent_tool_use_id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,

    /// The CLI's send time, RFC 3339 with milliseconds.
    #[serde(deserialize_with = "lenient")]
    pub timestamp: String,
    /// The upstream API request id, e.g. `req_011Cdz…`.
    #[serde(deserialize_with = "lenient")]
    pub request_id: String,
}

impl AssistantMessage {
    /// The concatenated text from all text content blocks.
    pub fn text(&self) -> String {
        self.message
            .content
            .iter()
            .filter(|b| b.block_type == block::TEXT)
            .map(|b| b.text.as_str())
            .collect()
    }

    /// The concatenated thinking text from all thinking content blocks.
    pub fn thinking(&self) -> String {
        self.message
            .content
            .iter()
            .filter(|b| b.block_type == block::THINKING)
            .map(|b| b.thinking.as_str())
            .collect()
    }

    /// The `tool_use` blocks of this turn — the tools the agent is asking to
    /// run. Each pairs by `id` to a `tool_result` block on the following
    /// [`UserMessage`].
    pub fn tool_uses(&self) -> Vec<&ContentBlock> {
        self.message
            .content
            .iter()
            .filter(|b| b.block_type == block::TOOL_USE || b.block_type == block::SERVER_TOOL_USE)
            .collect()
    }
}

/// A turn on the user side of the conversation.
///
/// Most of these are not typed by a human: the CLI emits one after every tool
/// call, carrying the tool's output as `tool_result` blocks. They also carry
/// every user turn of a stored transcript.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserMessage {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub message_type: String,
    pub message: MessagePayload,
    #[serde(deserialize_with = "lenient")]
    pub parent_tool_use_id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,

    /// The CLI's send time, RFC 3339 with milliseconds.
    #[serde(deserialize_with = "lenient")]
    pub timestamp: String,

    /// The tool's structured result, alongside the rendered form in the
    /// `tool_result` block. Raw because its shape is the tool's own: a Read
    /// sends an object with file metadata, a failure sends a plain string —
    /// both observed in one session.
    pub tool_use_result: Option<Box<RawValue>>,

    /// Marks a message the CLI generated rather than one the user or a tool
    /// produced.
    #[serde(rename = "isSynthetic", deserialize_with = "lenient")]
    pub is_synthetic: bool,
}

impl UserMessage {
    /// The concatenated text from all text content blocks. For a user turn sent
    /// as a bare string, that is the whole message.
    pub fn text(&self) -> String {
        self.message
            .content
            .iter()
            .filter(|b| b.block_type == block::TEXT)
            .map(|b| b.text.as_str())
            .collect()
    }

    /// The `tool_result` blocks of this turn, including the server-side result
    /// variants. Pair them to the preceding assistant turn's `tool_use` blocks
    /// by `tool_use_id`.
    pub fn tool_results(&self) -> Vec<&ContentBlock> {
        self.message
            .content
            .iter()
            .filter(|b| {
                matches!(
                    b.block_type.as_str(),
                    block::TOOL_RESULT
                        | block::ADVISOR_TOOL_RESULT
                        | block::WEB_SEARCH_TOOL_RESULT
                        | block::CODE_EXECUTION_TOOL_RESULT
                )
            })
            .collect()
    }
}

// ─── Stream events ───────────────────────────────────────────────────────────

/// Values of [`StreamEvent::event_type`].
pub mod stream {
    pub const MESSAGE_START: &str = "message_start";
    pub const MESSAGE_DELTA: &str = "message_delta";
    pub const MESSAGE_STOP: &str = "message_stop";
    pub const CONTENT_BLOCK_START: &str = "content_block_start";
    pub const CONTENT_BLOCK_DELTA: &str = "content_block_delta";
    pub const CONTENT_BLOCK_STOP: &str = "content_block_stop";
}

/// Values of [`StreamEventDelta::delta_type`].
pub mod delta {
    pub const TEXT: &str = "text_delta";
    pub const THINKING: &str = "thinking_delta";
    pub const INPUT_JSON: &str = "input_json_delta";
    pub const SIGNATURE: &str = "signature_delta";
}

/// The incremental content of a `stream_event` delta. Which field carries the
/// increment depends on `delta_type`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct StreamEventDelta {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub delta_type: String,
    #[serde(deserialize_with = "lenient")]
    pub text: String,
    #[serde(deserialize_with = "lenient")]
    pub thinking: String,

    /// A fragment of a tool's input, sent when `delta_type` is
    /// [`delta::INPUT_JSON`]. Fragments split at arbitrary points — including
    /// mid-token and mid-string — so they are only valid JSON once concatenated
    /// across the whole block.
    #[serde(deserialize_with = "lenient")]
    pub partial_json: String,

    /// A fragment of a thinking block's signature ([`delta::SIGNATURE`]).
    #[serde(deserialize_with = "lenient")]
    pub signature: String,

    /// `stop_reason` and `stop_sequence` ride the `message_delta` at the end of
    /// a turn, where this struct is the event's `delta` rather than a block's.
    #[serde(deserialize_with = "lenient")]
    pub stop_reason: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct StreamEventFields {
    #[serde(rename = "type", deserialize_with = "lenient")]
    event_type: String,
    #[serde(deserialize_with = "lenient")]
    delta: Option<StreamEventDelta>,
    #[serde(deserialize_with = "lenient")]
    index: i64,
    message: Option<Box<RawValue>>,
    content_block: Option<Box<RawValue>>,
    usage: Option<Box<RawValue>>,
    context_management: Option<Box<RawValue>>,
}

/// The inner `event` object of a [`StreamEventMessage`].
///
/// Its shape varies by `event_type`, so the fields specific to each are kept
/// raw and reached through the accessors below.
#[derive(Debug, Clone, Default)]
pub struct StreamEvent {
    pub event_type: String,
    pub delta: Option<StreamEventDelta>,
    pub index: i64,

    /// The opening message envelope on [`stream::MESSAGE_START`].
    pub message: Option<Box<RawValue>>,

    /// The block being opened on [`stream::CONTENT_BLOCK_START`]. For a
    /// `tool_use` block it already carries `id` and `name`, with `input`
    /// arriving as `input_json_delta` fragments.
    pub content_block: Option<Box<RawValue>>,

    /// Rides [`stream::MESSAGE_DELTA`] as a **sibling** of `delta`, not inside
    /// it.
    pub usage: Option<Box<RawValue>>,

    /// Edits the CLI applied to the context window.
    pub context_management: Option<Box<RawValue>>,

    /// The complete event as received. Always populated off the wire.
    pub raw: Option<Box<RawValue>>,
}

impl<'de> Deserialize<'de> for StreamEvent {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let fields: StreamEventFields = serde_json::from_str(raw.get()).unwrap_or_default();
        Ok(StreamEvent {
            event_type: fields.event_type,
            delta: fields.delta,
            index: fields.index,
            message: fields.message,
            content_block: fields.content_block,
            usage: fields.usage,
            context_management: fields.context_management,
            raw: Some(raw),
        })
    }
}

impl StreamEvent {
    /// The tool-input fragment carried by this event, if it is an
    /// `input_json_delta`. Concatenate the fragments of one block `index` in
    /// arrival order to rebuild the tool's input.
    pub fn partial_json(&self) -> Option<&str> {
        let d = self.delta.as_ref()?;
        (d.delta_type == delta::INPUT_JSON).then_some(d.partial_json.as_str())
    }

    /// The text increment carried by this event, if any.
    pub fn text_delta(&self) -> Option<&str> {
        let d = self.delta.as_ref()?;
        (d.delta_type == delta::TEXT).then_some(d.text.as_str())
    }

    /// The thinking increment carried by this event, if any.
    pub fn thinking_delta(&self) -> Option<&str> {
        let d = self.delta.as_ref()?;
        (d.delta_type == delta::THINKING).then_some(d.thinking.as_str())
    }

    /// The thinking-signature fragment carried by this event, if any.
    pub fn signature_delta(&self) -> Option<&str> {
        let d = self.delta.as_ref()?;
        (d.delta_type == delta::SIGNATURE).then_some(d.signature.as_str())
    }

    /// The block being opened, when this event is a `content_block_start`.
    /// This is where a streamed tool call announces its id and name, before any
    /// of its input has arrived.
    pub fn content_block_start(&self) -> Option<ContentBlock> {
        if self.event_type != stream::CONTENT_BLOCK_START {
            return None;
        }
        let raw = self.content_block.as_ref()?;
        serde_json::from_str(raw.get()).ok()
    }

    /// The tail of a turn: why it stopped and its final usage. The stop reason
    /// is empty if the CLI sent none.
    pub fn message_delta(&self) -> Option<(String, Option<&RawValue>)> {
        if self.event_type != stream::MESSAGE_DELTA {
            return None;
        }
        let stop_reason = self
            .delta
            .as_ref()
            .and_then(|d| d.stop_reason.clone())
            .unwrap_or_default();
        Some((stop_reason, self.usage.as_deref()))
    }
}

/// Carries incremental deltas during a streaming response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StreamEventMessage {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub message_type: String,
    pub event: StreamEvent,
    #[serde(deserialize_with = "lenient")]
    pub parent_tool_use_id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,

    /// Time to first token in milliseconds, sent on `message_start`.
    #[serde(rename = "ttft_ms", deserialize_with = "lenient")]
    pub ttft_ms: i64,
}

// ─── Usage ───────────────────────────────────────────────────────────────────

/// Token and cache usage from a completed agent run.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Usage {
    #[serde(deserialize_with = "lenient")]
    pub input_tokens: i64,
    #[serde(deserialize_with = "lenient")]
    pub output_tokens: i64,
    #[serde(deserialize_with = "lenient")]
    pub cache_read_input_tokens: i64,
    #[serde(deserialize_with = "lenient")]
    pub cache_creation_input_tokens: i64,

    /// Server-side tool invocations. On the wire these are nested under
    /// `usage.server_tool_use`, not top-level.
    #[serde(deserialize_with = "lenient")]
    pub server_tool_use: ServerToolUse,

    /// e.g. `standard`.
    #[serde(deserialize_with = "lenient")]
    pub service_tier: String,
}

/// Tool invocations the server performed on the model's behalf.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ServerToolUse {
    #[serde(deserialize_with = "lenient")]
    pub web_search_requests: i64,
    #[serde(deserialize_with = "lenient")]
    pub web_fetch_requests: i64,
}

/// Per-model token and cost breakdown, carried by a result's `modelUsage` map.
///
/// Its fields are **camelCase** on the wire — the CLI passes the value through
/// verbatim from its TypeScript shape — unlike the snake_case used everywhere
/// else in the protocol.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelUsage {
    #[serde(rename = "inputTokens", deserialize_with = "lenient")]
    pub input_tokens: i64,
    #[serde(rename = "outputTokens", deserialize_with = "lenient")]
    pub output_tokens: i64,
    #[serde(rename = "cacheReadInputTokens", deserialize_with = "lenient")]
    pub cache_read_input_tokens: i64,
    #[serde(rename = "cacheCreationInputTokens", deserialize_with = "lenient")]
    pub cache_creation_input_tokens: i64,
    #[serde(rename = "webSearchRequests", deserialize_with = "lenient")]
    pub web_search_requests: i64,
    #[serde(rename = "costUSD", deserialize_with = "lenient")]
    pub cost_usd: f64,
    #[serde(rename = "contextWindow", deserialize_with = "lenient")]
    pub context_window: i64,
    #[serde(rename = "maxOutputTokens", deserialize_with = "lenient")]
    pub max_output_tokens: i64,
    /// The model id without any suffix, e.g. `claude-haiku-4-5`.
    #[serde(rename = "canonicalModel", deserialize_with = "lenient")]
    pub canonical_model: String,
    /// e.g. `firstParty`, `bedrock`, `vertex`.
    #[serde(deserialize_with = "lenient")]
    pub provider: String,
}

/// Values of [`ModelUsage::provider`].
pub mod provider {
    pub const FIRST_PARTY: &str = "firstParty";
    pub const BEDROCK: &str = "bedrock";
    pub const VERTEX: &str = "vertex";
    pub const FOUNDRY: &str = "foundry";
    pub const ANTHROPIC_AWS: &str = "anthropicAws";
    pub const ANTHROPIC_GOOGLE_CLOUD: &str = "anthropicGoogleCloud";
    pub const MANTLE: &str = "mantle";
    pub const GATEWAY: &str = "gateway";
}

// ─── Result ──────────────────────────────────────────────────────────────────

/// Values of [`Result::subtype`]. These are **not** system message subtypes —
/// they classify how a run finished, and each pairs with a [`TerminalReason`].
pub mod result_subtype {
    pub const SUCCESS: &str = "success";
    pub const ERROR_DURING_EXECUTION: &str = "error_during_execution";
    pub const ERROR_MAX_TURNS: &str = "error_max_turns";
    pub const ERROR_MAX_BUDGET_USD: &str = "error_max_budget_usd";
    pub const ERROR_MAX_STRUCTURED_OUTPUT_RETRIES: &str = "error_max_structured_output_retries";
}

/// Why the agent's loop ended.
///
/// A named string, not a closed enum: the CLI may add reasons at any time and
/// an unrecognised one must decode through unchanged rather than being dropped.
///
/// This is the field that distinguishes a cancelled turn from a failed one —
/// `subtype` cannot, because interrupting a streaming turn produces
/// `error_during_execution`, the same subtype as any execution failure.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct TerminalReason(pub String);

impl TerminalReason {
    /// A run that finished on its own.
    pub const COMPLETED: &'static str = "completed";
    /// The turn limit from `with_max_turns` being reached.
    pub const MAX_TURNS: &'static str = "max_turns";
    /// The turn was cancelled while streaming a response.
    pub const ABORTED_STREAMING: &'static str = "aborted_streaming";
    /// The turn was cancelled while running tools.
    pub const ABORTED_TOOLS: &'static str = "aborted_tools";
    /// An upstream API failure; see [`Result::api_error_status`].
    pub const API_ERROR: &'static str = "api_error";
    /// The cost limit was hit.
    pub const BUDGET_EXHAUSTED: &'static str = "budget_exhausted";
    /// Structured output failed schema validation too many times.
    pub const STRUCTURED_OUTPUT_RETRY_EXHAUSTED: &'static str = "structured_output_retry_exhausted";
    /// A deferred tool could not be run.
    pub const TOOL_DEFERRED_UNAVAILABLE: &'static str = "tool_deferred_unavailable";
    /// The turn failed before it began.
    pub const TURN_SETUP_FAILED: &'static str = "turn_setup_failed";

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the run was cancelled rather than finishing or failing on its
    /// own. This is how a caller tells "the user stopped it" from "the model
    /// finished".
    pub fn aborted(&self) -> bool {
        self.0 == Self::ABORTED_STREAMING || self.0 == Self::ABORTED_TOOLS
    }
}

/// A tool call parked by a `PreToolUse` hook that returned permission decision
/// `defer`. The run stops and reports the call here so the caller can inspect
/// it and decide whether to resume.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DeferredToolUse {
    #[serde(deserialize_with = "lenient")]
    pub id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    pub input: Option<Box<RawValue>>,
}

/// One tool call that was refused during a run, as reported in the final
/// result message.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PermissionDenial {
    /// The tool that was denied (e.g. `Write`).
    #[serde(deserialize_with = "lenient")]
    pub tool_name: String,
    /// Identifies the specific denied call.
    #[serde(deserialize_with = "lenient")]
    pub tool_use_id: String,
    /// The raw input the tool would have been invoked with.
    pub tool_input: Option<Box<RawValue>>,
}

/// The final message emitted by the agent.
///
/// Covers both the success and error cases; check `is_error` (or `subtype`) to
/// determine which one you have.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Result {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub message_type: String,
    #[serde(deserialize_with = "lenient")]
    pub subtype: String,
    #[serde(deserialize_with = "lenient")]
    pub duration_ms: i64,
    #[serde(deserialize_with = "lenient")]
    pub duration_api_ms: i64,
    #[serde(deserialize_with = "lenient")]
    pub is_error: bool,
    #[serde(deserialize_with = "lenient")]
    pub num_turns: i64,
    #[serde(deserialize_with = "lenient")]
    pub result: String,
    #[serde(deserialize_with = "lenient")]
    pub stop_reason: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub total_cost_usd: f64,
    #[serde(deserialize_with = "lenient")]
    pub usage: Usage,
    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,

    /// Why the loop ended. Unlike `subtype`, this distinguishes a cancelled
    /// turn from a completed one — see [`TerminalReason::aborted`].
    #[serde(deserialize_with = "lenient")]
    pub terminal_reason: TerminalReason,

    /// The HTTP status of the failing upstream call (429, 500, 529, …) —
    /// `None` when the CLI sent none, which is why it is an `Option` rather
    /// than a zero-means-absent integer.
    ///
    /// It can be set on a `success` subtype whose `is_error` is true, so branch
    /// on this field rather than on `subtype` when classifying a failure for
    /// retry.
    #[serde(deserialize_with = "lenient")]
    pub api_error_status: Option<i64>,

    /// The tool call a `PreToolUse` hook deferred, if any.
    #[serde(deserialize_with = "lenient")]
    pub deferred_tool_use: Option<DeferredToolUse>,

    /// Per-model token and cost breakdowns keyed by model id.
    #[serde(rename = "modelUsage", deserialize_with = "lenient")]
    pub model_usages: std::collections::BTreeMap<String, ModelUsage>,

    /// Populated when `is_error` is true.
    #[serde(deserialize_with = "lenient")]
    pub errors: Vec<String>,

    /// Parsed structured output, when an output format of type `json` or
    /// `json_schema` was requested.
    pub structured_output: Option<Box<RawValue>>,

    /// Tool calls that were denied during the run.
    #[serde(deserialize_with = "lenient")]
    pub permission_denials: Vec<PermissionDenial>,
}

// ─── System message ──────────────────────────────────────────────────────────

/// One plugin loaded into the session, as reported on `system`/`init`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct PluginInfo {
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "lenient")]
    pub path: String,
    /// The marketplace reference, e.g. `lab-workflow@shaharia-lab`.
    #[serde(deserialize_with = "lenient")]
    pub source: String,
    #[serde(deserialize_with = "lenient")]
    pub version: String,
}

/// One MCP server as reported on `system`/`init`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpServerInit {
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    /// `connected`, `pending`, `failed`, …
    #[serde(deserialize_with = "lenient")]
    pub status: String,
}

/// All `system` typed messages from the CLI.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SystemMessage {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub message_type: String,
    #[serde(deserialize_with = "lenient")]
    pub subtype: String,

    /// Populated when `subtype` is [`system_subtype::STATUS`].
    #[serde(deserialize_with = "lenient")]
    pub status: String,

    /// The reason for a synthetic `system`/`error` event — one this SDK
    /// generates when the subprocess fails without sending a result.
    ///
    /// It is **not** a wire field (hence `skip`): the CLI never sends
    /// `system`/`error`. Carrying it as a general `message` field invited
    /// callers to read it on real system messages, which is why it is named
    /// `error` and skipped rather than decoded.
    #[serde(skip)]
    pub error: String,

    // Init subtype fields.
    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,
    #[serde(deserialize_with = "lenient")]
    pub cwd: String,
    #[serde(deserialize_with = "lenient")]
    pub model: String,
    #[serde(deserialize_with = "lenient")]
    pub tools: Vec<String>,
    #[serde(rename = "permissionMode", deserialize_with = "lenient")]
    pub permission_mode: String,
    #[serde(deserialize_with = "lenient")]
    pub claude_code_version: String,
    #[serde(rename = "apiKeySource", deserialize_with = "lenient")]
    pub api_key_source: String,

    #[serde(deserialize_with = "lenient")]
    pub agents: Vec<String>,
    #[serde(deserialize_with = "lenient")]
    pub betas: Vec<String>,
    #[serde(deserialize_with = "lenient")]
    pub skills: Vec<String>,
    #[serde(deserialize_with = "lenient")]
    pub plugins: Vec<PluginInfo>,
    #[serde(deserialize_with = "lenient")]
    pub slash_commands: Vec<String>,

    /// The CLI's feature list, e.g. `interrupt_receipt_v1`.
    ///
    /// This arrives **here** rather than in the initialize control response, so
    /// it is unknown until the first turn starts.
    #[serde(deserialize_with = "lenient")]
    pub capabilities: Vec<String>,

    /// Each configured MCP server and whether it connected.
    #[serde(deserialize_with = "lenient")]
    pub mcp_servers: Vec<McpServerInit>,

    /// The active output style, e.g. `default`.
    #[serde(deserialize_with = "lenient")]
    pub output_style: String,

    /// `on` or `off`; `fast_mode_disabled_reason` says why when the CLI turned
    /// it off.
    #[serde(deserialize_with = "lenient")]
    pub fast_mode_state: String,
    #[serde(deserialize_with = "lenient")]
    pub fast_mode_disabled_reason: String,
}

// ─── Tool progress ───────────────────────────────────────────────────────────

/// Reports that a tool is still running.
///
/// The field set is taken from the CLI's own emit code rather than from a
/// specification: it has three shapes (a Bash/PowerShell progress tick, a REPL
/// call, and a bare heartbeat), which share everything below. The `progress`
/// and `message` fields older type definitions declared exist nowhere on the
/// wire.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ToolProgressMessage {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub message_type: String,
    #[serde(deserialize_with = "lenient")]
    pub tool_use_id: String,

    /// The tool still running, e.g. `Bash` or `REPL`.
    #[serde(deserialize_with = "lenient")]
    pub tool_name: String,

    /// How long it has been running.
    #[serde(deserialize_with = "lenient")]
    pub elapsed_time_seconds: f64,

    /// Marks a keep-alive tick that reports no new progress.
    #[serde(deserialize_with = "lenient")]
    pub heartbeat: bool,

    /// Set when the tool runs as a background task.
    #[serde(deserialize_with = "lenient")]
    pub task_id: String,

    #[serde(deserialize_with = "lenient")]
    pub parent_tool_use_id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,

    /// The complete message; REPL ticks carry a `repl_call` object this struct
    /// does not model.
    #[serde(skip)]
    pub raw: Option<Box<RawValue>>,
}

// ─── Task lifecycle ──────────────────────────────────────────────────────────

/// The state of a background task.
///
/// Two vocabularies overlap here: `task_updated` reports the raw lifecycle
/// state (including `killed`), while `task_notification` reports the
/// user-facing outcome (where a killed task becomes `stopped`). Use
/// [`TaskStatus::is_terminal`] rather than comparing against one vocabulary.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct TaskStatus(pub String);

impl TaskStatus {
    pub const PENDING: &'static str = "pending";
    pub const RUNNING: &'static str = "running";
    pub const PAUSED: &'static str = "paused";
    pub const COMPLETED: &'static str = "completed";
    pub const FAILED: &'static str = "failed";
    /// The notification vocabulary's name for a task the caller stopped;
    /// `task_updated` calls the same event `killed`.
    pub const STOPPED: &'static str = "stopped";
    pub const KILLED: &'static str = "killed";

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the task has finished for good.
    ///
    /// A consumer tracking active tasks must clear its entry on a terminal
    /// status from *either* a `task_updated` or a `task_notification`. A
    /// stopped task reports `killed` via `task_updated`, and the matching
    /// notification is not guaranteed — so waiting only for the notification
    /// can leak the task forever.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.0.as_str(),
            Self::COMPLETED | Self::FAILED | Self::STOPPED | Self::KILLED
        )
    }
}

/// What a task has consumed so far.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct TaskUsage {
    #[serde(deserialize_with = "lenient")]
    pub total_tokens: i64,
    #[serde(deserialize_with = "lenient")]
    pub tool_uses: i64,
    #[serde(deserialize_with = "lenient")]
    pub duration_ms: i64,
}

/// Announces a background task, as `system`/`task_started`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TaskStartedMessage {
    #[serde(deserialize_with = "lenient")]
    pub subtype: String,
    #[serde(deserialize_with = "lenient")]
    pub task_id: String,
    #[serde(deserialize_with = "lenient")]
    pub tool_use_id: String,
    /// The human-readable label, e.g. `Sleep 20 seconds then echo`.
    #[serde(deserialize_with = "lenient")]
    pub description: String,
    /// The kind of task, e.g. `local_bash`.
    #[serde(deserialize_with = "lenient")]
    pub task_type: String,
    #[serde(deserialize_with = "lenient")]
    pub subagent_type: String,
    #[serde(deserialize_with = "lenient")]
    pub workflow_name: String,

    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,
    #[serde(skip)]
    pub raw: Option<Box<RawValue>>,
}

/// Reports a running task's progress, as `system`/`task_progress`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TaskProgressMessage {
    #[serde(deserialize_with = "lenient")]
    pub subtype: String,
    #[serde(deserialize_with = "lenient")]
    pub task_id: String,
    #[serde(deserialize_with = "lenient")]
    pub tool_use_id: String,
    #[serde(deserialize_with = "lenient")]
    pub description: String,
    #[serde(deserialize_with = "lenient")]
    pub subagent_type: String,

    #[serde(deserialize_with = "lenient")]
    pub usage: TaskUsage,
    /// The most recent tool the task ran.
    #[serde(deserialize_with = "lenient")]
    pub last_tool_name: String,
    #[serde(deserialize_with = "lenient")]
    pub summary: String,

    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,
    #[serde(skip)]
    pub raw: Option<Box<RawValue>>,
}

/// Reports a task's outcome, as `system`/`task_notification`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TaskNotificationMessage {
    #[serde(deserialize_with = "lenient")]
    pub subtype: String,
    #[serde(deserialize_with = "lenient")]
    pub task_id: String,
    #[serde(deserialize_with = "lenient")]
    pub tool_use_id: String,
    #[serde(deserialize_with = "lenient")]
    pub status: TaskStatus,

    /// Where the task's full output was written.
    #[serde(deserialize_with = "lenient")]
    pub output_file: String,
    #[serde(deserialize_with = "lenient")]
    pub summary: String,
    #[serde(deserialize_with = "lenient")]
    pub usage: TaskUsage,

    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,
    #[serde(skip)]
    pub raw: Option<Box<RawValue>>,
}

/// Carries a change to a task's state, as `system`/`task_updated`.
///
/// This is the message a consumer cannot skip: a task stopped via `stop_task`
/// reports `killed` here, and the corresponding notification is not guaranteed
/// to follow.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TaskUpdatedMessage {
    #[serde(deserialize_with = "lenient")]
    pub subtype: String,
    #[serde(deserialize_with = "lenient")]
    pub task_id: String,

    /// The raw change, e.g. `{"status":"completed","end_time":…}`. Kept whole
    /// because the CLI puts arbitrary task fields in it.
    pub patch: Option<Box<RawValue>>,

    /// The task's new state.
    ///
    /// CLI 2.1.224 sends it inside `patch` rather than at the top level, so
    /// [`parse_line`] lifts it out; the field stays so that a top-level status,
    /// which the official SDKs' types declare, still decodes and wins over the
    /// patch.
    #[serde(deserialize_with = "lenient")]
    pub status: TaskStatus,

    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,
    #[serde(skip)]
    pub raw: Option<Box<RawValue>>,
}

/// Reports a hook starting, progressing, or responding.
///
/// The CLI only emits these when hook events are enabled, so a session that
/// configures hooks does not necessarily observe them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HookLifecycleMessage {
    #[serde(deserialize_with = "lenient")]
    pub subtype: String,
    /// The hook that fired, e.g. `PreToolUse`.
    #[serde(deserialize_with = "lenient")]
    pub hook_event: String,

    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,
    #[serde(skip)]
    pub raw: Option<Box<RawValue>>,
}

// ─── Rate limit ──────────────────────────────────────────────────────────────

/// Values of [`RateLimitInfo::status`].
pub mod rate_limit_status {
    pub const ALLOWED: &str = "allowed";
    pub const ALLOWED_WARNING: &str = "allowed_warning";
    pub const REJECTED: &str = "rejected";
}

/// Values of [`RateLimitInfo::rate_limit_type`].
pub mod rate_limit_type {
    pub const FIVE_HOUR: &str = "five_hour";
    pub const SEVEN_DAY: &str = "seven_day";
    pub const SEVEN_DAY_OPUS: &str = "seven_day_opus";
    pub const SEVEN_DAY_SONNET: &str = "seven_day_sonnet";
    pub const OVERAGE: &str = "overage";
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct RateLimitInfoFields {
    #[serde(deserialize_with = "lenient")]
    status: String,
    #[serde(rename = "resetsAt", deserialize_with = "lenient")]
    resets_at: i64,
    #[serde(rename = "rateLimitType", deserialize_with = "lenient")]
    rate_limit_type: String,
    #[serde(deserialize_with = "lenient")]
    utilization: f64,
    #[serde(rename = "overageStatus", deserialize_with = "lenient")]
    overage_status: String,
    #[serde(rename = "overageResetsAt", deserialize_with = "lenient")]
    overage_resets_at: i64,
    #[serde(rename = "overageDisabledReason", deserialize_with = "lenient")]
    overage_disabled_reason: String,
    #[serde(rename = "isUsingOverage", deserialize_with = "lenient")]
    is_using_overage: bool,
    #[serde(rename = "errorCode", deserialize_with = "lenient")]
    error_code: String,
}

/// The rate-limit state carried by a `rate_limit_event`.
///
/// Its field names are camelCase on the wire — like `modelUsage`, and unlike
/// the snake_case used by the surrounding message.
#[derive(Debug, Clone, Default)]
pub struct RateLimitInfo {
    pub status: String,
    /// A Unix timestamp in seconds.
    pub resets_at: i64,
    pub rate_limit_type: String,
    /// The fraction of the window consumed, when sent.
    pub utilization: f64,

    /// Billing beyond the included allowance.
    pub overage_status: String,
    pub overage_resets_at: i64,
    pub overage_disabled_reason: String,
    pub is_using_overage: bool,

    pub error_code: String,

    /// The whole info object, including fields not modelled here.
    pub raw: Option<Box<RawValue>>,
}

impl<'de> Deserialize<'de> for RateLimitInfo {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let f: RateLimitInfoFields = serde_json::from_str(raw.get()).unwrap_or_default();
        Ok(RateLimitInfo {
            status: f.status,
            resets_at: f.resets_at,
            rate_limit_type: f.rate_limit_type,
            utilization: f.utilization,
            overage_status: f.overage_status,
            overage_resets_at: f.overage_resets_at,
            overage_disabled_reason: f.overage_disabled_reason,
            is_using_overage: f.is_using_overage,
            error_code: f.error_code,
            raw: Some(raw),
        })
    }
}

impl RateLimitInfo {
    /// Whether requests are currently being refused.
    pub fn limited(&self) -> bool {
        self.status == rate_limit_status::REJECTED
    }
}

/// Reports a change in rate-limit state. The CLI sends one at the start of a
/// turn and whenever the state changes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RateLimitEvent {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub message_type: String,
    pub rate_limit_info: RateLimitInfo,
    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,
}

// ─── Top-level event ─────────────────────────────────────────────────────────

/// The top-level value yielded from a stream.
///
/// `event_type` is always set. The corresponding typed field is populated for
/// known types. System messages additionally populate one of the lifecycle
/// fields according to their subtype, with `system` always set alongside them.
///
/// For unknown types only `raw` is set, so callers can handle
/// forward-compatibility themselves.
#[derive(Debug, Clone, Default)]
pub struct Event {
    pub event_type: String,
    pub assistant: Option<AssistantMessage>,
    pub user: Option<UserMessage>,
    pub stream_event: Option<StreamEventMessage>,
    pub result: Option<Result>,
    pub system: Option<SystemMessage>,
    pub tool_progress: Option<ToolProgressMessage>,
    pub rate_limit: Option<RateLimitEvent>,

    /// Task lifecycle, populated from the matching system subtype.
    pub task_started: Option<TaskStartedMessage>,
    pub task_progress: Option<TaskProgressMessage>,
    pub task_notification: Option<TaskNotificationMessage>,
    pub task_updated: Option<TaskUpdatedMessage>,

    /// Populated from the `hook_started` / `hook_progress` / `hook_response`
    /// subtypes.
    pub hook_lifecycle: Option<HookLifecycleMessage>,

    /// The verbatim line. Always populated off the wire, and always
    /// authoritative — Agento's chat SSE forwards exactly these bytes.
    pub raw: Option<Box<RawValue>>,

    /// Why a typed field decoded only partially, if it did.
    ///
    /// Typed fields are best-effort: on a mismatch the fields that did decode
    /// are kept and this is set, rather than the whole message being discarded.
    pub decode_err: Option<String>,
}

/// Decodes one JSON line from stdout into an [`Event`].
///
/// Unknown types are returned with only `event_type` and `raw` set. Returns
/// `None` when the line is not JSON at all, which the reader skips.
pub(crate) fn parse_line(line: &[u8]) -> Option<Event> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default, rename = "type")]
        event_type: String,
    }

    let envelope: Envelope = serde_json::from_slice(line).ok()?;
    let raw = std::str::from_utf8(line)
        .ok()
        .and_then(|s| RawValue::from_string(s.to_owned()).ok());

    let mut event = Event {
        event_type: envelope.event_type,
        raw,
        ..Default::default()
    };

    // Decoding is best-effort by design: a single unexpected field degrades
    // that field rather than nulling the whole message. `decode_err` records
    // what went wrong so drift stays observable instead of silent; `raw` is
    // always authoritative.
    match event.event_type.as_str() {
        message_type::ASSISTANT => {
            let (m, err) = decode::<AssistantMessage>(line);
            event.decode_err = err;
            event.assistant = Some(m);
        }
        message_type::USER => {
            // The user side is half of every conversation: the CLI reports each
            // tool's output as a user turn of tool_result blocks.
            let (m, err) = decode::<UserMessage>(line);
            event.decode_err = err;
            event.user = Some(m);
        }
        message_type::STREAM_EVENT => {
            let (m, err) = decode::<StreamEventMessage>(line);
            event.decode_err = err;
            event.stream_event = Some(m);
        }
        message_type::RESULT => {
            let (m, err) = decode::<Result>(line);
            event.decode_err = err;
            event.result = Some(m);
        }
        message_type::TOOL_PROGRESS => {
            let (mut m, err) = decode::<ToolProgressMessage>(line);
            m.raw = event.raw.as_deref().and_then(clone_raw);
            event.decode_err = err;
            event.tool_progress = Some(m);
        }
        message_type::RATE_LIMIT_EVENT => {
            let (m, err) = decode::<RateLimitEvent>(line);
            event.decode_err = err;
            event.rate_limit = Some(m);
        }
        message_type::SYSTEM => {
            let (m, err) = decode::<SystemMessage>(line);
            event.decode_err = err;
            let subtype = m.subtype.clone();
            event.system = Some(m);

            // Task and hook lifecycle messages arrive as system subtypes, never
            // as top-level types — dispatch on the subtype so they can be
            // populated, each into its own payload type.
            match subtype.as_str() {
                system_subtype::TASK_STARTED => {
                    let (mut t, err) = decode::<TaskStartedMessage>(line);
                    t.raw = event.raw.as_deref().and_then(clone_raw);
                    event.decode_err = event.decode_err.take().or(err);
                    event.task_started = Some(t);
                }
                system_subtype::TASK_PROGRESS => {
                    let (mut t, err) = decode::<TaskProgressMessage>(line);
                    t.raw = event.raw.as_deref().and_then(clone_raw);
                    event.decode_err = event.decode_err.take().or(err);
                    event.task_progress = Some(t);
                }
                system_subtype::TASK_NOTIFICATION => {
                    let (mut t, err) = decode::<TaskNotificationMessage>(line);
                    t.raw = event.raw.as_deref().and_then(clone_raw);
                    event.decode_err = event.decode_err.take().or(err);
                    event.task_notification = Some(t);
                }
                system_subtype::TASK_UPDATED => {
                    let (mut t, err) = decode::<TaskUpdatedMessage>(line);
                    t.raw = event.raw.as_deref().and_then(clone_raw);
                    event.decode_err = event.decode_err.take().or(err);

                    // The new status lives inside the patch, not at the top
                    // level. Lifting it out here is what lets a caller treat
                    // task_updated and task_notification the same way — which
                    // matters because a stopped task may report its terminal
                    // state only here. A top-level status, if a future CLI
                    // sends one, is authoritative and kept.
                    if t.status.is_empty() {
                        if let Some(patch) = t.patch.as_ref() {
                            #[derive(Deserialize)]
                            struct Patch {
                                #[serde(default)]
                                status: TaskStatus,
                            }
                            match serde_json::from_str::<Patch>(patch.get()) {
                                Ok(p) => t.status = p.status,
                                Err(e) => {
                                    if event.decode_err.is_none() {
                                        event.decode_err = Some(e.to_string());
                                    }
                                }
                            }
                        }
                    }
                    event.task_updated = Some(t);
                }
                system_subtype::HOOK_STARTED
                | system_subtype::HOOK_PROGRESS
                | system_subtype::HOOK_RESPONSE => {
                    let (mut h, err) = decode::<HookLifecycleMessage>(line);
                    h.raw = event.raw.as_deref().and_then(clone_raw);
                    event.decode_err = event.decode_err.take().or(err);
                    event.hook_lifecycle = Some(h);
                }
                _ => {}
            }
        }
        _ => {}
    }

    Some(event)
}

/// `RawValue` is not `Clone`; re-parsing its text is the cheapest faithful copy.
fn clone_raw(raw: &RawValue) -> Option<Box<RawValue>> {
    RawValue::from_string(raw.get().to_owned()).ok()
}

/// Builds the synthetic `system`/`error` event used for process-level failures.
///
/// The reason goes in `error`, not in a `message` field: the CLI has no system
/// `message` field, and carrying one invited callers to read it on real system
/// messages.
pub(crate) fn error_event(msg: impl Into<String>) -> Event {
    Event {
        event_type: message_type::SYSTEM.to_string(),
        system: Some(SystemMessage {
            message_type: message_type::SYSTEM.to_string(),
            subtype: system_subtype::ERROR.to_string(),
            error: msg.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Event {
        parse_line(line.as_bytes()).expect("a JSON line always parses into an event")
    }

    #[test]
    fn a_line_that_is_not_json_is_skipped_entirely() {
        assert!(parse_line(b"not json").is_none());
    }

    #[test]
    fn an_unknown_type_still_reaches_the_caller_with_its_raw() {
        // Forward-compatibility: a type this SDK has never heard of must not be
        // dropped, because the caller may know what to do with it.
        let event = parse(r#"{"type":"tool_use_summary","summary":"did a thing"}"#);
        assert_eq!(event.event_type, "tool_use_summary");
        assert!(event.assistant.is_none() && event.result.is_none());
        assert_eq!(
            event.raw.as_ref().unwrap().get(),
            r#"{"type":"tool_use_summary","summary":"did a thing"}"#
        );
    }

    #[test]
    fn the_raw_line_is_kept_byte_for_byte() {
        // Agento's chat SSE forwards these bytes rather than re-encoding, so
        // key order and spacing must survive the round trip untouched.
        let line = r#"{"type":"result","z":1,"a":2,"cost":0.30000000000000004}"#;
        assert_eq!(parse(line).raw.as_ref().unwrap().get(), line);
    }

    // ─── Content blocks ──────────────────────────────────────────────────────

    #[test]
    fn content_decodes_from_both_the_array_and_the_bare_string_form() {
        let blocks = parse(
            r#"{"type":"assistant","message":{"role":"assistant","content":[
                {"type":"text","text":"hi"},{"type":"thinking","thinking":"hmm"}]}}"#,
        );
        let assistant = blocks.assistant.unwrap();
        assert_eq!(assistant.text(), "hi");
        assert_eq!(assistant.thinking(), "hmm");

        // The shape this SDK itself sends for a user turn, and what stored
        // transcripts replay.
        let bare = parse(r#"{"type":"user","message":{"role":"user","content":"just text"}}"#);
        let user = bare.user.unwrap();
        assert_eq!(user.message.content.len(), 1);
        assert_eq!(user.message.content[0].block_type, block::TEXT);
        assert_eq!(user.text(), "just text");
    }

    #[test]
    fn null_content_is_no_content_rather_than_one_empty_block() {
        // Go unmarshals null into a string as a no-op, so the string branch
        // would otherwise manufacture a block that was never sent.
        let event = parse(r#"{"type":"user","message":{"role":"user","content":null}}"#);
        assert!(event.user.unwrap().message.content.is_empty());
    }

    #[test]
    fn an_unknown_block_degrades_to_its_type_and_raw() {
        let event = parse(
            r#"{"type":"assistant","message":{"role":"assistant","content":[
                {"type":"some_future_block","payload":{"deep":[1,2]}}]}}"#,
        );
        let blocks = event.assistant.unwrap().message.content;
        assert_eq!(blocks[0].block_type, "some_future_block");
        assert_eq!(
            blocks[0].raw.as_ref().unwrap().get(),
            r#"{"type":"some_future_block","payload":{"deep":[1,2]}}"#
        );
    }

    #[test]
    fn tool_uses_and_tool_results_pair_across_the_two_turns() {
        let assistant = parse(
            r#"{"type":"assistant","message":{"role":"assistant","content":[
                {"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"ls"},
                 "caller":{"type":"direct"}}]}}"#,
        )
        .assistant
        .unwrap();
        let uses = assistant.tool_uses();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].id, "tu_1");
        assert_eq!(uses[0].name, "Bash");
        // input and caller are kept raw: their schemas are the tool's own.
        assert_eq!(uses[0].input.as_ref().unwrap().get(), r#"{"command":"ls"}"#);
        assert_eq!(
            uses[0].caller.as_ref().unwrap().get(),
            r#"{"type":"direct"}"#
        );

        let user = parse(
            r#"{"type":"user","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"tu_1","content":"a b c","is_error":false},
                {"type":"web_search_tool_result","tool_use_id":"tu_2","content":[]}]}}"#,
        )
        .user
        .unwrap();
        let results = user.tool_results();
        assert_eq!(results.len(), 2, "server-side results count too");
        assert_eq!(results[0].tool_use_id, "tu_1");
        // A result's content is a string for some tools and an array for
        // others — both shapes were observed in one session.
        assert_eq!(results[0].content_text().as_deref(), Some("a b c"));
        assert!(!results[0].failed());
        assert_eq!(results[1].content_text(), None);
    }

    #[test]
    fn an_absent_is_error_is_not_the_same_as_false() {
        let blocks = parse(
            r#"{"type":"user","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"a"},
                {"type":"tool_result","tool_use_id":"b","is_error":true}]}}"#,
        )
        .user
        .unwrap()
        .message
        .content;
        assert_eq!(blocks[0].is_error, None, "absent, not false");
        assert!(!blocks[0].failed());
        assert!(blocks[1].failed());
    }

    // ─── Result ──────────────────────────────────────────────────────────────

    #[test]
    fn a_result_decodes_its_mixed_case_usage_and_open_terminal_reason() {
        let result = parse(
            r#"{"type":"result","subtype":"success","is_error":false,"num_turns":3,
                "result":"done","total_cost_usd":1.5,"terminal_reason":"completed",
                "usage":{"input_tokens":7,"output_tokens":2,
                         "server_tool_use":{"web_search_requests":1}},
                "modelUsage":{"claude-opus-5":{"inputTokens":7,"outputTokens":2,
                              "costUSD":1.5,"canonicalModel":"claude-opus-5"}}}"#,
        )
        .result
        .unwrap();

        assert_eq!(result.num_turns, 3);
        assert_eq!(result.usage.input_tokens, 7);
        // server_tool_use is nested under usage, not top-level.
        assert_eq!(result.usage.server_tool_use.web_search_requests, 1);
        // modelUsage's contents are camelCase inside a snake_case message.
        assert_eq!(result.model_usages["claude-opus-5"].cost_usd, 1.5);
        assert_eq!(
            result.model_usages["claude-opus-5"].canonical_model,
            "claude-opus-5"
        );
        assert_eq!(result.terminal_reason.as_str(), TerminalReason::COMPLETED);
    }

    #[test]
    fn an_unrecognised_terminal_reason_decodes_through_unchanged() {
        // A closed enum would drop a reason the CLI adds tomorrow, which is
        // exactly the field a caller needs to classify a failure.
        let result = parse(r#"{"type":"result","terminal_reason":"abducted_by_aliens"}"#)
            .result
            .unwrap();
        assert_eq!(result.terminal_reason.as_str(), "abducted_by_aliens");
        assert!(!result.terminal_reason.aborted());
    }

    #[test]
    fn aborted_distinguishes_a_cancelled_turn_from_a_failed_one() {
        // subtype cannot: interrupting a streaming turn produces
        // error_during_execution, the same subtype as any execution failure.
        for reason in [
            TerminalReason::ABORTED_STREAMING,
            TerminalReason::ABORTED_TOOLS,
        ] {
            assert!(TerminalReason(reason.into()).aborted(), "{reason}");
        }
        for reason in [TerminalReason::COMPLETED, TerminalReason::MAX_TURNS] {
            assert!(!TerminalReason(reason.into()).aborted(), "{reason}");
        }
    }

    #[test]
    fn an_api_error_status_is_absent_rather_than_zero_when_unsent() {
        let with = parse(r#"{"type":"result","api_error_status":429}"#)
            .result
            .unwrap();
        assert_eq!(with.api_error_status, Some(429));
        let without = parse(r#"{"type":"result","subtype":"success"}"#)
            .result
            .unwrap();
        assert_eq!(without.api_error_status, None);
    }

    #[test]
    fn one_bad_field_degrades_only_itself_and_is_reported() {
        // decode_tolerance_test.go's shape: num_turns mutated into an array.
        let event = parse(
            r#"{"type":"result","subtype":"success","num_turns":[1,2],"result":"survived",
                "total_cost_usd":0.5}"#,
        );
        let result = event.result.as_ref().unwrap();
        assert_eq!(result.num_turns, 0, "the bad field falls back");
        assert_eq!(result.result, "survived", "the rest of the message is kept");
        assert_eq!(result.total_cost_usd, 0.5);
        assert!(
            event.decode_err.is_some(),
            "drift must stay observable, not silent"
        );
        assert!(event.raw.is_some(), "raw is always authoritative");
    }

    // ─── System subtypes ─────────────────────────────────────────────────────

    #[test]
    fn init_carries_the_capabilities_the_handshake_does_not() {
        let event = parse(
            r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-opus-5",
                "tools":["Bash"],"capabilities":["interrupt_receipt_v1"],
                "mcp_servers":[{"name":"local-tools","status":"connected"}],
                "plugins":[{"name":"p","path":"/p","source":"m@o"}]}"#,
        );
        let system = event.system.unwrap();
        assert_eq!(system.subtype, system_subtype::INIT);
        assert_eq!(system.capabilities, vec!["interrupt_receipt_v1"]);
        assert_eq!(system.mcp_servers[0].status, "connected");
        assert_eq!(system.plugins[0].source, "m@o");
    }

    #[test]
    fn task_lifecycle_arrives_as_system_subtypes_not_top_level_types() {
        let event = parse(
            r#"{"type":"system","subtype":"task_started","task_id":"t1",
                "description":"Sleep then echo","task_type":"local_bash"}"#,
        );
        assert!(event.system.is_some(), "system is always set alongside");
        let started = event.task_started.unwrap();
        assert_eq!(started.task_id, "t1");
        assert_eq!(started.task_type, "local_bash");
        assert!(started.raw.is_some());
    }

    #[test]
    fn a_task_status_is_lifted_out_of_the_patch() {
        // CLI 2.1.224 sends it inside patch. A consumer that waited for the
        // matching notification would leak a stopped task forever, because that
        // notification is not guaranteed to follow.
        let event = parse(
            r#"{"type":"system","subtype":"task_updated","task_id":"t1",
                "patch":{"status":"killed","end_time":"now"}}"#,
        );
        let updated = event.task_updated.unwrap();
        assert_eq!(updated.status.as_str(), TaskStatus::KILLED);
        assert!(updated.status.is_terminal());
        assert_eq!(
            updated.patch.as_ref().unwrap().get(),
            r#"{"status":"killed","end_time":"now"}"#
        );
    }

    #[test]
    fn a_top_level_task_status_wins_over_the_patch() {
        // The official SDKs' types declare one; if a future CLI sends it, it is
        // authoritative.
        let event = parse(
            r#"{"type":"system","subtype":"task_updated","task_id":"t1","status":"completed",
                "patch":{"status":"killed"}}"#,
        );
        assert_eq!(
            event.task_updated.unwrap().status.as_str(),
            TaskStatus::COMPLETED
        );
    }

    #[test]
    fn both_task_vocabularies_agree_on_what_is_terminal() {
        // task_updated says "killed"; task_notification calls the same event
        // "stopped".
        for terminal in [
            TaskStatus::COMPLETED,
            TaskStatus::FAILED,
            TaskStatus::STOPPED,
            TaskStatus::KILLED,
        ] {
            assert!(TaskStatus(terminal.into()).is_terminal(), "{terminal}");
        }
        for live in [TaskStatus::PENDING, TaskStatus::RUNNING, TaskStatus::PAUSED] {
            assert!(!TaskStatus(live.into()).is_terminal(), "{live}");
        }
    }

    #[test]
    fn hook_lifecycle_subtypes_all_populate_one_payload() {
        for subtype in ["hook_started", "hook_progress", "hook_response"] {
            let event = parse(&format!(
                r#"{{"type":"system","subtype":"{subtype}","hook_event":"PreToolUse"}}"#
            ));
            let hook = event.hook_lifecycle.expect(subtype);
            assert_eq!(hook.hook_event, "PreToolUse");
        }
    }

    // ─── Streaming ───────────────────────────────────────────────────────────

    #[test]
    fn stream_deltas_are_read_through_their_own_accessors() {
        let text = parse(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"he"}}}"#,
        )
        .stream_event
        .unwrap();
        assert_eq!(text.event.text_delta(), Some("he"));
        assert_eq!(text.event.thinking_delta(), None);
        assert_eq!(text.event.partial_json(), None);

        let json = parse(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}}"#,
        )
        .stream_event
        .unwrap();
        // Fragments split at arbitrary points and are only valid JSON once
        // concatenated across the whole block.
        assert_eq!(json.event.partial_json(), Some(r#"{"a":"#));
    }

    #[test]
    fn a_content_block_start_announces_a_tool_call_before_its_input() {
        let event = parse(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,
                "content_block":{"type":"tool_use","id":"tu_1","name":"Bash"}}}"#,
        )
        .stream_event
        .unwrap();
        let block = event.event.content_block_start().unwrap();
        assert_eq!(block.id, "tu_1");
        assert_eq!(block.name, "Bash");
    }

    #[test]
    fn usage_rides_the_message_delta_as_a_sibling_of_delta() {
        let event = parse(
            r#"{"type":"stream_event","event":{"type":"message_delta",
                "delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}}"#,
        )
        .stream_event
        .unwrap();
        let (stop_reason, usage) = event.event.message_delta().unwrap();
        assert_eq!(stop_reason, "end_turn");
        assert_eq!(usage.unwrap().get(), r#"{"output_tokens":12}"#);
    }

    // ─── Rate limits ─────────────────────────────────────────────────────────

    #[test]
    fn rate_limit_info_is_camel_case_inside_a_snake_case_message() {
        let event = parse(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected",
                "resetsAt":1750000000,"rateLimitType":"five_hour","utilization":0.97}}"#,
        )
        .rate_limit
        .unwrap();
        let info = &event.rate_limit_info;
        assert!(info.limited());
        assert_eq!(info.resets_at, 1_750_000_000);
        assert_eq!(info.rate_limit_type, rate_limit_type::FIVE_HOUR);
        assert!(info.raw.is_some());
    }

    // ─── Synthetic errors ────────────────────────────────────────────────────

    #[test]
    fn the_synthetic_error_event_is_not_something_the_cli_can_send() {
        let event = error_event("bad flag");
        let system = event.system.unwrap();
        assert_eq!(system.subtype, system_subtype::ERROR);
        assert_eq!(system.error, "bad flag");
        // `error` is skipped on the wire, so a real system message can never
        // arrive carrying one and be mistaken for a process failure.
        let real = parse(r#"{"type":"system","subtype":"status","error":"not a field"}"#);
        assert_eq!(real.system.unwrap().error, "");
    }
}

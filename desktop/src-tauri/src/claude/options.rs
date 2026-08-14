//! Configuration, ported from `claude/options.go`.
//!
//! Options split across **two channels**, and picking the wrong one is silent:
//!
//! * most become CLI flags, built by [`Options::build_args`];
//! * the system prompt, agents, hooks, output format and sandbox settings are
//!   sent in the `initialize` control message over stdin, because that is the
//!   only channel that works in bidirectional mode.
//!
//! MCP servers are the subtle case. They travel as `--mcp-config`, an ordinary
//! transport config the CLI dials directly, and are deliberately **never**
//! named in the initialize message's `sdkMcpServers` — see
//! [`super::process::initialize_msg`].
//!
//! Go's functional options become builder methods here. The names are kept
//! (`WithModel` → [`Options::with_model`]) so the two files read against each
//! other, and every default in [`Options::default`] matches Go's
//! `defaultOptions()` exactly — including the two that matter most,
//! `bypassPermissions` and `allow_dangerously_skip_permissions`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

use super::errors::{Error, Result};
use super::hooks::HookMatcher;
use super::permissions::{ElicitationHandler, PermissionHandler};

/// Controls Claude's extended thinking behaviour.
pub mod thinking {
    /// Lets Claude decide when to think (the default).
    pub const ADAPTIVE: &str = "adaptive";
    /// Turns off extended thinking. Also sets `MAX_THINKING_TOKENS=0` in the
    /// subprocess environment.
    pub const DISABLED: &str = "disabled";
    /// Always enables extended thinking.
    pub const ENABLED: &str = "enabled";
}

/// Reasoning effort, via `--effort`.
pub mod effort {
    pub const LOW: &str = "low";
    pub const MEDIUM: &str = "medium";
    pub const HIGH: &str = "high";
    /// The highest possible reasoning effort.
    pub const MAX: &str = "max";
}

/// How Claude handles tool permission requests.
pub mod permission_mode {
    pub const DEFAULT: &str = "default";
    pub const ACCEPT_EDITS: &str = "acceptEdits";
    pub const BYPASS_PERMISSIONS: &str = "bypassPermissions";
    /// Planning mode: the agent plans actions but does not execute tools that
    /// modify state.
    pub const PLAN: &str = "plan";
    /// Silently denies any tool call that is not already pre-approved, without
    /// prompting the user.
    pub const DONT_ASK: &str = "dontAsk";
}

/// Which settings file(s) the subprocess should load.
///
/// By default the SDK loads **no** settings files (SDK isolation mode).
/// Explicitly listing sources opts in to loading those files.
pub mod setting_source {
    /// `~/.claude/settings.json` (global user settings).
    pub const USER: &str = "user";
    /// `.claude/settings.json` (shared, version-controlled).
    pub const PROJECT: &str = "project";
    /// `.claude/settings.local.json` (gitignored local overrides).
    pub const LOCAL: &str = "local";
}

// ─── MCP server configs ──────────────────────────────────────────────────────

/// An external MCP server launched as a subprocess; the CLI spawns the binary
/// and talks over its stdin/stdout.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct McpStdioServer {
    #[serde(rename = "type")]
    pub server_type: String,
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// An MCP server reachable over HTTP (streamable transport).
///
/// This is how an in-process server is exposed to the CLI: bind an HTTP
/// listener in this process and pass its URL here. See
/// [`super::mcp::start_in_process_mcp_server`].
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct McpHttpServer {
    #[serde(rename = "type")]
    pub server_type: String,
    pub url: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// An MCP server reachable over SSE.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct McpSseServer {
    #[serde(rename = "type")]
    pub server_type: String,
    pub url: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// A Claude Code plugin loaded for a session. Only local plugins (`type:
/// "local"`) are supported; each directory must contain a
/// `.claude-plugin/plugin.json` manifest.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SdkPluginConfig {
    #[serde(rename = "type")]
    pub plugin_type: String,
    pub path: String,
}

/// A preset system prompt instead of a plain string.
///
/// Takes precedence over a plain system prompt when both are set.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SystemPromptPreset {
    /// Must be `preset`.
    #[serde(rename = "type")]
    pub preset_type: String,
    /// Names the preset, e.g. `claude_code`.
    pub preset: String,
    /// Optional extra text appended after the preset system prompt.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub append: String,
}

/// The base tool set via a named preset instead of an explicit list. Passed to
/// the subprocess as `--tools`.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ToolsPreset {
    /// Must be `preset`.
    #[serde(rename = "type")]
    pub preset_type: String,
    /// Names the preset, e.g. `claude_code`.
    pub preset: String,
}

/// A named sub-agent the CLI can spawn.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct AgentDefinition {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(rename = "disallowedTools", skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(rename = "maxTurns", skip_serializing_if = "is_zero_i64")]
    pub max_turns: i64,
    #[serde(rename = "mcpServers", skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

/// Structured output configuration.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct OutputFormat {
    /// One of `text`, `json`, or `json_schema`.
    #[serde(rename = "type")]
    pub format_type: String,
    /// The JSON schema used when the type is `json_schema`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

/// Network access for sandboxed command execution.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct NetworkSandboxSettings {
    #[serde(rename = "allowLocalBinding", skip_serializing_if = "is_false")]
    pub allow_local_binding: bool,
    #[serde(rename = "allowUnixSockets", skip_serializing_if = "Vec::is_empty")]
    pub allow_unix_sockets: Vec<String>,
    #[serde(rename = "allowAllUnixSockets", skip_serializing_if = "is_false")]
    pub allow_all_unix_sockets: bool,
    #[serde(rename = "httpProxyPort", skip_serializing_if = "is_zero_i64")]
    pub http_proxy_port: i64,
    #[serde(rename = "socksProxyPort", skip_serializing_if = "is_zero_i64")]
    pub socks_proxy_port: i64,
}

/// Patterns for which sandbox violations are silently ignored.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SandboxIgnoreViolations {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub file: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<String>,
}

/// Command execution sandboxing.
///
/// These control whether shell commands run inside a sandbox; they do not
/// configure filesystem or network permissions, which are the permission
/// handler's and permission rules' job.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SandboxSettings {
    #[serde(skip_serializing_if = "is_false")]
    pub enabled: bool,
    #[serde(rename = "autoAllowBashIfSandboxed", skip_serializing_if = "is_false")]
    pub auto_allow_bash_if_sandboxed: bool,
    #[serde(rename = "excludedCommands", skip_serializing_if = "Vec::is_empty")]
    pub excluded_commands: Vec<String>,
    #[serde(rename = "allowUnsandboxedCommands", skip_serializing_if = "is_false")]
    pub allow_unsandboxed_commands: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkSandboxSettings>,
    #[serde(rename = "ignoreViolations", skip_serializing_if = "Option::is_none")]
    pub ignore_violations: Option<SandboxIgnoreViolations>,
    #[serde(rename = "enableWeakerNestedSandbox", skip_serializing_if = "is_false")]
    pub enable_weaker_nested_sandbox: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

// ─── Options ─────────────────────────────────────────────────────────────────

/// The default the CLI is asked for when the caller names no model.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// The floor the official SDKs use for the initialize handshake.
const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Receives each line the `claude` subprocess writes to stderr.
///
/// Shared rather than owned because the reader task and the options outlive
/// each other in either order, and a shadowed-permission-handler warning is
/// written from the caller's task before the subprocess exists at all.
pub type StderrSink = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// All configuration for a run. Build with [`Options::default`] plus the
/// `with_*` methods.
#[derive(Clone, Default)]
pub struct Options {
    /// Selects the Claude model. Defaults to [`DEFAULT_MODEL`].
    pub model: String,

    /// Overrides the default system prompt. Sent via the initialize message.
    pub system_prompt: String,
    /// Appended to the existing system prompt. Sent via the initialize message.
    pub append_system_prompt: String,

    /// Resumes an existing session by id (`--resume`).
    pub resume_session_id: String,
    /// Sets a custom UUID for a brand-new session (`--session-id`).
    pub custom_session_id: String,
    /// Resumes the most recent session (`--continue`).
    pub continue_session: bool,
    /// Forks the resumed session into a new id (`--fork-session`).
    pub fork_session: bool,

    /// Restricts which built-in tools may be used.
    pub allowed_tools: Vec<String>,
    /// Explicitly blocks specific tools.
    pub disallowed_tools: Vec<String>,

    /// Extended thinking mode. Defaults to [`thinking::ADAPTIVE`].
    pub thinking: String,
    /// Caps the thinking token budget via the `MAX_THINKING_TOKENS` env var.
    pub max_thinking_tokens: i64,
    /// Limits agentic turns via `--max-turns`.
    pub max_turns: i64,
    /// Reasoning effort via `--effort`.
    pub effort: String,
    /// Beta feature flags via `--betas`.
    pub betas: Vec<String>,
    /// The model to use when the primary model is unavailable.
    pub fallback_model: String,
    /// Maximum cost budget in USD via `--max-budget-usd`.
    pub max_budget_usd: f64,

    /// Structured output. Sent via the initialize message.
    pub output_format: Option<OutputFormat>,
    /// Enables file checkpointing.
    pub enable_file_checkpointing: bool,
    /// Enables strict MCP config validation.
    pub strict_mcp_config: bool,

    /// The working directory for the subprocess.
    pub cwd: String,

    /// Tool permission handling. Defaults to
    /// [`permission_mode::BYPASS_PERMISSIONS`].
    pub permission_mode: String,
    /// Must be true when using bypass permissions.
    pub allow_dangerously_skip_permissions: bool,
    /// The MCP tool name used for permission prompts.
    pub permission_prompt_tool_name: String,
    /// Called for each `can_use_tool` control_request.
    pub permission_handler: Option<PermissionHandler>,
    /// Called for each `elicitation` control_request.
    pub elicitation_handler: Option<ElicitationHandler>,

    /// Streams partial assistant messages.
    pub include_partial_messages: bool,

    /// External MCP servers, keyed by server name. Values are the serialized
    /// [`McpStdioServer`] / [`McpHttpServer`] / [`McpSseServer`] shapes.
    ///
    /// A `BTreeMap` rather than a `HashMap` because Go's `encoding/json` sorts
    /// map keys when marshalling, and `--mcp-config` carries this map — an
    /// unordered map would make the argument differ run to run.
    pub mcp_servers: BTreeMap<String, serde_json::Value>,

    /// Bounds the wait for the CLI to acknowledge the initialize handshake.
    /// `None` means the default (60s, or `CLAUDE_CODE_STREAM_CLOSE_TIMEOUT`).
    pub init_timeout: Option<Duration>,

    /// Named sub-agents. Sent via the initialize message.
    pub agents: BTreeMap<String, AgentDefinition>,
    /// Lifecycle hooks, keyed by event name. Sent via the initialize message.
    pub hooks: BTreeMap<String, Vec<HookMatcher>>,
    /// Local plugins loaded for this session.
    pub plugins: Vec<SdkPluginConfig>,

    /// A settings file path or an inline JSON string passed via `--settings`.
    /// Mutually exclusive with `setting_sources`: when both are set this wins
    /// and `setting_sources` is ignored.
    pub settings: String,
    /// Which settings files the subprocess loads. Empty means SDK isolation
    /// mode — no filesystem settings at all.
    pub setting_sources: Vec<String>,
    /// Extra directories added to the allowed set via `--add-dir`.
    pub additional_directories: Vec<String>,

    /// Arbitrary extra CLI flags passed verbatim, for forward-compatibility.
    /// Keys are flag names (e.g. `--some-flag`); an empty value means a boolean
    /// flag with no argument.
    pub extra_args: BTreeMap<String, String>,

    /// A named preset system prompt; takes precedence over `system_prompt`.
    pub system_prompt_preset: Option<SystemPromptPreset>,
    /// The base tool set via a preset; when set, `allowed_tools` is ignored by
    /// the CLI.
    pub tools_preset: Option<ToolsPreset>,

    /// Called with each line the subprocess writes to stderr. When absent,
    /// stderr is captured silently and included in errors on failure.
    pub stderr: Option<StderrSink>,

    /// Additional environment variables merged into the subprocess env, applied
    /// last so they win.
    pub env: BTreeMap<String, String>,

    /// A message id to resume the session from. Retained for
    /// forward-compatibility; **not wired to any flag**, because `--resume-at`
    /// does not exist in the current CLI.
    pub resume_session_at: String,

    /// Whether the CLI returns prompt suggestions. Sent via the initialize
    /// message.
    pub prompt_suggestions: bool,

    /// Command execution sandboxing. Sent via the initialize message.
    pub sandbox: Option<SandboxSettings>,

    /// Path to the `claude` binary. Defaults to the bare name, resolved on
    /// `PATH`.
    pub claude_executable: String,

    /// Set internally when the caller opened a persistent session: the
    /// subprocess stays alive across turns (stdin is not closed after a result)
    /// and the caller drives the conversation.
    pub(crate) session_mode: bool,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The handler fields are closures with no useful Debug; naming their
        // presence is what a reader actually wants when a session misbehaves.
        f.debug_struct("Options")
            .field("model", &self.model)
            .field("thinking", &self.thinking)
            .field("permission_mode", &self.permission_mode)
            .field("allowed_tools", &self.allowed_tools)
            .field("mcp_servers", &self.mcp_servers.keys().collect::<Vec<_>>())
            .field("cwd", &self.cwd)
            .field("claude_executable", &self.claude_executable)
            .field("session_mode", &self.session_mode)
            .field("has_permission_handler", &self.permission_handler.is_some())
            .field(
                "has_elicitation_handler",
                &self.elicitation_handler.is_some(),
            )
            .field("hook_events", &self.hooks.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Mirrors Go's `defaultOptions()`. The permission defaults are deliberate:
/// the SDK bypasses prompts unless the caller opts back in.
pub fn default_options() -> Options {
    Options {
        model: DEFAULT_MODEL.to_string(),
        thinking: thinking::ADAPTIVE.to_string(),
        permission_mode: permission_mode::BYPASS_PERMISSIONS.to_string(),
        allow_dangerously_skip_permissions: true,
        claude_executable: "claude".to_string(),
        ..Default::default()
    }
}

impl Options {
    /// A fresh set of options carrying the SDK defaults.
    pub fn new() -> Self {
        default_options()
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn with_append_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.append_system_prompt = prompt.into();
        self
    }

    /// Resumes an existing session by its id (`--resume`).
    pub fn with_session_id_to_resume(mut self, id: impl Into<String>) -> Self {
        self.resume_session_id = id.into();
        self
    }

    /// Sets a custom UUID for a brand-new session (`--session-id`).
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.custom_session_id = id.into();
        self
    }

    pub fn with_continue(mut self) -> Self {
        self.continue_session = true;
        self
    }

    pub fn with_fork_session(mut self) -> Self {
        self.fork_session = true;
        self
    }

    pub fn with_allowed_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_disallowed_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.disallowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_thinking(mut self, mode: impl Into<String>) -> Self {
        self.thinking = mode.into();
        self
    }

    pub fn with_max_thinking_tokens(mut self, n: i64) -> Self {
        self.max_thinking_tokens = n;
        self
    }

    pub fn with_max_turns(mut self, n: i64) -> Self {
        self.max_turns = n;
        self
    }

    pub fn with_effort(mut self, level: impl Into<String>) -> Self {
        self.effort = level.into();
        self
    }

    /// Appends, matching Go's `WithBetas`.
    pub fn with_betas<I, S>(mut self, betas: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.betas.extend(betas.into_iter().map(Into::into));
        self
    }

    pub fn with_fallback_model(mut self, model: impl Into<String>) -> Self {
        self.fallback_model = model.into();
        self
    }

    pub fn with_max_budget_usd(mut self, usd: f64) -> Self {
        self.max_budget_usd = usd;
        self
    }

    pub fn with_output_format(mut self, format: OutputFormat) -> Self {
        self.output_format = Some(format);
        self
    }

    pub fn with_enable_file_checkpointing(mut self) -> Self {
        self.enable_file_checkpointing = true;
        self
    }

    pub fn with_strict_mcp_config(mut self) -> Self {
        self.strict_mcp_config = true;
        self
    }

    pub fn with_cwd(mut self, dir: impl Into<String>) -> Self {
        self.cwd = dir.into();
        self
    }

    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = mode.into();
        self
    }

    /// Enables bypass permissions (the SDK default).
    pub fn with_bypass_permissions(mut self) -> Self {
        self.permission_mode = permission_mode::BYPASS_PERMISSIONS.to_string();
        self.allow_dangerously_skip_permissions = true;
        self
    }

    /// Restores normal (non-bypass) permission mode, overriding the SDK
    /// defaults. Use together with [`Options::with_permission_handler`] so the
    /// subprocess sends `can_use_tool` requests the handler can intercept.
    pub fn with_default_permissions(mut self) -> Self {
        self.permission_mode = permission_mode::DEFAULT.to_string();
        self.allow_dangerously_skip_permissions = false;
        self
    }

    pub fn with_permission_prompt_tool_name(mut self, name: impl Into<String>) -> Self {
        self.permission_prompt_tool_name = name.into();
        self
    }

    pub fn with_permission_handler(mut self, handler: PermissionHandler) -> Self {
        self.permission_handler = Some(handler);
        self
    }

    pub fn with_elicitation_handler(mut self, handler: ElicitationHandler) -> Self {
        self.elicitation_handler = Some(handler);
        self
    }

    pub fn with_include_partial_messages(mut self) -> Self {
        self.include_partial_messages = true;
        self
    }

    /// Sets how long to wait for the CLI to acknowledge the initialize
    /// handshake. Zero or negative restores the default.
    ///
    /// MCP servers declared in the options are started during the handshake, so
    /// a session with slow servers legitimately needs longer than a bare one.
    pub fn with_init_timeout(mut self, d: Duration) -> Self {
        self.init_timeout = if d.is_zero() { None } else { Some(d) };
        self
    }

    /// Replaces the MCP server map. Values are the serialized server configs.
    pub fn with_mcp_servers(mut self, servers: BTreeMap<String, serde_json::Value>) -> Self {
        self.mcp_servers = servers;
        self
    }

    /// Adds one MCP server under `name`.
    pub fn with_mcp_server(
        mut self,
        name: impl Into<String>,
        config: impl Serialize,
    ) -> Result<Self> {
        let value = serde_json::to_value(config)
            .map_err(|e| Error::wrap("encoding the MCP server config", e))?;
        self.mcp_servers.insert(name.into(), value);
        Ok(self)
    }

    pub fn with_agents(mut self, agents: BTreeMap<String, AgentDefinition>) -> Self {
        self.agents = agents;
        self
    }

    pub fn with_hooks(mut self, hooks: BTreeMap<String, Vec<HookMatcher>>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Appends, matching Go's `WithPlugins`.
    pub fn with_plugins<I>(mut self, plugins: I) -> Self
    where
        I: IntoIterator<Item = SdkPluginConfig>,
    {
        self.plugins.extend(plugins);
        self
    }

    /// Passes a settings file path or an inline JSON string via `--settings`.
    /// When set, `setting_sources` is ignored.
    pub fn with_settings(mut self, s: impl Into<String>) -> Self {
        self.settings = s.into();
        self
    }

    /// Appends, matching Go's `WithSettingSources`.
    pub fn with_setting_sources<I, S>(mut self, sources: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.setting_sources
            .extend(sources.into_iter().map(Into::into));
        self
    }

    /// Appends, matching Go's `WithAdditionalDirectories`.
    pub fn with_additional_directories<I, S>(mut self, dirs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.additional_directories
            .extend(dirs.into_iter().map(Into::into));
        self
    }

    /// Merges, matching Go's `WithExtraArgs`; later keys overwrite.
    pub fn with_extra_args<I, K, V>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in args {
            self.extra_args.insert(k.into(), v.into());
        }
        self
    }

    pub fn with_system_prompt_preset(mut self, preset: SystemPromptPreset) -> Self {
        self.system_prompt_preset = Some(preset);
        self
    }

    pub fn with_tools_preset(mut self, preset: ToolsPreset) -> Self {
        self.tools_preset = Some(preset);
        self
    }

    pub fn with_stderr(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.stderr = Some(std::sync::Arc::new(f));
        self
    }

    /// Merges, matching Go's `WithEnv`.
    pub fn with_env<I, K, V>(mut self, env: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in env {
            self.env.insert(k.into(), v.into());
        }
        self
    }

    pub fn with_sandbox(mut self, sandbox: SandboxSettings) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    pub fn with_claude_executable(mut self, path: impl Into<String>) -> Self {
        self.claude_executable = path.into();
        self
    }

    pub fn with_resume_session_at(mut self, message_id: impl Into<String>) -> Self {
        self.resume_session_at = message_id.into();
        self
    }

    pub fn with_prompt_suggestions(mut self, enabled: bool) -> Self {
        self.prompt_suggestions = enabled;
        self
    }

    /// Builds the argument list for the `claude` binary.
    ///
    /// Bidirectional mode: `--input-format stream-json` +
    /// `--output-format stream-json` + `--verbose`, and **no `--print`** —
    /// exactly what the official SDKs use. The prompt and system prompt are not
    /// passed as arguments; they go over stdin.
    pub fn build_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "--output-format".into(),
            "stream-json".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--verbose".into(),
        ];

        // A macro rather than a closure: a closure capturing `args` mutably
        // would conflict with the bare `args.push` calls for valueless flags.
        macro_rules! push {
            ($flag:expr, $value:expr) => {{
                args.push($flag.to_string());
                args.push($value);
            }};
        }

        if !self.model.is_empty() {
            push!("--model", self.model.clone());
        }

        // Every known mode is forwarded; an unrecognised one is dropped rather
        // than passed through, matching Go's switch over the three constants.
        match self.thinking.as_str() {
            thinking::ADAPTIVE | thinking::DISABLED | thinking::ENABLED => {
                push!("--thinking", self.thinking.clone());
            }
            _ => {}
        }

        if self.max_turns > 0 {
            push!("--max-turns", self.max_turns.to_string());
        }
        if !self.effort.is_empty() {
            push!("--effort", self.effort.clone());
        }
        if !self.resume_session_id.is_empty() {
            push!("--resume", self.resume_session_id.clone());
        }
        if !self.custom_session_id.is_empty() {
            push!("--session-id", self.custom_session_id.clone());
        }
        if self.continue_session {
            args.push("--continue".into());
        }
        if self.fork_session {
            // The CLI flag is --fork-session, not --fork.
            args.push("--fork-session".into());
        }
        if !self.allowed_tools.is_empty() {
            push!("--allowedTools", self.allowed_tools.join(","));
        }
        if !self.disallowed_tools.is_empty() {
            push!("--disallowedTools", self.disallowed_tools.join(","));
        }
        if !self.permission_mode.is_empty() {
            push!("--permission-mode", self.permission_mode.clone());
        }
        if self.allow_dangerously_skip_permissions {
            args.push("--allow-dangerously-skip-permissions".into());
        }
        if self.include_partial_messages {
            args.push("--include-partial-messages".into());
        }
        if !self.betas.is_empty() {
            push!("--betas", self.betas.join(","));
        }
        if !self.fallback_model.is_empty() {
            push!("--fallback-model", self.fallback_model.clone());
        }
        if self.max_budget_usd > 0.0 {
            push!("--max-budget-usd", format!("{:.6}", self.max_budget_usd));
        }
        if self.enable_file_checkpointing {
            args.push("--enable-file-checkpointing".into());
        }
        if self.strict_mcp_config {
            args.push("--strict-mcp-config".into());
        }

        // A registered handler is routed over stdio; an explicit tool name
        // selects an MCP tool instead. validate() guarantees the two are never
        // both set.
        if !self.permission_prompt_tool_name.is_empty() {
            push!(
                "--permission-prompt-tool",
                self.permission_prompt_tool_name.clone()
            );
        } else if self.permission_handler.is_some() {
            push!("--permission-prompt-tool", "stdio".into());
        }

        for plugin in &self.plugins {
            if !plugin.path.is_empty() {
                push!("--plugin-dir", plugin.path.clone());
            }
        }
        for dir in &self.additional_directories {
            if !dir.is_empty() {
                push!("--add-dir", dir.clone());
            }
        }

        if let Some(preset) = &self.tools_preset {
            if let Ok(encoded) = serde_json::to_string(preset) {
                push!("--tools", encoded);
            }
        }

        // Settings takes precedence; when it is set --setting-sources is
        // skipped entirely.
        if !self.settings.is_empty() {
            push!("--settings", self.settings.clone());
        } else if !self.setting_sources.is_empty() {
            push!("--setting-sources", self.setting_sources.join(","));
        }

        // MCP servers travel here and only here. They are deliberately not
        // named in the initialize message's sdkMcpServers, which is reserved
        // for servers the CLI expects to reach back over `mcp_message`.
        if !self.mcp_servers.is_empty() {
            let cfg = serde_json::json!({ "mcpServers": self.mcp_servers });
            if let Ok(encoded) = serde_json::to_string(&cfg) {
                push!("--mcp-config", encoded);
            }
        }

        // Boolean flags (empty value) are one element; flags with a value are
        // two.
        for (flag, value) in &self.extra_args {
            if flag.is_empty() {
                continue;
            }
            if value.is_empty() {
                args.push(flag.clone());
            } else {
                push!(flag, value.clone());
            }
        }

        // Note: sandbox settings go via the initialize message, not a flag, and
        // resume_session_at is omitted because --resume-at does not exist.

        args
    }

    /// Reports usage errors that would otherwise surface as confusing CLI
    /// failures or, worse, as silently missing enforcement.
    ///
    /// Runs before the process is spawned, so a misconfigured run fails without
    /// starting one.
    pub fn validate(&self) -> Result<()> {
        if self.permission_handler.is_some() && !self.permission_prompt_tool_name.is_empty() {
            return Err(Error::Other(
                "claude: with_permission_handler cannot be used with \
                 with_permission_prompt_tool_name: a handler is served over \
                 --permission-prompt-tool stdio, which would override the named tool"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Resolves the initialize handshake timeout: the explicit option first,
    /// then `CLAUDE_CODE_STREAM_CLOSE_TIMEOUT` (milliseconds, as the official
    /// SDKs read it), then the 60s default.
    ///
    /// Values at or below zero fall through to the default rather than
    /// disabling the timeout, which would let a wedged CLI hang startup forever.
    /// The environment variable only ever *raises* the floor.
    pub(crate) fn init_timeout(&self) -> Duration {
        if let Some(d) = self.init_timeout {
            if !d.is_zero() {
                return d;
            }
        }
        if let Ok(raw) = std::env::var("CLAUDE_CODE_STREAM_CLOSE_TIMEOUT") {
            if let Ok(ms) = raw.parse::<u64>() {
                if ms > 0 {
                    let d = Duration::from_millis(ms);
                    if d > DEFAULT_INIT_TIMEOUT {
                        return d;
                    }
                }
            }
        }
        DEFAULT_INIT_TIMEOUT
    }

    /// Warns when a permission handler is registered but cannot be reached,
    /// because the configuration grants the tools up front.
    ///
    /// Without this the handler simply never fires and the caller believes
    /// their policy is being enforced. The warning goes to the stderr callback
    /// when one is set, so it lands wherever the caller already routes CLI
    /// output.
    pub(crate) fn warn_permission_handler_shadowed(&self) {
        if self.permission_handler.is_none() {
            return;
        }

        let reason = if self.permission_mode == permission_mode::BYPASS_PERMISSIONS {
            format!("PermissionMode is {}", permission_mode::BYPASS_PERMISSIONS)
        } else if self.allow_dangerously_skip_permissions {
            "AllowDangerouslySkipPermissions is set".to_string()
        } else if !self.allowed_tools.is_empty() {
            format!(
                "AllowedTools pre-approves {}",
                self.allowed_tools.join(", ")
            )
        } else {
            return;
        };

        let msg = format!(
            "claude: warning: a PermissionHandler is registered but will not be consulted \
             for every tool call because {reason}; use with_default_permissions() to have \
             tool calls routed to the handler"
        );

        match &self.stderr {
            Some(f) => f(&msg),
            None => log::warn!("{msg}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_match_the_go_sdk() {
        let o = Options::new();
        assert_eq!(o.model, "claude-sonnet-4-6");
        assert_eq!(o.thinking, thinking::ADAPTIVE);
        assert_eq!(o.permission_mode, permission_mode::BYPASS_PERMISSIONS);
        assert!(o.allow_dangerously_skip_permissions);
        assert_eq!(o.claude_executable, "claude");
    }

    #[test]
    fn bidirectional_mode_leads_and_carries_no_print_flag() {
        let args = Options::new().build_args();
        assert_eq!(
            &args[..5],
            &[
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose"
            ]
        );
        assert!(!args.iter().any(|a| a == "--print"));
    }

    #[test]
    fn a_handler_routes_permissions_over_stdio() {
        let handler: PermissionHandler = std::sync::Arc::new(|_, _, _| {
            Box::pin(async { crate::claude::PermissionResult::allow() })
        });
        let args = Options::new()
            .with_default_permissions()
            .with_permission_handler(handler)
            .build_args();
        let i = args
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .unwrap();
        assert_eq!(args[i + 1], "stdio");
    }

    #[test]
    fn a_named_prompt_tool_wins_over_the_stdio_route() {
        let args = Options::new()
            .with_permission_prompt_tool_name("mcp__x__ask")
            .build_args();
        let i = args
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .unwrap();
        assert_eq!(args[i + 1], "mcp__x__ask");
    }

    #[test]
    fn a_handler_and_a_named_prompt_tool_together_are_a_usage_error() {
        let handler: PermissionHandler = std::sync::Arc::new(|_, _, _| {
            Box::pin(async { crate::claude::PermissionResult::allow() })
        });
        let o = Options::new()
            .with_permission_handler(handler)
            .with_permission_prompt_tool_name("mcp__x__ask");
        assert!(o.validate().is_err());
    }

    #[test]
    fn settings_suppresses_setting_sources() {
        let args = Options::new()
            .with_settings("/tmp/settings.json")
            .with_setting_sources([setting_source::PROJECT])
            .build_args();
        assert!(args.iter().any(|a| a == "--settings"));
        assert!(
            !args.iter().any(|a| a == "--setting-sources"),
            "settings takes precedence, and the two must not both be sent"
        );
    }

    #[test]
    fn setting_sources_are_comma_joined_when_settings_is_absent() {
        let args = Options::new()
            .with_setting_sources([setting_source::USER, setting_source::PROJECT])
            .build_args();
        let i = args.iter().position(|a| a == "--setting-sources").unwrap();
        assert_eq!(args[i + 1], "user,project");
    }

    #[test]
    fn mcp_servers_travel_as_one_mcp_config_argument() {
        let o = Options::new()
            .with_mcp_server(
                "local-tools",
                McpHttpServer {
                    server_type: "http".into(),
                    url: "http://127.0.0.1:5050".into(),
                    headers: BTreeMap::new(),
                },
            )
            .unwrap();
        let args = o.build_args();
        let i = args.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(
            args[i + 1],
            r#"{"mcpServers":{"local-tools":{"type":"http","url":"http://127.0.0.1:5050"}}}"#
        );
    }

    #[test]
    fn plugins_and_directories_each_get_their_own_flag() {
        let args = Options::new()
            .with_plugins([
                SdkPluginConfig {
                    plugin_type: "local".into(),
                    path: "/a".into(),
                },
                SdkPluginConfig {
                    plugin_type: "local".into(),
                    path: "/b".into(),
                },
            ])
            .with_additional_directories(["/x", "/y"])
            .build_args();
        assert_eq!(args.iter().filter(|a| *a == "--plugin-dir").count(), 2);
        assert_eq!(args.iter().filter(|a| *a == "--add-dir").count(), 2);
    }

    #[test]
    fn a_boolean_extra_arg_is_one_element_and_a_valued_one_is_two() {
        let args = Options::new()
            .with_extra_args([("--flagged", ""), ("--valued", "7")])
            .build_args();
        let flagged = args.iter().position(|a| a == "--flagged").unwrap();
        assert_ne!(args.get(flagged + 1).map(String::as_str), Some(""));
        let valued = args.iter().position(|a| a == "--valued").unwrap();
        assert_eq!(args[valued + 1], "7");
    }

    #[test]
    fn budget_is_formatted_to_six_decimal_places() {
        let args = Options::new().with_max_budget_usd(1.5).build_args();
        let i = args.iter().position(|a| a == "--max-budget-usd").unwrap();
        assert_eq!(args[i + 1], "1.500000");
    }

    #[test]
    fn fork_uses_the_flag_the_cli_actually_has() {
        let args = Options::new().with_fork_session().build_args();
        assert!(args.iter().any(|a| a == "--fork-session"));
        assert!(!args.iter().any(|a| a == "--fork"));
    }

    #[test]
    fn resume_session_at_is_retained_but_never_sent() {
        // --resume-at does not exist in the current CLI; sending it would fail
        // the spawn outright.
        let args = Options::new().with_resume_session_at("msg_1").build_args();
        assert!(!args.iter().any(|a| a.contains("resume-at")));
    }
}

//! Permission types and the `can_use_tool` round trip, ported from the
//! permission half of `claude/options.go`.
//!
//! The load-bearing rule here is **fail closed**. An absent handler is answered
//! with an error control_response, never an allow, and a result whose behaviour
//! is neither `allow` nor `deny` — including the default — is a usage error
//! rather than an implicit allow. Answering a permission question nobody was
//! asked would grant the tool call, which is the exact bug the explicit
//! [`PermissionBehavior`] enum exists to make unrepresentable.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::lenient::lenient;

/// The allow/deny/ask outcome for a permission rule.
pub mod permission_behavior {
    pub const ALLOW: &str = "allow";
    pub const DENY: &str = "deny";
    pub const ASK: &str = "ask";
}

/// Where a permission update is persisted.
pub mod permission_destination {
    /// The global user settings file.
    pub const USER_SETTINGS: &str = "userSettings";
    /// The shared project settings file.
    pub const PROJECT_SETTINGS: &str = "projectSettings";
    /// The gitignored local settings file.
    pub const LOCAL_SETTINGS: &str = "localSettings";
    /// Applies only for the current session.
    pub const SESSION: &str = "session";
}

/// A single permission rule identifying a tool and an optional content pattern
/// (e.g. a glob for the Bash tool's command argument).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PermissionRuleValue {
    /// The tool the rule applies to (e.g. `Bash`, `Read`).
    #[serde(rename = "toolName")]
    pub tool_name: String,
    /// An optional content pattern (e.g. `git commit:*`, `/src/**`). `None`
    /// matches all invocations of the tool.
    #[serde(rename = "ruleContent", skip_serializing_if = "Option::is_none")]
    pub rule_content: Option<String>,
}

/// A single permission mutation. `update_type` is the discriminant; fill only
/// the corresponding fields.
///
/// * `addRules` / `replaceRules` / `removeRules` → `rules`, `behavior`, `destination`
/// * `setMode` → `mode`, `destination`
/// * `addDirectories` / `removeDirectories` → `directories`, `destination`
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PermissionUpdate {
    #[serde(rename = "type")]
    pub update_type: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PermissionRuleValue>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub behavior: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub destination: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mode: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<String>,
}

/// Full context about a tool call the CLI is asking permission for.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct PermissionContext {
    /// Permission updates suggested by the CLI.
    #[serde(rename = "permission_suggestions", deserialize_with = "lenient")]
    pub suggestions: Vec<PermissionUpdate>,
    /// Populated when a path restriction triggered the request.
    #[serde(deserialize_with = "lenient")]
    pub blocked_path: String,
    /// The CLI's internal reason for asking.
    #[serde(deserialize_with = "lenient")]
    pub decision_reason: String,
    /// The tool use identifier for this specific call.
    #[serde(deserialize_with = "lenient")]
    pub tool_use_id: String,
    /// Set when the request originates from a sub-agent.
    #[serde(deserialize_with = "lenient")]
    pub agent_id: String,
    /// A short heading for the permission prompt, when the CLI supplies one.
    #[serde(deserialize_with = "lenient")]
    pub title: String,
    /// The human-friendly name of the tool being requested.
    #[serde(deserialize_with = "lenient")]
    pub display_name: String,
    /// What the tool call will do.
    #[serde(deserialize_with = "lenient")]
    pub description: String,
}

/// What a [`PermissionHandler`] decided.
///
/// Go models the behaviour as a string whose zero value is a usage error; Rust
/// makes the same distinction by making the enum have no default, so a handler
/// cannot accidentally return "unset" and have it read as an allow.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionResult {
    Allow {
        /// Replaces the tool input before execution. `None` echoes the CLI's
        /// original input back verbatim — the CLI expects the input it should
        /// actually run, so this field is always sent.
        updated_input: Option<serde_json::Value>,
        /// Persistent permission mutations to apply.
        updated_permissions: Vec<PermissionUpdate>,
    },
    Deny {
        /// Shown to the user, explaining the denial.
        message: String,
        /// Stops the agent entirely after this tool call.
        interrupt: bool,
    },
}

impl PermissionResult {
    /// The common allow: run the tool exactly as the model asked.
    pub fn allow() -> Self {
        PermissionResult::Allow {
            updated_input: None,
            updated_permissions: Vec::new(),
        }
    }

    /// The common deny, with a reason shown to the user.
    pub fn deny(message: impl Into<String>) -> Self {
        PermissionResult::Deny {
            message: message.into(),
            interrupt: false,
        }
    }
}

/// The future a [`PermissionHandler`] returns.
pub type PermissionFuture = Pin<Box<dyn Future<Output = PermissionResult> + Send>>;

/// Called when the CLI sends a `can_use_tool` control_request.
///
/// Registering a handler makes the SDK pass `--permission-prompt-tool stdio`,
/// which is what causes the CLI to route tool calls here; it cannot be combined
/// with a permission prompt tool name.
///
/// When no handler is registered, an incoming `can_use_tool` is answered with an
/// error rather than being allowed — the SDK never grants a permission no one
/// approved.
///
/// The handler is **async and awaited inline on the reader task**, mirroring Go,
/// where it runs on the goroutine that owns stdout. A slow handler therefore
/// stalls the message stream, which is the intended semantics: Agento's chat
/// blocks the turn while a human answers the prompt. Making it a future rather
/// than a blocking closure is the only departure, and it exists so a waiting
/// handler parks the task instead of pinning a runtime worker thread.
pub type PermissionHandler =
    Arc<dyn Fn(String, Option<Box<RawValue>>, PermissionContext) -> PermissionFuture + Send + Sync>;

/// The future an [`ElicitationHandler`] returns.
pub type ElicitationFuture = Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>;

/// Called when the CLI sends an `elicitation` control_request asking the SDK
/// host for user input. The handler receives the raw JSON payload and returns a
/// response object (e.g. `{"response": "user input"}`).
///
/// When absent — or when the handler returns `None` — elicitations are
/// auto-cancelled with `{"cancel": true}`.
pub type ElicitationHandler = Arc<dyn Fn(Option<Box<RawValue>>) -> ElicitationFuture + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_without_content_omits_it_rather_than_sending_null() {
        let rule = PermissionRuleValue {
            tool_name: "Bash".into(),
            rule_content: None,
        };
        assert_eq!(
            serde_json::to_string(&rule).unwrap(),
            r#"{"toolName":"Bash"}"#
        );
    }

    #[test]
    fn an_update_omits_its_empty_halves() {
        // addRules fills rules/behavior/destination; setMode fills mode. The
        // unused half must not appear, matching Go's omitempty.
        let update = PermissionUpdate {
            update_type: "setMode".into(),
            mode: "plan".into(),
            destination: "session".into(),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&update).unwrap(),
            r#"{"type":"setMode","destination":"session","mode":"plan"}"#
        );
    }

    #[test]
    fn the_context_decodes_from_the_control_request_field_names() {
        let ctx: PermissionContext = serde_json::from_str(
            r#"{"blocked_path":"/etc","decision_reason":"outside cwd","tool_use_id":"tu_1",
                "permission_suggestions":[{"type":"addDirectories","directories":["/etc"]}]}"#,
        )
        .unwrap();
        assert_eq!(ctx.blocked_path, "/etc");
        assert_eq!(ctx.tool_use_id, "tu_1");
        assert_eq!(ctx.suggestions[0].update_type, "addDirectories");
        assert_eq!(ctx.suggestions[0].directories, vec!["/etc"]);
    }
}

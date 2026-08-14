//! Lifecycle hooks, ported from `claude/hooks.go`.
//!
//! Hooks are declared in the initialize message as generated callback ids, and
//! the CLI calls back with `hook_callback` control_requests carrying the id.
//! Two details are easy to get wrong and were both shipped broken once:
//!
//! * the CLI expects **one entry per matcher**, carrying *all* of that
//!   matcher's callback ids under `hookCallbackIds` — not one entry per hook;
//! * `matcher` is **always present**, `null` when unset, mirroring the official
//!   Python SDK.
//!
//! The event name travels inside the hook input payload as `hook_event_name`,
//! not on the control_request envelope. See [`super::process`].

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use serde_json::value::RawValue;

use super::process::new_uuid;

/// The lifecycle event that triggered a hook callback.
pub mod hook_event {
    pub const PRE_TOOL_USE: &str = "PreToolUse";
    pub const POST_TOOL_USE: &str = "PostToolUse";
    /// Fires after a tool call fails.
    pub const POST_TOOL_USE_FAILURE: &str = "PostToolUseFailure";
    pub const NOTIFICATION: &str = "Notification";
    pub const STOP: &str = "Stop";
    pub const SUBAGENT_STOP: &str = "SubagentStop";
    /// Fires when a sub-agent is started.
    pub const SUBAGENT_START: &str = "SubagentStart";
    pub const PRE_COMPACT: &str = "PreCompact";
    pub const USER_PROMPT_SUBMIT: &str = "UserPromptSubmit";
    /// Fires when a session starts.
    pub const SESSION_START: &str = "SessionStart";
    pub const SETUP: &str = "Setup";
    /// Fires when Claude requests permission to use a tool.
    pub const PERMISSION_REQUEST: &str = "PermissionRequest";
    /// Fires when a session ends.
    pub const SESSION_END: &str = "SessionEnd";
    /// Fires when a teammate agent becomes idle.
    pub const TEAMMATE_IDLE: &str = "TeammateIdle";
    /// Fires when a task completes.
    pub const TASK_COMPLETED: &str = "TaskCompleted";
    /// Fires when the CLI requests user elicitation.
    pub const ELICITATION: &str = "Elicitation";
    /// Fires after an elicitation is resolved.
    pub const ELICITATION_RESULT: &str = "ElicitationResult";
    /// Fires when configuration changes mid-session.
    pub const CONFIG_CHANGE: &str = "ConfigChange";
    /// Fires when a git worktree is created.
    pub const WORKTREE_CREATE: &str = "WorktreeCreate";
    /// Fires when a git worktree is removed.
    pub const WORKTREE_REMOVE: &str = "WorktreeRemove";
}

/// The return value of a [`HookFunc`]. All fields are optional.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct HookOutput {
    /// Controls whether the operation continues.
    #[serde(rename = "continue", skip_serializing_if = "Option::is_none")]
    pub continue_: Option<bool>,
    /// Prevents the hook output from being shown to the user.
    #[serde(rename = "suppressOutput", skip_serializing_if = "is_false")]
    pub suppress_output: bool,
    /// The reason provided when the hook stops execution.
    #[serde(rename = "stopReason", skip_serializing_if = "String::is_empty")]
    pub stop_reason: String,
    /// An approval/rejection decision (`approve`, `reject`, `ask`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub decision: String,
    /// An additional message injected into the context.
    #[serde(rename = "systemMessage", skip_serializing_if = "String::is_empty")]
    pub system_message: String,
    /// The reason for the decision.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// Hook-type-specific structured output.
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<serde_json::Value>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The future a [`HookFunc`] returns. `Err` is answered with an error
/// control_response carrying the message.
pub type HookFuture = Pin<Box<dyn Future<Output = Result<Option<HookOutput>, String>> + Send>>;

/// A hook callback.
///
/// Receives the lifecycle event, the raw JSON payload from the CLI, and the
/// tool use id (non-empty for tool-related events). Like the permission
/// handler, it is awaited inline on the reader task.
pub type HookFunc = Arc<dyn Fn(String, Option<Box<RawValue>>, String) -> HookFuture + Send + Sync>;

/// One or more hook functions for a specific tool matcher pattern.
#[derive(Clone)]
pub struct HookMatcher {
    /// A glob-style pattern matching the tool name; empty matches all.
    pub matcher: String,
    /// The callbacks to invoke when the matcher fires.
    pub hooks: Vec<HookFunc>,
    /// Timeout in seconds for all hooks in this matcher (0 = the CLI default,
    /// which is 60s).
    pub timeout: i64,
}

impl std::fmt::Debug for HookMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookMatcher")
            .field("matcher", &self.matcher)
            .field("hooks", &self.hooks.len())
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Maps callback ids (assigned at init time) to their functions. Used by the
/// reader task to dispatch `hook_callback` control_requests.
pub(crate) type HookRegistry = BTreeMap<String, HookFunc>;

/// Converts the caller's hook map into the shape the initialize message wants,
/// and returns a registry mapping each generated callback id to its function.
///
/// An empty configuration yields an empty object rather than being omitted:
/// `hooks` is always present in the initialize request.
pub(crate) fn build_hooks_for_initialize(
    hooks: &BTreeMap<String, Vec<HookMatcher>>,
) -> (serde_json::Map<String, serde_json::Value>, HookRegistry) {
    let mut registry = HookRegistry::new();
    let mut config = serde_json::Map::new();

    if hooks.is_empty() {
        return (config, registry);
    }

    for (event, matchers) in hooks {
        let mut matcher_configs = Vec::new();
        for matcher in matchers {
            if matcher.hooks.is_empty() {
                continue;
            }
            let mut callback_ids = Vec::with_capacity(matcher.hooks.len());
            for hook in &matcher.hooks {
                let id = new_uuid();
                registry.insert(id.clone(), hook.clone());
                callback_ids.push(serde_json::Value::String(id));
            }

            // One entry per matcher, carrying all of that matcher's callback
            // ids. "matcher" is always present, null when unset.
            let mut cfg = serde_json::Map::new();
            cfg.insert(
                "matcher".into(),
                if matcher.matcher.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(matcher.matcher.clone())
                },
            );
            cfg.insert(
                "hookCallbackIds".into(),
                serde_json::Value::Array(callback_ids),
            );
            if matcher.timeout > 0 {
                cfg.insert("timeout".into(), matcher.timeout.into());
            }
            matcher_configs.push(serde_json::Value::Object(cfg));
        }
        if !matcher_configs.is_empty() {
            config.insert(event.clone(), serde_json::Value::Array(matcher_configs));
        }
    }

    (config, registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_hook() -> HookFunc {
        Arc::new(|_, _, _| Box::pin(async { Ok(None) }))
    }

    #[test]
    fn an_empty_configuration_is_an_empty_object() {
        let (cfg, reg) = build_hooks_for_initialize(&BTreeMap::new());
        assert!(cfg.is_empty());
        assert!(reg.is_empty());
        assert_eq!(serde_json::to_string(&cfg).unwrap(), "{}");
    }

    #[test]
    fn one_matcher_carries_all_its_callback_ids_in_one_entry() {
        // The shape that shipped broken: two hooks under one matcher must be
        // one entry with two ids, not two entries.
        let mut hooks = BTreeMap::new();
        hooks.insert(
            hook_event::PRE_TOOL_USE.to_string(),
            vec![HookMatcher {
                matcher: "Bash".into(),
                hooks: vec![noop_hook(), noop_hook()],
                timeout: 30,
            }],
        );

        let (cfg, reg) = build_hooks_for_initialize(&hooks);
        let entries = cfg["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "one entry per matcher, not per hook");
        assert_eq!(entries[0]["matcher"], "Bash");
        assert_eq!(entries[0]["hookCallbackIds"].as_array().unwrap().len(), 2);
        assert_eq!(entries[0]["timeout"], 30);
        assert_eq!(reg.len(), 2, "both callbacks are dispatchable");
    }

    #[test]
    fn an_unset_matcher_is_null_rather_than_absent() {
        let mut hooks = BTreeMap::new();
        hooks.insert(
            hook_event::STOP.to_string(),
            vec![HookMatcher {
                matcher: String::new(),
                hooks: vec![noop_hook()],
                timeout: 0,
            }],
        );

        let (cfg, _) = build_hooks_for_initialize(&hooks);
        let entry = &cfg["Stop"].as_array().unwrap()[0];
        assert!(entry.get("matcher").is_some(), "the key is always present");
        assert!(entry["matcher"].is_null());
        assert!(entry.get("timeout").is_none(), "zero means the CLI default");
    }

    #[test]
    fn a_matcher_with_no_hooks_contributes_nothing() {
        let mut hooks = BTreeMap::new();
        hooks.insert(
            hook_event::STOP.to_string(),
            vec![HookMatcher {
                matcher: "x".into(),
                hooks: vec![],
                timeout: 0,
            }],
        );
        let (cfg, reg) = build_hooks_for_initialize(&hooks);
        assert!(cfg.is_empty());
        assert!(reg.is_empty());
    }

    #[test]
    fn hook_output_omits_its_zero_values() {
        let out = HookOutput {
            decision: "approve".into(),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&out).unwrap(),
            r#"{"decision":"approve"}"#
        );
    }
}

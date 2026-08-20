//! The payload of the initialize control response — the CLI's own account of
//! what the connected binary can do. Ported from `claude/init_types.go`.
//!
//! Field names come from a response captured from a real CLI, not from any SDK
//! type definition. Note the mixed casing: the envelope is snake_case while the
//! entries inside `models` and `commands` are camelCase, because the CLI passes
//! those through verbatim from its TypeScript shape.
//!
//! Decoding is deliberately lenient throughout: an unknown or missing field must
//! never fail a session, because the SDK is expected to run against CLI versions
//! both older and newer than itself.

use serde::Deserialize;
use serde_json::value::RawValue;

use super::lenient::lenient;

/// One model the connected CLI offers.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelInfo {
    /// The identifier to pass to `with_model` or `set_model` (e.g. `default`,
    /// `sonnet`). Not necessarily a concrete model id — see `resolved_model`.
    #[serde(deserialize_with = "lenient")]
    pub value: String,
    /// The concrete model `value` resolves to, e.g. `claude-opus-5[1m]`.
    #[serde(rename = "resolvedModel", deserialize_with = "lenient")]
    pub resolved_model: String,
    /// Human-readable name, e.g. `Default (recommended)`.
    #[serde(rename = "displayName", deserialize_with = "lenient")]
    pub display_name: String,
    /// One-line summary of what the model is good for.
    #[serde(deserialize_with = "lenient")]
    pub description: String,

    #[serde(rename = "supportsEffort", deserialize_with = "lenient")]
    pub supports_effort: bool,
    #[serde(rename = "supportedEffortLevels", deserialize_with = "lenient")]
    pub supported_effort_levels: Vec<String>,
    #[serde(rename = "supportsAdaptiveThinking", deserialize_with = "lenient")]
    pub supports_adaptive_thinking: bool,
    #[serde(rename = "supportsFastMode", deserialize_with = "lenient")]
    pub supports_fast_mode: bool,
    #[serde(rename = "supportsAutoMode", deserialize_with = "lenient")]
    pub supports_auto_mode: bool,
}

/// One slash command available in the session.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SlashCommand {
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "lenient")]
    pub description: String,
    #[serde(deserialize_with = "lenient")]
    pub aliases: Vec<String>,
    /// Describes the command's expected arguments, when it takes any.
    #[serde(rename = "argumentHint", deserialize_with = "lenient")]
    pub argument_hint: String,
}

/// One subagent type the session can dispatch to.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentInfo {
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "lenient")]
    pub description: String,
    /// The agent's model override; empty when it inherits the session's.
    #[serde(deserialize_with = "lenient")]
    pub model: String,
}

/// The account the CLI is authenticated as.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AccountInfo {
    #[serde(deserialize_with = "lenient")]
    pub email: String,
    #[serde(deserialize_with = "lenient")]
    pub organization: String,
    /// e.g. `Claude Max`; empty on API-key auth.
    #[serde(rename = "subscriptionType", deserialize_with = "lenient")]
    pub subscription_type: String,
    /// e.g. `firstParty`, `bedrock`, `vertex`.
    #[serde(rename = "apiProvider", deserialize_with = "lenient")]
    pub api_provider: String,
}

/// The decoded body of the initialize control response.
///
/// `raw` holds the undecoded body so that fields this SDK does not model yet
/// remain reachable, and so a future CLI adding fields costs nothing here.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct InitializeResponse {
    #[serde(deserialize_with = "lenient")]
    pub commands: Vec<SlashCommand>,
    #[serde(deserialize_with = "lenient")]
    pub agents: Vec<AgentInfo>,
    #[serde(deserialize_with = "lenient")]
    pub models: Vec<ModelInfo>,
    #[serde(deserialize_with = "lenient")]
    pub account: AccountInfo,

    #[serde(deserialize_with = "lenient")]
    pub output_style: String,
    #[serde(deserialize_with = "lenient")]
    pub available_output_styles: Vec<String>,

    /// `off`, `on`, …; `fast_mode_disabled_reason` explains why fast mode is
    /// unavailable (e.g. `sdk_opt_in_required`).
    #[serde(deserialize_with = "lenient")]
    pub fast_mode_state: String,
    #[serde(deserialize_with = "lenient")]
    pub fast_mode_disabled_reason: String,

    /// The CLI process id as the CLI reports it.
    #[serde(deserialize_with = "lenient")]
    pub pid: i64,

    #[serde(skip)]
    pub raw: Option<Box<RawValue>>,
}

/// Parses an initialize control response body.
///
/// This never fails: the handshake succeeded if the CLI acknowledged it, so a
/// body that does not parse yields a response carrying only `raw` rather than
/// failing the session. Callers get empty lists instead of data in that case.
pub(crate) fn decode_initialize_response(body: Option<&RawValue>) -> InitializeResponse {
    let Some(body) = body else {
        return InitializeResponse::default();
    };

    let (mut resp, _) = super::lenient::decode::<InitializeResponse>(body.get().as_bytes());
    resp.raw = RawValue::from_string(body.get().to_owned()).ok();
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(s: &str) -> Box<RawValue> {
        RawValue::from_string(s.to_owned()).unwrap()
    }

    #[test]
    fn an_absent_body_is_a_successful_handshake_with_no_data() {
        let resp = decode_initialize_response(None);
        assert!(resp.models.is_empty());
        assert!(resp.raw.is_none());
    }

    #[test]
    fn an_unparseable_body_still_yields_a_response() {
        // The handshake succeeded; only the payload is unfamiliar.
        let body = raw(r#"["not","an","object"]"#);
        let resp = decode_initialize_response(Some(&body));
        assert!(resp.models.is_empty());
        assert_eq!(
            resp.raw.as_ref().map(|r| r.get()),
            Some(r#"["not","an","object"]"#)
        );
    }

    #[test]
    fn the_captured_shape_decodes_with_its_mixed_casing() {
        let body = raw(
            r#"{"models":[{"value":"default","resolvedModel":"claude-opus-5","displayName":"Default","supportsEffort":true}],
                "commands":[{"name":"init","argumentHint":"[path]"}],
                "account":{"email":"a@b.c","apiProvider":"firstParty"},
                "output_style":"default","pid":4242}"#,
        );
        let resp = decode_initialize_response(Some(&body));
        assert_eq!(resp.models[0].resolved_model, "claude-opus-5");
        assert!(resp.models[0].supports_effort);
        assert_eq!(resp.commands[0].argument_hint, "[path]");
        assert_eq!(resp.account.api_provider, "firstParty");
        assert_eq!(resp.output_style, "default");
        assert_eq!(resp.pid, 4242);
    }
}

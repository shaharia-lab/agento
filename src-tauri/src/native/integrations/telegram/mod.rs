//! The Telegram integration's **outbound** in-process MCP server, ported from
//! `internal/integrations/telegram/`.
//!
//! Eleven tools in one service group (`messaging`), over a bot token. The fifth
//! of the six (#314), and the largest tool set after GitHub's twenty.
//!
//! ## Outbound only — the inbound half is somebody else's issue
//!
//! Telegram is the one integration with two directions. This is the tool server;
//! `POST /webhooks/telegram/{id}` and `internal/trigger/`'s dispatcher are #319
//! and are **not** touched here. They matter to this port for one reason: the
//! webhook route is mounted at the *root* rather than under `/api`, arrives with
//! a foreign `Host`, and is authenticated by its own secret token, so
//! `guards.rs` deliberately does not cover it — see the root `CLAUDE.md`.
//! Nothing in this module is on that path.
//!
//! `webhook.go` is likewise unported: it answers the integration's webhook
//! management routes, which stay with Go.
//!
//! ## What is new here, and both were documented as unreached
//!
//! `claude/schema_vectors.rs` left three reflector divergences standing because
//! "nothing in the six integrations" reached them. This one reaches two — a
//! `[]string` parameter and `float64` parameters — and `messaging`'s header has
//! the detail. The slice is the one that needed new code: Go renders every slice
//! as `["null","array"]` and `schemars` renders a bare `array`, so the port adds
//! the null itself, exactly as the map's guidance says a port that needs one must.
//!
//! ## The bot token is in the URL path
//!
//! `apiURL` interpolates it into the path rather than a header, which is
//! Telegram's design. It changes what an error string may carry and it puts a
//! credential where `url::Url::parse` can normalise — see `client`'s header and
//! `Client::endpoint`.
//!
//! ## What has to match Go, and what checks it
//!
//! Four surfaces, pinned by `desktop/parity/telegram_vectors.json` — taken from
//! the **running Go server** over its real MCP transport against a fake Telegram
//! that records the request each tool built: the hosted tool set, each advertised
//! schema (including the `["null","array"]` one), the request body of all eleven,
//! and the result text of every success and every failure.
//!
//! `telegram/parity.go`'s `SetAPIBase` is the seam, as GitHub's and Slack's are:
//! the base is a package variable, so a test cannot pass one.
//!
//! `validate.go`'s `ValidateBotToken` is **not ported** — it answers
//! `POST /api/integrations/{id}/auth/validate`, which dials Telegram and stays
//! with Go. That route is covered by
//! [`reload_after_auth`](super::registry::reload_after_auth), which is type-blind
//! and gated on the row's type.

pub mod client;
pub mod messaging;
pub mod validate;

#[cfg(test)]
mod tests_vectors;

use std::collections::{BTreeMap, BTreeSet};

use rmcp::model::{CallToolResult, ContentBlock};

use crate::claude::{tool_server, InProcessMcpServer, Result, ToolDef};

use super::ServiceConfig;
use client::Client;

/// The one service group.
pub const SERVICES: &[&str] = &["messaging"];

/// Every tool this integration can host, in registration order — `SERVICES`
/// order, then each `register*Tools` batch's own.
///
/// **Not** the order `tools/list` answers in: both SDKs sort by name.
pub const TELEGRAM_TOOL_NAMES: &[&str] = &[
    "send_message",
    "send_photo",
    "send_location",
    "create_poll",
    "read_messages",
    "get_chat_info",
    "get_chat_members",
    "forward_message",
    "edit_message",
    "delete_message",
    "pin_message",
];

/// `fmt.Sprintf("telegram-%s", cfg.ID)` — `mcp.NewServer`'s implementation name,
/// **not** the prefix on a qualified tool name (that is the bare integration id).
pub fn server_name(integration_id: &str) -> String {
    format!("telegram-{integration_id}")
}

/// The union of every **enabled** service's `Tools`.
pub fn build_allowed_set(services: &BTreeMap<String, ServiceConfig>) -> BTreeSet<String> {
    let mut allowed = BTreeSet::new();
    for service in services.values() {
        if !service.enabled {
            continue;
        }
        for tool in service.tools.iter().flat_map(|tools| tools.iter()) {
            allowed.insert(tool.clone());
        }
    }
    allowed
}

/// Present **and** enabled.
pub fn service_enabled(services: &BTreeMap<String, ServiceConfig>, name: &str) -> bool {
    services.get(name).is_some_and(|service| service.enabled)
}

/// Every tool `buildMCPServer` would register for `services`, over `token`.
pub fn telegram_tools(services: &BTreeMap<String, ServiceConfig>, token: &str) -> Vec<ToolDef> {
    let allowed = build_allowed_set(services);
    let client = Client::new(token);
    let mut tools = Vec::new();

    // `len(allowed) == 0 || allowed[name]` — so an empty set admits everything.
    let mut push = |service: &str, name: &str, tool: fn(&Client) -> ToolDef| {
        if !service_enabled(services, service) {
            return;
        }
        if !allowed.is_empty() && !allowed.contains(name) {
            return;
        }
        tools.push(tool(&client));
    };

    push("messaging", "send_message", messaging::send_message);
    push("messaging", "send_photo", messaging::send_photo);
    push("messaging", "send_location", messaging::send_location);
    push("messaging", "create_poll", messaging::create_poll);
    push("messaging", "read_messages", messaging::read_messages);
    push("messaging", "get_chat_info", messaging::get_chat_info);
    push("messaging", "get_chat_members", messaging::get_chat_members);
    push("messaging", "forward_message", messaging::forward_message);
    push("messaging", "edit_message", messaging::edit_message);
    push("messaging", "delete_message", messaging::delete_message);
    push("messaging", "pin_message", messaging::pin_message);

    tools
}

/// Starts the integration's server on a random loopback port.
pub async fn start_telegram_mcp_server(
    integration_id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    token: &str,
) -> Result<InProcessMcpServer> {
    tool_server(
        &server_name(integration_id),
        telegram_tools(services, token),
    )
    .await
}

/// Go's `textResult`, shared by all eleven tools.
pub(crate) fn text_result(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::gojson::GoList;

    fn service(enabled: bool, tools: &[&str]) -> ServiceConfig {
        ServiceConfig {
            enabled,
            tools: Some(GoList(tools.iter().map(|t| t.to_string()).collect())),
        }
    }

    fn names(services: &BTreeMap<String, ServiceConfig>) -> Vec<String> {
        telegram_tools(services, "123456:token")
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    #[test]
    fn the_server_name_is_gos() {
        assert_eq!(server_name("abc123"), "telegram-abc123");
    }

    #[test]
    fn an_empty_allowed_set_hosts_every_tool() {
        let services = BTreeMap::from([("messaging".to_string(), service(true, &[]))]);
        assert_eq!(names(&services), TELEGRAM_TOOL_NAMES);
        assert!(build_allowed_set(&services).is_empty());
    }

    #[test]
    fn one_named_tool_narrows_the_service() {
        let services = BTreeMap::from([(
            "messaging".to_string(),
            service(true, &["send_message", "pin_message"]),
        )]);
        assert_eq!(names(&services), ["send_message", "pin_message"]);
    }

    /// An enabled service Telegram does not know still contributes its names.
    #[test]
    fn an_unknown_enabled_service_still_narrows_the_known_one() {
        let services = BTreeMap::from([
            ("messaging".to_string(), service(true, &[])),
            ("other".to_string(), service(true, &["create_poll"])),
        ]);
        assert_eq!(names(&services), ["create_poll"]);
    }

    #[test]
    fn a_disabled_service_is_invisible_in_both_directions() {
        let services = BTreeMap::from([
            ("messaging".to_string(), service(false, &[])),
            ("other".to_string(), service(true, &["send_photo"])),
        ]);
        assert!(names(&services).is_empty());
        assert!(!service_enabled(&services, "messaging"));
        assert!(!service_enabled(&services, "missing"));
    }

    #[test]
    fn no_services_at_all_hosts_nothing() {
        assert!(names(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_null_tools_column_is_not_a_filter() {
        let services = BTreeMap::from([(
            "messaging".to_string(),
            ServiceConfig {
                enabled: true,
                tools: None,
            },
        )]);
        assert!(build_allowed_set(&services).is_empty());
        assert_eq!(names(&services), TELEGRAM_TOOL_NAMES);
    }
}

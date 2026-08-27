//! The Slack integration's in-process MCP server, ported from
//! `internal/integrations/slack/`.
//!
//! Seven tools in one service group (`messaging`), over a workspace token. The
//! fourth of the six (#315), and the first that is not shaped like the three
//! before it.
//!
//! ## Two things here that no earlier port had
//!
//! 1. **The token can come from the `auth` column.** `resolveToken`
//!    (`slack/server.go`) switches on `credentials.auth_mode`: `bot_token` reads
//!    the credentials blob, `oauth` reads `cfg.ParseOAuthToken()` — which is the
//!    **`auth` column**, parsed as an `oauth2.Token`. Every integration before
//!    this one used `auth` only as a boolean ("is it authenticated"), and
//!    `native/integrations/registry.rs` collapsed it to one in SQL precisely so
//!    the value could not exist in this process to be echoed. Slack is why
//!    `HostingRow` now carries it; see that module on what changed and what did
//!    not.
//!
//!    There is a third arm, and it is the one a port would drop: an
//!    **unrecognized** `auth_mode` falls back to `bot_token` if it is non-empty,
//!    and only then fails. So a row with `auth_mode: ""` and a bot token works.
//!
//! 2. **Nothing model-supplied reaches the URL.** The base is a constant and
//!    every method is a literal (`conversations.list`, `chat.postMessage`), so
//!    there is no dot-segment guard and no [`base_url`](super::base_url) here —
//!    the whole class of problem #312 and #317 spent their reviews on does not
//!    arise. Model input goes in a form body or a JSON body instead.
//!
//! ## The other Slack-shaped surprises
//!
//! - **`ok` decides, not the HTTP status.** `readSlackResponse` checks 429 and
//!   then ignores the status entirely, so a `500` carrying `{"ok":true}` is a
//!   success. Reproduced in `client`; it is the opposite of the 2xx-range gate
//!   every sibling uses.
//! - **Five of the seven tools return Slack's body unlabelled**; the two senders
//!   prefix it.
//! - Timeout 60s and cap 5 MiB, the largest of the six.
//!
//! ## What has to match Go, and what checks it
//!
//! Four surfaces, pinned by `desktop/parity/slack_vectors.json` — taken from the
//! **running Go server** over its real MCP transport against a fake Slack that
//! records the request each tool built: the hosted tool set, each advertised
//! schema, the request (both encodings, and the sorted keys of each), and the
//! result text of every success and every failure.
//!
//! `slack/parity.go`'s `SetAPIBase` is the seam, exactly as GitHub's is and for
//! the same reason: the base is a package variable, so a test cannot pass one.
//! Confluence and Jira needed no such thing because theirs is per row.
//!
//! `validate.go`'s `ValidateToken` and `oauth.go` are **not ported**: they answer
//! `POST /api/integrations/{id}/auth/validate` and the OAuth flow (#318), which
//! dial Slack and stay with Go. The validate route is covered all the same —
//! `native::after_forward` fires
//! [`reload_after_auth`](super::registry::reload_after_auth) on Go's 2xx for
//! every hosted type.

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
pub const SLACK_TOOL_NAMES: &[&str] = &[
    "list_channels",
    "get_channel_info",
    "read_messages",
    "send_message",
    "send_reply",
    "list_users",
    "search_messages",
];

/// Which service group registers each tool — the `push` table in
/// [`slack_tools`] as data.
///
/// It exists for `filter_config_tools` (#501), which has to answer "what are
/// *this service's* tools" for a service whose stored row names none of its
/// own. Handing that service the caller's whole request instead was the first
/// fix and it was wrong in a way that widens privilege: `build_allowed_set` is
/// a union over **every** enabled service, so a name injected by a listless
/// service satisfies the `allowed` half for a *sibling* whose own list
/// deliberately excludes it — and the sibling's service gate passes, because it
/// is enabled too. `the_service_table_matches_what_is_registered` pins this table against what
/// [`slack_tools`] really registers, so the two cannot drift.
pub const SERVICE_TOOLS: &[(&str, &[&str])] = &[("messaging", SLACK_TOOL_NAMES)];

/// `fmt.Sprintf("slack-%s", cfg.ID)` — `mcp.NewServer`'s implementation name,
/// **not** the prefix on a qualified tool name (that is the bare integration id).
pub fn server_name(integration_id: &str) -> String {
    format!("slack-{integration_id}")
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
pub fn slack_tools(services: &BTreeMap<String, ServiceConfig>, token: &str) -> Vec<ToolDef> {
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

    push("messaging", "list_channels", messaging::list_channels);
    push("messaging", "get_channel_info", messaging::get_channel_info);
    push("messaging", "read_messages", messaging::read_messages);
    push("messaging", "send_message", messaging::send_message);
    push("messaging", "send_reply", messaging::send_reply);
    push("messaging", "list_users", messaging::list_users);
    push("messaging", "search_messages", messaging::search_messages);

    tools
}

/// Starts the integration's server on a random loopback port.
pub async fn start_slack_mcp_server(
    integration_id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    token: &str,
) -> Result<InProcessMcpServer> {
    tool_server(&server_name(integration_id), slack_tools(services, token)).await
}

/// Go's `textResult`, shared by all seven tools.
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
        slack_tools(services, "xoxb-token")
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    #[test]
    fn the_server_name_is_gos() {
        assert_eq!(server_name("abc123"), "slack-abc123");
    }

    #[test]
    fn an_empty_allowed_set_hosts_every_tool() {
        let services = BTreeMap::from([("messaging".to_string(), service(true, &[]))]);
        assert_eq!(names(&services), SLACK_TOOL_NAMES);
        assert!(build_allowed_set(&services).is_empty());
    }

    #[test]
    fn one_named_tool_narrows_the_service() {
        let services = BTreeMap::from([(
            "messaging".to_string(),
            service(true, &["send_message", "list_users"]),
        )]);
        assert_eq!(names(&services), ["send_message", "list_users"]);
    }

    /// An enabled service Slack does not know still contributes its names to the
    /// union.
    #[test]
    fn an_unknown_enabled_service_still_narrows_the_known_one() {
        let services = BTreeMap::from([
            ("messaging".to_string(), service(true, &[])),
            ("other".to_string(), service(true, &["read_messages"])),
        ]);
        assert_eq!(names(&services), ["read_messages"]);
    }

    #[test]
    fn a_disabled_service_is_invisible_in_both_directions() {
        let services = BTreeMap::from([
            ("messaging".to_string(), service(false, &[])),
            ("other".to_string(), service(true, &["send_reply"])),
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
        assert_eq!(names(&services), SLACK_TOOL_NAMES);
    }
    /// `SERVICE_TOOLS` is the `push` table as data, so it must be checked
    /// against the `push` table itself rather than transcribed beside it —
    /// enabling one service at a time makes the registration function report
    /// exactly which tools that service contributes.
    ///
    /// It matters because `filter_config_tools` uses this table to bound a
    /// service that stored no list of its own (#501). A tool listed under the
    /// wrong service there would put its name into the integration-wide union
    /// for an agent that never asked for it.
    #[test]
    fn the_service_table_matches_what_is_registered() {
        for (group, tools) in SERVICE_TOOLS {
            let only = BTreeMap::from([((*group).to_string(), service(true, &[]))]);
            let want: Vec<String> = tools.iter().map(|t| (*t).to_string()).collect();
            assert_eq!(names(&only), want, "service {group:?}");
        }

        // Every service group is present, in `SERVICES` order…
        assert_eq!(
            SERVICE_TOOLS
                .iter()
                .map(|(group, _)| *group)
                .collect::<Vec<_>>(),
            SERVICES
        );
        // …and the table partitions the tool set: no tool twice, none missing.
        let flat: Vec<&str> = SERVICE_TOOLS
            .iter()
            .flat_map(|(_, tools)| tools.iter().copied())
            .collect();
        let mut unique = flat.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            flat.len(),
            "a tool is listed under two services"
        );
        assert_eq!(flat, SLACK_TOOL_NAMES);
    }
}

//! The GitHub integration's in-process MCP server, ported from
//! `internal/integrations/github/`.
//!
//! Twenty tools across five service groups, over a personal access token read
//! from the integration row. It is the largest of the six integrations and the
//! only one with no OAuth, which is why it went first (#312).
//!
//! ## What has to match Go, and what checks it
//!
//! Four surfaces, all pinned by `desktop/parity/github_vectors.json` — taken
//! from the **running Go server** over its real MCP transport, against a fake
//! GitHub that records the request each tool built:
//!
//! 1. **Which tools are hosted**, which is [`build_allowed_set`] and
//!    [`service_enabled`] below. The names are in every agent's stored
//!    `capabilities.mcp` allowlist as `mcp__github-<id>__<tool>` and in every
//!    `tool_use` block already written to `chat_messages`.
//! 2. **The advertised schema**, which comes from each input struct.
//! 3. **The request each tool builds** — path escaping, query encoding, the
//!    per-page clamp, and the exact bytes of every request body.
//! 4. **The result text**, success and failure alike: a tool's error is
//!    `CallToolResult` content with `is_error`, so the message is what the
//!    model reads and retries on.
//!
//! ## The gating rule, which reads backwards
//!
//! A service is registered when its row says `enabled`. Within an enabled
//! service, a tool is registered when the **union of every enabled service's
//! `tools` list** contains its name — *or when that union is empty*. So an
//! integration whose services are all enabled with no tool lists hosts all
//! twenty, and one that names a single tool anywhere narrows every service at
//! once. Both halves are Go's `if len(allowed) > 0 && !allowed[name] { return }`
//! and both are in the vectors, because "an empty allowlist means everything"
//! is the rule a port gets backwards.
//!
//! ## Where this stops, and what #311 adds
//!
//! `Start(ctx, cfg)` does three things before `buildMCPServer`: it refuses an
//! unauthenticated integration, parses `config.GitHubCredentials` out of the
//! row, and hands the server to `StartInProcessMCPServer`. Only the third is
//! here. The first two read the `integrations` row's `auth` and `credentials`
//! columns — which `native/integrations.rs` deliberately **never selects**, so
//! there is nothing in this shell yet to read them from. #311 owns the registry
//! (`Start`/`Stop`/`Reload` and the `PUT`/`DELETE` routes) and is where a
//! credential is first read; [`start_github_mcp_server`] takes the token it
//! will have by then.
//!
//! For the same reason `auth.go`'s `ValidatePAT` is **not ported**. It exists
//! to answer `POST /api/integrations/{id}/auth`, which is unported, and a
//! function with no caller is dead code clippy would reject. #311 — or whichever
//! issue claims that route — is where it lands.

pub mod actions;
pub mod body;
pub mod client;
pub mod issues;
pub mod pulls;
pub mod releases;
pub mod repos;

#[cfg(test)]
mod tests_vectors;

use std::collections::{BTreeMap, BTreeSet};

use rmcp::model::{CallToolResult, ContentBlock};

use crate::claude::{tool_server, InProcessMcpServer, Result, ToolDef};

use super::ServiceConfig;
use client::Client;

/// The five service groups, in the order `buildMCPServer` gates them.
///
/// Order is observable: it is `tools/list`'s order, and therefore the order the
/// model reads the tool set in.
pub const SERVICES: &[&str] = &["repos", "issues", "pull_requests", "actions", "releases"];

/// Every tool this integration can host, in registration order — which is
/// `SERVICES` order, then each `register*Tools` function's own order.
///
/// The frontend carries its own copy for the allowlist picker, as the web UI
/// does; this is the list the server actually registers.
pub const GITHUB_TOOL_NAMES: &[&str] = &[
    "list_repos",
    "get_repo",
    "search_code",
    "list_issues",
    "get_issue",
    "create_issue",
    "update_issue",
    "list_pulls",
    "get_pull",
    "create_pull",
    "get_pull_diff",
    "list_pull_comments",
    "list_workflows",
    "list_workflow_runs",
    "trigger_workflow",
    "get_workflow_run",
    "get_run_logs",
    "list_releases",
    "create_release",
    "list_tags",
];

/// `fmt.Sprintf("github-%s", cfg.ID)` — the server's name, and the half of
/// `mcp__github-<id>__<tool>` that is not the tool.
pub fn server_name(integration_id: &str) -> String {
    format!("github-{integration_id}")
}

/// `buildAllowedSet`: the union of every **enabled** service's `Tools`.
///
/// A `BTreeSet` rather than a map of bools, since the only question asked of it
/// is membership — plus emptiness, which is what makes it an allowlist or a
/// no-op.
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

/// `serviceEnabled`: present **and** enabled. An absent service is not enabled,
/// which is how an integration configured before a service existed keeps
/// working.
pub fn service_enabled(services: &BTreeMap<String, ServiceConfig>, name: &str) -> bool {
    services.get(name).is_some_and(|service| service.enabled)
}

/// Every tool `buildMCPServer` would register for `services`, over `token`.
///
/// Separate from [`start_github_mcp_server`] so the tool set can be inspected
/// without binding a port — which is what the parity assertions on the hosted
/// set do, and what #311 will want when it reports an integration's live tools.
pub fn github_tools(services: &BTreeMap<String, ServiceConfig>, token: &str) -> Vec<ToolDef> {
    let allowed = build_allowed_set(services);
    let client = Client::new(token);
    let mut tools = Vec::new();

    // `len(allowed) > 0 && !allowed[name]` — so an empty set admits everything.
    let mut push = |service: &str, name: &str, tool: fn(&Client) -> ToolDef| {
        if !service_enabled(services, service) {
            return;
        }
        if !allowed.is_empty() && !allowed.contains(name) {
            return;
        }
        tools.push(tool(&client));
    };

    push("repos", "list_repos", repos::list_repos);
    push("repos", "get_repo", repos::get_repo);
    push("repos", "search_code", repos::search_code);

    push("issues", "list_issues", issues::list_issues);
    push("issues", "get_issue", issues::get_issue);
    push("issues", "create_issue", issues::create_issue);
    push("issues", "update_issue", issues::update_issue);

    push("pull_requests", "list_pulls", pulls::list_pulls);
    push("pull_requests", "get_pull", pulls::get_pull);
    push("pull_requests", "create_pull", pulls::create_pull);
    push("pull_requests", "get_pull_diff", pulls::get_pull_diff);
    push(
        "pull_requests",
        "list_pull_comments",
        pulls::list_pull_comments,
    );

    push("actions", "list_workflows", actions::list_workflows);
    push("actions", "list_workflow_runs", actions::list_workflow_runs);
    push("actions", "trigger_workflow", actions::trigger_workflow);
    push("actions", "get_workflow_run", actions::get_workflow_run);
    push("actions", "get_run_logs", actions::get_run_logs);

    push("releases", "list_releases", releases::list_releases);
    push("releases", "create_release", releases::create_release);
    push("releases", "list_tags", releases::list_tags);

    tools
}

/// Starts the integration's server on a random loopback port — the third of
/// `Start`'s three steps, and see the module header for why it is the only one.
///
/// The listener stops when the returned handle is dropped, which is what stands
/// in for Go's `ctx`: dropping it cancels every in-flight tool call's token, and
/// each of them watches it, so an outbound GitHub request does not outlive the
/// server.
pub async fn start_github_mcp_server(
    integration_id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    token: &str,
) -> Result<InProcessMcpServer> {
    tool_server(&server_name(integration_id), github_tools(services, token)).await
}

/// Go's `textResult`, shared by all twenty tools.
///
/// One text block, no structured content — `mcp.CallToolResult{Content:
/// []mcp.Content{&mcp.TextContent{Text: text}}}` with the `any` return left
/// `nil`.
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
        github_tools(services, "token")
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    /// The server name is half of every qualified tool name in an agent's
    /// stored allowlist. Spelled out rather than derived, because a test that
    /// rebuilt it from the format string would pass through a rename.
    #[test]
    fn the_qualified_name_is_the_one_already_on_disk() {
        assert_eq!(server_name("abc123"), "github-abc123");
        assert_eq!(
            format!("mcp__{}__{}", server_name("abc123"), "list_repos"),
            "mcp__github-abc123__list_repos"
        );
    }

    /// Every service enabled with no tool list hosts everything — the half of
    /// the rule that reads backwards.
    #[test]
    fn an_empty_allowed_set_hosts_every_tool() {
        let services: BTreeMap<String, ServiceConfig> = SERVICES
            .iter()
            .map(|name| (name.to_string(), service(true, &[])))
            .collect();
        assert_eq!(names(&services), GITHUB_TOOL_NAMES);
        assert!(build_allowed_set(&services).is_empty());
    }

    /// …and one name anywhere narrows every service at once, because the set is
    /// a union rather than a per-service list.
    #[test]
    fn one_named_tool_narrows_every_enabled_service() {
        let mut services: BTreeMap<String, ServiceConfig> = SERVICES
            .iter()
            .map(|name| (name.to_string(), service(true, &[])))
            .collect();
        services.insert("repos".to_string(), service(true, &["get_repo"]));
        assert_eq!(names(&services), ["get_repo"]);

        // A tool named by one service admits it in whichever service registers
        // it — `list_issues` here is named under `repos`.
        services.insert(
            "repos".to_string(),
            service(true, &["get_repo", "list_issues"]),
        );
        assert_eq!(names(&services), ["get_repo", "list_issues"]);
    }

    /// A disabled service contributes neither its gate nor its names.
    #[test]
    fn a_disabled_service_is_invisible_in_both_directions() {
        let services = BTreeMap::from([
            ("repos".to_string(), service(true, &["list_repos"])),
            // Disabled, so `get_repo` is not allowed anywhere…
            ("issues".to_string(), service(false, &["get_repo"])),
        ]);
        assert_eq!(names(&services), ["list_repos"]);
        assert!(!service_enabled(&services, "issues"));
        // …and an unknown service name is simply not enabled.
        assert!(!service_enabled(&services, "nope"));
    }

    #[test]
    fn no_services_at_all_hosts_nothing() {
        assert!(names(&BTreeMap::new()).is_empty());
    }

    /// A `null` tools column is a nil Go slice, which contributes no names —
    /// and is therefore an *empty* allowed set, not a filter.
    #[test]
    fn a_null_tools_column_is_not_a_filter() {
        let services = BTreeMap::from([(
            "repos".to_string(),
            ServiceConfig {
                enabled: true,
                tools: None,
            },
        )]);
        assert!(build_allowed_set(&services).is_empty());
        assert_eq!(names(&services), ["list_repos", "get_repo", "search_code"]);
    }
}

//! The Jira integration's in-process MCP server, ported from
//! `internal/integrations/jira/`.
//!
//! Nine tools in one service group (`project_management`), over an Atlassian site
//! URL, account email and API token read from the integration row. The third of
//! the six (#316), after GitHub (#312) settled how to port an integration and
//! Confluence (#317) settled what a **per-row API base** costs.
//!
//! ## It is Confluence's twin, and the differences are all #277's
//!
//! Both integrations share `config.AtlassianCredentials` — the same three fields
//! out of the same column. #277 pinned that they deliberately do **not** share
//! their validation, and the port has to keep every part of that:
//!
//! | | Confluence | Jira |
//! |---|---|---|
//! | create-time validator | HTTPS only, keeps the raw value | http **or** https, trims trailing `/`, **re-marshals** |
//! | inside `Start` | `ValidateSiteURL` again | nothing at all |
//! | client timeout | 30s | 15s |
//! | failure sentence | names nothing | names the method and the path |
//!
//! The second row is what shapes this port. Because `jira.Start` validates
//! nothing, Go hosts a server and advertises all nine tools whatever the stored
//! site URL says — so a base this build cannot send a request through is answered
//! **per call** rather than by refusing to host, which would change the
//! advertised tool set. `client::Client` holds the decision; see
//! [`super::base_url`] for why the base needs checking at all, and
//! `client`'s header for why the two integrations answer differently.
//!
//! ## What has to match Go, and what checks it
//!
//! Four surfaces, all pinned by `desktop/parity/jira_vectors.json` — taken from
//! the **running Go server** over its real MCP transport, against a fake Jira
//! that records the request each tool built: which tools are hosted, each
//! advertised schema, the request each tool builds (path escaping, the
//! `maxResults` clamp, and the exact bytes of all six request bodies), and the
//! result text of every success and every failure.
//!
//! Unlike Confluence, **no Go-side seam was needed**: `jira.Start` reads the site
//! URL out of the credentials and does not validate it, so the generator points a
//! real `jira.Start` at an `httptest.Server` by putting its URL in the row. There
//! is no `internal/integrations/jira/parity.go`, and that absence is a
//! consequence of the table above rather than an oversight.
//!
//! ## The gating rule, which reads backwards
//!
//! A service is registered when its row says `enabled`. Within it, a tool is
//! registered when the **union of every enabled service's `tools` list** names it
//! — *or when that union is empty*, which is the half a port gets backwards. Jira
//! has one service, so the union half has no shape of its own here either; it is
//! still written as `buildAllowedSet`'s loop over `cfg.Services`, and an enabled
//! service Jira does not know still contributes its names to the set. Both halves
//! are in the vectors.
//!
//! ## Where `Start` stops and [`registry`](super::registry) begins
//!
//! `Start` refuses an unauthenticated integration, parses
//! `config.AtlassianCredentials`, and hands the server to
//! `StartInProcessMCPServer`. The first two read the `auth` and `credentials`
//! columns, which `native/integrations.rs` never selects, so they live in
//! `native/integrations/registry.rs` — which already had
//! `atlassian_credentials` from #317 and passes `"jira"` for the one word that
//! differs in the wrapper (`parsing jira credentials for %q`).
//!
//! `validate.go`'s `ValidateCredentials` is **not ported**: it answers
//! `POST /api/integrations/{id}/auth/validate`, which dials Atlassian and stays
//! with Go, so a port would be dead code clippy rejects. The route is covered all
//! the same — `native::after_forward` fires
//! [`reload_after_auth`](super::registry::reload_after_auth) on Go's 2xx for every
//! hosted type, and it reads the row's type through `can_host`, so adding `jira`
//! to `HOSTED_TYPES` is all that was needed.

pub mod client;
pub mod project_management;
pub mod validate;

#[cfg(test)]
mod tests_vectors;

use std::collections::{BTreeMap, BTreeSet};

use rmcp::model::{CallToolResult, ContentBlock};

use crate::claude::{tool_server, InProcessMcpServer, Result, ToolDef};

use super::ServiceConfig;
use client::Client;

/// The one service group.
pub const SERVICES: &[&str] = &["project_management"];

/// Every tool this integration can host, in registration order — `SERVICES`
/// order, then each `register*Tools` batch's own.
///
/// **Not** the order `tools/list` answers in: both SDKs sort by name. Kept as
/// Go's registration order so the two `buildMCPServer`s read alike, and pinned by
/// `an_empty_allowed_set_hosts_every_tool`.
pub const JIRA_TOOL_NAMES: &[&str] = &[
    "list_projects",
    "get_project",
    "search_issues",
    "get_issue",
    "create_issue",
    "update_issue",
    "add_comment",
    "list_transitions",
    "transition_issue",
];

/// Which service group registers each tool — the `push` table in
/// [`jira_tools`] as data.
///
/// It exists for `filter_config_tools` (#501), which has to answer "what are
/// *this service's* tools" for a service whose stored row names none of its
/// own. Handing that service the caller's whole request instead was the first
/// fix and it was wrong in a way that widens privilege: `build_allowed_set` is
/// a union over **every** enabled service, so a name injected by a listless
/// service satisfies the `allowed` half for a *sibling* whose own list
/// deliberately excludes it — and the sibling's service gate passes, because it
/// is enabled too. `the_service_table_matches_what_is_registered` pins this table against what
/// [`jira_tools`] really registers, so the two cannot drift.
pub const SERVICE_TOOLS: &[(&str, &[&str])] = &[("project_management", JIRA_TOOL_NAMES)];

/// `fmt.Sprintf("jira-%s", cfg.ID)` — `mcp.NewServer`'s implementation name.
///
/// **Not** the prefix on a qualified tool name: that is the bare integration id,
/// because `StartInProcessMCPServer(ctx, cfg.ID, …)` keys the `mcpServers` map.
/// See `registry::allowed_tool_names`.
pub fn server_name(integration_id: &str) -> String {
    format!("jira-{integration_id}")
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

/// Present **and** enabled. An absent service is not enabled, which is how an
/// integration configured before a service existed keeps working.
pub fn service_enabled(services: &BTreeMap<String, ServiceConfig>, name: &str) -> bool {
    services.get(name).is_some_and(|service| service.enabled)
}

/// Every tool `buildMCPServer` would register for `services`, over the given site
/// and credentials.
///
/// Separate from [`start_jira_mcp_server`] so the tool set can be inspected
/// without binding a port — which is what the parity assertions on the hosted set
/// do, and what makes it cheap to assert that a bad base leaves the set alone.
pub fn jira_tools(
    services: &BTreeMap<String, ServiceConfig>,
    site_url: &str,
    email: &str,
    api_token: &str,
) -> Vec<ToolDef> {
    let allowed = build_allowed_set(services);
    let client = Client::new(site_url, email, api_token);
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

    push(
        "project_management",
        "list_projects",
        project_management::list_projects,
    );
    push(
        "project_management",
        "get_project",
        project_management::get_project,
    );
    push(
        "project_management",
        "search_issues",
        project_management::search_issues,
    );
    push(
        "project_management",
        "get_issue",
        project_management::get_issue,
    );
    push(
        "project_management",
        "create_issue",
        project_management::create_issue,
    );
    push(
        "project_management",
        "update_issue",
        project_management::update_issue,
    );
    push(
        "project_management",
        "add_comment",
        project_management::add_comment,
    );
    push(
        "project_management",
        "list_transitions",
        project_management::list_transitions,
    );
    push(
        "project_management",
        "transition_issue",
        project_management::transition_issue,
    );

    tools
}

/// Starts the integration's server on a random loopback port.
///
/// The listener stops when the returned handle is dropped, which is what stands
/// in for Go's `ctx`: dropping it cancels every in-flight tool call's token, and
/// each of them watches it, so an outbound Jira request does not outlive the
/// server.
///
/// Note it cannot fail on the site URL. That is `jira.Start`'s behaviour and the
/// reason `client::Client` carries the decision instead.
pub async fn start_jira_mcp_server(
    integration_id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    site_url: &str,
    email: &str,
    api_token: &str,
) -> Result<InProcessMcpServer> {
    tool_server(
        &server_name(integration_id),
        jira_tools(services, site_url, email, api_token),
    )
    .await
}

/// Go's `textResult`, shared by all nine tools.
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

    fn names_for(services: &BTreeMap<String, ServiceConfig>, site: &str) -> Vec<String> {
        jira_tools(services, site, "e", "t")
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    fn names(services: &BTreeMap<String, ServiceConfig>) -> Vec<String> {
        names_for(services, "https://jira.atlassian.net")
    }

    #[test]
    fn the_server_name_is_gos() {
        assert_eq!(server_name("abc123"), "jira-abc123");
    }

    /// The service enabled with no tool list hosts everything — the half of the
    /// rule that reads backwards.
    #[test]
    fn an_empty_allowed_set_hosts_every_tool() {
        let services = BTreeMap::from([("project_management".to_string(), service(true, &[]))]);
        assert_eq!(names(&services), JIRA_TOOL_NAMES);
        assert!(build_allowed_set(&services).is_empty());
    }

    #[test]
    fn one_named_tool_narrows_the_service() {
        let services = BTreeMap::from([(
            "project_management".to_string(),
            service(true, &["get_issue", "add_comment"]),
        )]);
        assert_eq!(names(&services), ["get_issue", "add_comment"]);
    }

    /// An enabled service Jira does not know still contributes its names to the
    /// union — which is what makes the set a union rather than the known
    /// service's own list.
    #[test]
    fn an_unknown_enabled_service_still_narrows_the_known_one() {
        let services = BTreeMap::from([
            ("project_management".to_string(), service(true, &[])),
            ("other".to_string(), service(true, &["list_projects"])),
        ]);
        assert_eq!(names(&services), ["list_projects"]);
    }

    #[test]
    fn a_disabled_service_is_invisible_in_both_directions() {
        let services = BTreeMap::from([
            ("project_management".to_string(), service(false, &[])),
            ("other".to_string(), service(true, &["get_issue"])),
        ]);
        assert!(names(&services).is_empty());
        assert!(!service_enabled(&services, "project_management"));
        assert!(!service_enabled(&services, "missing"));
    }

    #[test]
    fn no_services_at_all_hosts_nothing() {
        assert!(names(&BTreeMap::new()).is_empty());
    }

    /// A `null` tools column is a nil Go slice, which contributes no names — and
    /// is therefore an *empty* allowed set, not a filter.
    #[test]
    fn a_null_tools_column_is_not_a_filter() {
        let services = BTreeMap::from([(
            "project_management".to_string(),
            ServiceConfig {
                enabled: true,
                tools: None,
            },
        )]);
        assert!(build_allowed_set(&services).is_empty());
        assert_eq!(names(&services), JIRA_TOOL_NAMES);
    }

    /// **A site URL this build cannot send a request through does not change the
    /// tool set.** This is the whole reason Jira answers a bad base per call
    /// instead of refusing to host: `jira.Start` validates nothing, so Go
    /// advertises all nine whatever the base says, and the advertised set is what
    /// every agent's stored `capabilities.mcp` allowlist depends on.
    ///
    /// `client::tests::a_base_this_build_cannot_send_through_refuses_every_call`
    /// asserts the other half.
    #[test]
    fn a_base_this_build_cannot_send_through_still_hosts_every_tool() {
        let services = BTreeMap::from([("project_management".to_string(), service(true, &[]))]);
        for site in [
            r"https://evil.com\@jira.atlassian.net",
            "https://jira.atlassian.net%2Eevil.com",
            "https://jira.atlassian.net/a/../b",
            "http://plaintext.example.com",
            "",
        ] {
            assert_eq!(names_for(&services, site), JIRA_TOOL_NAMES, "{site}");
        }
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
        assert_eq!(flat, JIRA_TOOL_NAMES);
    }
}

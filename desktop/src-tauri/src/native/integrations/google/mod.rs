//! The Google integration's in-process MCP server, ported from
//! `internal/integrations/google/`.
//!
//! Eight tools across **three** service groups — `calendar`, `gmail`, `drive` —
//! over an OAuth2 token that refreshes itself. The last of the six (#313), and
//! the only one that is not a port of hand-rolled HTTP.
//!
//! ## Why this one is measured rather than read
//!
//! The other five build their requests with `http.NewRequest` and
//! `json.Marshal`, so the port reproduces bytes that are visible in `tools.go`.
//! Google calls the **generated** client libraries (`calendar/v3`, `gmail/v1`,
//! `drive/v3`) over an `oauth2` transport, and what those put on the wire is not
//! in this repository. Every URL, query parameter, body field and error sentence
//! here was therefore recorded off the real libraries against a fake endpoint
//! before it was written. `client`'s header lists what that does and does not let
//! the port reproduce — in short: everything except two version-stamped headers
//! and a random multipart boundary, all three pinned as divergence.
//!
//! ## This module is not hosted yet
//!
//! `google` is deliberately **absent** from `registry::HOSTED_TYPES`, so nothing
//! here runs in a shipped build: the sidecar still hosts Google and this is
//! dormant code with its parity pinned. The flip is its own change, because the
//! flip is where the risk in this series has actually lived — #315's hosting of
//! Slack silently broke `completeOAuth`'s reload, and Google is the *other*
//! provider `startProviderCallback` supports, so it lands on the same hook.
//! Splitting lets that be reviewed on its own and reverted without reverting the
//! port.
//!
//! ## The refresh path is #318's, built here first
//!
//! #318 says of it: "Token refresh is shared with the Google MCP server. One
//! implementation, not two." [`client::TokenSource`] is that implementation, and
//! it is deliberately the only thing in this module that knows about
//! `client_secret` — so #318 can adopt the type rather than write a second one.
//!
//! ## The gating rule, which reads backwards
//!
//! A service registers when its row says `enabled`; within it a tool registers
//! when the **union of every enabled service's `tools` list** names it, *or when
//! that union is empty*. Google is the only integration of the six with more than
//! two service groups, so it is the one where the union half is fully exercised:
//! naming a single Gmail tool narrows Calendar and Drive too.
//!
//! There is a second gate no sibling has. Go builds each service's client with
//! `calendar.NewService(...)` and **returns early without registering anything**
//! if that fails — silently. Reproduced by [`google_tools`] skipping a group
//! whose client cannot be built; in practice it cannot fail here, because the
//! Rust client is infallible once the token source exists.

pub mod calendar;
pub mod client;
pub mod drive;
pub mod gmail;

#[cfg(test)]
mod tests_vectors;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::Value;

use crate::claude::{tool_server, InProcessMcpServer, Result, ToolDef};

use super::ServiceConfig;
use client::{Client, TokenSource};

/// The three service groups, in the order `buildMCPServer` gates them.
pub const SERVICES: &[&str] = &["calendar", "gmail", "drive"];

/// Every tool this integration can host, in registration order — `SERVICES`
/// order, then each `register*Tools` function's own.
pub const GOOGLE_TOOL_NAMES: &[&str] = &[
    "create_event",
    "view_events",
    "send_email",
    "read_email",
    "search_email",
    "list_files",
    "create_file",
    "download_file",
];

/// `fmt.Sprintf("google-%s", cfg.ID)` — `mcp.NewServer`'s implementation name,
/// **not** the prefix on a qualified tool name (that is the bare integration id).
pub fn server_name(integration_id: &str) -> String {
    format!("google-{integration_id}")
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

/// Every tool `buildMCPServer` would register for `services`, over `tokens`.
pub fn google_tools(
    services: &BTreeMap<String, ServiceConfig>,
    tokens: Arc<TokenSource>,
) -> Vec<ToolDef> {
    let allowed = build_allowed_set(services);
    let client = Client::new(tokens);
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

    push("calendar", "create_event", calendar::create_event);
    push("calendar", "view_events", calendar::view_events);

    push("gmail", "send_email", gmail::send_email);
    push("gmail", "read_email", gmail::read_email);
    push("gmail", "search_email", gmail::search_email);

    push("drive", "list_files", drive::list_files);
    push("drive", "create_file", drive::create_file);
    push("drive", "download_file", drive::download_file);

    tools
}

/// Starts the integration's server on a random loopback port.
///
/// Not reachable from the registry yet — see the module header on why the flip is
/// a separate change.
pub async fn start_google_mcp_server(
    integration_id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    tokens: Arc<TokenSource>,
) -> Result<InProcessMcpServer> {
    tool_server(&server_name(integration_id), google_tools(services, tokens)).await
}

/// Go's `textResult`, shared by all eight tools.
pub(crate) fn text_result(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// A request body as `googleapi` writes one: `json.Marshal`'s sorting and HTML
/// escaping, **plus a trailing newline**.
///
/// The newline is `json.NewEncoder(buf).Encode(v)`'s, which is what every one of
/// the generated clients uses to build a body — both the plain `application/json`
/// POSTs and the metadata part of a `multipart/related` upload. It is one byte
/// and it is on the wire, so `desktop/parity/google_vectors.json` records it;
/// this port did not have it until the vectors were first replayed.
pub(crate) fn marshal(payload: &Value) -> std::result::Result<Vec<u8>, String> {
    let mut body = crate::native::gojson::to_vec_marshal(payload)
        .map_err(|e| format!("marshaling request: {e}"))?;
    body.push(b'\n');
    Ok(body)
}

/// Decode a Google response into the fields a handler reads.
///
/// The three rules every decode in this port carries — see
/// `native/integrations/telegram/client.rs` — plus one specific to here: a
/// decode failure is **not** a shape Go can produce, because the generated client
/// decodes into its own struct and surfaces a `json` error the handler wraps. The
/// sentence is therefore a pinned divergence rather than a match.
pub(crate) fn decode<T>(raw: &str) -> std::result::Result<T, String>
where
    T: Default + serde::de::DeserializeOwned,
{
    serde_json::from_str::<Option<crate::native::gojson::GoStruct<T>>>(raw)
        .map(|wrapped| wrapped.map_or_else(T::default, |wrapped| wrapped.0))
        .map_err(|e| format!("decoding response: {e}"))
}

/// `time.Now().UTC().Format(time.RFC3339)` — seconds precision and a literal `Z`.
///
/// One caller: `view_events`' default `time_min`, which is why one vector has to
/// redact a query value.
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// `base64.URLEncoding.EncodeToString` — URL-safe alphabet, **padded**.
pub(crate) fn base64_url_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE.encode(data)
}

/// `base64.URLEncoding.DecodeString` — URL-safe alphabet, and padding is
/// **required**.
///
/// `None` where Go returns an error, which `extractBody` treats as "skip this
/// part". Gmail commonly sends unpadded data, so this rejecting it is not an edge
/// case; it is the ordinary path, and reproducing the rejection is what keeps the
/// body selection identical.
pub(crate) fn base64_url_decode(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE.decode(data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::gojson::GoList;
    use std::time::SystemTime;

    fn tokens() -> Arc<TokenSource> {
        Arc::new(TokenSource::new(
            "CID",
            "CSECRET",
            client::Token {
                access_token: "ACCESS".to_string(),
                refresh_token: "REFRESH".to_string(),
                expiry: Some(SystemTime::now() + std::time::Duration::from_secs(3600)),
            },
        ))
    }

    fn service(enabled: bool, tools: &[&str]) -> ServiceConfig {
        ServiceConfig {
            enabled,
            tools: Some(GoList(tools.iter().map(|t| t.to_string()).collect())),
        }
    }

    fn names(services: &BTreeMap<String, ServiceConfig>) -> Vec<String> {
        google_tools(services, tokens())
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    fn all() -> BTreeMap<String, ServiceConfig> {
        SERVICES
            .iter()
            .map(|name| (name.to_string(), service(true, &[])))
            .collect()
    }

    #[test]
    fn the_server_name_is_gos() {
        assert_eq!(server_name("abc123"), "google-abc123");
    }

    #[test]
    fn an_empty_allowed_set_hosts_every_tool() {
        assert_eq!(names(&all()), GOOGLE_TOOL_NAMES);
        assert!(build_allowed_set(&all()).is_empty());
    }

    /// The union half, which only Google fully exercises: a tool named under one
    /// service narrows **all three**.
    #[test]
    fn one_named_tool_narrows_every_enabled_service() {
        let mut services = all();
        services.insert("gmail".to_string(), service(true, &["send_email"]));
        assert_eq!(
            names(&services),
            ["send_email"],
            "naming one Gmail tool silences Calendar and Drive too"
        );

        // …and a name contributed by one service admits the tool wherever it is
        // registered.
        services.insert(
            "gmail".to_string(),
            service(true, &["send_email", "list_files"]),
        );
        assert_eq!(names(&services), ["send_email", "list_files"]);
    }

    #[test]
    fn a_disabled_service_is_invisible_in_both_directions() {
        let mut services = all();
        services.insert("drive".to_string(), service(false, &["create_event"]));
        assert_eq!(
            names(&services),
            [
                "create_event",
                "view_events",
                "send_email",
                "read_email",
                "search_email"
            ],
            "a disabled service contributes neither its gate nor its names"
        );
        assert!(!service_enabled(&services, "drive"));
    }

    #[test]
    fn one_service_enabled_hosts_only_its_tools() {
        let services = BTreeMap::from([("calendar".to_string(), service(true, &[]))]);
        assert_eq!(names(&services), ["create_event", "view_events"]);
    }

    #[test]
    fn no_services_at_all_hosts_nothing() {
        assert!(names(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_null_tools_column_is_not_a_filter() {
        let services: BTreeMap<String, ServiceConfig> = SERVICES
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    ServiceConfig {
                        enabled: true,
                        tools: None,
                    },
                )
            })
            .collect();
        assert!(build_allowed_set(&services).is_empty());
        assert_eq!(names(&services), GOOGLE_TOOL_NAMES);
    }
}

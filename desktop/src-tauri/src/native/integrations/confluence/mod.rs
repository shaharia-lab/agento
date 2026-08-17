//! The Confluence integration's in-process MCP server, ported from
//! `internal/integrations/confluence/`.
//!
//! Six tools in one service group, over an Atlassian site URL, account email and
//! API token read from the integration row. The smallest of the six integrations
//! (#317), and the second ported after GitHub (#312) — which is the file to read
//! first, since it settles *how to port an integration* and this one only adds
//! what Atlassian does differently.
//!
//! ## What has to match Go, and what checks it
//!
//! Five surfaces, all pinned by `desktop/parity/confluence_vectors.json` — taken
//! from the **running Go server** over its real MCP transport, against a fake
//! Confluence that records the request each tool built:
//!
//! 1. **Which tools are hosted**, which is [`build_allowed_set`] and
//!    [`service_enabled`] below. The names are in every agent's stored
//!    `capabilities.mcp` allowlist as `mcp__<integration id>__<tool>` and in
//!    every `tool_use` block already written to `chat_messages`.
//! 2. **The advertised schema**, which comes from each input struct.
//! 3. **The request each tool builds** — path escaping, query escaping, the
//!    limit clamps, and the exact bytes of both request bodies.
//! 4. **The result text**, success and failure alike: a tool's error is
//!    `CallToolResult` content with `is_error`, so the message is what the model
//!    reads and retries on.
//! 5. **[`validate_site_url`]**, which is the one piece of `Start` that is a
//!    *decision* rather than plumbing — it is what refuses a plaintext site URL,
//!    and it runs before any credential is used.
//!
//! ## The gating rule, which reads backwards
//!
//! A service is registered when its row says `enabled`. Within an enabled
//! service, a tool is registered when the **union of every enabled service's
//! `tools` list** contains its name — *or when that union is empty*. So an
//! integration whose one service is enabled with no tool list hosts all six, and
//! one that names a single tool narrows it. Both halves are Go's
//! `if len(allowed) == 0 || allowed[name]` and both are in the vectors, because
//! "an empty allowlist means everything" is the rule a port gets backwards.
//!
//! Confluence has exactly one service (`content`), so the *union* half of the
//! rule — one service's list narrowing another's — has no shape to take here.
//! It is still written the same way as GitHub's, because `buildAllowedSet` is
//! the same loop over `cfg.Services` in both packages and #316's Jira port has
//! two services.
//!
//! ## Where `Start` stops and [`registry`](super::registry) begins
//!
//! `Start(ctx, cfg)` does four things before `buildMCPServer`: it refuses an
//! unauthenticated integration, parses `config.AtlassianCredentials` out of the
//! row, normalises the site URL, and hands the server to
//! `StartInProcessMCPServer`. The first two read the `integrations` row's `auth`
//! and `credentials` columns, which `native/integrations.rs` deliberately never
//! selects — so they live in `native/integrations/registry.rs`, which owns the
//! credential-carrying projection. The third is [`validate_site_url`], which is
//! here because the rule is Confluence's (HTTPS-only; Jira's is deliberately
//! different — see #277 and #316).
//!
//! `validate.go`'s `ValidateCredentials` is **not ported**: it answers
//! `POST /api/integrations/{id}/auth/validate`, which dials Atlassian and stays
//! with Go, so a port would be dead code clippy rejects. The route is still
//! covered — `native::after_forward` fires
//! [`reload_after_auth`](super::registry::reload_after_auth) on Go's 2xx, for
//! every hosted type — because that handler writes `auth` and calls Go's
//! `Reload`, which now reaches nothing for a type this shell hosts.

pub mod client;
pub mod content;

#[cfg(test)]
mod tests_vectors;

use std::collections::{BTreeMap, BTreeSet};

use rmcp::model::{CallToolResult, ContentBlock};

use crate::claude::{tool_server, InProcessMcpServer, Result, ToolDef};

use super::ServiceConfig;
use client::Client;

/// The one service group. A slice rather than a constant string so it reads
/// beside `github::SERVICES` and #316's two.
pub const SERVICES: &[&str] = &["content"];

/// Every tool this integration can host, in registration order — `SERVICES`
/// order, then `registerSpaceTools`'s and `registerPageTools`'s own.
///
/// **Not** the order `tools/list` answers in: both SDKs sort by name — `rmcp`'s
/// `ToolRouter::list_all` ends in `tools.sort_by(|a, b| a.name.cmp(&b.name))`,
/// and `modelcontextprotocol/go-sdk` holds tools in a `featureSet` that lists by
/// sorted key. Registration order is kept so the two `buildMCPServer`s can be
/// read side by side, and it is pinned by `an_empty_allowed_set_hosts_every_tool`.
pub const CONFLUENCE_TOOL_NAMES: &[&str] = &[
    "list_spaces",
    "get_space",
    "search_content",
    "get_page",
    "create_page",
    "update_page",
];

/// `fmt.Sprintf("confluence-%s", cfg.ID)` — `mcp.NewServer`'s implementation
/// name.
///
/// Note this is **not** the prefix on a qualified tool name: that is the bare
/// integration id, because `StartInProcessMCPServer(ctx, cfg.ID, …)` is what
/// keys the `mcpServers` map. See `registry::allowed_tool_names`.
pub fn server_name(integration_id: &str) -> String {
    format!("confluence-{integration_id}")
}

/// The union of every **enabled** service's `Tools`.
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

/// Present **and** enabled. An absent service is not enabled, which is how an
/// integration configured before a service existed keeps working.
pub fn service_enabled(services: &BTreeMap<String, ServiceConfig>, name: &str) -> bool {
    services.get(name).is_some_and(|service| service.enabled)
}

/// Every tool `buildMCPServer` would register for `services`, over the given
/// site and credentials.
///
/// Separate from [`start_confluence_mcp_server`] so the tool set can be
/// inspected without binding a port — which is what the parity assertions on the
/// hosted set do.
pub fn confluence_tools(
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

    push("content", "list_spaces", content::list_spaces);
    push("content", "get_space", content::get_space);
    push("content", "search_content", content::search_content);
    push("content", "get_page", content::get_page);
    push("content", "create_page", content::create_page);
    push("content", "update_page", content::update_page);

    tools
}

/// Starts the integration's server on a random loopback port.
///
/// The listener stops when the returned handle is dropped, which is what stands
/// in for Go's `ctx`: dropping it cancels every in-flight tool call's token, and
/// each of them watches it, so an outbound Confluence request does not outlive
/// the server.
pub async fn start_confluence_mcp_server(
    integration_id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    site_url: &str,
    email: &str,
    api_token: &str,
) -> Result<InProcessMcpServer> {
    tool_server(
        &server_name(integration_id),
        confluence_tools(services, site_url, email, api_token),
    )
    .await
}

/// `ValidateSiteURL`: HTTPS, a hostname, and no trailing slash.
///
/// The **only** validation `Start` performs beyond the credential parse, and the
/// one that matters: it is what stops an `http://` site URL carrying the user's
/// API token in a `Basic` header over plaintext. #277 pinned that Confluence's
/// rule is *not* Jira's — Jira trims trailing slashes and re-marshals but does
/// not require HTTPS — so the two must not be merged into a shared helper.
///
/// # Reproducing `net/url` without `net/url`
///
/// Go parses the raw string with `url.Parse` and then asks two questions of the
/// result: is the scheme `https`, and is the host non-empty. `url::Url::parse`
/// cannot stand in for that, because it *refuses* strings `url.Parse` accepts
/// (a bare `foo`, `https:no-authority`) and so would answer "invalid" where Go
/// answers "not HTTPS" — a different rejection reason for the same input, and
/// one that would be a silently different log line. The two questions are
/// therefore answered directly off the raw string, mirroring `net/url`'s own
/// `getScheme` and its authority split:
///
/// - A scheme is the run of `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` before
///   the first `:`, lower-cased. Anything else — a leading digit, an invalid
///   byte, no colon at all — means *no scheme*, which is an empty one rather
///   than an error, and reaches the same "must use HTTPS" refusal.
/// - A host exists only when the scheme is followed by `//`, and is what remains
///   of the authority after any `userinfo@`. `https:` and `https://` and
///   `https://user@` all have none.
///
/// The returned value is `strings.TrimRight(rawURL, "/")` of the **raw** string,
/// not of a re-rendered URL — so nothing is normalised, and
/// `client::Client::absolute` is what catches a site URL this port cannot build
/// Go's request from.
///
/// # The divergence, and why it is only a sentence
///
/// `url.Parse` fails outright on a few inputs — an ASCII control character, a
/// scheme-less URL whose first path segment holds a colon, a non-numeric port, a
/// bad `%` escape in the host — and answers `invalid site URL: parse "…": …`
/// with `net/url`'s own vocabulary. That wording is not reproducible: it is
/// `%q`-quoted Go string escaping over the caller's input, which agrees with
/// Rust's `{:?}` for printable ASCII and then stops agreeing (`\x01` against
/// `\u{1}`). It is also a **log line rather than an interface** — `Start`'s
/// error is logged by the registry and never reaches a response or the model —
/// which is the same trade `registry::github_token` makes on the credential
/// blob, for the same reason and with the extra motive that Go's message quotes
/// the caller's input back.
///
/// So the **classification** is reproduced and the sentence is not: the two
/// failures reachable from a stored site URL are refused here too, each under
/// this port's own `invalid site URL: …` wording, and
/// `confluence_vectors.json` pins both as divergences rather than hiding them.
/// The remaining `url.Parse` failures (a non-numeric port, a bad `%` escape in
/// the host) are *accepted* here and refused at the first request instead, by
/// [`client::Client::absolute`] — a refusal either way, and one the vectors do
/// not need to enumerate because it never reaches a different endpoint.
pub fn validate_site_url(raw: &str) -> std::result::Result<String, String> {
    // `url.Parse`'s first act — `stringContainsCTLByte`, which is `< 0x20 ||
    // == 0x7f`, exactly `is_ascii_control`. Reproduced because letting a control
    // character through would hand `reqwest` a string Go never sends.
    if raw.chars().any(|c| c.is_ascii_control()) {
        return Err("invalid site URL: it holds a control character".to_string());
    }

    let (scheme, rest) = go_scheme(raw);
    // A URL with no scheme is a *relative* one to `net/url`, and it refuses one
    // whose first path segment holds a colon — because re-parsing the result
    // would read that colon as a scheme. `1https://acme.atlassian.net` is the
    // shape a person actually types, and without this it would fall through to
    // the "must use HTTPS" refusal, which names the wrong problem.
    if scheme.is_empty()
        && !raw.starts_with("//")
        && raw
            .split('/')
            .next()
            .is_some_and(|first| first.contains(':'))
    {
        return Err("invalid site URL: its first path segment holds a colon".to_string());
    }
    if scheme != "https" {
        // `%q` over a scheme is plain quoting — the charset `go_scheme` admits
        // has nothing either language escapes — so `{:?}` is Go's rendering.
        return Err(format!("site URL must use HTTPS (got {scheme:?})"));
    }
    if go_host(rest).is_empty() {
        return Err("site URL must include a hostname".to_string());
    }
    Ok(raw.trim_end_matches('/').to_string())
}

/// `net/url`'s `getScheme`, lower-cased as `url.Parse` lower-cases it.
///
/// Answers `("", raw)` for anything without a valid scheme, which is what
/// `getScheme` does for every case except a leading `:` — and that one is a
/// parse error in Go, so it reaches the same refusal by a different route.
fn go_scheme(raw: &str) -> (String, &str) {
    for (i, c) in raw.char_indices() {
        match c {
            'a'..='z' | 'A'..='Z' => {}
            '0'..='9' | '+' | '-' | '.' if i > 0 => {}
            ':' if i > 0 => return (raw[..i].to_ascii_lowercase(), &raw[i + 1..]),
            _ => return (String::new(), raw),
        }
    }
    (String::new(), raw)
}

/// The host `url.Parse` would set, given everything after the scheme's colon.
///
/// Empty unless an authority is present (`//…`), and empty for an authority that
/// is only userinfo — both of which Go answers with `u.Host == ""`.
fn go_host(rest: &str) -> &str {
    let Some(authority) = rest.strip_prefix("//") else {
        return "";
    };
    let authority = match authority.find(['/', '?', '#']) {
        Some(end) => &authority[..end],
        None => authority,
    };
    match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    }
}

/// Go's `textResult`, shared by all six tools.
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
        confluence_tools(services, "https://acme.atlassian.net", "e", "t")
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    /// The implementation name, spelled out rather than derived — a test that
    /// rebuilt it from the format string would pass through a rename.
    #[test]
    fn the_server_name_is_gos() {
        assert_eq!(server_name("abc123"), "confluence-abc123");
    }

    /// The service enabled with no tool list hosts everything — the half of the
    /// rule that reads backwards.
    #[test]
    fn an_empty_allowed_set_hosts_every_tool() {
        let services = BTreeMap::from([("content".to_string(), service(true, &[]))]);
        assert_eq!(names(&services), CONFLUENCE_TOOL_NAMES);
        assert!(build_allowed_set(&services).is_empty());
    }

    /// …and one name narrows it.
    #[test]
    fn one_named_tool_narrows_the_service() {
        let services = BTreeMap::from([(
            "content".to_string(),
            service(true, &["get_page", "list_spaces"]),
        )]);
        assert_eq!(names(&services), ["list_spaces", "get_page"]);
    }

    /// A disabled service contributes neither its gate nor its names — and a
    /// second, unknown service is simply not enabled.
    #[test]
    fn a_disabled_service_is_invisible_in_both_directions() {
        let services = BTreeMap::from([
            ("content".to_string(), service(false, &["list_spaces"])),
            ("nope".to_string(), service(true, &["get_page"])),
        ]);
        assert!(names(&services).is_empty());
        assert!(!service_enabled(&services, "content"));
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
            "content".to_string(),
            ServiceConfig {
                enabled: true,
                tools: None,
            },
        )]);
        assert!(build_allowed_set(&services).is_empty());
        assert_eq!(names(&services), CONFLUENCE_TOOL_NAMES);
    }

    /// The scheme split, at the shapes `getScheme` distinguishes. Every one of
    /// these is also a `validate_site_url` vector; this is the unit view of why.
    #[test]
    fn the_scheme_is_read_the_way_net_url_reads_it() {
        for (raw, scheme) in [
            ("https://acme.atlassian.net", "https"),
            // `url.Parse` lower-cases the scheme, so this passes the gate.
            ("HTTPS://acme.atlassian.net", "https"),
            ("http://acme.atlassian.net", "http"),
            ("ftp://acme", "ftp"),
            // A scheme may hold digits, `+`, `-` and `.` after the first byte.
            ("h2.c+x-y://acme", "h2.c+x-y"),
            // No colon at all, a leading digit, and an invalid byte all mean
            // *no scheme* rather than an error.
            ("acme.atlassian.net", ""),
            ("1https://acme", ""),
            ("ht tps://acme", ""),
            ("/wiki", ""),
        ] {
            assert_eq!(go_scheme(raw).0, scheme, "{raw}");
        }
    }

    /// The host, at the shapes that make it empty — which is the second refusal.
    #[test]
    fn a_host_needs_an_authority_and_more_than_userinfo() {
        assert_eq!(go_host("//acme.atlassian.net/wiki"), "acme.atlassian.net");
        assert_eq!(go_host("//user:pw@acme:8443?x"), "acme:8443");
        assert_eq!(go_host("//acme#frag"), "acme");
        for rest in ["", "//", "//user@", "opaque", "/path"] {
            assert!(go_host(rest).is_empty(), "{rest}");
        }
    }
}

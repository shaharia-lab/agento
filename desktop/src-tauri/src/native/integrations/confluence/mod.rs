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
/// **The second of two places that reason about `net/url`'s rules.** The first
/// is `native/integration_credentials.rs`'s `split_url`, and the difference is
/// which question is being asked: there it is *create* — may this row be stored
/// — and forwarding the whole request to Go is always available, so it decides
/// only the shapes it is sure of. Here it is *start* — may a stored row be
/// hosted — and there is nobody to forward to, so this reproduces `getScheme`
/// and the authority split outright and answers every input. #316 adds a third
/// caller of the same Go rules with a different answer again, since Jira does
/// not require HTTPS.
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
/// not of a re-rendered URL — Go concatenates that string with every tool's
/// path, so anything else would be a different request.
///
/// # The base has to be one this port can send where Go sends it
///
/// Everything above is Go's. The checks that follow are not, and they are what
/// [`client::Client::absolute`] rests on. That guard compares two `url`-parsed
/// targets, so it is blind by construction to any place `url` and `net/url`
/// disagree about the **base** — and the two do disagree, in one way that is a
/// wrong path and one that is a different *host*:
///
/// - **Authority.** `url` treats `\` as an authority separator for a special
///   scheme; `net/url` does not, and rejects the userinfo that results. So
///   `https://evil.com\@acme.atlassian.net` is a parse error to Go — nothing is
///   hosted — while `url` reads the host as `evil.com` and the rest as a path.
///   Both sides of `absolute`'s comparison would then say `evil.com`, and every
///   tool would send the user's `Basic` credentials to it. So the host `url`
///   resolved is compared against the one [`go_host`] resolved, through `url`
///   on both sides so that case-folding, IDN and the default port cannot make a
///   false difference.
///   The comparison has teeth only where the two parsers **split** the
///   authority differently, so it is paired with a flat refusal of a `%` in the
///   raw authority — `net/url`'s `parseHost` decodes an escape and then rejects
///   one that decoded to a byte it would have escaped (`%2E` is an error,
///   `%C3%A9` is not), while `url` decodes them all. That gap is the same
///   attack by another route: `https://acme.atlassian.net%2Eevil.com` is a
///   parse error to Go and `acme.atlassian.net.evil.com` here. `split_url` in
///   `native/integration_credentials.rs` forwards on the same byte for the same
///   reason; there is nobody to forward to here, so it is a refusal.
/// - **Path encoding.** Go escapes `\ ^ |` in a path and `url` does not, and
///   `url` removes dot segments where `net/url` never does — so `/a\b` leaves
///   here as `/a/b` against Go's `/a%5Cb`, and `/a/../b` as `/b`. So `url`'s
///   rendering of the base path is compared against **`EscapedPath()`**, not
///   against `escape(Path, encodePath)`: the two are different, and the
///   difference is a whole class of ordinary characters. See
///   [`crate::native::gourl::valid_encoded_path`] — `/a!b` and `/a%2Fb` are both
///   sent verbatim by Go even though `escape` would rewrite them, so comparing
///   against `escape` alone would refuse a base that works.
///
/// Two more shapes have no sound comparison at all and are refused with them: a
/// base `url` cannot parse (a non-numeric or out-of-range port), and a base
/// carrying its own `?` or `#`, after which the concatenation is not a prefix of
/// the target.
///
/// Note what is deliberately **not** required: that the base is already
/// percent-encoded. `https://intranet.example.com/my atlassian` is a site URL Go
/// accepts and sends as `/my%20atlassian/…` through `EscapedPath`, and `url`
/// encodes it identically, so it is admitted.
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
/// So the **classification** is reproduced and the sentence is not: each refusal
/// carries this port's own `invalid site URL: …` wording, and
/// `confluence_vectors.json` pins every one as a `rust_error` divergence rather
/// than hiding it — including the three above, which are refusals Go does not
/// always make.
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
    //
    // The fragment and the query are cut first, because `net/url` cuts them
    // first: `Parse` splits on `#` and `parse` splits on `?`, both *before* the
    // check, so `acme?x:y` is a colon in the query and not in a path segment.
    // The prefix test is `/`, not `//`, for the same reason it is in `parse` —
    // a rooted path is not a relative reference.
    let head = raw.split(['#', '?']).next().unwrap_or("");
    if scheme.is_empty()
        && !head.starts_with('/')
        && head
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

    let clean = raw.trim_end_matches('/');
    // The shapes `client::Client::absolute` cannot work behind. See the section
    // above for why each is refused here rather than per call.
    let parsed = reqwest::Url::parse(clean)
        .map_err(|_| "invalid site URL: it is not a URL this build can send a request to")?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("invalid site URL: a query or fragment leaves the path open".to_string());
    }

    // The host, which is the one disagreement that would send a credential
    // somewhere else. Both sides go through `url` so that a lower-cased scheme,
    // an IDN host or an explicit `:443` is not a false difference — what is
    // being compared is *which host each parser found*, not how it spells one.
    let authority = go_host(rest);
    // The comparison below can only see a *split* the two parsers disagree on.
    // Where they read the same substring and interpret it differently it is a
    // tautology — and a `%` in a host is exactly that case, since `parseHost`
    // decodes an escape and then rejects one that decoded to a byte it would
    // have escaped, while `url` decodes them all. `%2E` is the reachable one.
    if authority.contains('%') {
        return Err("invalid site URL: its host holds a percent escape".to_string());
    }
    let go_authority = reqwest::Url::parse(&format!("https://{authority}"))
        .map_err(|_| "invalid site URL: its host is not one this build can send to")?;
    if go_authority.host() != parsed.host()
        || go_authority.port_or_known_default() != parsed.port_or_known_default()
    {
        return Err("invalid site URL: net/url and url read a different host".to_string());
    }

    // …and the path, which is a wrong endpoint on the right host.
    let raw_path = base_path_of(clean);
    let go_path = if raw_path.is_empty() {
        // `url` renders a base with no path of its own as `/`; Go sends the
        // tool's own leading slash and nothing before it.
        "/".to_string()
    } else {
        // `setPath` decodes first and a bad escape is a parse error there, so
        // this runs whichever branch of `EscapedPath` wins below.
        let decoded = crate::native::gourl::unescape_path(raw_path)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or("invalid site URL: its path does not decode")?;
        // `EscapedPath()`: the raw text when it is validly encoded, and the
        // re-escaped decode otherwise. Both branches are needed — see
        // `gourl::valid_encoded_path`.
        if crate::native::gourl::valid_encoded_path(raw_path) {
            raw_path.to_string()
        } else {
            crate::native::gourl::escape_path(&decoded)
        }
    };
    if parsed.path() != go_path {
        return Err("invalid site URL: net/url and url encode its path differently".to_string());
    }

    Ok(clean.to_string())
}

/// The raw text of a site URL's path — everything from the `/` that ends the
/// authority.
///
/// Raw rather than parsed, because the dot-segment check above exists precisely
/// because parsing removes them.
pub(super) fn base_path_of(clean: &str) -> &str {
    let Some(rest) = clean.split_once("://").map(|(_, rest)| rest) else {
        return "";
    };
    match rest.find('/') {
        Some(start) => &rest[start..],
        None => "",
    }
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

    /// The refusal that matters most: a base where `url` and `net/url` read a
    /// **different host**, which is how the user's `Basic` credentials would
    /// leave for somebody else's server.
    ///
    /// `url` treats `\` as an authority separator for a special scheme, so it
    /// reads `evil.com` as the host and `@acme.atlassian.net` as a path;
    /// `net/url` does not, and rejects the userinfo that leaves, so Go hosts
    /// nothing at all. A guard that compared two url-parsed values would agree
    /// with itself and send the request. Pinned as a vector too, but asserted
    /// here as well because it is the one failure with a blast radius.
    #[test]
    fn a_base_the_two_parsers_read_as_different_hosts_is_refused() {
        for site in [
            r"https://evil.com\@acme.atlassian.net",
            r"https://evil.com\@acme.atlassian.net/wiki",
        ] {
            assert_eq!(
                validate_site_url(site),
                Err("invalid site URL: net/url and url read a different host".to_string()),
                "{site}"
            );
        }

        // The same graft by the other route: `parseHost` rejects a `%` escape
        // that decodes to a byte it would have escaped, and `url` decodes them
        // all — so this reads as the legitimate host and dials a stranger's.
        for site in [
            "https://acme.atlassian.net%2Eevil.com",
            "https://acme.atlassian.net%41x.com",
            // Go accepts this one (a percent-encoded IDN host); refusing it is
            // the cost of not being able to tell the two apart without
            // reproducing `parseHost`, and it is a logged non-start.
            "https://caf%C3%A9.example.com",
        ] {
            assert_eq!(
                validate_site_url(site),
                Err("invalid site URL: its host holds a percent escape".to_string()),
                "{site}"
            );
        }

        // …and the legitimate userinfo it must not be confused with.
        assert_eq!(
            validate_site_url("https://user:pw@acme.atlassian.net"),
            Ok("https://user:pw@acme.atlassian.net".to_string())
        );
    }

    /// The path half of the same disagreement, plus what it must still admit.
    ///
    /// Go escapes `\ ^ | [ ]` in a path and `url` does not; `url` removes dot
    /// segments and `net/url` never does. But an *unencoded* path is fine —
    /// both render `/my atlassian` as `/my%20atlassian` — which is the whole
    /// reason the comparison is against Go's escaped form rather than the raw
    /// text.
    #[test]
    fn a_base_path_the_two_parsers_encode_differently_is_refused() {
        for site in [
            r"https://acme.atlassian.net/a\b",
            "https://acme.atlassian.net/a^b",
            "https://acme.atlassian.net/a|b",
            "https://acme.atlassian.net/a/../b",
            "https://acme.atlassian.net/./a",
        ] {
            assert_eq!(
                validate_site_url(site),
                Err("invalid site URL: net/url and url encode its path differently".to_string()),
                "{site}"
            );
        }

        for site in [
            "https://acme.atlassian.net",
            "https://acme.atlassian.net:8443",
            "https://intranet.example.com/atlassian",
            "https://intranet.example.com/my atlassian",
            "https://intranet.example.com/a%20b",
            "https://intranet.example.com/café",
            "https://intranet.example.com/a+b:c@d",
            // `EscapedPath` sends all of these verbatim even though `escape`
            // would rewrite them, and `url` agrees — so comparing against
            // `escape` alone would have refused a base that works.
            "https://intranet.example.com/a!b",
            "https://intranet.example.com/a(b)c",
            "https://intranet.example.com/a[b]",
            "https://intranet.example.com/a'b*c",
            "https://intranet.example.com/a%2Fb",
        ] {
            assert_eq!(
                validate_site_url(site),
                Ok(site.to_string()),
                "{site} is a site URL Go serves"
            );
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

//! The Rust half of `desktop/parity/confluence_vectors.json`.
//!
//! The Go half (`desktop/parity/confluence_parity_test.go`) stands the **real**
//! integration up through `confluence.StartAtSiteURL` — `Start` with only its
//! HTTPS check removed — against an `httptest.Server` that plays a scripted
//! Confluence and records what it was asked, and writes down five things: what
//! `ValidateSiteURL` answers per input, the tool set hosted, the advertised
//! schema, the request each tool built, and the result the model would read.
//!
//! This half replays the same script through the same three layers — an axum
//! server on loopback standing in for `httptest`, the real
//! [`start_confluence_mcp_server`](super::start_confluence_mcp_server), and a
//! real `tools/call` over the MCP HTTP transport — and asserts all five. Nothing
//! is stubbed: a change to the client, to a path, to a body or to a sentence
//! fails here.
//!
//! Three fields carry a *pinned divergence* rather than a match, each documented
//! at its use below and in the Go generator:
//!
//! - `rust_error` — the site-URL gate. Two kinds: `net/url`'s parse-failure
//!   vocabulary (`%q`-quoted Go string escaping over the caller's own input,
//!   reaching a log line rather than the model — the refusal is reproduced, the
//!   sentence is not), and three bases Go accepts that this port refuses to host
//!   because `client::Client::absolute` cannot work behind them.
//! - `rust_text` — the dot-segment refusal, where this port answers the sentence
//!   a transport failure produces rather than calling a different endpoint; and
//!   a zero-fraction float for an integer field, which the Go SDK re-marshals
//!   into an integer and `serde_json` refuses.
//! - `rust_no_request` — the same two cases seen from the network: Go reached
//!   the fake and this port never built the request.
//!
//! Note there is **no `rust_target`**. #312 needed one because the Go MCP SDK
//! rounds an integer argument above 2^53 before the handler sees it; the two
//! integers here (`limit`, `version`) have no reachable value that large and no
//! vector exercises one, so the field would have no user.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rmcp::ServerHandler;
use serde::Deserialize;
use serde_json::Value;

use super::{
    confluence_tools, server_name, start_confluence_mcp_server, validate_site_url,
    CONFLUENCE_TOOL_NAMES, SERVICES,
};
use crate::claude::ToolServer;
use crate::native::gojson::GoList;
use crate::native::integrations::ServiceConfig;

// ─── The vectors ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SiteUrlVector {
    case: String,
    input: String,
    /// The cleaned value, on the inputs Go accepts.
    #[serde(default)]
    clean: String,
    /// Go's refusal, on the inputs it rejects.
    #[serde(default)]
    error: String,
    /// This port's refusal, where the two disagree on the sentence but not on
    /// the outcome. See the module header.
    #[serde(default)]
    rust_error: String,
}

#[derive(Deserialize)]
struct ToolVector {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Deserialize)]
struct HostingVector {
    case: String,
    services: BTreeMap<String, GoServiceConfig>,
    tools: Vec<String>,
}

/// `config.ServiceConfig` as the Go generator marshals it. Converted rather than
/// deserialized straight into [`ServiceConfig`], because that type's `tools` is a
/// [`GoList`] whose job is nil-versus-empty fidelity on the *write path* — here
/// the vector is plain JSON and a nil list is simply `null`.
#[derive(Deserialize)]
struct GoServiceConfig {
    enabled: bool,
    tools: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RequestVector {
    method: String,
    target: String,
    authorization: String,
    accept: String,
    content_type: String,
    body: String,
}

#[derive(Deserialize)]
struct ResponseScript {
    status: u16,
    body: String,
}

#[derive(Deserialize)]
struct CallVector {
    case: String,
    tool: String,
    /// The path appended to the fake's URL to make this case's site URL, empty
    /// for the fake's root. A site URL is per row, so a case that exercises a
    /// base path needs its own server.
    #[serde(default)]
    base_path: String,
    arguments: Value,
    response: ResponseScript,
    request: Option<RequestVector>,
    is_error: bool,
    text: String,
    #[serde(default)]
    rust_text: String,
    #[serde(default)]
    rust_no_request: bool,
}

#[derive(Deserialize)]
struct Vectors {
    integration_id: String,
    server_name: String,
    email: String,
    api_token: String,
    site_urls: Vec<SiteUrlVector>,
    tools: Vec<ToolVector>,
    hosting: Vec<HostingVector>,
    calls: Vec<CallVector>,
}

fn vectors() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../parity/confluence_vectors.json"
    );
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
    serde_json::from_str(&raw).expect("parsing the confluence vectors")
}

fn services(from: &BTreeMap<String, GoServiceConfig>) -> BTreeMap<String, ServiceConfig> {
    from.iter()
        .map(|(name, service)| {
            (
                name.clone(),
                ServiceConfig {
                    enabled: service.enabled,
                    tools: service.tools.clone().map(GoList),
                },
            )
        })
        .collect()
}

/// The one service enabled with no tool list — the configuration the six schemas
/// and every call vector were taken under.
fn all_services() -> BTreeMap<String, ServiceConfig> {
    SERVICES
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
        .collect()
}

// ─── The site-URL gate ───────────────────────────────────────────────────────

/// `ValidateSiteURL`, per input: what it accepts, what it returns, and what it
/// refuses.
///
/// This is the check that keeps the user's API token off a plaintext connection,
/// so the interesting assertion is the *accept* set — a port that admitted
/// `http://` would still pass every other test in this file.
/// `rust_error` is the whole answer wherever it is set, and it is set in two
/// different situations — which is why it wins over both of the other fields
/// rather than only over `error`:
///
/// - Go refused and this port refuses under different wording (`net/url`'s parse
///   vocabulary), and
/// - Go **accepted** and this port refuses, for the three bases
///   `client::Client::absolute` cannot work behind. Those carry a `clean` value
///   from Go and must still fail here.
#[test]
fn the_site_url_gate_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.site_urls.len() >= 15, "vectors look partial");
    for case in &v.site_urls {
        let got = validate_site_url(&case.input);
        if !case.rust_error.is_empty() {
            assert_eq!(
                got,
                Err(case.rust_error.clone()),
                "{}: a pinned divergence",
                case.case
            );
        } else if case.error.is_empty() {
            assert_eq!(
                got,
                Ok(case.clean.clone()),
                "{}: Go accepted this and returned {:?}",
                case.case,
                case.clean
            );
        } else {
            assert_eq!(got, Err(case.error.clone()), "{}: refusal", case.case);
        }
    }
}

// ─── The advertised surface ──────────────────────────────────────────────────

/// The server name, every tool name, its description and its reflected input
/// schema — all taken from a live `tools/list` against the Go server.
///
/// The schema is compared as a `Value` rather than as bytes, for #310's reason:
/// JSON object order is meaningful to neither the CLI nor `schemars`, while a
/// key present on one side and not the other is exactly what value equality
/// catches. `$schema`, `format` and `default` are three such keys — see
/// `crate::claude::tool`'s `normalize_go_schema`.
#[test]
fn the_advertised_surface_matches_the_go_vectors() {
    let v = vectors();
    assert_eq!(v.server_name, server_name(&v.integration_id));
    assert_eq!(
        v.tools.len(),
        CONFLUENCE_TOOL_NAMES.len(),
        "vectors look partial"
    );

    let server = ToolServer::new(&v.server_name).with_tools(confluence_tools(
        &all_services(),
        "https://acme.atlassian.net",
        &v.email,
        &v.api_token,
    ));
    assert_eq!(
        server.tool_names(),
        v.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        // Both sides are already name-sorted — `tool_names` goes through
        // `ToolRouter::list_all`, which sorts, and the Go SDK's `featureSet`
        // lists by sorted key — so this is set equality in a stable order, not
        // an order assertion. *Registration* order is pinned by
        // `super::tests::an_empty_allowed_set_hosts_every_tool` against
        // `CONFLUENCE_TOOL_NAMES`.
        "the hosted tool set, as tools/list answers it"
    );

    for want in &v.tools {
        let tool = server
            .get_tool(&want.name)
            .unwrap_or_else(|| panic!("{} is not registered", want.name));
        assert_eq!(
            tool.description.as_deref(),
            Some(want.description.as_str()),
            "{}'s description is what the model reads",
            want.name
        );
        assert_eq!(
            serde_json::to_value(&*tool.input_schema).expect("schema"),
            want.input_schema,
            "{}'s input schema is what the model is steered by",
            want.name
        );
    }
}

/// The gating rule, in every shape the Go generator recorded — including the one
/// that reads backwards, where an empty allowed set hosts everything.
#[test]
fn the_hosted_tool_set_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.hosting.len() >= 5, "vectors look partial");
    for case in &v.hosting {
        let mut hosted: Vec<String> = confluence_tools(
            &services(&case.services),
            "https://acme.atlassian.net",
            &v.email,
            &v.api_token,
        )
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
        // The Go vector sorts, because what it records is the *set*; the order
        // is asserted by the six-tool block above.
        hosted.sort();
        assert_eq!(hosted, case.tools, "hosting case: {}", case.case);
    }
}

// ─── The fake Confluence ─────────────────────────────────────────────────────

/// What the fake was asked, in the shape the Go generator recorded it.
struct Recorded {
    method: String,
    target: String,
    authorization: String,
    accept: String,
    content_type: String,
    body: String,
}

#[derive(Default)]
struct Fake {
    status: u16,
    body: String,
    seen: Option<Recorded>,
}

#[derive(Clone, Default)]
struct FakeState(Arc<Mutex<Fake>>);

impl FakeState {
    fn arm(&self, script: &ResponseScript) {
        let mut fake = self.0.lock().expect("fake lock");
        fake.status = script.status;
        fake.body.clone_from(&script.body);
        fake.seen = None;
    }
}

/// One handler for every path, exactly as the Go fake does it: the point is to
/// capture the request the tool *built*, and a router would have to agree with
/// this port about each path's shape — which is the thing under test.
async fn serve_fake(
    State(state): State<FakeState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };

    let (status, reply) = {
        let mut fake = state.0.lock().expect("fake lock");
        fake.seen = Some(Recorded {
            method: method.to_string(),
            // `URL.RequestURI()`: the encoded path plus the encoded query.
            target: uri
                .path_and_query()
                .map(|pq| pq.as_str().to_string())
                .unwrap_or_default(),
            authorization: header("authorization"),
            accept: header("accept"),
            content_type: header("content-type"),
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        (fake.status, fake.body.clone())
    };

    (
        StatusCode::from_u16(status).expect("a scripted status"),
        reply,
    )
        .into_response()
}

// ─── The calls ───────────────────────────────────────────────────────────────

/// Every call vector, driven end to end.
///
/// One test rather than one per case, because one server and one fake serve them
/// all — and, unlike #312's, nothing here is process-wide: the site URL is a
/// field on the client, so this can run in parallel with anything.
#[tokio::test]
async fn every_call_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.calls.len() >= 18, "vectors look partial");

    let state = FakeState::default();
    let app = axum::Router::new()
        .fallback(serve_fake)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // One server per distinct site URL, as the Go generator opens one session
    // per distinct base path. The site URL is a field on the client, not a
    // per-call argument, so a base-path case cannot share the plain server.
    let mut servers: BTreeMap<String, crate::claude::InProcessMcpServer> = BTreeMap::new();
    for case in &v.calls {
        if servers.contains_key(&case.base_path) {
            continue;
        }
        let server = start_confluence_mcp_server(
            &v.integration_id,
            &all_services(),
            &format!("{base}{}", case.base_path),
            &v.email,
            &v.api_token,
        )
        .await
        .expect("start the confluence server");
        servers.insert(case.base_path.clone(), server);
    }

    for case in &v.calls {
        let server = &servers[&case.base_path];
        state.arm(&case.response);
        let result = call_tool(server, &case.tool, &case.arguments).await;

        // The credentials must never reach the model, on either path.
        for secret in [&v.api_token, &v.email] {
            assert!(
                !result.text.contains(secret),
                "{}: a credential leaked into a tool result: {}",
                case.case,
                result.text
            );
        }

        // Every text this port substitutes is a *failure* this port produces
        // where Go produced something else, so `rust_text` carries `is_error`
        // with it. That is not always Go's own flag: the zero-fraction-float
        // cases script a 200, because what they pin is that Go went ahead —
        // performing `update_page`'s write with the bumped version — while the
        // decode here fails before any handler runs.
        let (want_text, want_error) = if case.rust_text.is_empty() {
            (&case.text, case.is_error)
        } else {
            (&case.rust_text, true)
        };
        assert_eq!(result.text, *want_text, "{}: result text", case.case);
        assert_eq!(result.is_error, want_error, "{}: is_error", case.case);

        let seen = state.0.lock().expect("fake lock").seen.take();
        let want_request = if case.rust_no_request {
            // The dot-segment refusal: Go reached the fake, this port never
            // built the request. Asserted as "and nothing was sent" rather than
            // skipped, because sending *something* — the resolved path — is the
            // outcome `client::Client::absolute` exists to prevent.
            assert!(
                seen.is_none(),
                "{}: the refusal still reached the network: {:?}",
                case.case,
                seen.as_ref().map(|r| &r.target)
            );
            None
        } else {
            case.request.as_ref()
        };
        match (want_request, seen) {
            (None, seen) => assert!(
                seen.is_none(),
                "{}: Go made no request and this one did",
                case.case
            ),
            (Some(_), None) => panic!("{}: Go made a request and this one did not", case.case),
            (Some(want), Some(got)) => {
                assert_eq!(got.method, want.method, "{}: method", case.case);
                assert_eq!(got.target, want.target, "{}: request target", case.case);
                assert_eq!(
                    got.authorization, want.authorization,
                    "{}: the Basic prefix and the base64 of email:token",
                    case.case
                );
                assert_eq!(got.accept, want.accept, "{}: Accept", case.case);
                assert_eq!(
                    got.content_type, want.content_type,
                    "{}: Content-Type is sent only when there is a body",
                    case.case
                );
                assert_eq!(got.body, want.body, "{}: request body", case.case);
            }
        }
    }
}

struct ToolAnswer {
    text: String,
    is_error: bool,
}

/// One `tools/call` over the server's own HTTP transport, sent the way the CLI
/// sends it — including the bearer token the handle's config carries.
async fn rpc_call(
    server: &crate::claude::InProcessMcpServer,
    name: &str,
    arguments: &Value,
) -> Value {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    });

    let mut request = reqwest::Client::new()
        .post(server.url())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    for (header, value) in &server.config().headers {
        request = request.header(header, value);
    }
    let response = request
        .body(payload.to_string())
        .send()
        .await
        .expect("send tools/call");
    serde_json::from_str(&response.text().await.expect("text")).expect("a JSON-RPC reply")
}

/// [`rpc_call`]'s success shape: exactly one text block, and no protocol error.
///
/// A failing tool is never a protocol error — the convention `new_tool` ports
/// from `mcp.AddTool`, and the Go generator asserts the same thing by reading
/// `result.Content` unconditionally. Malformed *arguments* are the path that
/// assertion really guards, and no vector sends those; the property is
/// `new_tool`'s and is pinned once for every ported integration by
/// `github::tests_vectors::malformed_arguments_are_a_tool_error_rather_than_a_protocol_error`.
async fn call_tool(
    server: &crate::claude::InProcessMcpServer,
    name: &str,
    arguments: &Value,
) -> ToolAnswer {
    let body = rpc_call(server, name, arguments).await;

    assert!(
        body.get("error").is_none(),
        "{name}: a tool must not raise a protocol error: {body}"
    );
    let content = body["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: no content array: {body}"));
    assert_eq!(content.len(), 1, "{name}: want exactly one content block");
    assert_eq!(content[0]["type"], "text");

    ToolAnswer {
        text: content[0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: content is not text: {body}"))
            .to_string(),
        is_error: body["result"]["isError"].as_bool().unwrap_or(false),
    }
}

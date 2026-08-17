//! The Rust half of `desktop/parity/jira_vectors.json`.
//!
//! The Go half (`desktop/parity/jira_parity_test.go`) stands the **real**
//! integration up through `jira.Start` — no seam, because `Start` does not look
//! at the site URL — against an `httptest.Server` that plays a scripted Jira and
//! records what it was asked. This half replays the same script through an axum
//! server on loopback, the real
//! [`start_jira_mcp_server`](super::start_jira_mcp_server), and a real
//! `tools/call` over the MCP HTTP transport.
//!
//! Four surfaces, and one of them is Jira's own:
//!
//! 1. the advertised tool set, schemas and descriptions;
//! 2. the gating rule in every shape the generator recorded;
//! 3. **what a site URL this build cannot send a request through does** — Go
//!    hosts all nine tools and fails per call, so this asserts the tool set is
//!    untouched *and* that the call refuses. That is the property that decided
//!    where the base check lives; see `super::client`'s header;
//! 4. every call, request and result text.
//!
//! Pinned divergences, all inherited: `rust_text` + `rust_no_request` for the
//! dot-segment refusal and the zero-fraction float, and `rust_call_text` for a
//! site URL `url.Parse` rejects — where Go answers `creating request: parse "…"`
//! with `net/url`'s vocabulary and the stored site URL interpolated, and this port
//! answers the transport sentence.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rmcp::ServerHandler;
use serde::Deserialize;
use serde_json::Value;

use super::{jira_tools, server_name, start_jira_mcp_server, JIRA_TOOL_NAMES, SERVICES};
use crate::claude::ToolServer;
use crate::native::gojson::GoList;
use crate::native::integrations::ServiceConfig;

// ─── The vectors ─────────────────────────────────────────────────────────────

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

/// `config.ServiceConfig` as the Go generator marshals it.
#[derive(Deserialize)]
struct GoServiceConfig {
    enabled: bool,
    tools: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct SiteUrlVector {
    case: String,
    site_url: String,
    tools: Vec<String>,
    call_text: String,
    is_error: bool,
    #[serde(default)]
    rust_call_text: String,
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
    tools: Vec<ToolVector>,
    hosting: Vec<HostingVector>,
    site_urls: Vec<SiteUrlVector>,
    calls: Vec<CallVector>,
}

fn vectors() -> Vectors {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/jira_vectors.json");
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
    serde_json::from_str(&raw).expect("parsing the jira vectors")
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

/// The one service enabled with no tool list — the configuration the nine schemas
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

// ─── The advertised surface ──────────────────────────────────────────────────

/// The server name, every tool name, its description and its reflected input
/// schema, from a live `tools/list` against the Go server.
///
/// `list_projects` is the interesting one: its Go handler binds `*struct{}`, so
/// the schema is `{"type":"object","additionalProperties":false}` with no
/// `properties` at all — the only such tool in the six integrations, and the one
/// a fieldless Rust struct has to reproduce exactly.
#[test]
fn the_advertised_surface_matches_the_go_vectors() {
    let v = vectors();
    assert_eq!(v.server_name, server_name(&v.integration_id));
    assert_eq!(v.tools.len(), JIRA_TOOL_NAMES.len(), "vectors look partial");

    let server = ToolServer::new(&v.server_name).with_tools(jira_tools(
        &all_services(),
        "https://jira.atlassian.net",
        &v.email,
        &v.api_token,
    ));
    assert_eq!(
        server.tool_names(),
        v.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
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
        let mut hosted: Vec<String> = jira_tools(
            &services(&case.services),
            "https://jira.atlassian.net",
            &v.email,
            &v.api_token,
        )
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
        hosted.sort();
        assert_eq!(hosted, case.tools, "hosting case: {}", case.case);
    }
}

// ─── The site URLs Go serves without a glance ────────────────────────────────

/// A site URL `jira.Start` accepts and this build cannot send a request through
/// **must not change the tool set**, and must refuse per call.
///
/// This is the assertion that decided the design. Go validates nothing in
/// `Start`, so it advertises all nine tools whatever the stored site URL says;
/// refusing to host would drop tools that agents' stored `capabilities.mcp`
/// allowlists name. So the tool set is asserted against the Go vector, and the
/// call is asserted to refuse — with Go's own sentence where the two agree, and
/// with `rust_call_text` where Go fails inside
/// `http.NewRequestWithContext` and quotes the site URL back.
///
/// The hosts are all `.invalid`, so nothing leaves the machine on either side.
#[tokio::test]
async fn a_site_url_go_serves_and_this_build_cannot_keeps_its_tools_and_refuses_its_calls() {
    let v = vectors();
    assert!(v.site_urls.len() >= 4, "vectors look partial");

    for case in &v.site_urls {
        let server = start_jira_mcp_server(
            &v.integration_id,
            &all_services(),
            &case.site_url,
            &v.email,
            &v.api_token,
        )
        .await
        .expect("a bad site URL must not stop the server starting");

        assert!(
            !server.config().url.is_empty(),
            "{}: the server must still be listening",
            case.case
        );

        let tools = ToolServer::new(&v.server_name).with_tools(jira_tools(
            &all_services(),
            &case.site_url,
            &v.email,
            &v.api_token,
        ));
        let mut names = tools.tool_names();
        names.sort();
        assert_eq!(
            names, case.tools,
            "{}: the advertised tool set must not depend on the site URL",
            case.case
        );

        let result = call_tool(&server, "list_projects", &serde_json::json!({})).await;
        let want = if case.rust_call_text.is_empty() {
            &case.call_text
        } else {
            &case.rust_call_text
        };
        assert_eq!(result.text, *want, "{}: call text", case.case);
        assert_eq!(result.is_error, case.is_error, "{}: is_error", case.case);
        for secret in [&v.api_token, &v.email] {
            assert!(
                !result.text.contains(secret),
                "{}: a credential leaked into a tool result: {}",
                case.case,
                result.text
            );
        }
    }
}

// ─── The fake Jira ───────────────────────────────────────────────────────────

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
/// capture the request the tool *built*.
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

#[tokio::test]
async fn every_call_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.calls.len() >= 20, "vectors look partial");

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

    // One server per distinct site URL, as the Go generator opens one session per
    // distinct base path: the site URL is a field on the client, not a per-call
    // argument.
    let mut servers: BTreeMap<String, crate::claude::InProcessMcpServer> = BTreeMap::new();
    for case in &v.calls {
        if servers.contains_key(&case.base_path) {
            continue;
        }
        let server = start_jira_mcp_server(
            &v.integration_id,
            &all_services(),
            &format!("{base}{}", case.base_path),
            &v.email,
            &v.api_token,
        )
        .await
        .expect("start the jira server");
        servers.insert(case.base_path.clone(), server);
    }

    for case in &v.calls {
        let server = &servers[&case.base_path];
        state.arm(&case.response);
        let result = call_tool(server, &case.tool, &case.arguments).await;

        for secret in [&v.api_token, &v.email] {
            assert!(
                !result.text.contains(secret),
                "{}: a credential leaked into a tool result: {}",
                case.case,
                result.text
            );
        }

        // Every text this port substitutes is a *failure* it produces where Go
        // produced something else, so `rust_text` carries `is_error` with it —
        // and that is not always Go's flag: the zero-fraction-float case scripts
        // a 200, because what it pins is that Go went ahead while the decode here
        // fails before any handler runs.
        let (want_text, want_error) = if case.rust_text.is_empty() {
            (&case.text, case.is_error)
        } else {
            (&case.rust_text, true)
        };
        assert_eq!(result.text, *want_text, "{}: result text", case.case);
        assert_eq!(result.is_error, want_error, "{}: is_error", case.case);

        let seen = state.0.lock().expect("fake lock").seen.take();
        let want_request = if case.rust_no_request {
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
/// from `mcp.AddTool`. Malformed *arguments* are the path that really guards, and
/// the property is `new_tool`'s: it is pinned once for every ported integration by
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

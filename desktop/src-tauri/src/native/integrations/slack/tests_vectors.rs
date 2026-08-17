//! The Rust half of `desktop/parity/slack_vectors.json`.
//!
//! The Go half (`desktop/parity/slack_parity_test.go`) stands the **real**
//! integration up through `slack.Start`, points `slackAPIBase` at an
//! `httptest.Server` that plays a scripted Slack and records what it was asked,
//! and writes down four things. This half replays the same script through an axum
//! server on loopback, the real
//! [`start_slack_mcp_server`](super::start_slack_mcp_server), and a real
//! `tools/call` over the MCP HTTP transport.
//!
//! The fourth block is Slack's own: `tokens` pins `resolveToken`, which is the
//! only place in the port that reads the **`auth` column** as a value. Both
//! languages observe the resolved token the same way — by making one call and
//! reading the `Authorization` header the fake received — rather than recording
//! it directly, which would put the thing under test into the fixture that
//! verifies it.
//!
//! Pinned divergences: `rust_text` + `rust_no_request` for the zero-fraction
//! float, `rust_text` alone for `encoding/json`'s syntax-error vocabulary when
//! Slack's body will not parse, and `rust_error` for a credential blob that will
//! not decode — where Go carries `encoding/json`'s wording and this carries line
//! and column, because serde's message quotes the value, which in the `oauth` arm
//! *is* the access token.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rmcp::ServerHandler;
use serde::Deserialize;
use serde_json::Value;

use super::client::{api_base_lock, set_api_base};
use super::{server_name, slack_tools, start_slack_mcp_server, SERVICES, SLACK_TOOL_NAMES};
use crate::claude::ToolServer;
use crate::native::gojson::GoList;
use crate::native::integrations::registry::resolve_slack_token;
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

#[derive(Deserialize)]
struct GoServiceConfig {
    enabled: bool,
    tools: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct TokenVector {
    case: String,
    credentials: String,
    auth: String,
    #[serde(default)]
    authorization: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    rust_error: String,
}

#[derive(Deserialize)]
struct RequestVector {
    method: String,
    target: String,
    authorization: String,
    content_type: String,
    body: String,
}

#[derive(Deserialize)]
struct ResponseScript {
    status: u16,
    #[serde(default)]
    retry_after: String,
    body: String,
}

#[derive(Deserialize)]
struct CallVector {
    case: String,
    tool: String,
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
    token: String,
    tools: Vec<ToolVector>,
    hosting: Vec<HostingVector>,
    tokens: Vec<TokenVector>,
    calls: Vec<CallVector>,
}

fn vectors() -> Vectors {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/slack_vectors.json");
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
    serde_json::from_str(&raw).expect("parsing the slack vectors")
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

#[test]
fn the_advertised_surface_matches_the_go_vectors() {
    let v = vectors();
    assert_eq!(v.server_name, server_name(&v.integration_id));
    assert_eq!(
        v.tools.len(),
        SLACK_TOOL_NAMES.len(),
        "vectors look partial"
    );

    let server = ToolServer::new(&v.server_name).with_tools(slack_tools(&all_services(), &v.token));
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

#[test]
fn the_hosted_tool_set_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.hosting.len() >= 5, "vectors look partial");
    for case in &v.hosting {
        let mut hosted: Vec<String> = slack_tools(&services(&case.services), &v.token)
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        hosted.sort();
        assert_eq!(hosted, case.tools, "hosting case: {}", case.case);
    }
}

// ─── `resolveToken`, and the auth column ─────────────────────────────────────

/// Every arm of `resolveToken`, observed the way Go observed it: by making a call
/// and reading the `Authorization` header the fake received.
///
/// The two rows that matter most are the ones no other integration has — `oauth`
/// mode reading the **`auth` column** rather than the credentials blob, and the
/// fallback arm where an unrecognized `auth_mode` still works if a bot token is
/// present.
#[tokio::test]
async fn resolve_token_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.tokens.len() >= 6, "vectors look partial");

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

    let _guard = api_base_lock().await;
    set_api_base(Some(base));

    for case in &v.tokens {
        let resolved = resolve_slack_token(&v.integration_id, &case.credentials, &case.auth);

        if !case.error.is_empty() {
            let want = if case.rust_error.is_empty() {
                &case.error
            } else {
                &case.rust_error
            };
            assert_eq!(resolved, Err(want.clone()), "{}: refusal", case.case);
            continue;
        }

        let token = resolved
            .unwrap_or_else(|e| panic!("{}: Go resolved a token and this did not: {e}", case.case));
        let server = start_slack_mcp_server(&v.integration_id, &all_services(), &token)
            .await
            .expect("start the slack server");

        state.arm(&ResponseScript {
            status: 200,
            retry_after: String::new(),
            body: r#"{"ok":true}"#.to_string(),
        });
        let _ = call_tool(
            &server,
            "list_users",
            &serde_json::json!({"limit": 0, "cursor": ""}),
        )
        .await;

        let seen = state.0.lock().expect("fake lock").seen.take();
        let seen = seen.unwrap_or_else(|| panic!("{}: no request reached the fake", case.case));
        assert_eq!(
            seen.authorization, case.authorization,
            "{}: the header Go sent",
            case.case
        );
    }

    set_api_base(None);
}

// ─── The fake Slack ──────────────────────────────────────────────────────────

struct Recorded {
    method: String,
    target: String,
    authorization: String,
    content_type: String,
    body: String,
}

#[derive(Default)]
struct Fake {
    status: u16,
    retry_after: String,
    body: String,
    seen: Option<Recorded>,
}

#[derive(Clone, Default)]
struct FakeState(Arc<Mutex<Fake>>);

impl FakeState {
    fn arm(&self, script: &ResponseScript) {
        let mut fake = self.0.lock().expect("fake lock");
        fake.status = script.status;
        fake.retry_after.clone_from(&script.retry_after);
        fake.body.clone_from(&script.body);
        fake.seen = None;
    }
}

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

    let (status, retry_after, reply) = {
        let mut fake = state.0.lock().expect("fake lock");
        fake.seen = Some(Recorded {
            method: method.to_string(),
            target: uri
                .path_and_query()
                .map(|pq| pq.as_str().to_string())
                .unwrap_or_default(),
            authorization: header("authorization"),
            content_type: header("content-type"),
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        (fake.status, fake.retry_after.clone(), fake.body.clone())
    };

    let mut response = (
        StatusCode::from_u16(status).expect("a scripted status"),
        reply,
    )
        .into_response();
    if !retry_after.is_empty() {
        response.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            retry_after.parse().expect("a scripted Retry-After"),
        );
    }
    response
}

// ─── The calls ───────────────────────────────────────────────────────────────

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

    let _guard = api_base_lock().await;
    set_api_base(Some(base));

    let server = start_slack_mcp_server(&v.integration_id, &all_services(), &v.token)
        .await
        .expect("start the slack server");

    for case in &v.calls {
        state.arm(&case.response);
        let result = call_tool(&server, &case.tool, &case.arguments).await;

        assert!(
            !result.text.contains(&v.token),
            "{}: the token leaked into a tool result: {}",
            case.case,
            result.text
        );

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
                "{}: the refusal still reached the network",
                case.case
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
                    "{}: the Bearer prefix and the token",
                    case.case
                );
                assert_eq!(
                    got.content_type, want.content_type,
                    "{}: form or JSON, and the charset",
                    case.case
                );
                assert_eq!(got.body, want.body, "{}: request body", case.case);
            }
        }
    }

    set_api_base(None);
}

struct ToolAnswer {
    text: String,
    is_error: bool,
}

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

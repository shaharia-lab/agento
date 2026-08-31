//! The Rust half of `desktop/parity/github_vectors.json`.
//!
//! The Go half (`desktop/parity/github_parity_test.go`) stands the **real**
//! integration up through `github.Start`, points `githubAPIBase` at an
//! `httptest.Server` that plays a scripted GitHub and records what it was
//! asked, and writes down four things per case: the tool set hosted, the
//! advertised schema, the request the tool built, and the result the model
//! would read.
//!
//! This half replays the same script through the same three layers — an axum
//! server on loopback standing in for `httptest`, the real
//! [`start_github_mcp_server`](super::start_github_mcp_server), and a real
//! `tools/call` over the MCP HTTP transport — and asserts all four. Nothing is
//! stubbed: a change to the client, to a path, to a body or to a sentence fails
//! here.
//!
//! Three fields carry a *pinned divergence* rather than a match, across four
//! causes, each documented at its use below and in the Go generator:
//!
//! - `rust_text` — Go's `encoding/json` syntax-error vocabulary; the
//!   dot-segment refusal; and `rmcp`'s wording for an argument it will not
//!   decode.
//! - `rust_target` — the Go MCP SDK rounds an integer argument above 2^53
//!   before the handler sees it.
//! - `rust_no_request` — Go reached the fake and this port did not: a path
//!   holding a `.` or `..` segment, which [`super::client::absolute`] refuses
//!   because `url` would resolve it into a different endpoint than Go calls,
//!   and a zero-fraction float, which the Go SDK re-marshals into an integer
//!   and `serde_json` refuses.

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
use super::{github_tools, server_name, start_github_mcp_server, GITHUB_TOOL_NAMES, SERVICES};
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

/// `config.ServiceConfig` as the Go generator marshals it. Converted rather
/// than deserialized straight into [`ServiceConfig`], because that type's
/// `tools` is a [`GoList`] whose job is nil-versus-empty fidelity on the *write
/// path* — here the vector is plain JSON and a nil list is simply `null`.
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
    #[serde(default)]
    location: String,
    body: String,
}

#[derive(Deserialize)]
struct CallVector {
    case: String,
    tool: String,
    arguments: Value,
    response: ResponseScript,
    /// Absent when the tool answered without making a request — only
    /// `trigger_workflow`'s inputs parse does that.
    request: Option<RequestVector>,
    is_error: bool,
    text: String,
    /// See the module header: a pinned divergence, not a match.
    #[serde(default)]
    rust_text: String,
    #[serde(default)]
    rust_target: String,
    /// Go made a request and this port deliberately makes none — a path
    /// carrying a `.` or `..` segment, which `super::client::absolute` refuses
    /// rather than let `url` resolve into a different endpoint.
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
    calls: Vec<CallVector>,
}

fn vectors() -> Vectors {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/github_vectors.json");
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
    serde_json::from_str(&raw).expect("parsing the github vectors")
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

/// Every service enabled with no tool list — the configuration the twenty
/// schemas and every call vector were taken under.
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
        GITHUB_TOOL_NAMES.len(),
        "vectors look partial"
    );

    let server =
        ToolServer::new(&v.server_name).with_tools(github_tools(&all_services(), &v.token));
    assert_eq!(
        server.tool_names(),
        v.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        // Both sides are already name-sorted — `tool_names` goes through
        // `ToolRouter::list_all`, which sorts, and the Go SDK's `featureSet`
        // lists by sorted key — so this is set equality in a stable order, not
        // an order assertion. *Registration* order is pinned by
        // `super::tests::an_empty_allowed_set_hosts_every_tool` against
        // `GITHUB_TOOL_NAMES`; see `super::SERVICES`.
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

/// The gating rule, in every shape the Go generator recorded — including the
/// one that reads backwards, where an empty allowed set hosts everything.
#[test]
fn the_hosted_tool_set_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.hosting.len() >= 5, "vectors look partial");
    for case in &v.hosting {
        let mut hosted: Vec<String> = github_tools(&services(&case.services), &v.token)
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        // The Go vector sorts, because what it records is the *set*; the order
        // is asserted by the twenty-tool block above.
        hosted.sort();
        assert_eq!(hosted, case.tools, "hosting case: {}", case.case);
    }
}

// ─── The fake GitHub ─────────────────────────────────────────────────────────

/// What the fake was asked, in the shape the Go generator recorded it.
#[derive(Default)]
struct Recorded {
    method: String,
    target: String,
    authorization: String,
    accept: String,
    content_type: String,
    /// Not a vector field — no golden records a `User-Agent`, and
    /// `parity/google_vectors.json` says so in as many words. It is captured so
    /// the twenty tools' real requests can be asserted to carry Agento's own
    /// (#514), which is what the Go server got free from `net/http` and this
    /// port did not.
    user_agent: String,
    body: String,
}

#[derive(Default)]
struct Fake {
    status: u16,
    location: String,
    body: String,
    seen: Option<Recorded>,
}

#[derive(Clone, Default)]
struct FakeState(Arc<Mutex<Fake>>);

impl FakeState {
    fn arm(&self, script: &ResponseScript) {
        let mut fake = self.0.lock().expect("fake lock");
        fake.status = script.status;
        fake.location.clone_from(&script.location);
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

    let (status, location, reply) = {
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
            user_agent: header("user-agent"),
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        (fake.status, fake.location.clone(), fake.body.clone())
    };

    let mut response = (
        StatusCode::from_u16(status).expect("a scripted status"),
        reply,
    )
        .into_response();
    if !location.is_empty() {
        response.headers_mut().insert(
            axum::http::header::LOCATION,
            location.parse().expect("a scripted Location"),
        );
    }
    response
}

// ─── The calls ───────────────────────────────────────────────────────────────

/// Every call vector, driven end to end.
///
/// One test rather than one per case, because [`set_api_base`] is process-wide
/// — as Go's `githubAPIBase` is — and `cargo test` runs tests in parallel where
/// `go test` runs a package's in sequence. [`api_base_lock`] is what keeps this
/// from racing `client`'s own base test.
#[tokio::test]
async fn every_call_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.calls.len() >= 40, "vectors look partial");

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

    let server = start_github_mcp_server(&v.integration_id, &all_services(), &v.token)
        .await
        .expect("start the github server");

    for case in &v.calls {
        state.arm(&case.response);
        let result = call_tool(&server, &case.tool, &case.arguments).await;

        // The credential must never reach the model, on either path.
        assert!(
            !result.text.contains(&v.token),
            "{}: the token leaked into a tool result: {}",
            case.case,
            result.text
        );

        let want_text = if case.rust_text.is_empty() {
            &case.text
        } else {
            &case.rust_text
        };
        assert_eq!(result.text, *want_text, "{}: result text", case.case);
        assert_eq!(result.is_error, case.is_error, "{}: is_error", case.case);

        let seen = state.0.lock().expect("fake lock").seen.take();
        let want_request = if case.rust_no_request {
            // The dot-segment refusal: Go reached the fake, this port never
            // built the request. Asserted as "and nothing was sent" rather than
            // skipped, because sending *something* — the resolved path — is the
            // outcome `absolute` exists to prevent.
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
                let want_target = if case.rust_target.is_empty() {
                    &want.target
                } else {
                    &case.rust_target
                };
                assert_eq!(got.method, want.method, "{}: method", case.case);
                assert_eq!(got.target, *want_target, "{}: request target", case.case);
                assert_eq!(
                    got.authorization, want.authorization,
                    "{}: the Bearer prefix and the token",
                    case.case
                );
                assert_eq!(got.accept, want.accept, "{}: Accept", case.case);
                assert_eq!(
                    got.content_type, want.content_type,
                    "{}: Content-Type is sent only when there is a body",
                    case.case
                );
                assert_eq!(got.body, want.body, "{}: request body", case.case);
                // Every one of the twenty tools, not just the client that
                // built the first request: GitHub answers 403 to a request
                // without it, so this is the difference between the
                // integration working and none of it working.
                assert_eq!(
                    got.user_agent,
                    crate::native::http::USER_AGENT,
                    "{}: User-Agent",
                    case.case
                );
            }
        }
    }

    set_api_base(None);
}

/// A malformed argument is a **tool** error, not a protocol error.
///
/// `desktop/CLAUDE.md` promises that a missing, extra or wrong-typed field
/// yields a `CallToolResult` carrying `IsError` — text the model can read and
/// retry against — exactly as the Go server does, and never a JSON-RPC `error`.
/// Nothing in this port implements that: it rests entirely on `rmcp`'s
/// `into_tool_argument_error`, which **prefix-matches a hardcoded string**
/// (`"failed to deserialize parameters:"`) against its own extractor's message
/// (`handler/server/router/tool.rs`). An upgrade that reworded either half
/// would silently flip all 62 ported tools to protocol errors, which the CLI
/// renders as "Tool result missing due to internal error" — nothing to retry
/// against, on every integration at once.
///
/// [`every_call_matches_the_go_vectors`] cannot notice: [`call_tool`] asserts
/// the absence of an `error` member, but no vector ever sends bad arguments, so
/// the assertion never runs on the path it guards. This sends them.
///
/// The **kind** is pinned, not the text: the wording divergence (Go's
/// `validating "arguments": …` against `rmcp`'s `failed to deserialize
/// parameters: …`) is already accepted and documented, and pinning it here
/// would fail on a harmless rmcp rewording while still missing the real one.
#[tokio::test]
async fn malformed_arguments_are_a_tool_error_rather_than_a_protocol_error() {
    let v = vectors();
    // Nothing should reach the network — the decode fails before a handler
    // runs — so the base points at a port with nothing behind it. If that ever
    // stops being true this fails loudly instead of calling api.github.com.
    let _guard = api_base_lock().await;
    set_api_base(Some("http://127.0.0.1:1".to_string()));

    let server = start_github_mcp_server(&v.integration_id, &all_services(), &v.token)
        .await
        .expect("start the github server");

    for (case, arguments) in [
        // `repo` is required — every field of all twenty tools is, since no Go
        // params struct in this integration carries `omitempty`.
        (
            "a missing required field",
            serde_json::json!({"owner": "octocat"}),
        ),
        // `deny_unknown_fields`, which is what emits `additionalProperties:
        // false` — the key Go's reflector sets and its server validates on.
        (
            "an unknown field",
            serde_json::json!({"owner": "octocat", "repo": "hello-world", "nope": 1}),
        ),
    ] {
        let body = rpc_call(&server, "get_repo", &arguments).await;
        assert!(
            body.get("error").is_none(),
            "{case}: raised a JSON-RPC error rather than a tool error: {body}"
        );
        assert_eq!(
            body["result"]["isError"].as_bool(),
            Some(true),
            "{case}: want a tool error: {body}"
        );
        let content = body["result"]["content"]
            .as_array()
            .unwrap_or_else(|| panic!("{case}: no content array: {body}"));
        assert_eq!(content.len(), 1, "{case}: want exactly one content block");
        assert_eq!(content[0]["type"], "text");
        assert!(
            content[0]["text"].as_str().is_some_and(|t| !t.is_empty()),
            "{case}: the model is handed an empty message: {body}"
        );
    }

    set_api_base(None);
}

struct ToolAnswer {
    text: String,
    is_error: bool,
}

/// One `tools/call` over the server's own HTTP transport, sent the way the CLI
/// sends it — including the bearer token the handle's config carries. Answers
/// the raw JSON-RPC reply, so a caller can assert on the envelope as well as on
/// the result.
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
async fn call_tool(
    server: &crate::claude::InProcessMcpServer,
    name: &str,
    arguments: &Value,
) -> ToolAnswer {
    let body = rpc_call(server, name, arguments).await;

    // A failing tool is never a protocol error — the convention `new_tool`
    // ports from `mcp.AddTool`, and the Go generator asserts the same thing by
    // reading `result.Content` unconditionally. Malformed *arguments* are the
    // path that assertion really guards, and no vector sends those; see
    // [`malformed_arguments_are_a_tool_error_rather_than_a_protocol_error`].
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

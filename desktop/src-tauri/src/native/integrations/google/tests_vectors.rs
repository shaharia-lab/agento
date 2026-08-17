//! The Rust half of `desktop/parity/google_vectors.json`.
//!
//! The Go half (`desktop/parity/google_parity_test.go`) stands the **real**
//! integration up through `google.Start`, points the generated clients and the
//! OAuth2 token endpoint at an `httptest.Server` that plays a scripted Google and
//! records what it was asked. This half replays the same script through an axum
//! server on loopback, the real
//! [`start_google_mcp_server`](super::start_google_mcp_server), and a real
//! `tools/call` over the MCP HTTP transport.
//!
//! # Why this file carries more weight than its five siblings
//!
//! For the other integrations the vectors *confirm* a port a reader could check
//! by eye against `tools.go`. Here they are the **only** written statement of
//! what `calendar/v3`, `gmail/v1` and `drive/v3` put on the wire, because those
//! requests are built by a code generator that lives outside this repository.
//! Three mismatches were found by running these vectors for the first time, and
//! none of them was visible from either side's source:
//!
//! 1. **Path parameters use `googleapi.Expand`, not `url.PathEscape`** — so an
//!    `&` in a message id is `%26`, where `PathEscape` leaves it alone and starts
//!    a query parameter. [`super::client::expand_path_segment`].
//! 2. **The multipart metadata part carries a trailing newline**, because
//!    `googleapi` writes it with `json.NewEncoder(…).Encode`.
//! 3. **`oauth2.RetrieveError` has two forms**, and a Google refresh refusal
//!    takes the one the port did not have:
//!    `oauth2: "invalid_grant" "Token has been expired or revoked."`.
//!
//! # Two things the replay has to normalize, and only two
//!
//! - **`«now»`** — `view_events`' `time_min` defaults to `time.Now()`. The
//!   recorded target holds the placeholder; this half substitutes it after
//!   asserting the value it replaced is a seconds-precision RFC3339 instant in
//!   UTC, so the *shape* is still pinned.
//! - **The multipart boundary** — random in both languages, so the fake parses
//!   the body into parts and compares those.
//!
//! Everything else is byte-for-byte, including the `Authorization` header, which
//! is how a refresh is observed reaching the request that follows it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rmcp::ServerHandler;
use serde::Deserialize;
use serde_json::Value;

use super::client::{api_base_lock, set_api_base, Token, TokenSource};
use super::{google_tools, server_name, start_google_mcp_server, GOOGLE_TOOL_NAMES, SERVICES};
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

#[derive(Deserialize)]
struct GoServiceConfig {
    enabled: bool,
    tools: Option<Vec<String>>,
}

#[derive(Clone, Deserialize)]
struct PartVector {
    content_type: String,
    body: String,
}

#[derive(Clone, Deserialize)]
struct RequestVector {
    method: String,
    target: String,
    authorization: String,
    content_type: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    parts: Vec<PartVector>,
}

#[derive(Clone, Deserialize)]
struct ResponseScript {
    status: u16,
    body: String,
}

#[derive(Deserialize)]
struct CallVector {
    case: String,
    tool: String,
    arguments: Value,
    responses: Vec<ResponseScript>,
    requests: Vec<RequestVector>,
    is_error: bool,
    text: String,
    #[serde(default)]
    rust_text: String,
    #[serde(default)]
    rust_no_request: bool,
}

#[derive(Deserialize)]
struct RefreshVector {
    case: String,
    expires_in: Option<i64>,
    token_response: Option<ResponseScript>,
    api_response: ResponseScript,
    refresh_request: Option<RequestVector>,
    api_request: Option<RequestVector>,
    is_error: bool,
    text: String,
    #[serde(default)]
    rust_text: String,
}

#[derive(Deserialize)]
struct Vectors {
    integration_id: String,
    server_name: String,
    client_id: String,
    client_secret: String,
    refresh_token: String,
    access_token: String,
    now_placeholder: String,
    tools: Vec<ToolVector>,
    hosting: Vec<HostingVector>,
    calls: Vec<CallVector>,
    refreshes: Vec<RefreshVector>,
}

fn vectors() -> Vectors {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/google_vectors.json");
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
    serde_json::from_str(&raw).expect("parsing the google vectors")
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

/// A token source in the ordinary state: an hour from expiry, so nothing
/// refreshes.
fn hour_source(v: &Vectors) -> Arc<TokenSource> {
    Arc::new(TokenSource::new(
        &v.client_id,
        &v.client_secret,
        Token {
            access_token: v.access_token.clone(),
            refresh_token: v.refresh_token.clone(),
            expiry: Some(SystemTime::now() + Duration::from_secs(3600)),
        },
    ))
}

// ─── The advertised surface ──────────────────────────────────────────────────

/// The server name, every tool name, its description and its reflected input
/// schema, from a live `tools/list` against the Go server.
#[test]
fn the_advertised_surface_matches_the_go_vectors() {
    let v = vectors();
    assert_eq!(v.server_name, server_name(&v.integration_id));
    assert_eq!(
        v.tools.len(),
        GOOGLE_TOOL_NAMES.len(),
        "vectors look partial"
    );

    let server =
        ToolServer::new(&v.server_name).with_tools(google_tools(&all_services(), hour_source(&v)));
    let mut hosted = server.tool_names();
    hosted.sort();
    assert_eq!(
        hosted,
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
    assert!(v.hosting.len() >= 8, "vectors look partial");
    for case in &v.hosting {
        let mut hosted: Vec<String> = google_tools(&services(&case.services), hour_source(&v))
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        hosted.sort();
        assert_eq!(hosted, case.tools, "hosting case: {}", case.case);
    }
}

// ─── The fake Google ─────────────────────────────────────────────────────────

#[derive(Clone)]
struct Recorded {
    method: String,
    target: String,
    authorization: String,
    content_type: String,
    body: String,
    parts: Vec<PartVector>,
}

#[derive(Default)]
struct Fake {
    /// Consumed in order; the last entry is reused, exactly as the Go fake does,
    /// which is what lets one `search_email` case script a list and every fetch
    /// behind it.
    api: Vec<ResponseScript>,
    api_seen: Vec<Recorded>,
    token: Option<ResponseScript>,
    token_seen: Option<Recorded>,
}

#[derive(Clone, Default)]
struct FakeState(Arc<Mutex<Fake>>);

impl FakeState {
    fn arm_api(&self, scripts: &[ResponseScript]) {
        let mut fake = self.0.lock().expect("fake lock");
        fake.api = scripts.to_vec();
        fake.api_seen.clear();
    }

    fn arm_token(&self, script: Option<&ResponseScript>) {
        let mut fake = self.0.lock().expect("fake lock");
        fake.token = script.cloned();
        fake.token_seen = None;
    }

    fn api_seen(&self) -> Vec<Recorded> {
        self.0.lock().expect("fake lock").api_seen.clone()
    }

    fn token_seen(&self) -> Option<Recorded> {
        self.0.lock().expect("fake lock").token_seen.clone()
    }
}

async fn serve_fake(
    State(state): State<FakeState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let recorded = record(&method, &uri, &headers, &body);

    let (status, reply) = {
        let mut fake = state.0.lock().expect("fake lock");
        if path == "/token" {
            fake.token_seen = Some(recorded);
            match fake.token.clone() {
                Some(script) => (script.status, script.body),
                // Matching the Go fake: an unscripted refresh is answered with a
                // failure the assertion shows rather than one it hides.
                None => (500, r#"{"error":"unexpected refresh"}"#.to_string()),
            }
        } else {
            fake.api_seen.push(recorded);
            match fake.api.len() {
                0 => (500, r#"{"error":"no response scripted"}"#.to_string()),
                1 => (fake.api[0].status, fake.api[0].body.clone()),
                _ => {
                    let script = fake.api.remove(0);
                    (script.status, script.body)
                }
            }
        }
    };

    (
        StatusCode::from_u16(status).expect("a scripted status"),
        reply,
    )
        .into_response()
}

fn record(method: &Method, uri: &Uri, headers: &HeaderMap, body: &Bytes) -> Recorded {
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let raw = String::from_utf8_lossy(body).into_owned();

    // The boundary is random, so a multipart body is recorded as its parts —
    // which is what the Go fake records too.
    let (content_type, body, parts) = match multipart_boundary_of(&content_type) {
        Some((media_type, boundary)) => (media_type, String::new(), split_parts(&raw, &boundary)),
        None => (content_type, raw, Vec::new()),
    };

    Recorded {
        method: method.to_string(),
        target: uri
            .path_and_query()
            .map(|pq| pq.as_str().to_string())
            .unwrap_or_default(),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string(),
        content_type,
        body,
        parts,
    }
}

/// `mime.ParseMediaType`, over the one shape this fake sees.
fn multipart_boundary_of(content_type: &str) -> Option<(String, String)> {
    let mut fields = content_type.split(';');
    let media_type = fields.next()?.trim().to_string();
    if !media_type.starts_with("multipart/") {
        return None;
    }
    for field in fields {
        let (key, value) = field.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("boundary") {
            return Some((media_type, value.trim().trim_matches('"').to_string()));
        }
    }
    None
}

/// `multipart.Reader`, over the framing `googleapi` writes and nothing more:
/// each part is `Content-Type: …` then a blank line then the body.
fn split_parts(body: &str, boundary: &str) -> Vec<PartVector> {
    let mut parts = Vec::new();
    for chunk in body.split(&format!("--{boundary}")) {
        let chunk = chunk.trim_start_matches("\r\n");
        if chunk.is_empty() || chunk.starts_with("--") {
            continue;
        }
        let Some((headers, content)) = chunk.split_once("\r\n\r\n") else {
            continue;
        };
        let content_type = headers
            .lines()
            .find_map(|line| line.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("Content-Type"))
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_default();
        parts.push(PartVector {
            content_type,
            // The reader strips the CRLF that precedes the next boundary.
            body: content.strip_suffix("\r\n").unwrap_or(content).to_string(),
        });
    }
    parts
}

// ─── Comparing a request ─────────────────────────────────────────────────────

/// Substitutes the placeholder for a `timeMin` this run generated, after
/// asserting it is the shape Go's `time.Now().UTC().Format(time.RFC3339)`
/// produces — so the value is checked even though it cannot be compared.
fn substitute_now(case: &str, target: &str, placeholder: &str) -> String {
    let Some(start) = target.find("timeMin=") else {
        return target.to_string();
    };
    let value_at = start + "timeMin=".len();
    let end = target[value_at..]
        .find('&')
        .map_or(target.len(), |offset| value_at + offset);
    let encoded = &target[value_at..end];

    // Percent-decoding the one escape a `timeMin` can carry.
    let decoded = encoded.replace("%3A", ":");
    // `2026-08-17T12:34:56Z` — seconds precision, a literal `Z`, never an offset.
    let shaped = decoded.len() == 20
        && decoded.ends_with('Z')
        && decoded.as_bytes()[10] == b'T'
        && decoded
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | 'T' | 'Z'));
    if !shaped {
        // Not a generated one — an explicit `time_min` the caller passed through.
        return target.to_string();
    }
    let Ok(instant) = chrono::DateTime::parse_from_rfc3339(&decoded) else {
        panic!("{case}: a time_min of this shape must be RFC3339: {decoded}");
    };
    // An explicit `time_min` argument has the same shape, so only a value
    // actually close to now is the generated one — the same test the Go half
    // applies before it writes the placeholder.
    if (chrono::Utc::now() - instant.with_timezone(&chrono::Utc))
        .num_seconds()
        .abs()
        > 3600
    {
        return target.to_string();
    }

    format!(
        "{}{}{}",
        &target[..value_at],
        urlencoding_of(placeholder),
        &target[end..]
    )
}

/// `url.QueryEscape`, over the placeholder alone.
fn urlencoding_of(value: &str) -> String {
    crate::native::gourl::query_escape(value)
}

fn assert_request(case: &str, want: &RequestVector, got: &Recorded, placeholder: &str) {
    assert_eq!(got.method, want.method, "{case}: method");
    assert_eq!(
        substitute_now(case, &got.target, placeholder),
        want.target,
        "{case}: the request target, query included"
    );
    assert_eq!(
        got.authorization, want.authorization,
        "{case}: Authorization — which is how a refresh is observed"
    );
    assert_eq!(got.content_type, want.content_type, "{case}: Content-Type");
    assert_eq!(got.body, want.body, "{case}: request body");
    assert_eq!(
        got.parts.len(),
        want.parts.len(),
        "{case}: multipart part count"
    );
    for (index, (got, want)) in got.parts.iter().zip(&want.parts).enumerate() {
        assert_eq!(
            got.content_type, want.content_type,
            "{case}: part {index}'s Content-Type — sniffed, not the mime_type argument"
        );
        assert_eq!(got.body, want.body, "{case}: part {index}'s body");
    }
}

// ─── The calls ───────────────────────────────────────────────────────────────

struct Harness {
    state: FakeState,
    base: String,
}

async fn harness() -> Harness {
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
    Harness { state, base }
}

#[tokio::test]
async fn every_call_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.calls.len() >= 35, "vectors look partial");

    let fake = harness().await;
    let _guard = api_base_lock().await;
    set_api_base(Some(fake.base.clone()));

    let server = start_google_mcp_server(&v.integration_id, &all_services(), hour_source(&v))
        .await
        .expect("start the google server");

    for case in &v.calls {
        fake.state.arm_api(&case.responses);
        // The token is an hour out, so a refresh arriving is itself a defect.
        fake.state.arm_token(None);

        let result = call_tool(&server, &case.tool, &case.arguments).await;

        // A result must never carry the durable credentials, in either language.
        assert!(
            !result.text.contains(&v.client_secret) && !result.text.contains(&v.refresh_token),
            "{}: a credential leaked into a tool result: {}",
            case.case,
            result.text
        );
        assert!(
            fake.state.token_seen().is_none(),
            "{}: an hour-valid token refreshed, which it must not",
            case.case
        );

        let (want_text, want_error) = if case.rust_text.is_empty() {
            (&case.text, case.is_error)
        } else {
            (&case.rust_text, true)
        };
        assert_eq!(result.text, *want_text, "{}: result text", case.case);
        assert_eq!(result.is_error, want_error, "{}: is_error", case.case);

        let seen = fake.state.api_seen();
        if case.rust_no_request {
            assert!(
                seen.is_empty(),
                "{}: the refusal still reached the network",
                case.case
            );
            continue;
        }
        assert_eq!(
            seen.len(),
            case.requests.len(),
            "{}: request count — search_email is the one that makes N+1",
            case.case
        );
        for (index, (want, got)) in case.requests.iter().zip(&seen).enumerate() {
            assert_request(
                &format!("{} [request {index}]", case.case),
                want,
                got,
                &v.now_placeholder,
            );
        }
    }

    set_api_base(None);
}

// ─── The refreshes ───────────────────────────────────────────────────────────

/// The whole refresh decision, recorded rather than asserted: whether one
/// happened at all, what it sent, and which access token the request that
/// followed carried.
#[tokio::test]
async fn every_refresh_matches_the_go_vectors() {
    let v = vectors();
    assert!(v.refreshes.len() >= 5, "vectors look partial");

    let fake = harness().await;
    let _guard = api_base_lock().await;
    set_api_base(Some(fake.base.clone()));

    // The one service the Go half used: the shortest call that reaches the
    // transport, so each case is about the token and nothing else.
    let drive_only = BTreeMap::from([(
        "drive".to_string(),
        ServiceConfig {
            enabled: true,
            tools: Some(GoList(vec!["list_files".to_string()])),
        },
    )]);

    for case in &v.refreshes {
        fake.state.arm_api(std::slice::from_ref(&case.api_response));
        fake.state.arm_token(case.token_response.as_ref());

        let tokens = Arc::new(TokenSource::new(
            &v.client_id,
            &v.client_secret,
            Token {
                access_token: v.access_token.clone(),
                refresh_token: v.refresh_token.clone(),
                // `None` is Go's zero `time.Time`, which never expires — and a
                // negative offset is a token already past its expiry.
                expiry: case.expires_in.map(|seconds| {
                    let delta = Duration::from_secs(seconds.unsigned_abs());
                    if seconds >= 0 {
                        SystemTime::now() + delta
                    } else {
                        SystemTime::now() - delta
                    }
                }),
            },
        ));

        let server = start_google_mcp_server(&v.integration_id, &drive_only, tokens)
            .await
            .expect("start the google server");
        let result = call_tool(
            &server,
            "list_files",
            &serde_json::json!({"query": "", "max_results": 1}),
        )
        .await;

        let (want_text, want_error) = if case.rust_text.is_empty() {
            (&case.text, case.is_error)
        } else {
            (&case.rust_text, true)
        };
        assert_eq!(result.text, *want_text, "{}: result text", case.case);
        assert_eq!(result.is_error, want_error, "{}: is_error", case.case);

        // Whether a refresh happened at all is the assertion — a port that
        // refreshed on every call would pass every text comparison above.
        match (&case.refresh_request, fake.state.token_seen()) {
            (None, seen) => assert!(
                seen.is_none(),
                "{}: Go made no refresh and this one did",
                case.case
            ),
            (Some(_), None) => panic!("{}: Go refreshed and this one did not", case.case),
            (Some(want), Some(got)) => assert_request(
                &format!("{} [refresh]", case.case),
                want,
                &got,
                &v.now_placeholder,
            ),
        }

        match (&case.api_request, fake.state.api_seen().first()) {
            (None, seen) => assert!(
                seen.is_none(),
                "{}: Go reached no API request and this one did",
                case.case
            ),
            (Some(_), None) => panic!("{}: Go reached the API and this one did not", case.case),
            (Some(want), Some(got)) => assert_request(
                &format!("{} [api]", case.case),
                want,
                got,
                &v.now_placeholder,
            ),
        }
    }

    set_api_base(None);
}

// ─── Driving a tool over the real transport ──────────────────────────────────

struct ToolAnswer {
    text: String,
    is_error: bool,
}

async fn call_tool(
    server: &crate::claude::InProcessMcpServer,
    name: &str,
    arguments: &Value,
) -> ToolAnswer {
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
    let body: Value =
        serde_json::from_str(&response.text().await.expect("text")).expect("a JSON-RPC reply");

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

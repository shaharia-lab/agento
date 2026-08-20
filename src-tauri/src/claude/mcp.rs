//! In-process MCP servers, ported from `claude/mcp.go`.
//!
//! "In-process" is a description of where the *tools* live, not of the
//! transport. The CLI has no way to call into this process directly, so the
//! bridge is a loopback HTTP listener: bind `127.0.0.1:0`, serve MCP over it,
//! and hand the CLI the URL as an ordinary `http` server config. That is the
//! whole of what the Go SDK does here, and it is why in-process servers reach
//! the CLI through `--mcp-config` rather than through the initialize message's
//! `sdkMcpServers` — see [`super::process::initialize_msg`] for why naming one
//! there would silently break it.
//!
//! ## Who implements the protocol
//!
//! Go's `StartInProcessMCPServer` takes an `*mcp.Server` from the official
//! `modelcontextprotocol/go-sdk` and hosts it with that SDK's
//! `NewStreamableHTTPHandler`. This module does the same thing with `rmcp`, the
//! same project's Rust SDK: the caller hands over an [`rmcp::ServerHandler`],
//! this module owns the listener. #281 shipped a hand-rolled `McpService`
//! trait instead, because nothing had an MCP server to host yet; #282 settled
//! it before phase 4 could settle it by accident. `desktop/CLAUDE.md` →
//! "Hosting a tool" carries the reasoning.
//!
//! Go's `ServeStdioMCP` / `SelfAsStdioMCPServer` — hosting a server over the
//! current process's stdin/stdout — are **not** ported. Nothing in Agento calls
//! them; the whole point of this PR is that there is exactly one way to host a
//! tool, and a second, untested one would undo that. [`McpStdioServer`] the
//! *config* stays, because `internal/config/mcp.go` builds one for every
//! external server in `mcps.yaml` — that is describing somebody else's
//! subprocess, not hosting our own.
//!
//! [`McpStdioServer`]: super::options::McpStdioServer
//!
//! ## The transport is stateless, and that is a choice
//!
//! `rmcp`'s streamable-HTTP service defaults to sessions: a POST is answered on
//! an SSE stream, the client carries an `Mcp-Session-Id`, and the server keeps
//! per-session state. This module turns that off (`server_config()`) because an
//! in-process tool server has **no server-to-client traffic at all** — no
//! sampling, no elicitation, no progress notifications, nothing that needs a
//! stream held open. What is left is exactly the shape this module already had:
//! a POST carrying one JSON-RPC message, answered with one JSON reply, or with
//! `202 Accepted` when the message is a notification and by definition has none.
//!
//! Stateless is spec-legal on both counts a client can notice — a server MAY
//! decline to assign a session id, and MUST answer `405` to the `GET` that
//! opens the optional server-initiated stream — and it is what removes session
//! expiry, session storage and a per-session task from the seven servers a
//! fully-integrated desktop app runs (six integrations plus `internal/tools`).
//! Both properties are asserted in this module's own tests, not only in the
//! `#[ignore]`d live suite: they are the two things a client notices, and a
//! POST behaves identically in either mode, so nothing else here would catch a
//! regression.
//!
//! Turning sessions back on is a **two**-line change — `legacy_session_mode`
//! plus the session manager, which is [`NeverSessionManager`] precisely so that
//! "stateless" is a property of the type rather than of a flag someone could
//! flip halfway.
//!
//! ## Shutdown is graceful, like Go's
//!
//! Dropping an [`InProcessMcpServer`] stops the listener accepting and lets the
//! requests already on a connection finish, which is what
//! `httpServer.Shutdown(context.Background())` does on the Go side. The
//! difference is not academic: since #311 a `PUT /api/integrations/{id}`
//! reloads the integration's server unconditionally, so "a tool call in flight
//! during a shutdown" is an ordinary Tuesday rather than a race to reason about.
//!
//! It rests on one line's placement — the transport's `CancellationToken` is
//! fired **after** `axum::serve` returns, not as the shutdown signal — because
//! that token is the parent of every tool call's, so signalling with it aborts
//! the outbound HTTP request the handler is waiting on. Both halves are still
//! wanted: the abort is what stops a detached handler outliving its listener.
//!
//! ## Every server carries a bearer token
//!
//! This is the one place the port does **more** than Go, deliberately. Go's
//! `StartInProcessMCPServer` binds an unauthenticated loopback port; from phase
//! 4 that port answers `tools/call` using the user's live Slack, GitHub and
//! Google credentials, and loopback is not a boundary between processes — any
//! other program running as the user can dial it. The browser vector is already
//! closed (the transport requires non-safelisted headers, so a page's `fetch`
//! is preflighted and gets a bare `405`, and `allowed_hosts` blocks DNS
//! rebinding), but nothing stopped a local process.
//!
//! So each server mints a random token at start and requires it as
//! `Authorization: Bearer …`. It travels to the CLI in [`McpHttpServer`]'s
//! `headers` map — a field that existed and was always empty — which the CLI
//! sends on every request to that server; verified against the real CLI
//! (2.1.224) and covered by `tests/claude_mcp_live.rs`. It costs one header
//! compare per request.
//!
//! **What it does not buy**, stated so nobody over-trusts it: `--mcp-config` is
//! inline JSON in the subprocess's argv (`options.rs`), so the token is legible
//! to anything that can read `/proc/<pid>/cmdline` — the same user always, and
//! any local user on a default Linux. What is closed is the caller that can
//! only *speak HTTP* to a port it found: it now needs a secret it has no way to
//! guess, where before the port alone was enough. Code already running as this
//! user is not in scope and never was — it can read the integration credentials
//! out of `agento.db` without going near an MCP port.

use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServerHandler;

use super::errors::{Error, Result};
use super::options::McpHttpServer;

/// The streamable-HTTP settings every in-process server is hosted under.
///
/// `legacy_session_mode: false` plus `json_response: true` is the stateless
/// request/response mode described in the module docs. `allowed_hosts` keeps
/// `rmcp`'s loopback-only default: the listener binds `127.0.0.1`, so a request
/// arriving under any other `Host` is a DNS-rebinding attempt rather than the
/// CLI — the same reasoning `server/guards.go` applies on the Go side.
fn server_config() -> StreamableHttpServerConfig {
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
    config.json_response = true;
    config
}

/// Compares two credentials without an early exit, so a wrong token leaks
/// nothing about the right one through response timing.
fn credentials_match(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A running in-process MCP server.
///
/// Dropping this stops the listener. Go ties the server's lifetime to a
/// context; here it is tied to the handle, so a caller that forgets it does not
/// leak a bound port for the life of the process.
pub struct InProcessMcpServer {
    config: McpHttpServer,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl InProcessMcpServer {
    /// The config to hand to [`super::Options::with_mcp_server`].
    pub fn config(&self) -> &McpHttpServer {
        &self.config
    }

    /// The URL the CLI will dial.
    pub fn url(&self) -> &str {
        &self.config.url
    }
}

impl Drop for InProcessMcpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// Starts an HTTP MCP server for `service` and returns a handle carrying the
/// config to pass to [`super::Options::with_mcp_server`].
///
/// The listener is bound to a random port on `127.0.0.1` and stops when the
/// returned handle is dropped.
///
/// `service` is cloned per request rather than shared behind a reference,
/// because that is the shape `rmcp` asks for — its transport builds a handler
/// per incoming message so a stateful server can hold per-session data. The
/// bound is `Clone` rather than a factory closure so the call site stays Go's:
/// one server value, handed over once. [`super::ToolServer`] clones as a
/// `HashMap` of `Arc`'d handlers, so every clone shares the captured
/// credentials exactly as Go's single `*mcp.Server` does.
pub async fn start_in_process_mcp_server<S>(name: &str, service: S) -> Result<InProcessMcpServer>
where
    S: ServerHandler + Clone,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| Error::Other(format!("claude: mcp {name:?}: listen: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| Error::Other(format!("claude: mcp {name:?}: local address: {e}")))?;

    let config = server_config();
    // Cancelling this is what tears down any work the transport still holds
    // after the listener is gone; dropping the axum listener alone would leave
    // it running. It is also the parent of every tool call's
    // `CancellationToken`, which is why it is fired **after** the graceful
    // shutdown completes rather than as the shutdown signal — see below.
    let cancel = config.cancellation_token.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(service.clone()),
        // Not `LocalSessionManager`: `legacy_session_mode: false` makes the
        // session map unreachable, and a store nothing can write to is a
        // question every later reader has to re-answer.
        Arc::new(NeverSessionManager::default()),
        config,
    );

    // 122 bits from the OS CSPRNG, which is what `Uuid::new_v4` is. Not a
    // secret worth rotating — it lives and dies with this listener.
    let expected = format!("Bearer {}", uuid::Uuid::new_v4().simple());

    // `fallback_service` rather than a route: Go mounts the handler as the
    // whole `http.Server`, so every path reaches it, and the CLI is free to
    // dial the bare origin or any path under it.
    let app = axum::Router::new()
        .fallback_service(mcp)
        .layer(axum::middleware::from_fn({
            let expected = expected.clone();
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let expected = expected.clone();
                async move {
                    let presented = request
                        .headers()
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default();
                    if credentials_match(presented, &expected) {
                        next.run(request).await
                    } else {
                        axum::response::IntoResponse::into_response((
                            axum::http::StatusCode::UNAUTHORIZED,
                            "Unauthorized",
                        ))
                    }
                }
            }
        }));

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server_name = name.to_string();
    tokio::spawn(async move {
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
        // **Shutdown is graceful, and the ordering here is the whole of it.**
        //
        // Go stops the server with `httpServer.Shutdown(context.Background())`
        // and gives each tool handler the *HTTP request's* context, so a
        // `tools/call` in flight when the server ctx is cancelled runs to
        // completion and its response is delivered. `#311` reloads the server
        // on every `PUT /api/integrations/{id}`, which makes that window
        // routine rather than exotic: a model mid-`create_issue` when the user
        // saves the integration form gets its answer.
        //
        // `cancel` is the parent of every tool call's `CancellationToken`, so
        // firing it *as* the shutdown signal aborted the outbound HTTP request
        // the handler was awaiting — the response the client was still on the
        // connection for became an error. Firing it after `serve` returns keeps
        // the teardown (an orphaned detached handler cannot outlive the
        // listener) without the abort.
        cancel.cancel();
        if let Err(e) = served {
            log::warn!("claude: mcp {server_name:?}: server stopped: {e}");
        }
    });

    // Several of these run at once on ports the OS picked, so "which port is
    // Slack on" is otherwise unanswerable from a log. The token is not logged.
    log::info!("claude: mcp {name:?}: serving on http://{addr}");

    Ok(InProcessMcpServer {
        config: McpHttpServer {
            server_type: "http".to_string(),
            url: format!("http://{addr}"),
            headers: [("Authorization".to_string(), expected)]
                .into_iter()
                .collect(),
        },
        shutdown: Some(shutdown_tx),
    })
}

#[cfg(test)]
mod tests {
    use super::super::tool::{new_tool, CancellationToken, ToolServer};
    use super::*;
    use rmcp::model::{CallToolResult, ContentBlock};

    /// What a conforming client sends: the two headers the MCP streamable-HTTP
    /// transport requires of every POST (a request missing either is answered
    /// `406`/`415` before it reaches the server), plus whatever the handle's
    /// own config carries — which is where the bearer token lives, exactly as
    /// it does for the CLI.
    async fn post(server: &InProcessMcpServer, body: &'static str) -> reqwest::Response {
        let mut request = reqwest::Client::new()
            .post(server.url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (name, value) in &server.config().headers {
            request = request.header(name, value);
        }
        request.body(body).send().await.unwrap()
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct EchoInput {
        /// What to echo back.
        text: String,
    }

    fn echo_server() -> ToolServer {
        ToolServer::new("probe").with_tool(new_tool(
            "echo",
            "Echoes its input back.",
            |input: EchoInput, _ct| async move {
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    input.text,
                )]))
            },
        ))
    }

    #[tokio::test]
    async fn the_server_binds_loopback_and_reports_an_http_config() {
        let server = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();

        assert_eq!(server.config().server_type, "http");
        assert!(
            server.url().starts_with("http://127.0.0.1:"),
            "must not be reachable off-host: {}",
            server.url()
        );
    }

    #[tokio::test]
    async fn a_request_is_answered_and_a_notification_is_only_accepted() {
        let server = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();

        let response = post(&server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await;
        assert_eq!(response.status(), 200);
        // Parsed by hand rather than via reqwest's `json` feature: this is
        // the only caller, and the feature would ship in the release binary.
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["tools"][0]["name"], "echo");

        let response = post(
            &server,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;
        assert_eq!(response.status(), 202, "a notification has no reply");
    }

    #[tokio::test]
    async fn a_tool_call_reaches_the_handler() {
        let server = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();

        let response = post(
            &server,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call",
                "params":{"name":"echo","arguments":{"text":"pong"}}}"#,
        )
        .await;
        assert_eq!(response.status(), 200);
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(body["result"]["content"][0]["text"], "pong");
    }

    #[tokio::test]
    async fn dropping_the_handle_stops_the_listener() {
        let server = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();
        let url = server.url().to_string();
        drop(server);

        // Graceful shutdown is not instantaneous; poll briefly rather than
        // racing it.
        let client = reqwest::Client::new();
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if client.post(&url).body("{}").send().await.is_err() {
                return;
            }
        }
        panic!("the listener outlived its handle");
    }

    /// A `tools/call` already in flight when the handle is dropped still gets
    /// its answer — Go's `Shutdown(context.Background())` is graceful, and
    /// #311's unconditional reload on every integration `PUT` is what makes
    /// that window routine.
    ///
    /// The tool sleeps rather than blocking on anything real so the drop can be
    /// timed against it; what it proves is the ordering, which is that the
    /// transport's cancellation token is fired after `serve` returns and not as
    /// the shutdown signal. With the two swapped this test gets a transport
    /// error instead of a result.
    ///
    /// The drop is sequenced on a signal from inside the handler rather than on
    /// a sleep, so a loaded machine cannot turn "the POST had not arrived yet"
    /// into a failure.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tool_call_in_flight_survives_the_handles_drop() {
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        struct SlowInput {
            /// Milliseconds to wait before answering.
            millis: u64,
        }

        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let server = start_in_process_mcp_server(
            "probe",
            ToolServer::new("probe").with_tool(new_tool(
                "slow",
                "Answers after a delay.",
                move |input: SlowInput, ct: CancellationToken| {
                    let started = started_tx.clone();
                    async move {
                        let _ = started.send(());
                        // Waiting on the token as well as on the clock is what
                        // makes an abort observable as a *different answer*
                        // rather than as a hang: it is the same token a
                        // cancelled turn fires, and it is the parent token this
                        // test is about.
                        tokio::select! {
                            () = tokio::time::sleep(
                                std::time::Duration::from_millis(input.millis)) => {
                                Ok(CallToolResult::success(vec![ContentBlock::text("done")]))
                            }
                            () = ct.cancelled() => Err("cancelled".to_string()),
                        }
                    }
                },
            )),
        )
        .await
        .unwrap();

        let url = server.url().to_string();
        let headers = server.config().headers.clone();
        let call = tokio::spawn(async move {
            let mut request = reqwest::Client::new()
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream");
            for (name, value) in &headers {
                request = request.header(name, value);
            }
            request
                .body(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call",
                        "params":{"name":"slow","arguments":{"millis":400}}}"#,
                )
                .send()
                .await
        });

        started_rx.recv().await.expect("the handler is running");
        drop(server);

        let response = call.await.expect("the request task").expect("a response");
        assert_eq!(response.status(), 200);
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(
            body["result"]["content"][0]["text"], "done",
            "an in-flight tool call must run to completion across a shutdown, \
             as it does in Go: {body}"
        );
    }

    // The two properties `server_config()`'s stateless mode is *for*. Every
    // other test here posts, and a POST is answered identically in either
    // session mode — so without these two a flipped `legacy_session_mode`
    // reaches `desktop` with the suite green.

    #[tokio::test]
    async fn initialize_mints_no_session_id() {
        let server = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();

        let response = post(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"probe","version":"0"}}}"#,
        )
        .await;

        assert_eq!(response.status(), 200);
        assert!(
            response.headers().get("mcp-session-id").is_none(),
            "a stateless server assigns no session: {:?}",
            response.headers()
        );
    }

    #[tokio::test]
    async fn the_stream_get_is_method_not_allowed() {
        let server = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();

        let mut request = reqwest::Client::new()
            .get(server.url())
            .header("Accept", "text/event-stream");
        for (name, value) in &server.config().headers {
            request = request.header(name, value);
        }
        let response = request.send().await.unwrap();

        // The spec's MUST for a server that does not offer the optional
        // server-initiated stream.
        assert_eq!(response.status(), 405);
    }

    #[tokio::test]
    async fn a_request_without_the_bearer_token_is_rejected() {
        let server = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();

        let token = server
            .config()
            .headers
            .get("Authorization")
            .expect("the handle carries the token the CLI must send")
            .clone();
        assert!(token.starts_with("Bearer "));

        let unauthenticated = reqwest::Client::new()
            .post(server.url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(
            unauthenticated.status(),
            401,
            "another local process must not be able to call these tools"
        );

        let wrong = reqwest::Client::new()
            .post(server.url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Authorization", "Bearer 0000")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 401);
    }

    #[tokio::test]
    async fn two_servers_do_not_share_a_token() {
        let first = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();
        let second = start_in_process_mcp_server("probe", echo_server())
            .await
            .unwrap();

        let borrowed = reqwest::Client::new()
            .post(second.url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Authorization", &first.config().headers["Authorization"])
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .send()
            .await
            .unwrap();

        assert_eq!(
            borrowed.status(),
            401,
            "an integration's token must not open another integration's tools"
        );
    }
}

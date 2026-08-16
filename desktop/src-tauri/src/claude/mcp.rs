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
//! expiry, session storage and a per-session task from eight servers running in
//! a desktop app. Flipping `legacy_session_mode` back on in `server_config()`
//! is the one-line escape hatch if a future CLI turns out to need one.

use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServerHandler;

use super::errors::{Error, Result};
use super::options::{McpHttpServer, McpStdioServer};

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
    // Cancelling this is what tears down any work the transport has in flight;
    // dropping the axum listener alone would leave it running.
    let cancel = config.cancellation_token.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(service.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    // `fallback_service` rather than a route: Go mounts the handler as the
    // whole `http.Server`, so every path reaches it, and the CLI is free to
    // dial the bare origin or any path under it.
    let app = axum::Router::new().fallback_service(mcp);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server_name = name.to_string();
    tokio::spawn(async move {
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
                cancel.cancel();
            })
            .await;
        if let Err(e) = served {
            log::warn!("claude: mcp {server_name:?}: server stopped: {e}");
        }
    });

    Ok(InProcessMcpServer {
        config: McpHttpServer {
            server_type: "http".to_string(),
            url: format!("http://{addr}"),
            headers: Default::default(),
        },
        shutdown: Some(shutdown_tx),
    })
}

/// Runs `service` as an MCP stdio server, reading from stdin and writing to
/// stdout. Intended for a standalone binary registered via [`McpStdioServer`].
///
/// Blocks until stdin closes.
pub async fn serve_stdio_mcp<S>(service: S) -> Result<()>
where
    S: ServerHandler,
{
    use rmcp::ServiceExt;

    let running = service
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| Error::wrap("serving MCP over stdio", e))?;
    running
        .waiting()
        .await
        .map_err(|e| Error::wrap("serving MCP over stdio", e))?;
    Ok(())
}

/// Returns an [`McpStdioServer`] that runs the current binary with the given
/// extra arguments — the self-invoking MCP stdio pattern, where one executable
/// is both the client and, under a flag, the server.
pub fn self_as_stdio_mcp_server<I, S>(extra_args: I) -> Result<McpStdioServer>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let exe = std::env::current_exe()
        .map_err(|e| Error::wrap("resolve executable", e))?
        .to_string_lossy()
        .into_owned();

    Ok(McpStdioServer {
        server_type: "stdio".to_string(),
        command: exe,
        args: extra_args.into_iter().map(Into::into).collect(),
        env: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::tool::{new_tool, ToolServer};
    use super::*;
    use rmcp::model::{CallToolResult, ContentBlock};

    /// The headers the MCP streamable-HTTP transport requires of every POST. A
    /// request missing either is answered `406`/`415` before it reaches the
    /// server, so the tests send what a conforming client sends.
    async fn post(url: &str, body: &'static str) -> reqwest::Response {
        reqwest::Client::new()
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(body)
            .send()
            .await
            .unwrap()
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
            |input: EchoInput| async move {
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

        let response = post(
            server.url(),
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;
        assert_eq!(response.status(), 200);
        // Parsed by hand rather than via reqwest's `json` feature: this is
        // the only caller, and the feature would ship in the release binary.
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["tools"][0]["name"], "echo");

        let response = post(
            server.url(),
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
            server.url(),
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

    #[test]
    fn the_self_stdio_config_points_at_this_binary() {
        let cfg = self_as_stdio_mcp_server(["--mcp-server"]).unwrap();
        assert_eq!(cfg.server_type, "stdio");
        assert!(!cfg.command.is_empty());
        assert_eq!(cfg.args, vec!["--mcp-server"]);
    }
}

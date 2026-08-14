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
//! ## One deliberate divergence from the Go SDK
//!
//! Go's `StartInProcessMCPServer` takes an `*mcp.Server` from the official
//! `modelcontextprotocol/go-sdk` and hosts it with that SDK's
//! `NewStreamableHTTPHandler`. This port takes an [`McpService`] — a trait for
//! "handle one JSON-RPC message" — and owns only the transport.
//!
//! The reason is scope, and it is worth stating plainly rather than
//! discovering later: the MCP *protocol* implementation (initialize,
//! `tools/list`, `tools/call`, schema generation) is what the Go MCP SDK
//! supplies, and on the Rust side that is `rmcp`. Nothing in this application
//! has an MCP server to host yet — Agento's seven integrations are phase 4 of
//! the port — so binding this module to `rmcp`'s type surface now would add a
//! large dependency to satisfy no caller, and would pin an API that phase 4
//! should be free to choose. The trait keeps the seam exactly where Go's is
//! (the SDK owns the listener, the caller owns the tools), and an `rmcp`
//! service is one adapter away.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::value::RawValue;

use super::errors::{Error, Result};
use super::options::{McpHttpServer, McpStdioServer};

/// The future an [`McpService`] returns: the JSON-RPC response, or `None` for a
/// notification, which by definition has no reply.
pub type McpResponseFuture = Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>;

/// Something that can answer MCP JSON-RPC messages.
///
/// Implementors own the MCP protocol itself; this module owns getting messages
/// to and from them.
pub trait McpService: Send + Sync + 'static {
    /// Handles one JSON-RPC message. Returning `None` means "no reply", which
    /// is the correct answer for a notification and the only case where the
    /// HTTP transport answers `202 Accepted`.
    fn handle(&self, message: Box<RawValue>) -> McpResponseFuture;
}

impl<F> McpService for F
where
    F: Fn(Box<RawValue>) -> McpResponseFuture + Send + Sync + 'static,
{
    fn handle(&self, message: Box<RawValue>) -> McpResponseFuture {
        self(message)
    }
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
pub async fn start_in_process_mcp_server(
    name: &str,
    service: impl McpService,
) -> Result<InProcessMcpServer> {
    let service = Arc::new(service);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| Error::Other(format!("claude: mcp {name:?}: listen: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| Error::Other(format!("claude: mcp {name:?}: local address: {e}")))?;

    let app = axum::Router::new().route(
        "/",
        axum::routing::post({
            let service = service.clone();
            move |body: String| {
                let service = service.clone();
                async move {
                    let Ok(message) = RawValue::from_string(body) else {
                        // A malformed body is a parse error in JSON-RPC terms,
                        // but the transport's job is only to say it could not
                        // be read.
                        return axum::http::Response::builder()
                            .status(axum::http::StatusCode::BAD_REQUEST)
                            .body(axum::body::Body::empty())
                            .expect("a static response always builds");
                    };

                    match service.handle(message).await {
                        Some(response) => {
                            let body = serde_json::to_vec(&response).unwrap_or_default();
                            axum::http::Response::builder()
                                .status(axum::http::StatusCode::OK)
                                .header(axum::http::header::CONTENT_TYPE, "application/json")
                                .body(axum::body::Body::from(body))
                                .expect("a static response always builds")
                        }
                        // A notification has no reply; 202 is what the
                        // streamable-HTTP transport says to answer.
                        None => axum::http::Response::builder()
                            .status(axum::http::StatusCode::ACCEPTED)
                            .body(axum::body::Body::empty())
                            .expect("a static response always builds"),
                    }
                }
            }
        }),
    );

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server_name = name.to_string();
    tokio::spawn(async move {
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
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
pub async fn serve_stdio_mcp(service: impl McpService) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| Error::wrap("reading stdin", e))?
    {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = RawValue::from_string(line) else {
            continue;
        };
        if let Some(response) = service.handle(message).await {
            let mut encoded = serde_json::to_vec(&response)
                .map_err(|e| Error::wrap("encoding an MCP response", e))?;
            encoded.push(b'\n');
            stdout
                .write_all(&encoded)
                .await
                .map_err(|e| Error::wrap("writing stdout", e))?;
            stdout
                .flush()
                .await
                .map_err(|e| Error::wrap("flushing stdout", e))?;
        }
    }

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
    use super::*;

    fn echo_service() -> impl McpService {
        |message: Box<RawValue>| -> McpResponseFuture {
            Box::pin(async move {
                let parsed: serde_json::Value =
                    serde_json::from_str(message.get()).unwrap_or_default();
                // A notification has no id, and therefore no reply.
                let id = parsed.get("id")?.clone();
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "echo": parsed.get("method").cloned() },
                }))
            })
        }
    }

    #[tokio::test]
    async fn the_server_binds_loopback_and_reports_an_http_config() {
        let server = start_in_process_mcp_server("probe", echo_service())
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
        let server = start_in_process_mcp_server("probe", echo_service())
            .await
            .unwrap();
        let client = reqwest::Client::new();

        let response = client
            .post(server.url())
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["echo"], "tools/list");

        let response = client
            .post(server.url())
            .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202, "a notification has no reply");
    }

    #[tokio::test]
    async fn dropping_the_handle_stops_the_listener() {
        let server = start_in_process_mcp_server("probe", echo_service())
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

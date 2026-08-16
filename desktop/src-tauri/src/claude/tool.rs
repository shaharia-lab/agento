//! The typed-tool layer, ported from `claude/tool.go`.
//!
//! This is the ergonomic half of hosting tools: [`mcp`](super::mcp) owns the
//! listener, and this owns "a tool is a function". Define an input struct,
//! write an async function over it, and the JSON Schema the CLI needs is
//! derived from the struct rather than written by hand — the job
//! `jsonschema:` struct tags do on the Go side, done here by `schemars`.
//!
//! ```no_run
//! # use agento_lib::claude::{new_tool, tool_server};
//! # use rmcp::model::CallToolResult;
//! # use schemars::JsonSchema;
//! #[derive(serde::Deserialize, JsonSchema)]
//! struct CurrentTimeInput {
//!     /// IANA timezone name, e.g. UTC or America/New_York. Defaults to UTC.
//!     timezone: Option<String>,
//! }
//!
//! # async fn example() -> agento_lib::claude::Result<()> {
//! let server = tool_server(
//!     "local-tools",
//!     [new_tool(
//!         "current_time",
//!         "Returns the current date and time for a given IANA timezone.",
//!         |input: CurrentTimeInput| async move {
//!             let tz = input.timezone.unwrap_or_else(|| "UTC".to_string());
//!             Ok(CallToolResult::success(vec![rmcp::model::ContentBlock::text(tz)]))
//!         },
//!     )],
//! )
//! .await?;
//! # let _ = server;
//! # Ok(())
//! # }
//! ```
//!
//! ## Two departures from Go, both deliberate
//!
//! **Tools are added at runtime, not derived at compile time.** `rmcp` ships
//! `#[tool_router]` / `#[tool]` macros that build a fixed tool set from an
//! `impl` block. Agento cannot use them: every integration registers only the
//! tools its own `services[].tools` allowlist names, over credentials read from
//! the database at start time, so the set is not known until the server is
//! built. [`ToolServer`] is therefore a value with [`add_tool`](ToolServer::add_tool),
//! which is what Go's `mcp.AddTool(server, …)` loop is. The macros are switched
//! off in `Cargo.toml` so there is exactly one way to declare a tool.
//!
//! **`NewTool[In, Out]`'s `Out` has no counterpart.** Go's handler returns
//! `(*mcp.CallToolResult, Out, error)`, where `Out` is the *structured* result.
//! No Agento tool uses it — all sixty-odd return `nil` there — and `rmcp` folds
//! the same thing into [`CallToolResult::structured_content`], so a caller that
//! wants one sets that field instead of naming a second type parameter.

use std::future::Future;

use rmcp::handler::server::router::tool::{
    CallToolHandlerExt, IntoToolRoute, ToolRoute, ToolRouter,
};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use super::errors::Result;
use super::mcp::{start_in_process_mcp_server, InProcessMcpServer};

/// One tool: its name, its description, the schema derived from its input type,
/// and the handler. Build these with [`new_tool`] and give them to a
/// [`ToolServer`].
///
/// Go's `ToolDef` is a closure that registers itself on an `*mcp.Server`;
/// here it is the route itself, because `rmcp`'s router is a value that can be
/// added to rather than a server that must already exist.
pub struct ToolDef(ToolRoute<ToolServer>);

impl ToolDef {
    /// The tool's name, as the CLI will see it (before the `mcp__<server>__`
    /// prefix the CLI adds).
    pub fn name(&self) -> &str {
        self.0.name()
    }
}

/// Creates a [`ToolDef`] from a name, a description, and a handler over a
/// strongly-typed input.
///
/// `In` supplies the tool's JSON Schema through `schemars`, so the description
/// the model reads for each field is the doc comment on that field.
///
/// An `Err(ErrorData)` becomes a JSON-RPC error the CLI surfaces as a failed
/// tool call, which is what returning a non-nil `error` does in Go. A failure
/// the *model* should see and retry belongs in an `Ok` carrying
/// `is_error: true` instead.
pub fn new_tool<In, F, Fut>(
    name: impl Into<std::borrow::Cow<'static, str>>,
    description: impl Into<std::borrow::Cow<'static, str>>,
    handler: F,
) -> ToolDef
where
    In: JsonSchema + DeserializeOwned + Send + 'static,
    F: Fn(In) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = std::result::Result<CallToolResult, ErrorData>> + Send + 'static,
{
    let call = move |Parameters(input): Parameters<In>| handler(input);
    ToolDef(
        call.name(name)
            .description(description)
            .parameters::<In>()
            .into_tool_route(),
    )
}

/// An MCP server whose whole surface is a set of [`ToolDef`]s.
///
/// Cloning is cheap and shares the handlers — the transport clones one of these
/// per request, so a clone must mean "the same server", exactly as Go's single
/// `*mcp.Server` shared across sessions does.
#[derive(Clone)]
pub struct ToolServer {
    info: ServerInfo,
    router: ToolRouter<ToolServer>,
}

impl ToolServer {
    /// A server named `name`, with no tools yet.
    ///
    /// `name` is the server's own identity in the initialize handshake. What
    /// the CLI actually prefixes tool names with is the **key** the server is
    /// registered under in [`super::Options::with_mcp_server`] — a tool
    /// `send_message` under the key `telegram-42` is
    /// `mcp__telegram-42__send_message` in an agent's allowlist. [`tool_server`]
    /// and [`super::Options::with_tools`] pass one string to both, so the two
    /// only diverge if a caller wires them up by hand.
    pub fn new(name: impl Into<String>) -> Self {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new(name, super::SDK_VERSION);
        Self {
            info,
            router: ToolRouter::new(),
        }
    }

    /// Adds one tool. The builder form of [`add_tool`](Self::add_tool).
    #[must_use]
    pub fn with_tool(mut self, tool: ToolDef) -> Self {
        self.add_tool(tool);
        self
    }

    /// Adds every tool in `tools`. The builder form of [`add_tool`](Self::add_tool).
    #[must_use]
    pub fn with_tools(mut self, tools: impl IntoIterator<Item = ToolDef>) -> Self {
        for tool in tools {
            self.add_tool(tool);
        }
        self
    }

    /// Adds one tool, replacing any tool already registered under its name.
    ///
    /// This is the call an integration makes inside its allowlist check, and
    /// the reason a [`ToolServer`] is a mutable value rather than a type.
    pub fn add_tool(&mut self, tool: ToolDef) {
        self.router.add_route(tool.0);
    }

    /// The registered tool names, sorted — what an integration reports as the
    /// tools it exposes.
    pub fn tool_names(&self) -> Vec<String> {
        self.router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect()
    }
}

impl ServerHandler for ToolServer {
    fn get_info(&self) -> ServerInfo {
        self.info.clone()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.router.list_all()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        self.router
            .call(ToolCallContext::new(self, request, context))
            .await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.router.get(name).cloned()
    }
}

/// Builds a [`ToolServer`] from `tools` and starts it on a random loopback
/// port. Go's `ToolServer(ctx, name, tools...)`.
///
/// The listener stops when the returned handle is dropped — the port's
/// lifetime is the handle's, which is what stands in for Go's `ctx` throughout
/// this SDK.
pub async fn tool_server(
    name: &str,
    tools: impl IntoIterator<Item = ToolDef>,
) -> Result<InProcessMcpServer> {
    start_in_process_mcp_server(name, ToolServer::new(name).with_tools(tools)).await
}

impl super::Options {
    /// Starts a [`tool_server`] and registers it under `name`. Go's
    /// `WithTools`.
    ///
    /// The handle comes back alongside the options because it *is* the
    /// server's lifetime. Go's version hides that: its listener dies with the
    /// `ctx` the caller already holds, so an `Option` alone is enough. Here
    /// there is no context to attach to, and an `Options` that owned the handle
    /// would tie the listener to a value that is `Clone` — two clones of one
    /// run's options would then disagree about when the port closes. Keeping
    /// the handle is the caller's job, and dropping it is how the server stops.
    ///
    /// ```no_run
    /// # use agento_lib::claude::{new_tool, Options};
    /// # async fn example(tool: agento_lib::claude::ToolDef) -> agento_lib::claude::Result<()> {
    /// let (options, _tools) = Options::new().with_tools("my-tools", [tool]).await?;
    /// let mut stream = agento_lib::claude::query("Add 2+3", options).await?;
    /// # let _ = &mut stream;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_tools(
        self,
        name: &str,
        tools: impl IntoIterator<Item = ToolDef>,
    ) -> Result<(Self, InProcessMcpServer)> {
        let server = tool_server(name, tools).await?;
        let options = self.with_mcp_server(name, server.config())?;
        Ok((options, server))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ContentBlock;

    #[derive(serde::Deserialize, JsonSchema)]
    struct AddInput {
        /// The left operand.
        a: i64,
        /// The right operand.
        b: i64,
    }

    fn add_tool() -> ToolDef {
        new_tool("add", "Adds two numbers.", |input: AddInput| async move {
            Ok(CallToolResult::success(vec![ContentBlock::text(
                (input.a + input.b).to_string(),
            )]))
        })
    }

    #[test]
    fn the_input_type_supplies_the_schema() {
        let server = ToolServer::new("calc").with_tool(add_tool());
        let tool = server.get_tool("add").expect("the tool is registered");

        assert_eq!(tool.description.as_deref(), Some("Adds two numbers."));
        let schema = serde_json::to_value(&*tool.input_schema).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["a"]["type"], "integer");
        assert_eq!(
            schema["properties"]["b"]["description"], "The right operand.",
            "a field's doc comment is what the model reads"
        );
        // Both fields are non-`Option`, so both are required — the property
        // `jsonschema:\"required,…\"` carries on the Go side.
        let required: Vec<_> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(required.contains(&"a".to_string()) && required.contains(&"b".to_string()));
    }

    #[test]
    fn tools_are_chosen_at_runtime() {
        // The shape every integration needs: register only what the
        // integration's allowlist names.
        let allowed = ["add"];
        let mut server = ToolServer::new("calc");
        if allowed.contains(&"add") {
            server.add_tool(add_tool());
        }
        if allowed.contains(&"subtract") {
            server.add_tool(new_tool(
                "subtract",
                "Subtracts.",
                |input: AddInput| async move {
                    Ok(CallToolResult::success(vec![ContentBlock::text(
                        (input.a - input.b).to_string(),
                    )]))
                },
            ));
        }

        assert_eq!(server.tool_names(), vec!["add".to_string()]);
    }

    #[test]
    fn a_clone_is_the_same_server() {
        let server = ToolServer::new("calc").with_tool(add_tool());
        let clone = server.clone();
        assert_eq!(server.tool_names(), clone.tool_names());
    }

    #[tokio::test]
    async fn with_tools_registers_the_server_it_started() {
        let (options, server) = super::super::Options::new()
            .with_tools("calc", [add_tool()])
            .await
            .unwrap();

        let registered = options.mcp_servers.get("calc").expect("registered by name");
        assert_eq!(registered["type"], "http");
        assert_eq!(registered["url"], server.url());
    }

    #[tokio::test]
    async fn the_convenience_starter_hosts_the_tools() {
        let server = tool_server("calc", [add_tool()]).await.unwrap();
        assert!(server.url().starts_with("http://127.0.0.1:"));

        let response = reqwest::Client::new()
            .post(server.url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":"add","arguments":{"a":2,"b":3}}}"#,
            )
            .send()
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(body["result"]["content"][0]["text"], "5");
    }
}

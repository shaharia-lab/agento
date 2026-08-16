//! The typed-tool layer, ported from `claude/tool.go`.
//!
//! This is the ergonomic half of hosting tools: [`mcp`](super::mcp) owns the
//! listener, and this owns "a tool is a function". Define an input struct,
//! write an async function over it, and the JSON Schema the CLI needs is
//! derived from the struct rather than written by hand — the job
//! `jsonschema:` struct tags do on the Go side, done here by `schemars`.
//!
//! The example is the shape **every** ported tool takes, and the two things it
//! does that are not obvious are the two things all 62 of them need: it
//! captures a credential, and it clones that credential per call.
//!
//! ```no_run
//! # use agento_lib::claude::{new_tool, tool_server, CancellationToken};
//! # use rmcp::model::{CallToolResult, ContentBlock};
//! # use schemars::JsonSchema;
//! #[derive(serde::Deserialize, JsonSchema)]
//! struct SendMessageInput {
//!     /// The chat to post to.
//!     chat_id: String,
//!     /// The message text.
//!     text: String,
//! }
//!
//! # async fn post_to_telegram(
//! #     bot_token: String,
//! #     input: SendMessageInput,
//! #     ct: CancellationToken,
//! # ) -> std::result::Result<String, std::io::Error> { Ok(String::new()) }
//! # async fn example(bot_token: String) -> agento_lib::claude::Result<()> {
//! let server = tool_server(
//!     "telegram-42",
//!     [new_tool(
//!         "send_message",
//!         "Sends a message to a Telegram chat.",
//!         move |input: SendMessageInput, ct: CancellationToken| {
//!             // Cloned here, *outside* the async block, and this is not
//!             // stylistic: moving `bot_token` into the block would consume the
//!             // closure's own capture, making it `FnOnce` where `new_tool`
//!             // needs `Fn` — the tool is called many times.
//!             let bot_token = bot_token.clone();
//!             async move {
//!                 let body = post_to_telegram(bot_token, input, ct)
//!                     .await
//!                     .map_err(|e| format!("telegram: send_message: {e}"))?;
//!                 Ok(CallToolResult::success(vec![ContentBlock::text(body)]))
//!             }
//!         },
//!     )],
//! )
//! .await?;
//! # let _ = server;
//! # Ok(())
//! # }
//! ```
//!
//! Capture is the *only* channel a handler has for its credentials. `rmcp`
//! offers a second form where the handler is a method taking `&S` — the server
//! value — but [`ToolServer`] is one shared type across every integration and
//! holds nothing integration-specific, so there is no `&self` to read a bot
//! token from. What a tool needs, it closes over.
//!
//! ## Three departures from Go, all deliberate
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
//!
//! **Go's `ctx` becomes a [`CancellationToken`], and it is a real parameter
//! rather than a dropped one.** Go's handler signature is
//! `func(ctx, *mcp.CallToolRequest, In)`; the request is `_` at every Agento
//! call site, so it is not carried here, but `ctx` is threaded into every
//! `http.NewRequestWithContext` an integration makes — cancelling the turn
//! aborts the outbound Slack/GitHub/Google call. Rust does **not** inherit that
//! for free: `rmcp` spawns a tool handler as a detached task and cancellation
//! only cancels the token, never the task (`rmcp::service`'s serve loop calls
//! `ct.cancel()` on the request's token and nothing else). A handler with no
//! token to watch runs to completion after the caller has gone. So the token is
//! in the signature, and a handler that makes a network call is expected to
//! honour it — with `reqwest`, `tokio::select!` on `ct.cancelled()`. Widening
//! the signature later would have been a 62-site edit.

use std::future::Future;

use rmcp::handler::server::router::tool::{
    CallToolHandlerExt, IntoToolRoute, ToolRoute, ToolRouter,
};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
pub use tokio_util::sync::CancellationToken;

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
/// # The error type is a `String`, and the model reads it
///
/// This is Go's convention, ported exactly. `claude.NewTool` registers through
/// `mcp.AddTool`, whose `ToolHandlerFor` documents that "an error result is
/// treated as a tool error, rather than a protocol error, and is therefore
/// packed into `CallToolResult.Content`, with `IsError` set" — so every
/// `return nil, nil, fmt.Errorf("github: list issues: %w", err)` in an Agento
/// integration is **text the model sees and retries on**. That is the whole
/// reason those messages are written the way they are.
///
/// So a handler returns `Result<CallToolResult, String>` and the wrapper turns
/// an `Err` into [`CallToolResult::error`] — a successful JSON-RPC response
/// carrying `is_error: true` and the message as its text. There is no way to
/// raise a *protocol* error from here, exactly as there is none in Go: `rmcp`
/// would render one as "Tool result missing due to internal error", which tells
/// the model nothing and gives it nothing to retry against.
///
/// The practical consequence for a port is that `?` needs a `String`, which
/// means every fallible call carries its own `.map_err(|e| format!("…: {e}"))`.
/// That is not friction to route around — it is the same context Go's
/// `fmt.Errorf` wrap supplies, and it is the message the model gets.
///
/// The [`CancellationToken`] is the run's; see the module docs for why it is in
/// the signature rather than dropped.
pub fn new_tool<In, F, Fut>(
    name: impl Into<std::borrow::Cow<'static, str>>,
    description: impl Into<std::borrow::Cow<'static, str>>,
    handler: F,
) -> ToolDef
where
    In: JsonSchema + DeserializeOwned + Send + 'static,
    F: Fn(In, CancellationToken) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = std::result::Result<CallToolResult, String>> + Send + 'static,
{
    let call = move |Parameters(input): Parameters<In>, ct: CancellationToken| {
        let running = handler(input, ct);
        async move {
            running
                .await
                .unwrap_or_else(|message| CallToolResult::error(vec![ContentBlock::text(message)]))
        }
    };
    let mut route = call
        .name(name)
        .description(description)
        .parameters::<In>()
        .into_tool_route();
    normalize_go_schema(&mut route.attr);
    ToolDef(route)
}

/// Makes a `schemars` schema say what `google/jsonschema-go` says, for the
/// shapes where the difference is mechanical.
///
/// A tool's input schema is not internal: `tools/list` hands it to the CLI and
/// the CLI hands it to the model as the tool's `input_schema`. Any key one
/// reflector emits and the other does not is a field in front of the model that
/// the same tool hosted by the Go server does not have — on the one surface
/// (#310–#317) where the two are meant to be indistinguishable.
///
/// Three removals, and they are the three that are safe to make blindly —
/// each is a key `jsonschema.For` can **never** emit, so removing it can only
/// move a Rust schema towards Go's and never away from it:
///
/// 1. **`$schema`** — the dialect key `schemars` stamps on every schema it
///    generates. `jsonschema-go` emits none, ever.
/// 2. **`format`** — `schemars` writes `"format": "int64"` for an `i64`,
///    `"uint32"` for a `u32` and so on. `Schema.Format` exists on the Go side
///    but only a hand-written schema fills it, and no Agento tool writes one.
///    #312's twenty GitHub tools — six of which take an `int` or an `int64` —
///    would otherwise diverge on every one of them.
/// 3. **`default`** — `#[serde(default)]` is the Rust shape that reproduces
///    Go's `omitempty` (see below), and `schemars` reads it as metadata worth
///    advertising: a defaulted `String` field picks up `"default": ""`.
///    `jsonschema.For` sets no defaults, so the field would otherwise be
///    optional in both languages and *described* differently in one. Removing
///    it here is what keeps `#[serde(default)]` the whole of the guidance
///    rather than a two-attribute incantation every port has to remember.
///
/// The walk is **structure-aware, not a key sweep**: all three are perfectly
/// ordinary property *names*, so they are removed only where a subschema sits,
/// and recursion follows the positions `schemars` can put one in.
///
/// # What is deliberately not reconciled, and what a port must write instead
///
/// `desktop/parity/jsonschema_reflect_vectors.json` is the generated map of
/// every shape class, with the Rust half in [`super::schema_vectors`]. Three
/// divergences are left standing because the fix belongs in the *port*, where
/// the intent is known, rather than in a rewriter that would have to guess:
///
/// - **`Option<T>` is not an optional Go field.** `schemars` renders
///   `"type": ["string","null"]`; a Go field with `omitempty` keeps
///   `"type":"string"` and merely leaves `required`. Write
///   `#[serde(default)] field: String`, not `Option<String>`. (Not needed by
///   #312 at all: no params struct in `internal/integrations/github/` carries
///   `omitempty`, so every field of all twenty tools is required.)
/// - **A nested struct is inlined by Go** and lifted into `$defs`/`$ref` by
///   `schemars`. Every Go params struct in the six integrations is flat; a port
///   that needs nesting has to inline it, and that work belongs to the port
///   that needs it rather than to a general de-referencer here.
/// - **A sized integer carries bounds in Go, not a format.** `int32` reflects
///   as `minimum`/`maximum` and `uint` as `minimum: 0`. Use `i64` for a Go
///   `int` or `int64` — which is what every integer field in the integrations
///   is — and nothing has to be reconciled.
///
/// # Why the `Arc` is replaced rather than written through
///
/// `rmcp` memoizes one generated schema per input type and hands every
/// `ToolRoute` a clone of the same `Arc`, so `get_mut` would refuse and an
/// in-place edit would reach into a process-wide cache. The clone is one map,
/// once per tool at start-up.
fn normalize_go_schema(tool: &mut Tool) {
    let mut schema = (*tool.input_schema).clone();
    if !normalize_object(&mut schema) {
        return;
    }
    tool.input_schema = std::sync::Arc::new(schema);
}

/// One subschema: drop the two keys, then recurse. Reports whether anything
/// changed, so an already-clean schema keeps its memoized `Arc`.
fn normalize_object(schema: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    let mut changed = schema.remove("$schema").is_some();
    changed |= schema.remove("format").is_some();
    changed |= schema.remove("default").is_some();

    // Positions holding a single subschema. The last six are unreachable from
    // the integrations as they stand — nothing there is conditional or uses
    // `flatten` — but they are what a *future* one would hit, and the failure
    // is silent: a nested `$schema`/`format`/`default` under an unwalked
    // keyword survives into a schema the model reads and nothing says so.
    // `unevaluatedProperties`/`unevaluatedItems` are what `schemars` 1.x emits
    // for `#[serde(flatten)]` under `deny_unknown_fields`, which is on for every
    // params struct in this port.
    for key in [
        "items",
        "additionalProperties",
        "propertyNames",
        "not",
        "unevaluatedProperties",
        "unevaluatedItems",
        "if",
        "then",
        "else",
        "contains",
    ] {
        if let Some(serde_json::Value::Object(child)) = schema.get_mut(key) {
            changed |= normalize_object(child);
        }
    }
    // Positions holding a map of name → subschema. `properties` is why this
    // walk is structure-aware: a tool may legitimately have a field called
    // `format`, and a blind key sweep would delete it.
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(serde_json::Value::Object(children)) = schema.get_mut(key) {
            for child in children.values_mut() {
                if let serde_json::Value::Object(child) = child {
                    changed |= normalize_object(child);
                }
            }
        }
    }
    // Positions holding a list of subschemas.
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(serde_json::Value::Array(children)) = schema.get_mut(key) {
            for child in children.iter_mut() {
                if let serde_json::Value::Object(child) = child {
                    changed |= normalize_object(child);
                }
            }
        }
    }
    changed
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
        new_tool(
            "add",
            "Adds two numbers.",
            |input: AddInput, _ct| async move {
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    (input.a + input.b).to_string(),
                )]))
            },
        )
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

    /// `schemars` stamps a `$schema` dialect key on everything it generates and
    /// `google/jsonschema-go` stamps none, so the key would be a field the model
    /// sees from a Rust-hosted tool and not from the same Go-hosted one.
    #[test]
    fn the_schema_carries_no_dialect_key() {
        let server = ToolServer::new("calc").with_tool(add_tool());
        let tool = server.get_tool("add").expect("the tool is registered");
        let schema = serde_json::to_value(&*tool.input_schema).unwrap();
        assert!(
            schema.get("$schema").is_none(),
            "Go's reflected schemas carry no dialect key: {schema}"
        );
    }

    /// `jsonschema.For` never sets `Format`, so a `schemars` `"format"` is a
    /// key the model would see from a Rust-hosted tool and not from the Go one.
    /// Six of #312's twenty GitHub tools take an integer, so this is not a
    /// corner.
    #[test]
    fn an_integer_carries_no_format_key() {
        let server = ToolServer::new("calc").with_tool(add_tool());
        let tool = server.get_tool("add").expect("the tool is registered");
        let schema = serde_json::to_value(&*tool.input_schema).unwrap();
        assert_eq!(schema["properties"]["a"]["type"], "integer");
        assert!(
            schema["properties"]["a"].get("format").is_none(),
            "Go reflects an int64 as a bare integer: {schema}"
        );
    }

    /// The reason the walk is structure-aware. `format` is a legal field name,
    /// and a tool that has one must keep the property while still losing the
    /// *keyword* on it.
    #[test]
    fn a_property_named_format_survives_the_normalization() {
        #[allow(dead_code)] // reflected for its schema, never deserialized
        #[derive(serde::Deserialize, JsonSchema)]
        struct ExportInput {
            /// The output format.
            format: String,
            /// How many rows.
            limit: i64,
        }

        let server = ToolServer::new("export").with_tool(new_tool(
            "export",
            "Exports.",
            |_input: ExportInput, _ct| async move { Ok(CallToolResult::success(vec![])) },
        ));
        let tool = server.get_tool("export").expect("registered");
        let schema = serde_json::to_value(&*tool.input_schema).unwrap();
        assert_eq!(schema["properties"]["format"]["type"], "string");
        assert_eq!(
            schema["properties"]["format"]["description"],
            "The output format."
        );
        assert!(schema["properties"]["limit"].get("format").is_none());
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
                |input: AddInput, _ct| async move {
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

    /// One `tools/call`, sent the way the CLI sends it — including whatever
    /// headers the handle's config carries.
    async fn call(server: &InProcessMcpServer, body: &'static str) -> serde_json::Value {
        let mut request = reqwest::Client::new()
            .post(server.url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (name, value) in &server.config().headers {
            request = request.header(name, value);
        }
        let response = request.body(body).send().await.unwrap();
        serde_json::from_str(&response.text().await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn the_convenience_starter_hosts_the_tools() {
        let server = tool_server("calc", [add_tool()]).await.unwrap();
        assert!(server.url().starts_with("http://127.0.0.1:"));

        let body = call(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"add","arguments":{"a":2,"b":3}}}"#,
        )
        .await;
        assert_eq!(body["result"]["content"][0]["text"], "5");
    }

    #[tokio::test]
    async fn a_failing_handler_is_a_tool_error_the_model_reads() {
        // The Go convention this ports: `return nil, nil, fmt.Errorf(…)` is
        // text the model sees and retries on, not a JSON-RPC error it never
        // gets shown.
        let server = tool_server(
            "calc",
            [new_tool(
                "divide",
                "Divides.",
                |input: AddInput, _ct| async move {
                    if input.b == 0 {
                        return Err("calc: divide: division by zero".to_string());
                    }
                    Ok(CallToolResult::success(vec![ContentBlock::text(
                        (input.a / input.b).to_string(),
                    )]))
                },
            )],
        )
        .await
        .unwrap();

        let body = call(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"divide","arguments":{"a":1,"b":0}}}"#,
        )
        .await;

        assert!(
            body.get("error").is_none(),
            "a failing tool must not be a protocol error: {body}"
        );
        assert_eq!(body["result"]["isError"], true);
        assert_eq!(
            body["result"]["content"][0]["text"],
            "calc: divide: division by zero"
        );
    }

    #[tokio::test]
    async fn a_captured_credential_survives_repeated_calls() {
        // The shape every ported tool takes: the closure owns the credential
        // and hands out a clone per call. If it moved the value into the async
        // block instead the closure would be `FnOnce` and this would not
        // compile, which is the mistake the module docs call out.
        let api_key = "s3cr3t".to_string();
        let server = tool_server(
            "calc",
            [new_tool(
                "whoami",
                "Reports the captured credential.",
                move |_input: AddInput, _ct: CancellationToken| {
                    let api_key = api_key.clone();
                    async move { Ok(CallToolResult::success(vec![ContentBlock::text(api_key)])) }
                },
            )],
        )
        .await
        .unwrap();

        for _ in 0..3 {
            let body = call(
                &server,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":"whoami","arguments":{"a":0,"b":0}}}"#,
            )
            .await;
            assert_eq!(body["result"]["content"][0]["text"], "s3cr3t");
        }
    }

    // `flavor = "multi_thread"`: the request is in flight on another task while
    // this one drops the handle, and a current-thread runtime would not run
    // both.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_server_cancels_an_in_flight_handler() {
        // The reason the token is in the signature at all. `rmcp` spawns a tool
        // handler detached — cancelling a request cancels its token and nothing
        // else — so a handler that does not watch it keeps its outbound HTTP
        // call alive after the caller has gone.
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (cancelled_tx, mut cancelled_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

        let server = tool_server(
            "calc",
            [new_tool(
                "wait",
                "Waits to be cancelled.",
                move |_input: AddInput, ct: CancellationToken| {
                    let started = started_tx.clone();
                    let cancelled = cancelled_tx.clone();
                    async move {
                        let _ = started.send(());
                        ct.cancelled().await;
                        let _ = cancelled.send(());
                        Ok(CallToolResult::success(vec![]))
                    }
                },
            )],
        )
        .await
        .unwrap();

        let url = server.url().to_string();
        let auth = server.config().headers["Authorization"].clone();
        let in_flight = tokio::spawn(async move {
            let _ = reqwest::Client::new()
                .post(url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("Authorization", auth)
                .body(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call",
                        "params":{"name":"wait","arguments":{"a":0,"b":0}}}"#,
                )
                .send()
                .await;
        });

        started_rx.recv().await.expect("the handler ran");
        drop(server);

        tokio::time::timeout(std::time::Duration::from_secs(5), cancelled_rx.recv())
            .await
            .expect("the handler's token was never cancelled")
            .expect("the handler observed the cancellation");
        in_flight.abort();
    }
}

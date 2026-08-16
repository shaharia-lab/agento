//! The local in-process tools MCP server, ported from `internal/tools`.
//!
//! This is the first real caller of the typed-tool layer #282 settled
//! ([`crate::claude::new_tool`] / [`crate::claude::tool_server`]), and it is
//! deliberately the smallest one: no OAuth, no network, no credentials — one
//! tool over the machine clock. Everything the six integrations will need is
//! exercised here except the credential capture.
//!
//! ## What has to match Go exactly, and why
//!
//! The server's **name** is the prefix the CLI puts on every tool it hosts:
//! `local-tools` makes `current_time` reachable as
//! `mcp__local-tools__current_time`. That string is in the agent's stored
//! `capabilities.local` allowlist *and* in every `tool_use` block already
//! written to `chat_messages`, so renaming either half silently breaks agents
//! that exist. [`allowed_tool_name`] is the one place the two are joined,
//! exactly as `LocalMCPConfig.AllowedToolName` is on the Go side.
//!
//! ## One departure from Go, and it is invisible on the wire
//!
//! Go starts this server **once per process** (`cmd/web.go`, `cmd/ask.go`) and
//! hands every run the same `McpHTTPServer`. Here the listener's life is its
//! handle's — the rule the whole SDK port runs on — so
//! [`start_local_mcp_server`] is called per turn and the handle is held by the
//! turn's stream task. Nothing observable follows from that: the URL is
//! argv-only, it is never stored, and a per-turn port closes when the
//! subprocess it was started for is gone. What it buys is that a crashed or
//! forwarded turn leaks no bound port.
//!
//! The other difference is the version in the initialize handshake — Go's
//! `mcp.Implementation` says `1.0.0`, [`crate::claude::ToolServer`] reports the
//! SDK's. Neither reaches the CLI's transcript, and there is deliberately no
//! second way to build a server just to carry a different string.
//!
//! ## Malformed arguments: same kind, different wording
//!
//! This is a property of [`crate::claude::new_tool`], not of `current_time`, so
//! every ported tool inherits it. For all four malformed-input classes — a
//! missing field, an extra field (`deny_unknown_fields`), a wrong type, and an
//! absent `arguments` object — the **kind** of failure already matches Go: the
//! `modelcontextprotocol/go-sdk` server returns a `CallToolResult` with
//! `IsError` rather than a JSON-RPC error, and `rmcp`'s
//! `into_tool_argument_error` intercepts exactly the `INVALID_PARAMS` its own
//! `Parameters` extractor raises and converts it to `CallToolResult::error`.
//! Acceptance is therefore identical: the same inputs are refused on both
//! sides, and the model gets something it can retry against either way.
//!
//! What differs is the **message text** the model reads: Go says
//! `validating "arguments": …`, `rmcp` says `failed to deserialize parameters:
//! …`, each followed by its own reflector's account of what was wrong. Neither
//! is reachable from the vectors, which drive `format_current_time` directly,
//! so it is written down here rather than pinned — and it is a wording
//! difference, not a missing conversion to go and add.

mod current_time;

/// Re-exported for the parity vectors, which drive the formatting directly at a
/// fixed instant. [`local_tools`] is how the tool itself is reached.
pub use current_time::format_current_time;

use crate::claude::{tool_server, InProcessMcpServer, Result, ToolDef};

/// The MCP server name every local tool is hosted under — `LocalMCPServerName`.
///
/// Load-bearing: it is half of every qualified tool name in an agent allowlist
/// and in every stored `tool_use` block.
pub const LOCAL_MCP_SERVER_NAME: &str = "local-tools";

/// The fully qualified name the CLI knows a local tool by, and the string an
/// agent's allowlist has to contain — `LocalMCPConfig.AllowedToolName`.
pub fn allowed_tool_name(tool_name: &str) -> String {
    format!("mcp__{LOCAL_MCP_SERVER_NAME}__{tool_name}")
}

/// [`allowed_tool_name`] over a subset — `LocalMCPConfig.AllowedToolNames`.
///
/// Note what it does **not** do: it never checks that the name is one this
/// server registers. Go does not either, and that is the behaviour to keep —
/// an agent naming a local tool that no longer exists gets a qualified name in
/// its allowlist and a model that cannot call it, rather than a run that fails
/// to start.
pub fn allowed_tool_names<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .map(|name| allowed_tool_name(name.as_ref()))
        .collect()
}

/// Every local tool, as `StartLocalMCPServer` registers them.
///
/// This *is* the list — there is deliberately no second `&[&str]` of names
/// beside it. Go's `LocalMCPConfig.ToolNames` is such a list, hand-copied from
/// the `mcp.AddTool` calls above it and, as of #310, read by nothing; a Rust
/// copy would be a third statement of the same truth, free to drift from the
/// registration it claims to describe. What a caller actually wants —
/// "what is hosted?" — is answered by [`crate::claude::ToolServer::tool_names`],
/// which is derived from the registered set and so cannot disagree with it.
pub fn local_tools() -> Vec<ToolDef> {
    vec![current_time::tool()]
}

/// Starts the local tools server on a random loopback port —
/// `StartLocalMCPServer`.
///
/// The listener stops when the returned handle is dropped, so the caller owns
/// its lifetime; see the module docs for why that is per-turn here and
/// per-process in Go.
pub async fn start_local_mcp_server() -> Result<InProcessMcpServer> {
    tool_server(LOCAL_MCP_SERVER_NAME, local_tools()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string an existing agent's allowlist and every stored `tool_use`
    /// block already contains. Spelled out rather than derived, because a test
    /// that rebuilt it from the constants would pass through a rename.
    #[test]
    fn the_qualified_name_is_the_one_already_on_disk() {
        assert_eq!(
            allowed_tool_name("current_time"),
            "mcp__local-tools__current_time"
        );
        assert_eq!(
            allowed_tool_names(["current_time"]),
            vec!["mcp__local-tools__current_time".to_string()]
        );
    }

    /// Go appends a qualified name for whatever the agent asked for, registered
    /// or not.
    #[test]
    fn an_unregistered_name_is_still_qualified() {
        assert_eq!(
            allowed_tool_names(["nope"]),
            vec!["mcp__local-tools__nope".to_string()]
        );
        assert!(allowed_tool_names(Vec::<String>::new()).is_empty());
    }

    /// The hosted set, spelled out — for the reason
    /// `the_qualified_name_is_the_one_already_on_disk` spells its string out: a
    /// list rebuilt from the registration agrees with it by construction and so
    /// could never notice a tool being renamed or dropped. It stays a fixture
    /// local to this test rather than a constant of the module, because nothing
    /// in the crate should be reading a hand-written copy of what
    /// [`local_tools`] already is.
    ///
    /// It is not folded into the vector test below either, because that one
    /// needs `desktop/parity/local_tools_vectors.json` on disk and answers a
    /// different question — "does Rust host what Go hosts?" rather than "is
    /// this still the set we think we wrote?".
    ///
    /// Both UIs carry their own copy of this list —
    /// `desktop/src/views/AgentsView.tsx` and `frontend/src/types.ts` — so
    /// adding a tool means editing them too; neither can be reached from here.
    #[test]
    fn the_server_hosts_exactly_the_named_tools() {
        const LOCAL_TOOL_NAMES: &[&str] = &["current_time"];

        let server =
            crate::claude::ToolServer::new(LOCAL_MCP_SERVER_NAME).with_tools(local_tools());
        assert_eq!(server.tool_names(), LOCAL_TOOL_NAMES);
    }

    /// One `tools/call` over the real transport, which is the only way to see
    /// the bytes the CLI will see.
    #[tokio::test]
    async fn a_call_over_the_wire_answers_gos_sentence() {
        let server = start_local_mcp_server().await.expect("start");
        let mut request = reqwest::Client::new()
            .post(server.url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (name, value) in &server.config().headers {
            request = request.header(name, value);
        }
        let response = request
            .body(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":"current_time","arguments":{"timezone":"UTC"}}}"#,
            )
            .send()
            .await
            .expect("send");
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.expect("text")).expect("json");

        let text = body["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content: {body}"));
        assert!(text.starts_with("Current time in UTC: "), "{text}");
        assert!(text.contains(" (ISO 8601: "), "{text}");
        assert_eq!(body["result"]["content"][0]["type"], "text");
    }

    /// A bad zone is a **tool** error the model can retry on, never a JSON-RPC
    /// one — the convention `new_tool` ports from `mcp.AddTool`.
    #[tokio::test]
    async fn a_bad_timezone_is_a_tool_error_not_a_protocol_error() {
        let server = start_local_mcp_server().await.expect("start");
        let mut request = reqwest::Client::new()
            .post(server.url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (name, value) in &server.config().headers {
            request = request.header(name, value);
        }
        let response = request
            .body(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":"current_time","arguments":{"timezone":"Nowhere/Bad"}}}"#,
            )
            .send()
            .await
            .expect("send");
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.expect("text")).expect("json");

        assert!(body.get("error").is_none(), "{body}");
        assert_eq!(body["result"]["isError"], true);
        assert_eq!(
            body["result"]["content"][0]["text"],
            "unknown timezone \"Nowhere/Bad\": unknown time zone Nowhere/Bad"
        );
    }

    // ─── The Go vectors ───────────────────────────────────────────────────────
    //
    // `desktop/parity/local_tools_vectors.json` is generated from the Go
    // implementation and asserted against it by
    // `desktop/parity/local_tools_parity_test.go`. These read the same file, so
    // a change on either side fails the other language.

    #[derive(serde::Deserialize)]
    struct ToolVector {
        name: String,
        qualified_name: String,
        description: String,
        input_schema: serde_json::Value,
    }

    #[derive(serde::Deserialize)]
    struct CurrentTimeVector {
        timezone: String,
        at: String,
        #[serde(default)]
        want: String,
        #[serde(default)]
        error: String,
    }

    #[derive(serde::Deserialize)]
    struct Vectors {
        server_name: String,
        tools: Vec<ToolVector>,
        current_time: Vec<CurrentTimeVector>,
    }

    fn vectors() -> Vectors {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../parity/local_tools_vectors.json"
        );
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
        serde_json::from_str(&raw).expect("parsing local tools vectors")
    }

    /// The advertised surface: the server name, every tool name, its
    /// description, and the reflected input schema — all taken from a live
    /// `tools/list` against the Go server.
    ///
    /// The schema is compared as a value rather than as bytes because JSON
    /// object order is not meaningful to the CLI or to `schemars`; what is
    /// meaningful is that no key appears on one side and not the other, which
    /// is what value equality checks. `$schema` is exactly such a key —
    /// `schemars` stamps one and `google/jsonschema-go` does not, which is why
    /// [`crate::claude::new_tool`] strips it.
    #[test]
    fn the_advertised_surface_matches_the_go_vectors() {
        use rmcp::ServerHandler;

        let v = vectors();
        assert_eq!(v.server_name, LOCAL_MCP_SERVER_NAME);
        assert!(!v.tools.is_empty(), "vectors look truncated");

        let server =
            crate::claude::ToolServer::new(LOCAL_MCP_SERVER_NAME).with_tools(local_tools());
        assert_eq!(
            server.tool_names(),
            v.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            "the hosted tool set is not Go's"
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
            assert_eq!(allowed_tool_name(&want.name), want.qualified_name);
        }
    }

    /// The answer text, including the two failure sentences — every one of them
    /// a byte Go produced.
    #[test]
    fn current_time_matches_the_go_vectors() {
        let v = vectors();
        assert!(v.current_time.len() >= 20, "vectors look truncated");
        for case in v.current_time {
            let at = chrono::DateTime::parse_from_rfc3339(&case.at)
                .unwrap_or_else(|e| panic!("vector instant {:?}: {e}", case.at))
                .with_timezone(&chrono::Utc);
            let got = format_current_time(&case.timezone, at);
            if case.error.is_empty() {
                assert_eq!(
                    got.as_deref(),
                    Ok(case.want.as_str()),
                    "current_time({:?}, {})",
                    case.timezone,
                    case.at
                );
            } else {
                assert_eq!(
                    got.as_ref().err().map(String::as_str),
                    Some(case.error.as_str()),
                    "current_time({:?}, {})",
                    case.timezone,
                    case.at
                );
            }
        }
    }
}

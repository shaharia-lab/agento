//! The in-process MCP server, checked against a **real MCP client**.
//!
//! `src/claude/mcp.rs` hosts tools over a *stateless* streamable-HTTP
//! transport: no `Mcp-Session-Id`, and `405` for the `GET` that opens the
//! optional server-initiated stream. Both are spec-legal, and the unit tests
//! prove the server answers correctly — but they prove it by sending the bytes
//! the port itself believes are right, which is exactly the thing that cannot
//! catch a wrong belief. The Go side does not have this problem: it hands the
//! job to `modelcontextprotocol/go-sdk`'s own handler, whose defaults are
//! whatever that SDK ships.
//!
//! The same argument covers the second thing a real client has to agree with:
//! every server requires an `Authorization: Bearer …` that travels in the
//! config's `headers` map, and nothing in-process can prove the CLI sends it.
//!
//! So this suite asks the Claude Code CLI — the only client that will ever dial
//! these servers — to connect to one and list its tools. It is `#[ignore]`d
//! because it needs the CLI installed and a signed-in profile; `cargo test
//! --test claude_mcp_live -- --ignored` runs it. Run it whenever the transport
//! settings in `server_config()` change, which is the one edit these tests
//! exist to guard.

use std::process::Command;

use agento_lib::claude::{new_tool, tool_server};
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;

#[derive(serde::Deserialize, JsonSchema)]
struct EchoInput {
    /// What to echo back.
    text: String,
}

/// Skips rather than fails when the CLI is absent, the way the SDK suite skips
/// without `python3`.
fn claude_cli() -> Option<&'static str> {
    Command::new("claude")
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|_| "claude")
}

// A multi-thread runtime, and that is not incidental: `Command::output` blocks,
// and on the default current-thread runtime it would starve the listener task
// this test exists to reach — the CLI would time out against a server that
// never got scheduled.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the real claude CLI and a signed-in profile"]
async fn the_cli_connects_to_a_stateless_in_process_server() {
    let Some(cli) = claude_cli() else {
        eprintln!("skipping: no claude CLI on PATH");
        return;
    };

    let server = tool_server(
        "agento-probe",
        [new_tool(
            "echo",
            "Echoes its input back.",
            |input: EchoInput, _ct| async move {
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    input.text,
                )]))
            },
        )],
    )
    .await
    .unwrap();

    // `mcp add --scope local` records the server against this working
    // directory, so a scratch cwd keeps the developer's own MCP config alone.
    // `.mcp.json` would not do: a project-scope server the user has not
    // approved is listed as pending and never dialled, so the check would pass
    // without connecting to anything.
    let dir = tempfile::tempdir().unwrap();

    // `add-json` rather than `add --transport http`, because the config handed
    // over is the *serialized `McpHttpServer`* — the same bytes
    // `Options::with_mcp_server` puts in `--mcp-config`, headers and all. This
    // is what makes the bearer token part of what the live run proves: without
    // the header the CLI would get a `401` and report a failed connection.
    let config = serde_json::to_string(server.config()).unwrap();
    let added = Command::new(cli)
        .args(["mcp", "add-json", "--scope", "local"])
        .arg("agento-probe")
        .arg(&config)
        .current_dir(dir.path())
        .output()
        .expect("claude mcp add-json runs");
    assert!(
        added.status.success(),
        "claude mcp add-json failed: {}",
        String::from_utf8_lossy(&added.stderr)
    );

    // `mcp get` health-checks: it performs the full initialize handshake and
    // then lists the tools it found.
    let got = Command::new(cli)
        .args(["mcp", "get", "agento-probe"])
        .current_dir(dir.path())
        .output()
        .expect("claude mcp get runs");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&got.stdout),
        String::from_utf8_lossy(&got.stderr)
    );

    let _ = Command::new(cli)
        .args(["mcp", "remove", "--scope", "local", "agento-probe"])
        .current_dir(dir.path())
        .output();

    // "Connected" is the whole assertion, and it is enough: the CLI reaches it
    // only by completing the initialize handshake over this transport, with the
    // token, or it would have been turned away with a `401`. It does not print
    // the tool list, so the tools themselves are covered by the unit tests —
    // what cannot be covered there is whether a real client accepts a server
    // that issues no session id, answers `405` to the stream `GET`, and demands
    // a header the config told it to send.
    assert!(
        report.contains("Connected"),
        "the CLI could not connect to the stateless server:\n{report}"
    );
}

//! `rmcp`'s own diagnostics have to reach the app's log, and nothing in the
//! type system says they do.
//!
//! `rmcp` instruments itself with `tracing`; this app logs through
//! `tauri_plugin_log`, which is a `log` implementation; and nothing installs a
//! `tracing` subscriber. Left alone, every protocol-layer event is discarded —
//! including the one below, which is the transport telling us somebody dialled
//! a loopback MCP port under a foreign `Host`. `Cargo.toml` turns on `tracing`'s
//! `log` feature to bridge the two, and this asserts the bridge is real rather
//! than assumed: the feature only forwards *while no `tracing` subscriber has
//! been installed*, so a future dependency that installs one would silence
//! these again, and this test is what would notice.
//!
//! Its own integration binary because it installs a process-global `log`
//! implementation, which can only be done once.

use std::sync::Mutex;

use agento_lib::claude::{new_tool, tool_server, CancellationToken, InProcessMcpServer};
use rmcp::model::CallToolResult;
use schemars::JsonSchema;

static RECORDS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        RECORDS
            .lock()
            .unwrap()
            .push((record.target().to_string(), record.args().to_string()));
    }

    fn flush(&self) {}
}

fn captured() -> Vec<(String, String)> {
    RECORDS.lock().unwrap().clone()
}

#[derive(serde::Deserialize, JsonSchema)]
struct EchoInput {
    /// What to echo back.
    text: String,
}

async fn probe() -> InProcessMcpServer {
    tool_server(
        "probe",
        [new_tool(
            "echo",
            "Echoes its input back.",
            |input: EchoInput, _ct: CancellationToken| async move {
                Ok(CallToolResult::success(vec![
                    rmcp::model::ContentBlock::text(input.text),
                ]))
            },
        )],
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn rmcp_tracing_events_are_forwarded_to_the_log_crate() {
    log::set_logger(&Capture).expect("this binary owns the global logger");
    log::set_max_level(log::LevelFilter::Trace);

    let server = probe().await;

    // A `Host` the server is not served under. `rmcp` answers `403` and warns
    // through `tracing`; the warning is the thing under test.
    let response = reqwest::Client::new()
        .post(server.url())
        .header("Host", "attacker.example.com")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header(
            "Authorization",
            &server.config().headers["Authorization"].clone(),
        )
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403, "the guard itself must still fire");

    let records = captured();
    assert!(
        records
            .iter()
            .any(|(target, message)| target.starts_with("rmcp")
                && message.contains("possible DNS rebinding attempt")),
        "rmcp's tracing warning never reached the log crate: {records:?}"
    );

    // The server's own start line, so "which port is Slack on" is answerable.
    assert!(
        records
            .iter()
            .any(|(_, message)| message.contains("claude: mcp \"probe\": serving on")),
        "the start line is missing: {records:?}"
    );
}

//! Can a real turn **see and call** an in-process MCP tool? (#501)
//!
//! `claude_mcp_live.rs` is the sibling of this file and stops one step short on
//! purpose: it proves the CLI completes the `initialize` handshake against our
//! stateless transport, and its own comment leaves the tool list to the unit
//! tests. That gap is what #501 was reported through — an integration hosting
//! **zero** tools binds a port, serves `initialize`, and logs the same
//! `serving on http://…` line as a healthy one, so "Connected" was true of a
//! server the model could reach nothing through.
//!
//! So this asserts the two facts that were missing, against the real CLI:
//!
//! 1. the tool is **listed** — it appears in the `init` system event's `tools`,
//!    which is the CLI telling us what the model was actually given; and
//! 2. the tool is **called** — a `tool_use` for it appears and its
//!    `tool_result` carries the text our handler produced.
//!
//! **It is driven through `runner::build_options`, not through a hand-built
//! command line**, and that is the point of promoting the scratch probe rather
//! than committing it as it was. The bug was never in the transport (triage
//! confirmed a hand-built `claude -p` reached the tool perfectly); it was in
//! what the option assembly handed that transport. A probe that rebuilds the
//! arguments itself agrees with whatever the test author wrote and cannot see
//! that class of defect at all. Here the server, the `--mcp-config` document,
//! `--strict-mcp-config`, `--allowedTools` and the permission mode all come
//! from the same function a chat turn and a scheduled run use.
//!
//! The local-tools server (#310) is the fixture because it needs no credentials
//! and no database row — the `mcp_plan` branch that reads `integrations` is
//! never entered for an agent whose capabilities name only local tools. What it
//! covers is the assembly, which is shared with every integration.
//!
//! `#[ignore]`d on the same terms as its siblings — it needs the CLI installed,
//! a signed-in profile, and it spends tokens:
//!
//! ```text
//! cargo test --test mcp_e2e_probe -- --ignored --nocapture
//! ```

use std::process::Command;
use std::sync::Arc;

use agento_lib::native::agents::{Agent, Capabilities};
use agento_lib::native::chat::runner::{build_options, RunSpec, TurnSettings};

/// Skips rather than fails when the CLI is absent, as `claude_mcp_live.rs` does.
fn have_claude_cli() -> bool {
    Command::new("claude")
        .arg("--version")
        .output()
        .ok()
        .is_some_and(|out| out.status.success())
}

/// An agent whose only capability is the one local tool.
///
/// `permission_mode` is empty, which in the **no-handler** branch of
/// `build_options` is bypass — the mode a scheduled run gets, and the only one
/// under which nothing is waiting to answer a prompt.
fn spec() -> RunSpec {
    RunSpec {
        agent: Some(Agent {
            name: "probe".into(),
            slug: "probe".into(),
            description: String::new(),
            model: String::new(),
            thinking: "disabled".into(),
            permission_mode: String::new(),
            system_prompt: String::new(),
            capabilities: Capabilities {
                built_in: None,
                local: Some(vec!["current_time".to_string()].into()),
                mcp: None,
            },
            claude_config_dir: String::new(),
        }),
        no_agent_model: Box::new(String::new),
        settings: Arc::new(TurnSettings::none()),
        working_dir: String::new(),
        settings_profile_id: String::new(),
        permission_mode: String::new(),
        resume_session_id: None,
        custom_session_id: String::new(),
    }
}

// A multi-thread runtime for `claude_mcp_live.rs`' reason: the listener task has
// to be scheduled while this test is awaiting the subprocess, or the CLI dials a
// server that never answers.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the real claude CLI and a signed-in profile, and spends tokens"]
async fn a_turn_lists_and_calls_a_tool_through_the_options_a_turn_builds() {
    if !have_claude_cli() {
        eprintln!("skipping: no claude CLI on PATH");
        return;
    }

    let spec = spec();
    // `_servers` is the listener's lifetime — dropping it stops the server and
    // cancels every handler's token, so it has to outlive the stream.
    let (options, _servers) = build_options(&spec, None)
        .await
        .expect("the turn's own option assembly");

    // The assembly's own claims, before a subprocess exists. If these are wrong
    // the CLI half below fails for a reason that is hard to read out of a model
    // transcript, so they are asserted here where the message is exact.
    assert!(
        options.mcp_servers.contains_key("local-tools"),
        "the server is registered under the key the CLI prefixes tool names with"
    );
    assert!(options.strict_mcp_config, "the user's own .mcp.json is out");
    assert_eq!(
        options.allowed_tools,
        vec!["mcp__local-tools__current_time".to_string()]
    );

    let mut stream = agento_lib::claude::query(
        "Call the current_time tool with timezone \"Asia/Tokyo\". \
         Reply with exactly the text the tool returned and nothing else.",
        options,
    )
    .await
    .expect("the CLI spawns");

    // The raw lines are what Agento's SSE forwards verbatim, so collecting them
    // is both the assertion material and the thing a `--nocapture` run shows
    // when this fails.
    let mut listed = false;
    let mut called = false;
    let mut answered = false;
    let mut transcript = String::new();

    while let Some(event) = stream.next_event().await {
        let Some(raw) = event.raw.as_ref() else {
            continue;
        };
        let line = raw.get();
        transcript.push_str(line);
        transcript.push('\n');

        // The `init` system event carries the tool list the CLI gave the model.
        // This is the half no unit test can reach: our own `--allowedTools` says
        // what we asked for, and only the CLI can say what it found.
        if event.system.as_ref().is_some_and(|s| s.subtype == "init")
            && line.contains("mcp__local-tools__current_time")
        {
            listed = true;
        }
        if line.contains("\"tool_use\"") && line.contains("mcp__local-tools__current_time") {
            called = true;
        }
        // `current_time`'s RFC 1123 output for Asia/Tokyo ends in the zone
        // abbreviation, which is the one part of the answer the model cannot
        // have invented from the prompt.
        if line.contains("JST") {
            answered = true;
        }
    }

    assert!(
        listed,
        "the CLI never listed the tool for the model:\n{transcript}"
    );
    assert!(called, "the model never called the tool:\n{transcript}");
    assert!(
        answered,
        "the tool's own output never came back:\n{transcript}"
    );
}

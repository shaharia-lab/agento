//! A spawned agent runs on the login shell's `PATH`, not the one a GUI launch
//! inherited (#588).
//!
//! Driven end to end — `build_options`, then a real spawn of a scripted CLI that
//! records the `PATH` it was given and whether a tool installed only on the
//! login shell's `PATH` resolves — because the property is about the child's
//! environment: asserting `Options::env` alone would still pass with `build_env`
//! ignoring it.
//!
//! **One test per binary**, like `claude_cli_refresh.rs`: the login `PATH` is a
//! process-wide cache, and a second test would read the first's answer.
//!
//! `AGENTO_CLAUDE_EXECUTABLE` is set here on purpose, where the other
//! `claude_cli_*` suites must leave it unset: rule 1 never asks the login shell
//! where the CLI is, and the spawned process's `PATH` must come from it anyway.
//!
//! Verified in both directions: without the `PATH` override in `build_options`
//! the child sees only the inherited entries and `mytool` does not resolve.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agento_lib::native::agents::{Agent, Capabilities};
use agento_lib::native::chat::runner::{build_options, RunSpec, TurnSettings};

/// launchd's environment for a Finder, Dock or Spotlight launch.
const LAUNCHD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// No capabilities, so `build_options` starts no MCP server and opens no
/// database.
fn spec() -> RunSpec {
    RunSpec {
        agent: Some(Agent {
            name: "login-path".into(),
            slug: "login-path".into(),
            description: String::new(),
            model: String::new(),
            thinking: "disabled".into(),
            permission_mode: String::new(),
            system_prompt: String::new(),
            capabilities: Capabilities {
                built_in: None,
                local: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawned_agent_gets_the_login_shells_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");

    // A tool that exists only on the login shell's `PATH` — Homebrew's or
    // nvm's bin directory, as far as the agent can tell.
    let login_bin = tmp.path().join("login-only/bin");
    std::fs::create_dir_all(&login_bin).expect("login bin");
    let tool = write_script(&login_bin, "mytool", "exit 0");

    // A login shell prints chatter, exports its `PATH` and runs what it was
    // given with `-c` (`$1` is `-lic`); `/bin/sh` is named absolutely because
    // the inherited `PATH` below is launchd's.
    let shell = write_script(
        tmp.path(),
        "login-shell",
        &format!(
            "echo 'Now using node v22.11.0'\n\
             PATH='{}':/usr/bin:/bin\n\
             export PATH\n\
             exec /bin/sh -c \"$2\"",
            login_bin.display()
        ),
    );

    let path_out = tmp.path().join("child-path");
    let tool_out = tmp.path().join("child-tool");
    let cli = write_script(
        tmp.path(),
        "claude",
        &format!(
            "printf '%s' \"$PATH\" > '{}'\n\
             command -v mytool > '{}.tmp'\n\
             mv '{}.tmp' '{}'\n\
             exit 1",
            path_out.display(),
            tool_out.display(),
            tool_out.display(),
            tool_out.display()
        ),
    );

    // SAFETY: one test in this binary, so nothing else reads the environment
    // concurrently.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", LAUNCHD_PATH);
        std::env::set_var("SHELL", &shell);
        std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
        std::env::remove_var("CLAUDE_CONFIG_DIR");
    }

    // Spawned, as production reaches `build_options` from a spawned task.
    let options = tokio::spawn(async {
        let spec = spec();
        let (options, _servers, _hosted) = build_options(&spec, None)
            .await
            .expect("the turn's own option assembly");
        options
    })
    .await
    .expect("the option assembly task");

    assert_eq!(
        options.claude_executable,
        cli.to_string_lossy(),
        "AGENTO_CLAUDE_EXECUTABLE is still taken verbatim"
    );

    // The scripted CLI exits without a handshake, so the query itself fails;
    // what is under test happened when the process started.
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        agento_lib::claude::query("hello", options),
    )
    .await;

    let deadline = Instant::now() + Duration::from_secs(10);
    while !tool_out.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let child_path = std::fs::read_to_string(&path_out).expect("the CLI recorded its PATH");
    let entries: Vec<PathBuf> = std::env::split_paths(&child_path).collect();
    assert_eq!(
        entries.first(),
        Some(&login_bin),
        "the login shell's PATH must lead the child's; it got {child_path:?}"
    );
    for inherited in std::env::split_paths(LAUNCHD_PATH) {
        assert!(
            entries.contains(&inherited),
            "the inherited entry {inherited:?} was dropped; the child got {child_path:?}"
        );
    }

    let resolved = std::fs::read_to_string(&tool_out).expect("the CLI looked the tool up");
    assert_eq!(
        resolved.trim(),
        tool.to_string_lossy(),
        "a tool on the login shell's PATH must resolve for the agent"
    );
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

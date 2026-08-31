//! Re-resolving the CLI must not park a runtime worker (#533, #366).
//!
//! The fourth copy of `a_contended_write_lock_does_not_stall_the_runtime`, and
//! it is here rather than beside the other three for one reason: they all reach
//! the runner through `AGENTO_CLAUDE_EXECUTABLE`, which is rule 1 and returns
//! before any walk. A test that sets it cannot see this property at all.
//!
//! What is under test is `runner::claude_executable`'s `spawn_blocking`. Before
//! #533 that function was a `OnceLock` read and inlining it was free; now it can
//! walk the order again, which spawns `$SHELL -lic` bounded at three seconds
//! plus a `--version` bounded at two, through a `std::thread::sleep` poll loop.
//! `build_options` is awaited on the runtime by both the chat turn and the
//! scheduler's executor, so inline that is a multi-second stall on a worker —
//! and #533's own note that it is *bounded* is exactly the argument #366 refused.
//!
//! **One test per binary**, like `claude_cli_refresh.rs`: this primes the
//! process-wide resolution cache, and a second test would run against the
//! first's.
//!
//! Verified in both directions rather than assumed: reverting `runner.rs` to the
//! inline `claude_cli::executable()` takes the longest gap from ~10 ms to the
//! whole of `PROBE`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agento_lib::native::agents::{Agent, Capabilities};
use agento_lib::native::chat::runner::{build_options, RunSpec, TurnSettings};

/// How long the fake login shell takes to answer. Long enough that a parked
/// worker is unmistakable, and well inside `claude_cli`'s own 3 s `PROBE_TIMEOUT`
/// so the walk completes rather than being killed.
const PROBE: Duration = Duration::from_millis(1_500);

/// No capabilities, so `build_options` starts no MCP server and opens no
/// database — the only work left in it is resolving the executable, which is
/// what this measures.
fn spec() -> RunSpec {
    RunSpec {
        agent: Some(Agent {
            name: "offload".into(),
            slug: "offload".into(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn re_resolving_the_cli_does_not_stall_the_runtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let installed = home.join(".local/bin");
    std::fs::create_dir_all(&installed).expect("install dir");
    let cli = write_script(
        &installed,
        "claude",
        "echo '2.1.231 (Claude Code)'\nexit 0\n",
    );

    // A login shell that takes its time, which is what an rc file sourcing a
    // version manager does on a real machine. `sleep` is resolved to an absolute
    // path **now**, while `PATH` is still the real one: the override below
    // empties it so that rule 4 contributes nothing, and a `sleep` the script
    // cannot find fails silently and leaves this measuring an instant probe.
    let sleep_bin = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("sleep"))
                .find(|candidate| candidate.is_file())
        })
        .expect("a sleep binary on PATH");
    let shell = write_script(
        tmp.path(),
        "slow-login-shell",
        &format!("{} {}\nexit 1\n", sleep_bin.display(), PROBE.as_secs_f32()),
    );

    // SAFETY: one test in this binary, so nothing else reads the environment
    // concurrently. `AGENTO_CLAUDE_EXECUTABLE` must stay **unset** — it is rule
    // 1 and returns before any walk, so setting it would make this test pass
    // against the very thing it is written to catch.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", tmp.path().join("no-such-bin"));
        std::env::set_var("SHELL", &shell);
        std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE");
    }

    // Prime off the runtime, so the startup walk is not what is being measured.
    let primed = tokio::task::spawn_blocking(|| agento_lib::claude_cli::prime(None))
        .await
        .expect("prime")
        .expect("the installed CLI is found");
    assert_eq!(primed.path, cli.to_string_lossy());

    // Now break it, so the next resolution walks the order — through the slow
    // shell — instead of stopping at the `stat`.
    std::fs::remove_file(&cli).expect("remove the CLI");

    let worst_gap_ms = Arc::new(AtomicU64::new(0));
    let ticks = Arc::new(AtomicU64::new(0));
    let ticker = {
        // `last` is seeded **before** the spawn: a starved task is never polled,
        // so seeding it on the first poll starts the clock after the stall and
        // the test passes against the defect.
        let (worst_gap_ms, ticks, mut last) = (
            Arc::clone(&worst_gap_ms),
            Arc::clone(&ticks),
            Instant::now(),
        );
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let now = Instant::now();
                let gap = u64::try_from(now.duration_since(last).as_millis()).unwrap_or(u64::MAX);
                worst_gap_ms.fetch_max(gap, Ordering::Relaxed);
                ticks.fetch_add(1, Ordering::Relaxed);
                last = now;
            }
        })
    };

    // **Spawned, not awaited inline**, and that is the difference between a
    // regression test and a decoration. `block_on` drives the test body on the
    // *calling* thread, so blocking there leaves the single worker free and the
    // ticker keeps running — the inline revert passes such a test. Production
    // reaches `build_options` from an axum handler and from the scheduler's
    // executor, both of which are spawned tasks on a worker, so the task has to
    // be spawned here to model it. Confirmed in both directions: with the
    // revert this fails, and inline-awaited it would not.
    let built = tokio::spawn(async move {
        let spec = spec();
        let (options, _servers) = build_options(&spec, None)
            .await
            .expect("the turn's own option assembly");
        options.claude_executable
    });
    let claude_executable = built.await.expect("the option assembly task");
    ticker.abort();

    // The walk really did run — otherwise the measurement below is of nothing.
    assert!(
        agento_lib::claude_cli::cached().is_none(),
        "the CLI was still found, so no walk was paid for and nothing was measured"
    );
    assert_eq!(
        claude_executable, "claude",
        "with nothing installed the spawn falls back to the bare name"
    );

    let worst = worst_gap_ms.load(Ordering::Relaxed);
    assert!(
        worst < 500,
        "the runtime stalled for {worst} ms while the CLI was being re-resolved \
         (the probe takes {} ms; anything near it means the walk blocked a worker)",
        PROBE.as_millis()
    );
    let ticks = ticks.load(Ordering::Relaxed);
    assert!(
        ticks > 50,
        "the ticker only advanced {ticks} times across a {} ms walk",
        PROBE.as_millis()
    );
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

//! Recovering when the resolved Claude Code CLI stops being spawnable (#533).
//!
//! Claude Code updates itself, and the native install is a symlink into a
//! versioned directory that a self-update swaps. For the length of that swap
//! the symlink dangles, `execve` answers `ENOENT`, and — because the resolution
//! used to be a `OnceLock` filled once at startup — **every chat and every
//! scheduled run failed for the rest of the process's life**, naming a path
//! that works perfectly in a terminal.
//!
//! **One test, and that is deliberate**, for the same reason
//! `claude_cli_detection.rs` holds one: everything here turns on process-wide
//! state. `PATH`, `HOME`, `SHELL` and `AGENTO_CLAUDE_EXECUTABLE` are shared by
//! cargo's threaded runner, and so — the whole point of this suite — is the
//! resolution cache itself, which can be primed exactly once. Two tests would
//! not fail; they would *pass against each other's cache*, which is the one
//! thing a suite about a stale cache cannot afford.
//!
//! **The walk counter is the assertion**, not a convenience. The properties
//! being pinned are "the happy path spawns nothing" and "a missing CLI does not
//! re-probe on every turn", and neither is visible in a return value. The fake
//! `$SHELL` appends a line every time it runs, and rule 3 runs it once per walk
//! that gets that far — so the file's length counts walks that reached the
//! probe. Every phase below says which of those two facts it is reading, because
//! a walk short-circuiting at rule 2 leaves the counter still and that is a
//! *positive* result there, not an absent one.
//!
//! It is not `#[ignore]`d: it needs no corpus and no Claude Code install, only a
//! tempdir and `/bin/sh`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agento_lib::claude_cli::{cached, executable, prime, spawnable_at, Source, REFRESH_COOLDOWN};

#[test]
fn a_cli_that_stops_being_spawnable_is_resolved_again_without_a_restart() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let installed = home.join(".local/bin");
    std::fs::create_dir_all(&installed).expect("install dir");

    // What the native installer leaves: a symlink under `~/.local/bin` into a
    // versioned directory. Resolving it is what a self-update breaks.
    let versions = home.join(".local/share/claude/versions");
    std::fs::create_dir_all(versions.join("2.1.231")).expect("version dir");
    let real = write_cli(
        &versions.join("2.1.231"),
        "claude",
        "2.1.231 (Claude Code)",
        0,
    );
    let link = installed.join("claude");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    // Every walk of the order runs the login shell exactly once (rule 3), so
    // counting its invocations counts walks. It answers "no such command", which
    // is an answer rather than an error, and resolution continues past it.
    let counter = tmp.path().join("walks");
    let shell = write_script(
        tmp.path(),
        "counting-shell",
        &format!("echo walk >> {}\nexit 1\n", counter.display()),
    );

    // A path the user *named*, which does not exist yet. It is deliberately not
    // Claude-Code-shaped: nothing in the detection branches could ever accept
    // it, so seeing it come back later can only mean the override was
    // remembered across the refresh.
    let wrapper_dir = tmp.path().join("wrapper");
    std::fs::create_dir_all(&wrapper_dir).expect("wrapper dir");
    let wrapper = wrapper_dir.join("claude-wrapper");

    // SAFETY: this binary holds exactly one test, so nothing else is reading
    // the environment concurrently — which is why the module is written that
    // way. See the header. `PATH` points at nothing so rule 4 contributes
    // nothing, and the walk is decided by the candidate list alone.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", tmp.path().join("no-such-bin"));
        std::env::set_var("SHELL", &shell);
        std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE");
    }

    // ── Startup: one walk, and the override is stored beside its answer. ─────
    // The wrapper does not exist yet, so rule 2 logs and falls through and the
    // candidate list wins. What matters is that the override is *kept*.
    let found = prime(Some(&wrapper.to_string_lossy())).expect("the installed CLI is found");
    assert_eq!(found.source, Source::Candidate);
    assert_eq!(found.path, link.to_string_lossy());
    assert_eq!(walks(&counter), 1, "priming should walk the order once");

    // ── The happy path costs one `stat` and no subprocess. ───────────────────
    // Not a micro-optimisation: a `--version` round trip per turn would put two
    // seconds of worst case in front of every message.
    for _ in 0..3 {
        assert_eq!(executable(), link.to_string_lossy());
    }
    assert_eq!(
        walks(&counter),
        1,
        "a spawnable path must not re-walk the order"
    );

    // ── The self-update: the version directory goes, the symlink dangles. ────
    // This is the reported failure verbatim. `~/.local/bin/claude` still exists
    // as a directory entry and still looks fine to `ls -l`; it just resolves to
    // nothing, so `execve` answers ENOENT.
    std::fs::remove_dir_all(versions.join("2.1.231")).expect("swap the version out");
    assert!(
        std::fs::metadata(&link).is_err(),
        "the symlink should dangle"
    );

    // Meanwhile the user's configured wrapper has appeared — the escape hatch
    // they were told to use.
    write_script(&wrapper_dir, "claude-wrapper", "echo 'my wrapper'");

    // ── The next spawn recovers, on the same process. ────────────────────────
    // Before #533 this returned the dangling path for the life of the app.
    assert_eq!(
        executable(),
        wrapper.to_string_lossy(),
        "a dangling path must be resolved again rather than spawned"
    );

    // And it walked with the override the *first* walk was given. Nothing in
    // detection can produce this path — it does not answer `--version` like
    // Claude Code and it is on no `PATH` and in no candidate directory — and
    // `Source::Setting` can only come from rule 2. Passing `None` on the
    // refresh, which is what losing the override would mean, fails both halves.
    let after = cached().expect("a resolution after the recovery");
    assert_eq!(
        after.source,
        Source::Setting,
        "the refresh lost the stored override"
    );
    assert_eq!(after.path, wrapper.to_string_lossy());

    // The counter has *not* moved, and that is the same fact from the other
    // side: rule 2 hits before rule 3, so a walk that consults the override
    // never reaches the login shell. A refresh that had dropped the override
    // would have walked past it and shown up here.
    assert_eq!(
        walks(&counter),
        1,
        "the refresh probed the login shell, so it walked past the stored override"
    );

    // ── The banner reports the recovery, not the path that failed. ───────────
    // `host_info` reads `cached()`, so this is the whole of #503's invariant —
    // the banner and the spawn are one answer — carried through a refresh.
    assert_eq!(
        cached().expect("resolved").path,
        executable(),
        "the banner and the spawn disagree after a recovery"
    );
    assert_eq!(walks(&counter), 1, "reading the banner must not walk");

    // ── A CLI that is genuinely gone does not re-probe on every turn. ────────
    // Without the cooldown this is 3 s of login shell plus a `--version` per
    // candidate, on every message and every scheduled run.
    std::fs::remove_file(&wrapper).expect("remove the wrapper too");
    for _ in 0..3 {
        assert_eq!(
            executable(),
            wrapper.to_string_lossy(),
            "inside the cooldown the last walk's answer is returned untouched"
        );
    }
    // With nothing at the override's path any more, a walk would fall through
    // rule 2 and reach the login shell — so the counter is exactly what says
    // whether one ran.
    assert_eq!(
        walks(&counter),
        1,
        "a walk ran inside the cooldown; a missing CLI would re-probe on every turn"
    );

    // ── A walk that finds nothing is allowed to say so. ──────────────────────
    // Past the cooldown the order is walked again, and this time there is no
    // CLI anywhere. The old path is **not** kept: `cached()` is what the banner
    // reads, and a banner claiming an install that is not there is the #503
    // defect this module exists to prevent. The spawn falls back to the bare
    // name, which is what a machine that never had the CLI has always reported
    // — so the failure a user sees is unchanged rather than newly worded.
    //
    // The boundary is constructed rather than slept through: `spawnable_at`
    // takes the clock, and `REFRESH_COOLDOWN` is read from the crate so the two
    // cannot drift.
    let past_cooldown = std::time::Instant::now() + REFRESH_COOLDOWN;
    assert!(
        spawnable_at(past_cooldown).is_none(),
        "a walk with no CLI anywhere must resolve to nothing"
    );
    assert_eq!(walks(&counter), 2, "the cooldown never expired");
    assert_eq!(
        executable(),
        "claude",
        "a CLI that is genuinely gone must report the same failure it always did"
    );
    assert!(
        cached().is_none(),
        "the banner is claiming an install that detection could not find"
    );

    // ── The environment override is verbatim, and never stat-gated. ──────────
    // Rule 1 is taken on trust by design: a wrapper script is a documented
    // reason to set it, and a spawn error naming the user's own path is a better
    // diagnostic than silently resolving somewhere else.
    unsafe { std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", "/nonexistent/wrapper/claude") }
    assert_eq!(executable(), "/nonexistent/wrapper/claude");
    assert_eq!(
        walks(&counter),
        2,
        "the environment override must not trigger a walk"
    );
    unsafe { std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE") }
}

/// How many times the fake login shell has run — i.e. how many times the order
/// has been walked. A file that does not exist yet is zero walks.
fn walks(counter: &Path) -> usize {
    std::fs::read_to_string(counter)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// A fake CLI: prints `banner` to stdout and exits with `code`.
fn write_cli(dir: &Path, name: &str, banner: &str, code: i32) -> PathBuf {
    write_script(dir, name, &format!("echo '{banner}'\nexit {code}\n"))
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

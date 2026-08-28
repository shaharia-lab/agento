//! Finding the Claude Code CLI when the environment is a GUI launch's (#503).
//!
//! **One test, and that is deliberate.** Everything here turns on process-wide
//! state — `PATH`, `HOME`, `SHELL`, `AGENTO_CLAUDE_EXECUTABLE` — which cargo's
//! threaded test runner shares between every test in a binary. Two tests that
//! each set `PATH` do not fail; they *pass against the wrong environment*, which
//! is the failure mode a detection suite can least afford. So the ordering is
//! driven as one sequence in one test, and the module holds nothing else.
//!
//! It is not `#[ignore]`d: it needs no corpus and no Claude Code install, only a
//! tempdir and `/bin/sh`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agento_lib::claude_cli::{executable, resolve, Source};

/// launchd's environment, verbatim. This is what a macOS app launched from
/// Finder, the Dock or Spotlight gets — no Homebrew, no nvm, no npm prefix,
/// nothing any `.zshrc` ever exported. On this `PATH` the `$PATH` branch
/// contributes nothing, which is the whole premise of the issue.
const LAUNCHD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

#[test]
fn the_resolver_walks_its_order_under_a_gui_launchs_environment() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");

    // The install `claude migrate-installer` produces: a real binary under
    // `~/.claude/local`, reachable from a terminal **only** through a shell
    // alias, so it is on no PATH anywhere.
    let local = home.join(".claude/local");
    std::fs::create_dir_all(&local).expect("local");
    let aliased = write_cli(&local, "claude", "2.1.231 (Claude Code)", 0);

    // A `$SHELL` that behaves like a login shell with that alias defined: it
    // prints an rc banner first, because real ones do, and answers
    // `command -v claude` in zsh's alias spelling rather than with a bare path.
    let shell = write_script(
        tmp.path(),
        "fake-login-shell",
        &format!(
            "echo 'Now using node v22.11.0'\n\
             echo \"claude={}\"\n",
            aliased.display()
        ),
    );

    // SAFETY: this binary holds exactly one test, so nothing else is reading
    // the environment concurrently — which is why the module is written that
    // way. See the header.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", LAUNCHD_PATH);
        std::env::set_var("SHELL", &shell);
        std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE");
    }

    // ── The acceptance case: only the login shell can see this install. ──────
    let found = resolve(None).expect("the login shell knows where it is");
    assert_eq!(found.source, Source::LoginShell);
    assert_eq!(found.path, aliased.to_string_lossy());

    // ── An unset PATH must still reach the static candidate scan. ────────────
    // This was the second defect in the reported function: `let path =
    // std::env::var_os("PATH")?` returned `None` outright, so the fallback
    // written for impoverished environments never ran in the one case it was
    // written for. With no PATH *and* no usable shell, only the candidate list
    // is left — and `~/.claude/local` is on it.
    unsafe {
        std::env::remove_var("PATH");
        std::env::set_var("SHELL", "/nonexistent/shell");
    }
    let found = resolve(None).expect("the candidate list still runs without a PATH");
    assert_eq!(found.source, Source::Candidate);
    assert_eq!(found.path, aliased.to_string_lossy());

    // ── A program named `claude` that is not Claude Code is not the CLI. ─────
    // Left unchecked it reads as a healthy install *and* gets spawned for every
    // turn, failing with something that looks nothing like a missing dependency.
    let impostor_dir = tmp.path().join("impostor");
    std::fs::create_dir_all(&impostor_dir).expect("impostor dir");
    write_cli(&impostor_dir, "claude", "GNU claude 1.0", 0);
    unsafe { std::env::set_var("PATH", format!("{}:{LAUNCHD_PATH}", impostor_dir.display())) }
    let found = resolve(None).expect("resolution continues past the impostor");
    assert_eq!(
        found.source,
        Source::Candidate,
        "the impostor on PATH was accepted"
    );

    // ── A real CLI on PATH outranks the candidate list. ──────────────────────
    let path_dir = tmp.path().join("onpath");
    std::fs::create_dir_all(&path_dir).expect("path dir");
    let on_path = write_cli(&path_dir, "claude", "2.1.231 (Claude Code)", 0);
    unsafe {
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}:{LAUNCHD_PATH}",
                impostor_dir.display(),
                path_dir.display()
            ),
        )
    }
    let found = resolve(None).expect("found on PATH");
    assert_eq!(found.source, Source::Path);
    assert_eq!(found.path, on_path.to_string_lossy());

    // ── The stored setting beats every detection branch. ─────────────────────
    let chosen_dir = tmp.path().join("chosen");
    std::fs::create_dir_all(&chosen_dir).expect("chosen dir");
    // Deliberately *not* Claude-Code-shaped: a path the user named is not held
    // to the `--version` check, because a wrapper script is a documented reason
    // to point Agento somewhere and refusing it would take away the very escape
    // hatch this setting exists to be.
    let chosen = write_cli(&chosen_dir, "claude", "my wrapper", 0);
    let found = resolve(Some(&chosen.to_string_lossy())).expect("the setting wins");
    assert_eq!(found.source, Source::Setting);
    assert_eq!(found.path, chosen.to_string_lossy());

    // A stored path that no longer resolves falls through to detection rather
    // than leaving the app with no CLI at all.
    let found = resolve(Some("/nonexistent/claude")).expect("falls through");
    assert_eq!(found.source, Source::Path);

    // ── AGENTO_CLAUDE_EXECUTABLE beats everything, including the setting. ────
    // The banner reporting this is the point: it steered the spawn already, and
    // a user who set it was still told the CLI was not installed.
    unsafe { std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", "/opt/wrapper/claude") }
    let found = resolve(Some(&chosen.to_string_lossy())).expect("the env var wins");
    assert_eq!(found.source, Source::Env);
    assert_eq!(found.path, "/opt/wrapper/claude");

    // ── Nothing anywhere is `None`, not a panic and not a bare "claude". ─────
    unsafe {
        std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE");
        std::env::set_var("HOME", tmp.path().join("empty-home"));
        std::env::set_var("PATH", LAUNCHD_PATH);
    }
    assert!(
        resolve(None).is_none(),
        "an environment with no CLI anywhere must resolve to nothing"
    );

    // ── What the banner reports is what a turn spawns. ───────────────────────
    // `runner::claude_executable` is `claude_cli::executable()` and nothing
    // else, and `host_info` reports `cached()` — so the two cannot disagree by
    // construction. What is worth asserting is the one branch where they are
    // *not* the same code path: the environment override, which the runner
    // re-reads per call so a test binary pointing successive cases at different
    // fake CLIs is not frozen by the cache.
    unsafe { std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", "/opt/wrapper/claude") }
    assert_eq!(executable(), "/opt/wrapper/claude");
    assert_eq!(
        executable(),
        resolve(Some(&chosen.to_string_lossy()))
            .expect("resolved")
            .path,
        "the spawn and the banner disagree about the environment override"
    );
    // And with nothing found anywhere, the spawn falls back to the bare name —
    // which fails with `exec: not found`, the honest answer the banner is
    // already giving.
    unsafe { std::env::remove_var("AGENTO_CLAUDE_EXECUTABLE") }

    // ── A login shell that hangs is killed, and resolution continues. ────────
    // An rc file waiting on input must degrade to "found by a later rule",
    // never to a window that does not open.
    let hanging = write_script(tmp.path(), "hanging-shell", "sleep 120");
    unsafe {
        std::env::set_var("SHELL", &hanging);
        std::env::set_var("HOME", &home);
    }
    let started = std::time::Instant::now();
    let found = resolve(None).expect("resolution survives a hung shell");
    assert_eq!(found.source, Source::Candidate);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "a hung login shell was not bounded: {:?}",
        started.elapsed()
    );
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

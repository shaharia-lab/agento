//! Where the Claude Code CLI is, decided once per launch (#503).
//!
//! Everything Agento ships is self-contained except this one binary: agents run
//! by spawning `claude` as a subprocess ([`crate::claude`]), and that CLI is a
//! separate install we do not redistribute. **The same answer drives two
//! things** — the startup banner that tells the user whether the CLI is
//! installed, and the path every turn and every scheduled run actually spawns.
//! They were two lookups that happened to call one function; they are one
//! resolution now, because the failure this module exists for is precisely the
//! two disagreeing.
//!
//! # Why a `$PATH` scan is not enough, and never was
//!
//! **A GUI application does not inherit the user's shell `PATH`.** On macOS an
//! app launched from Finder, the Dock or Spotlight inherits launchd's
//! environment, which is `/usr/bin:/bin:/usr/sbin:/sbin` and nothing else — no
//! Homebrew, no nvm, no npm prefix, nothing any `.zshrc` ever exported. On Linux
//! a `.desktop` launch inherits the session manager's environment, which is only
//! marginally better. So on a GUI launch the `$PATH` scan contributes close to
//! nothing, and whatever list of directories sits behind it is the whole
//! decision.
//!
//! The list behind it used to be six home-relative directories, and it missed
//! most of the ways Claude Code is actually installed: `~/.claude/local` (what
//! `claude migrate-installer` produces — wired up as a **shell alias**, so there
//! is no binary on any `PATH` at all), every version manager (nvm, fnm, asdf,
//! `n`), Homebrew, pnpm and yarn globals. A user running `2.1.231 (Claude Code)`
//! in their terminal was told the CLI was not installed, and their agents
//! genuinely could not run, because the banner and the spawn shared the miss.
//!
//! # The order
//!
//! First hit wins, and the result is cached for the process:
//!
//! 1. **`AGENTO_CLAUDE_EXECUTABLE`** — an explicit instruction, taken verbatim.
//! 2. **The stored `claude_executable_path` setting** — the in-product escape
//!    hatch, because detection can always fail on somebody's machine and
//!    exporting an environment variable into a GUI app on macOS means
//!    `launchctl setenv`, which is not a thing to ask a user to do.
//! 3. **The user's login shell** — `$SHELL -lic 'command -v claude'`. This is
//!    the standard answer to the launchd-`PATH` problem and the *only* mechanism
//!    that resolves an alias install or a shim whose path `.zshrc` computes.
//! 4. **The current `$PATH`.**
//! 5. **A static candidate list**, covering the table above.
//!
//! **Do not "simplify" step 3 away.** It looks like a redundant subprocess next
//! to four filesystem checks; it is the only branch that can see a `~/.claude/local`
//! alias install, which is Anthropic's own documented local layout.
//!
//! # What is verified, and what is taken on trust
//!
//! A *discovered* path (3, 4, 5) is accepted only when it is an executable file
//! **and** `<path> --version` answers with Claude Code's own banner. That is
//! what stops an unrelated program named `claude` on the `PATH` being spawned
//! for every turn — and a stale symlink reading as a healthy install.
//!
//! A path the user *named* (1, 2) is not held to the `--version` check. A
//! wrapper script is a documented reason to set `AGENTO_CLAUDE_EXECUTABLE`
//! ([`crate::native::chat::runner`]), and a wrapper need not print the CLI's
//! banner; refusing it would take away the escape hatch the setting exists to
//! be. `AGENTO_CLAUDE_EXECUTABLE` is not even checked for existence, so the
//! banner reports exactly what a turn will spawn — a wrong value produces a
//! spawn error naming the user's own path, which is a better diagnostic than a
//! banner claiming nothing is installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How the CLI was found. Reported to the frontend so a user debugging a
/// surprising path can see *which* rule produced it rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `AGENTO_CLAUDE_EXECUTABLE`.
    Env,
    /// The `claude_executable_path` setting.
    Setting,
    /// `$SHELL -lic 'command -v claude'`.
    LoginShell,
    /// A directory on the current `PATH`.
    Path,
    /// One of the known install locations.
    Candidate,
}

impl Source {
    /// The wire spelling, which is also what the Settings pane shows.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::Setting => "setting",
            Source::LoginShell => "login-shell",
            Source::Path => "path",
            Source::Candidate => "candidate",
        }
    }
}

/// A resolved CLI: the path to spawn, and the rule that produced it.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub path: String,
    pub source: Source,
}

/// How long the login-shell probe and each `--version` check may take.
///
/// `std::process` has no native timeout, so both are bounded by
/// [`wait_bounded`]. The probe runs the user's own rc files, which can be slow
/// (a `nvm use` on a cold cache) but must never be able to hold up a launch: the
/// budget is generous enough for an ordinary shell and short enough that a
/// pathological one degrades to "found by a later rule" rather than to a frozen
/// window.
#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The binary's name on this platform.
fn cli_name() -> &'static str {
    if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    }
}

static CACHE: OnceLock<Option<Resolution>> = OnceLock::new();

/// Resolve once and remember the answer for the life of the process.
///
/// Called from `lib.rs`'s setup **after the database is open**, so the stored
/// override is in hand; every later reader gets that same answer through
/// [`cached`]. Priming is what keeps the expensive branch — a login shell,
/// sourcing the user's rc files — off both the banner's path and a chat turn's.
pub fn prime(stored_override: Option<&str>) -> Option<&'static Resolution> {
    CACHE.get_or_init(|| resolve(stored_override)).as_ref()
}

/// The cached resolution.
///
/// Resolves on first call if `prime` has not run — which is the case in unit
/// tests and would be the case for any caller reached before setup finishes.
/// That fallback deliberately passes **no** stored override: it is a safety net
/// for ordering, not a second way to read the setting, and `lib.rs` primes
/// before the proxy is listening so nothing in the app reaches it first.
pub fn cached() -> Option<&'static Resolution> {
    CACHE.get_or_init(|| resolve(None)).as_ref()
}

/// The binary to spawn: the cached resolution, or the bare name when nothing
/// was found — at which point the spawn fails with `exec: not found`, which is
/// the honest answer and the one the banner is already giving.
///
/// **This is the runner's whole implementation**, not a convenience beside it,
/// so "the banner and the spawn agree" is structural rather than a rule two
/// call sites have to keep. [`crate::native::chat::runner`] calls it and adds
/// nothing.
///
/// `AGENTO_CLAUDE_EXECUTABLE` is re-read here rather than taken from the cache,
/// and that is load-bearing for the tests rather than for the app: the cache is
/// a `OnceLock`, so a test binary whose cases each point at a *different*
/// scripted CLI would otherwise all run the first one's. In the app the two are
/// the same value, because [`resolve`] reads the same variable first.
pub fn executable() -> String {
    if let Ok(explicit) = std::env::var("AGENTO_CLAUDE_EXECUTABLE") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    cached()
        .map(|r| r.path.clone())
        .unwrap_or_else(|| "claude".to_string())
}

/// The ordered walk. `stored_override` is the `claude_executable_path` setting,
/// passed in rather than read here: this module opens no database connection,
/// following the same one-read rule `runner::TurnSettings` is built on.
pub fn resolve(stored_override: Option<&str>) -> Option<Resolution> {
    // 1. The explicit instruction, verbatim — see the module header for why it
    //    is not checked at all.
    if let Ok(explicit) = std::env::var("AGENTO_CLAUDE_EXECUTABLE") {
        if !explicit.is_empty() {
            return Some(Resolution {
                path: explicit,
                source: Source::Env,
            });
        }
    }

    // 2. The stored setting. Executable-checked but not `--version`-checked: the
    //    user named it, and `PUT /api/settings` already refused a path that was
    //    not absolute and executable when they saved it. A path that has since
    //    stopped being executable (an unmounted volume, a reinstall) falls
    //    through to detection with a warning rather than leaving the app with no
    //    CLI at all.
    if let Some(stored) = stored_override.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(expand_tilde(stored));
        if is_executable_file(&path) {
            return Some(Resolution {
                path: path.to_string_lossy().into_owned(),
                source: Source::Setting,
            });
        }
        log::warn!(
            "claude cli: the configured path {stored:?} is not an executable file; \
             falling back to detection"
        );
    }

    // 3. The login shell. Unix only — Windows has no equivalent, and the
    //    launchd/session-manager problem this answers is not a Windows problem.
    #[cfg(unix)]
    if let Some(path) = login_shell_path(cli_name()) {
        let path = PathBuf::from(path);
        if verify(&path) {
            return Some(Resolution {
                path: path.to_string_lossy().into_owned(),
                source: Source::LoginShell,
            });
        }
    }

    // 4. The current PATH. Read independently of HOME below: an unset PATH used
    //    to return early from the whole function, so the fallback that exists
    //    precisely for impoverished environments never ran in the one case it
    //    was written for.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(cli_name());
            if verify(&candidate) {
                return Some(Resolution {
                    path: candidate.to_string_lossy().into_owned(),
                    source: Source::Path,
                });
            }
        }
    }

    // 5. The known install locations.
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    for candidate in candidates(Path::new(&home)) {
        if verify(&candidate) {
            return Some(Resolution {
                path: candidate.to_string_lossy().into_owned(),
                source: Source::Candidate,
            });
        }
    }

    None
}

/// Every place a Claude Code install is known to land, in the order they are
/// tried. Absolute system paths first — they are the ones a GUI launch is most
/// likely to be missing from its `PATH` while the user's terminal has them.
///
/// The globbed entries (`~/.nvm/versions/node/*/bin`, fnm's multishell dirs) are
/// listed newest-first by directory name, so a machine with several Node
/// versions installed prefers the most recent — which is the one the user's
/// shell almost certainly selects.
fn candidates(home: &Path) -> Vec<PathBuf> {
    let name = cli_name();
    let mut dirs: Vec<PathBuf> = Vec::new();

    // Homebrew (Apple silicon, then Intel) and the system npm prefix. Absent
    // from launchd's PATH, present in every macOS user's shell.
    for abs in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
        dirs.push(PathBuf::from(abs));
    }

    for rel in [
        // The native installer.
        ".local/bin",
        // `claude migrate-installer` — the documented local install. Wired up
        // with a shell alias, so nothing but this list and the login-shell
        // probe can ever see it.
        ".claude/local",
        ".npm-global/bin",
        ".bun/bin",
        ".volta/bin",
        // pnpm's global bin dir on macOS, then its Linux/XDG spelling.
        "Library/pnpm",
        ".local/share/pnpm",
        ".yarn/bin",
        ".config/yarn/global/node_modules/.bin",
        // asdf and `n` put a shim or a real binary on a fixed path.
        ".asdf/shims",
        "n/bin",
        "bin",
        "AppData/Roaming/npm",
    ] {
        dirs.push(home.join(rel));
    }

    // Version managers that key a directory on the Node version, so the name is
    // not knowable in advance.
    dirs.extend(newest_first(&home.join(".nvm/versions/node"), "bin"));
    dirs.extend(newest_first(
        &home.join(".local/state/fnm_multishells"),
        "bin",
    ));
    dirs.extend(newest_first(
        &home.join(".fnm/node-versions"),
        "installation/bin",
    ));

    dirs.into_iter().map(|d| d.join(name)).collect()
}

/// The subdirectories of `parent`, each with `suffix` appended, newest name
/// first. A `parent` that does not exist or cannot be listed contributes
/// nothing — "we could not look" and "there is nothing there" are the same
/// answer to a candidate scan, unlike in the session walk where the distinction
/// decides whether rows are deleted.
fn newest_first(parent: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut names: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name())
        .collect();
    names.sort();
    names.reverse();
    names
        .into_iter()
        .map(|n| parent.join(n).join(suffix))
        .collect()
}

/// Ask the user's login shell where `claude` is.
///
/// `-l` sources the login profile and `-i` the interactive rc file, which
/// together are what a terminal does — and what a GUI launch does not. Both are
/// needed: `PATH` is commonly exported from `.zprofile`/`.bash_profile` while an
/// alias only ever exists in `.zshrc`/`.bashrc`.
///
/// **The command is a constant.** Nothing user-supplied, and nothing from the
/// network or the API, reaches this command line; what it runs is the user's own
/// configuration, executed exactly as their terminal executes it. Do not make
/// the probed name a parameter reachable from a request.
#[cfg(unix)]
fn login_shell_path(name: &str) -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty())?;
    let command = format!("command -v {name}");
    let child = Command::new(&shell)
        .arg("-lic")
        .arg(&command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| log::debug!("claude cli: spawning {shell} to probe: {e}"))
        .ok()?;

    let output = wait_bounded(child, PROBE_TIMEOUT)?;
    // A non-zero exit is `command -v` saying "no such command", which is an
    // answer and not an error. Take it as "nothing here" and read no further.
    if !output.status.success() {
        return None;
    }
    parse_command_v(&String::from_utf8_lossy(&output.stdout), name)
}

/// Pull a path out of what `command -v` printed.
///
/// Three things make this more than "trim the output". Login shells print
/// things — a fortune, a version banner, `nvm` chatter — so only the **last**
/// non-empty line can be the answer. An *alias* install answers with the alias
/// rather than a path, and the spelling differs by shell: zsh's builtin prints
/// `claude=/path` or `claude: aliased to /path`, bash's prints
/// `alias claude='/path'`. And anything that is not an absolute path after all
/// that — a shell function's body, a relative name, an error line — is rejected
/// rather than guessed at.
///
/// `cfg(unix)` with its one caller: Windows has no login shell to probe, so on
/// that target this is code with no reachable path, which `-D warnings` is
/// right to refuse.
#[cfg(unix)]
fn parse_command_v(stdout: &str, name: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).rfind(|l| !l.is_empty())?;

    // `alias claude='/path/to/claude'` (bash) → the right-hand side.
    let line = line.strip_prefix("alias ").unwrap_or(line);
    // `claude is aliased to '/path'` / `claude: aliased to /path` (zsh/ksh).
    let value = if let Some(rest) = line.split_once(" aliased to ") {
        rest.1
    } else if let Some(rest) = line.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
        // `claude=/path` — zsh's `command -v` for an alias.
        rest
    } else {
        line
    };

    // An alias body is a command line, so only its first word is the binary; a
    // bare path has no second word and is unaffected.
    let value = value.trim().trim_matches(['\'', '"']).trim();
    let first = value.split_whitespace().next()?;
    let first = first.trim_matches(['\'', '"']);
    let expanded = expand_tilde(first);
    // `filepath.IsAbs` is the wrong rule here — this is a real filesystem path
    // from a real shell, not a Go-compatible wire value.
    Path::new(&expanded).is_absolute().then_some(expanded)
}

/// Expand a leading `~` against `HOME`. A shell prints `~` where the user typed
/// it, and nothing downstream expands it for us.
fn expand_tilde(raw: &str) -> String {
    let Some(rest) = raw.strip_prefix('~') else {
        return raw.to_string();
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        // `~other/...` is another user's home, which we cannot resolve.
        return raw.to_string();
    }
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return raw.to_string();
    };
    let mut out = PathBuf::from(home);
    if let Some(rest) = rest.strip_prefix('/') {
        out.push(rest);
    }
    out.to_string_lossy().into_owned()
}

/// Is this an executable file? A directory, a dangling symlink and a
/// non-executable file are all "no".
///
/// `is_file` follows symlinks, so a dangling one fails here and never reaches
/// the `--version` spawn.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        // Windows has no executable bit; the extension is the whole rule, and
        // the candidate paths already carry `.exe`.
        true
    }
}

/// Is this actually Claude Code?
///
/// The `--version` round trip is what separates "there is a file called
/// `claude` here" from "this is the CLI". Without it an unrelated program of
/// that name on the `PATH` is reported as a healthy install and then spawned for
/// every turn, failing with something that looks nothing like a missing
/// dependency.
///
/// One spawn per candidate, and the walk stops at the first hit, so an ordinary
/// launch pays for one.
fn verify(path: &Path) -> bool {
    if !is_executable_file(path) {
        return false;
    }
    let Ok(child) = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let Some(output) = wait_bounded(child, VERSION_TIMEOUT) else {
        log::warn!(
            "claude cli: {} did not answer --version in time; skipping it",
            path.display()
        );
        return false;
    };
    output.status.success() && String::from_utf8_lossy(&output.stdout).contains("Claude Code")
}

/// Wait for a child, killing it at `timeout`.
///
/// `std::process` has no timed wait, so this polls `try_wait` — a watchdog
/// thread would need to own the `Child` to kill it, which is the same handle the
/// caller needs to read the output from. `None` means it was killed: a hung
/// login shell (an rc file waiting on input) or a binary that never answers.
///
/// The output is read **after** the process has exited rather than through a
/// blocking read, so a child that writes more than a pipe buffer and then hangs
/// cannot deadlock this — the timeout still fires. The strings involved are one
/// short line.
fn wait_bounded(mut child: std::process::Child, timeout: Duration) -> Option<std::process::Output> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four alias spellings were measured against real shells, not guessed:
    /// zsh's `command -v` for an alias prints `claude=/path`, its `whence -v`
    /// prints `claude: aliased to /path`, bash's prints
    /// `alias claude='/path'`, and a real binary prints a bare path. A parser
    /// that only trims would hand three of those four straight to `Command::new`.
    #[cfg(unix)]
    #[test]
    fn every_measured_command_v_spelling_parses_to_the_same_path() {
        for line in [
            "/home/u/.claude/local/claude",
            "alias claude='/home/u/.claude/local/claude'",
            "claude=/home/u/.claude/local/claude",
            "claude is aliased to '/home/u/.claude/local/claude'",
            "claude: aliased to /home/u/.claude/local/claude",
        ] {
            assert_eq!(
                parse_command_v(line, "claude").as_deref(),
                Some("/home/u/.claude/local/claude"),
                "parsing {line:?}"
            );
        }
    }

    /// Login shells print things. Only the last line can be the answer, and the
    /// noise above it must not be read as a path.
    #[cfg(unix)]
    #[test]
    fn only_the_last_line_of_a_chatty_shell_is_read() {
        let stdout = "Now using node v22.11.0\n\
                      Welcome back!\n\
                      \n\
                      /opt/homebrew/bin/claude\n";
        assert_eq!(
            parse_command_v(stdout, "claude").as_deref(),
            Some("/opt/homebrew/bin/claude")
        );
    }

    /// Anything that is not an absolute path is rejected rather than spawned —
    /// a shell function's body, a relative name, an error sentence, an empty
    /// answer.
    #[cfg(unix)]
    #[test]
    fn non_path_output_is_rejected() {
        for line in [
            "claude () { npx claude \"$@\" }",
            "claude",
            "command not found: claude",
            "",
            "   \n  \n",
        ] {
            assert_eq!(
                parse_command_v(line, "claude"),
                None,
                "should have rejected {line:?}"
            );
        }
    }

    /// An alias body is a command line. Only its first word is the binary, or
    /// `Command::new` would be handed `"/path --flag"` as one filename.
    #[cfg(unix)]
    #[test]
    fn an_alias_carrying_arguments_yields_only_the_binary() {
        assert_eq!(
            parse_command_v("alias claude='/opt/bin/claude --verbose'", "claude").as_deref(),
            Some("/opt/bin/claude")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_tilde_in_the_shells_answer_is_expanded() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        assert_eq!(
            parse_command_v("alias claude='~/.claude/local/claude'", "claude").as_deref(),
            Some(format!("{home}/.claude/local/claude").as_str())
        );
    }

    /// The candidate list is what a GUI launch actually rests on, so the
    /// locations the issue enumerated are pinned by name rather than left to a
    /// reading of the source.
    #[test]
    fn the_candidate_list_covers_every_documented_install_location() {
        let home = Path::new("/home/u");
        let found: Vec<String> = candidates(home)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let name = cli_name();
        for expected in [
            format!("/opt/homebrew/bin/{name}"),
            format!("/usr/local/bin/{name}"),
            format!("/home/u/.local/bin/{name}"),
            // `claude migrate-installer`'s layout — the one with no binary on
            // any PATH at all.
            format!("/home/u/.claude/local/{name}"),
            format!("/home/u/Library/pnpm/{name}"),
            format!("/home/u/.yarn/bin/{name}"),
            format!("/home/u/.asdf/shims/{name}"),
            format!("/home/u/.bun/bin/{name}"),
            format!("/home/u/.volta/bin/{name}"),
            format!("/home/u/.npm-global/bin/{name}"),
            format!("/home/u/AppData/Roaming/npm/{name}"),
        ] {
            assert!(
                found.contains(&expected),
                "candidate list is missing {expected}; it has {found:?}"
            );
        }
    }

    /// Version-manager directories are named for the version, so the list is
    /// built by reading the filesystem — and the newest is tried first, since
    /// that is the one the user's shell almost certainly selects.
    #[test]
    fn version_manager_directories_are_globbed_newest_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nvm = tmp.path().join(".nvm/versions/node");
        for version in ["v18.19.0", "v20.11.0", "v22.11.0"] {
            std::fs::create_dir_all(nvm.join(version).join("bin")).expect("mkdir");
        }
        let found: Vec<String> = candidates(tmp.path())
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| p.contains(".nvm"))
            .collect();
        let name = cli_name();
        assert_eq!(
            found,
            vec![
                format!(
                    "{}/.nvm/versions/node/v22.11.0/bin/{name}",
                    tmp.path().display()
                ),
                format!(
                    "{}/.nvm/versions/node/v20.11.0/bin/{name}",
                    tmp.path().display()
                ),
                format!(
                    "{}/.nvm/versions/node/v18.19.0/bin/{name}",
                    tmp.path().display()
                ),
            ]
        );
    }

    /// A `parent` that does not exist contributes nothing rather than panicking
    /// or producing a path under it.
    #[test]
    fn a_missing_version_manager_directory_contributes_nothing() {
        assert!(newest_first(Path::new("/nonexistent/nvm/versions/node"), "bin").is_empty());
    }

    #[test]
    fn a_directory_and_a_non_executable_file_are_not_executables() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!is_executable_file(tmp.path()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let plain = tmp.path().join("claude");
            std::fs::write(&plain, "#!/bin/sh\n").expect("write");
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
            assert!(!is_executable_file(&plain));
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            assert!(is_executable_file(&plain));
        }
    }

    /// A dangling symlink is the shape a stale install leaves behind, and it
    /// must read as "not there" rather than as a candidate to spawn.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_not_an_executable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let link = tmp.path().join("claude");
        std::os::unix::fs::symlink(tmp.path().join("gone"), &link).expect("symlink");
        assert!(!is_executable_file(&link));
        assert!(!verify(&link));
    }

    /// The whole point of the `--version` round trip: a program named `claude`
    /// that is not Claude Code is refused, so it is never spawned for a turn.
    #[cfg(unix)]
    #[test]
    fn verify_accepts_only_a_binary_that_answers_like_claude_code() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let real = write_script(tmp.path(), "claude", "echo '2.1.231 (Claude Code)'");
        assert!(verify(&real));

        let impostor = write_script(tmp.path(), "other", "echo 'GNU claude 1.0'");
        assert!(!verify(&impostor));

        let failing = write_script(
            tmp.path(),
            "failing",
            "echo '2.1.231 (Claude Code)'; exit 1",
        );
        assert!(!verify(&failing));
    }

    /// A binary that never answers is killed at the bound rather than holding
    /// startup for as long as it likes.
    #[cfg(unix)]
    #[test]
    fn a_hanging_binary_is_killed_at_the_timeout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hang = write_script(tmp.path(), "hang", "sleep 30");
        let started = Instant::now();
        let child = Command::new(&hang)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        assert!(wait_bounded(child, Duration::from_millis(200)).is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait was not bounded: {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }
}

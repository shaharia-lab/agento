//! Where the Claude Code CLI is: resolved once per launch, revalidated before
//! every spawn (#503, #533).
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
//! First hit wins, and the result is cached:
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
//!
//! # How long the answer is trusted (#533)
//!
//! **The walk runs once; its answer is checked before every spawn.** Claude
//! Code updates itself, and the native install is a symlink
//! (`~/.local/bin/claude`) into a versioned directory
//! (`~/.local/share/claude/versions/<version>`) that a self-update swaps. For
//! the length of that swap the symlink is dangling, `execve` answers `ENOENT`,
//! and the SDK reports [`crate::claude::Error::CliNotFound`]. The path was
//! right when it was resolved; nothing was wrong with the order. What was wrong
//! is that the answer was never revisited — so **every chat and every scheduled
//! run failed for the rest of the process's life**, naming a path that works
//! perfectly in a terminal.
//!
//! So [`executable`] revalidates: one `stat` (through [`is_executable_file`],
//! which follows symlinks and is therefore exactly the check a dangling one
//! fails), and only when that fails does the walk run again — with the stored
//! override the first walk was given, never `None`. Three properties hold it
//! together, and each is an acceptance criterion rather than an optimisation:
//!
//! - **The happy path costs one `stat` and no subprocess.** The `--version`
//!   round trip is emphatically *not* on it: it is what makes the walk
//!   expensive, and a path that stats clean is the path that was already
//!   verified.
//! - **Re-resolution is rate-limited** ([`REFRESH_COOLDOWN`]). A CLI that is
//!   genuinely gone would otherwise pay a login-shell probe plus a `--version`
//!   per candidate on *every* turn. The slot is claimed under the write lock
//!   before the walk starts, so two concurrent turns produce one walk.
//! - **The first failure always refreshes.** The cooldown gates *repeated*
//!   attempts, so it is keyed on the last refresh rather than on when the
//!   resolution was made — a CLI that vanishes two seconds after launch still
//!   recovers on the very next turn.
//!
//! [`cached`] reads the same value, so the banner and Settings report the
//! recovery rather than the stale path. That is #503's invariant — the banner
//! and the spawn are one answer — kept true through a refresh.
//!
//! What this deliberately is **not**: a filesystem watcher, a background
//! re-detection timer, or a `--version` check per turn.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{PoisonError, RwLock};
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
/// [`wait_bounded`]. **These two numbers are the worst case a user waits for a
/// window**, because [`prime`] runs inside `lib.rs`'s startup block — so they
/// are as small as an honest answer allows rather than as large as patience
/// allows. An ordinary login shell answers in 100–500 ms and a real
/// `claude --version` in well under a second; 3 s and 2 s leave room for a
/// cold `nvm use` while capping a shell whose rc file is waiting on input at
/// five seconds total, after which resolution simply continues down the order.
///
/// The alternative — priming on a background thread — was rejected: `cached()`
/// would then race the spawned resolution, and the loser fills the cache
/// *without* the stored override, so a user's configured path would be silently
/// ignored on some launches and not others. Deterministic and bounded beats
/// fast and occasionally wrong. (#533 keeps that true from the other side: the
/// override is stored *beside* the resolution, so a refresh cannot lose it
/// either.)
#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// The shortest interval between two walks of the order (#533).
///
/// It bounds what a *missing* CLI costs. Every re-resolution is a login-shell
/// probe plus up to one `--version` per candidate — up to five seconds of
/// subprocess — and with nothing installed there is no answer to find, so an
/// ungated refresh would pay that on every turn and on every scheduled run.
///
/// Ten seconds rather than a minute because the other side of the trade is a
/// user retrying: someone who repairs their install and presses send again
/// should not be told the CLI is missing for as long as they can be bothered to
/// wait. It does **not** delay the first recovery — see [`refresh_due`].
///
/// `pub` for one reason: the integration test drives [`spawnable_at`] with an
/// instant this far ahead rather than sleeping, so the two cannot drift if this
/// number changes.
pub const REFRESH_COOLDOWN: Duration = Duration::from_secs(10);

/// The cached answer, and what is needed to produce it again.
///
/// `stored_override` is remembered rather than re-derived. Re-reading the
/// setting here would mean opening a database connection from this module,
/// which it does not do; *dropping* it would mean a refresh silently demoting a
/// user's configured path to whatever detection finds — the exact defect the
/// module header rejects background priming over.
struct Cached {
    resolution: Option<Resolution>,
    stored_override: Option<String>,
    /// When the last **refresh** finished, not when the resolution was made.
    /// `None` until one has run, which is what makes the first failure recover
    /// immediately however recently the process started.
    refreshed_at: Option<Instant>,
}

/// A `RwLock` rather than a `OnceLock`, because the answer can now change.
///
/// Reads are on a hot path only in the sense that a chat turn takes one, and a
/// chat turn also spawns a process — the lock is not the cost. Nothing holds it
/// across a subprocess except the very first fill, which is
/// [`OnceLock::get_or_init`]'s own behaviour and is what keeps two concurrent
/// first callers from resolving twice with different overrides.
static CACHE: RwLock<Option<Cached>> = RwLock::new(None);

/// Is a refresh allowed yet?
///
/// `None` — nothing has refreshed since this process resolved — is always due,
/// so the turn that first meets a broken path re-resolves whatever the clock
/// says. Only the *second* and later attempts wait out [`REFRESH_COOLDOWN`].
///
/// Split out and given `now` as a parameter so a test can construct the
/// boundary rather than race it.
fn refresh_due(refreshed_at: Option<Instant>, now: Instant) -> bool {
    refreshed_at.is_none_or(|t| now.saturating_duration_since(t) >= REFRESH_COOLDOWN)
}

/// A poisoned lock is read through rather than panicked on: every critical
/// section here is a clone or an assignment, so there is no torn state to
/// protect against, and a panic elsewhere must not take the CLI path down with
/// it.
fn read_cache() -> std::sync::RwLockReadGuard<'static, Option<Cached>> {
    CACHE.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_cache() -> std::sync::RwLockWriteGuard<'static, Option<Cached>> {
    CACHE.write().unwrap_or_else(PoisonError::into_inner)
}

/// The binary's name on this platform.
fn cli_name() -> &'static str {
    if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    }
}

/// Fill the cache if it is empty, and hand back whatever it holds.
///
/// The write lock is held across [`resolve`] deliberately, which is the one
/// place in this module that happens: it is [`OnceLock::get_or_init`]'s
/// semantics, and it is what stops a second caller resolving concurrently with
/// a *different* `stored_override` and installing the losing answer.
fn fill(stored_override: Option<&str>) -> Option<Resolution> {
    // An explicit scope, not an `if let`: the read guard must be dropped before
    // the write lock is taken or this deadlocks itself, and leaving that to
    // temporary-lifetime rules makes adding an `else` here a hang.
    {
        if let Some(cached) = read_cache().as_ref() {
            return cached.resolution.clone();
        }
    }
    let mut guard = write_cache();
    // Somebody may have filled it between dropping the read lock and taking
    // this one.
    if let Some(cached) = guard.as_ref() {
        return cached.resolution.clone();
    }
    let resolution = resolve(stored_override);
    *guard = Some(Cached {
        resolution: resolution.clone(),
        stored_override: stored_override.map(str::to_owned),
        refreshed_at: None,
    });
    resolution
}

/// Resolve once and remember the answer, along with the override that produced
/// it.
///
/// Called from `lib.rs`'s setup **after the database is open**, so the stored
/// override is in hand; every later reader gets that same answer through
/// [`cached`]. Priming is what keeps the expensive branch — a login shell,
/// sourcing the user's rc files — off both the banner's path and a chat turn's.
///
/// Returns an owned value rather than a `&'static`: the answer can change under
/// a reader now (#533), so handing out a borrow into the cache would be a lie
/// about its lifetime as well as impossible to write.
pub fn prime(stored_override: Option<&str>) -> Option<Resolution> {
    fill(stored_override)
}

/// The cached resolution — what the startup banner and Settings report.
///
/// Resolves on first call if `prime` has not run — which is the case in unit
/// tests and would be the case for any caller reached before setup finishes.
/// That fallback deliberately passes **no** stored override: it is a safety net
/// for ordering, not a second way to read the setting, and `lib.rs` primes
/// before the proxy is listening so nothing in the app reaches it first.
///
/// It reads, and never refreshes. A refresh is a spawn's business, and a banner
/// that started subprocesses would put a login-shell probe behind every
/// `host_info` — but because [`executable`] writes its recovery back here, this
/// still reports the *refreshed* path rather than the one that failed.
pub fn cached() -> Option<Resolution> {
    fill(None)
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
/// and that is load-bearing for the tests rather than for the app: a test
/// binary whose cases each point at a *different* scripted CLI would otherwise
/// all run the first one's. In the app the two are the same value, because
/// [`resolve`] reads the same variable first. It is also returned **verbatim**,
/// with no `stat` and no refresh — rule 1 is taken on trust by design, and a
/// wrapper script that does not exist yet at the moment it is asked about is a
/// spawn error naming the user's own path, which is the better diagnostic.
///
/// Everything else is revalidated first — see the module header.
pub fn executable() -> String {
    if let Ok(explicit) = std::env::var("AGENTO_CLAUDE_EXECUTABLE") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    spawnable_at(Instant::now())
        .map(|r| r.path)
        .unwrap_or_else(|| "claude".to_string())
}

/// The cached resolution, re-resolved if it has stopped being spawnable (#533).
///
/// Three outcomes, and the second is the whole issue:
///
/// 1. The path still stats as an executable file — returned untouched, which is
///    every ordinary turn.
/// 2. It does not, and no walk has run too recently — the order is walked again
///    and **whatever it concludes is what the cache now holds**, including
///    `None`. A walk that finds nothing must not leave the old path behind: the
///    banner reads the same value, and a banner claiming an install that is not
///    there is exactly the #503 defect. The spawn then falls back to the bare
///    name and fails with `claude: binary not found: "claude"` — which is what
///    a machine that never had the CLI has always reported, so the failure is
///    unchanged rather than newly worded.
/// 3. It does not, and a walk *has* run too recently — whatever that walk
///    concluded is returned unchanged, and no probe is paid for.
///
/// `now` is a parameter so a test can place the cooldown boundary instead of
/// sleeping through it; [`executable`] is the production entry point and passes
/// the real clock.
pub fn spawnable_at(now: Instant) -> Option<Resolution> {
    let (resolution, stored_override) = {
        // `fill` first, so a caller reached before `prime` still gets an
        // answer — the ordering safety net `cached` documents. Then **one**
        // guard for both fields: taking two would let a refresh land between
        // them and pair one walk's path with another walk's override, which is
        // the same defect `host_info`'s read-once split avoids.
        fill(None);
        let guard = read_cache();
        match guard.as_ref() {
            Some(cached) => (cached.resolution.clone(), cached.stored_override.clone()),
            // `fill` has just populated it, so this is unreachable; answering
            // "nothing resolved" beats an `expect` on a spawn path.
            None => (None, None),
        }
    };

    if let Some(found) = resolution.as_ref() {
        // One `stat`, following symlinks: a self-update that has swapped the
        // version directory out from under `~/.local/bin/claude` leaves exactly
        // a dangling symlink, and that is what this sees. No `--version` here —
        // that is the expensive half, and a path that stats clean is a path the
        // walk already verified.
        if is_executable_file(Path::new(&found.path)) {
            return resolution;
        }
    }

    // Claim the refresh slot before walking, so a second turn arriving while
    // this one is probing waits out the cooldown instead of probing too.
    {
        let mut guard = write_cache();
        match guard.as_mut() {
            Some(cached) if refresh_due(cached.refreshed_at, now) => {
                cached.refreshed_at = Some(now);
            }
            _ => return resolution,
        }
    }

    // Outside every lock: this spawns a login shell and up to one `--version`
    // per candidate.
    let fresh = resolve(stored_override.as_deref());

    {
        let mut guard = write_cache();
        if let Some(cached) = guard.as_mut() {
            // Unconditional, `None` included — see outcome 2 above.
            cached.resolution = fresh.clone();
            // The real clock rather than `now`, and from completion rather than
            // from the claim, so a walk that took its full five seconds does
            // not immediately allow another.
            cached.refreshed_at = Some(Instant::now());
        }
    }

    // The same line the startup banner logs, so a recovery reads as one in the
    // log the user exports rather than as silence between two failures.
    match fresh.as_ref() {
        Some(found) => log::info!(
            "claude cli re-resolved path={:?} source={}",
            found.path,
            found.source.as_str()
        ),
        None => log::warn!("claude cli: the resolved path is gone and detection found no other"),
    }
    fresh
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

    // 5. The known install locations. `home` is an `Option`, deliberately: the
    //    absolute entries (Homebrew, `/usr/local/bin`) do not depend on it, and
    //    an early `?` here would be the same defect as the one fixed above one
    //    variable along — no HOME, no scan at all.
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    for candidate in candidates(home.as_ref().map(Path::new)) {
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
/// listed by directory name **descending**, which puts the newest version first
/// for the usual `vNN.NN.NN` spellings. It is a lexical sort, not a semver one,
/// so `v9` sorts above `v10` — which decides only *which* of several installs
/// wins, never whether one is found, and every hit is `--version`-verified
/// either way. A semver parser here would be a second thing to be wrong about
/// for no better outcome.
///
/// `home` is optional because the absolute entries do not need it: an
/// environment with no `HOME` still gets Homebrew and `/usr/local/bin` checked.
fn candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let name = cli_name();
    let mut dirs: Vec<PathBuf> = Vec::new();

    // Homebrew (Apple silicon, then Intel) and the system npm prefix. Absent
    // from launchd's PATH, present in every macOS user's shell.
    for abs in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
        dirs.push(PathBuf::from(abs));
    }

    let Some(home) = home else {
        return dirs.into_iter().map(|d| d.join(name)).collect();
    };

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
        let found: Vec<String> = candidates(Some(home))
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
        let found: Vec<String> = candidates(Some(tmp.path()))
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

    /// No `HOME` must not mean no scan. The reported function returned `None`
    /// the moment `PATH` was unset, and writing the same `?` one variable along
    /// would reintroduce it in the branch that matters most on macOS — where
    /// Homebrew and `/usr/local/bin` are exactly what launchd's `PATH` omits.
    #[test]
    fn the_absolute_locations_are_scanned_even_with_no_home() {
        let name = cli_name();
        let found: Vec<String> = candidates(None)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            found,
            vec![
                format!("/opt/homebrew/bin/{name}"),
                format!("/usr/local/bin/{name}"),
                format!("/opt/local/bin/{name}"),
            ]
        );
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

    /// The cooldown gates *repeated* walks, never the first one (#533).
    ///
    /// Keying it on "how long since this process resolved" instead would leave
    /// the one case the issue is about — a CLI that goes away shortly after
    /// launch — unable to recover on the turn that meets it. The boundary is
    /// constructed rather than raced: `now` is a parameter precisely so a test
    /// can place an instant exactly [`REFRESH_COOLDOWN`] ago.
    #[test]
    fn the_first_refresh_is_always_due_and_the_next_one_waits() {
        let now = Instant::now();
        assert!(
            refresh_due(None, now),
            "a path that broke before any refresh must recover on the next spawn"
        );

        assert!(
            !refresh_due(Some(now), now),
            "a second turn arriving immediately must not walk the order again"
        );

        let just_short = now
            .checked_sub(REFRESH_COOLDOWN - Duration::from_millis(1))
            .expect("an instant inside the cooldown");
        assert!(!refresh_due(Some(just_short), now));

        // Exactly on the boundary is due — `>=`, so the cooldown is the wait
        // and not one tick more than it.
        let exactly = now.checked_sub(REFRESH_COOLDOWN).expect("the boundary");
        assert!(refresh_due(Some(exactly), now), "the boundary is inclusive");
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

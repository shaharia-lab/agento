//! Where Agento's data lives.
//!
//! Two things now need this answer and **must** agree: the sidecar, which tells
//! the Go server which directory to open, and the ported endpoints in
//! `native/`, which read that same SQLite file directly. A drift between them
//! would not fail — it would quietly serve one database's contents while the
//! rest of the app wrote to another.

use std::path::PathBuf;

/// The user's home directory.
///
/// **Not `os.UserHomeDir`, and the difference only shows on Windows** — this
/// comment claimed it was until #374 un-gated the three surfaces that reach it.
/// Go switches on `GOOS` and reads `$HOME` on Unix and `%USERPROFILE%` on
/// Windows, *never both*; this is a fallback chain, so a Windows shell that
/// exports `HOME` (MSYS2, Git Bash) wins over `USERPROFILE` and Agento then
/// resolves a different home than the Claude Code CLI does.
///
/// Left as a chain deliberately rather than fixed here: [`data_dir`] is built
/// on this, so switching the precedence relocates the database of any Windows
/// install that has `HOME` set. That is a migration, not a path fix, and it is
/// out of scope for #374.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The data directory the Go server is (or will be) started with.
///
/// Release mirrors Go's own resolution: `AGENTO_DATA_DIR` if set — with the
/// leading `~` expanded, because the value reaches the server through an
/// environment variable and no shell gets to expand it — otherwise `~/.agento`.
///
/// Debug deliberately ignores the environment and uses its own directory. Two
/// Agento processes sharing `~/.agento` share one SQLite file *and* one
/// scheduler, so a scheduled task fires twice and the Telegram webhook is
/// re-registered under whichever instance registered it last. A developer whose
/// shell exports `AGENTO_DATA_DIR=~/.agento` would otherwise get exactly that
/// collision from a `npm run app`.
#[cfg(debug_assertions)]
pub fn data_dir() -> Option<PathBuf> {
    home().map(|h| h.join(".agento-desktop-dev"))
}

/// The data directory the Go server is started with — `AGENTO_DATA_DIR` if set,
/// otherwise `~/.agento`. See the debug variant above for why the two differ.
#[cfg(not(debug_assertions))]
pub fn data_dir() -> Option<PathBuf> {
    match std::env::var("AGENTO_DATA_DIR") {
        Ok(dir) if !dir.is_empty() => expand_tilde(&dir),
        _ => home().map(|h| h.join(".agento")),
    }
}

/// The SQLite database every subsystem persists to.
pub fn database_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("agento.db"))
}

/// Expand a leading `~`, matching `config.resolveDataDir`. Both separators are
/// accepted because the Go side accepts both.
#[cfg(any(not(debug_assertions), test))]
fn expand_tilde(dir: &str) -> Option<PathBuf> {
    if dir == "~" {
        return home();
    }
    if let Some(rest) = dir.strip_prefix("~/").or_else(|| dir.strip_prefix("~\\")) {
        return home().map(|h| h.join(rest));
    }
    Some(PathBuf::from(dir))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Serialises every test that reads or writes `HOME`.
    ///
    /// `HOME` is process-global, so a test that swaps it races any test that
    /// reads it — and these read it twice (once directly, once inside
    /// `expand_tilde`), so a swap landing between the two reads fails on a
    /// mismatch that has nothing to do with the code. `native::scan`'s no-dirs
    /// test has to swap it, because `claude_config_dirs` always walks the real
    /// `~/.claude` first and would otherwise find the developer's own corpus.
    ///
    /// Lives here rather than in `native` because `HOME` is this module's
    /// concern; the lock belongs with the thing it protects, so the next test
    /// that touches `HOME` finds it without knowing about the scanner.
    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// An environment variable swapped for the lifetime of the guard.
    ///
    /// Restoring in a trailing block at the end of the test is not enough: a
    /// failed assertion panics past it, and [`env_lock`] deliberately recovers
    /// from poisoning, so the swapped value **survives into every later test in
    /// the same binary**. One real failure then becomes a cascade of unrelated
    /// ones — or, worse, silences them, since several tests here skip
    /// themselves when a variable is set. A `HOME` left pointing at a deleted
    /// `TempDir` breaks every subsequent [`home`] read outright.
    ///
    /// Holding the guard alongside `env_lock()`'s is what makes the swap safe:
    /// the lock serialises the tests, the guard bounds the swap to one of them.
    pub(crate) struct EnvVar {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVar {
        /// Set `name`, remembering what was there.
        pub(crate) fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let guard = Self {
                name,
                previous: std::env::var_os(name),
            };
            std::env::set_var(name, value);
            guard
        }

        /// Remove `name`, remembering what was there.
        pub(crate) fn unset(name: &'static str) -> Self {
            let guard = Self {
                name,
                previous: std::env::var_os(name),
            };
            std::env::remove_var(name);
            guard
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn tilde_expands_like_the_go_resolver() {
        let _env = env_lock();
        let home = home().expect("a home directory");
        assert_eq!(expand_tilde("~"), Some(home.clone()));
        assert_eq!(
            expand_tilde("~/.agento-dev"),
            Some(home.join(".agento-dev"))
        );
        assert_eq!(
            expand_tilde("/var/lib/agento"),
            Some(PathBuf::from("/var/lib/agento"))
        );
    }

    #[test]
    fn database_sits_beside_the_data_dir() {
        let _env = env_lock();
        let dir = data_dir().expect("a data dir");
        assert_eq!(database_path(), Some(dir.join("agento.db")));
    }
}

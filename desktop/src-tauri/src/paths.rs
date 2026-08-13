//! Where Agento's data lives.
//!
//! Two things now need this answer and **must** agree: the sidecar, which tells
//! the Go server which directory to open, and the ported endpoints in
//! `native/`, which read that same SQLite file directly. A drift between them
//! would not fail — it would quietly serve one database's contents while the
//! rest of the app wrote to another.

use std::path::PathBuf;

/// The user's home directory, by the same variables Go's `os.UserHomeDir`
/// consults.
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
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_like_the_go_resolver() {
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
        let dir = data_dir().expect("a data dir");
        assert_eq!(database_path(), Some(dir.join("agento.db")));
    }
}

//! The user preferences a read has to honour, and the Claude config dirs they
//! scope it to.
//!
//! Go keeps both as process-wide snapshots (`claudesessions.dataSettings`,
//! `config.claudeDirs`) installed during startup wiring, because the readers —
//! the scanner, nine insight processors, the journey builder, the sessions list
//! — have no settings dependency and must all agree. A ported endpoint has no
//! startup wiring to hook into, so it reads the same row from SQLite on demand.
//! That is not merely convenient: the settings row is the authority, and
//! re-reading it means a preference saved through the Go server is honoured by
//! the very next native request rather than at some later restart.
//!
//! Getting this wrong does not fail loudly. A hidden project that is not
//! filtered out simply appears — in a list the user has told us to leave it out
//! of, and in totals the Go server computes without it.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

use crate::paths;

/// The Data & Analytics preferences a session read depends on.
#[derive(Debug, Clone, Default)]
pub struct DataSettings {
    /// Decoded project paths excluded from every figure Agento reports.
    /// Matched against `claude_session_cache.project_path` exactly.
    pub hidden_projects: Vec<String>,
    /// Every Claude config dir Agento indexes, default first. Order is
    /// load-bearing on the Go side (it decides which dir owns a session present
    /// in two); here it only has to contain the same set.
    pub indexed_config_dirs: Vec<String>,
}

/// Read the settings row. A missing row, a missing column, or malformed JSON
/// all degrade to defaults rather than failing the request — exactly as Go's
/// snapshot starts at its defaults before `ApplyDataSettings` runs.
pub fn load(conn: &Connection) -> DataSettings {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT COALESCE(hidden_projects, ''),
                    COALESCE(claude_config_dir, ''),
                    COALESCE(claude_config_dirs, '')
             FROM user_settings WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .unwrap_or_else(|e| {
            log::warn!("native settings: reading user_settings failed: {e}");
            None
        });

    let (hidden_raw, run_override, extra_raw) = row.unwrap_or_default();
    DataSettings {
        hidden_projects: decode_string_array(&hidden_raw),
        indexed_config_dirs: claude_config_dirs(&run_override, &decode_string_array(&extra_raw)),
    }
}

/// Decode a JSON string array column, treating anything unusable as empty.
fn decode_string_array(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_else(|e| {
        log::warn!("native settings: malformed string array {raw:?}: {e}");
        Vec::new()
    })
}

/// Every config dir Agento indexes, mirroring `config.ClaudeConfigDirs`:
/// the default dir, the dir a run targets, then the user's extras, deduped.
///
/// **Reading is a set, running is a choice.** This is the union the sessions
/// list is scoped to — analytics is retrospective, and a machine with two
/// accounts wants both corpora in every total.
fn claude_config_dirs(run_override: &str, extra: &[String]) -> Vec<String> {
    let mut dirs = Vec::with_capacity(extra.len() + 2);
    dirs.push(default_claude_config_dir());
    dirs.push(run_config_dir(run_override));
    dirs.extend(extra.iter().cloned());

    let mut out: Vec<String> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let normalized = match absolute_dir(&normalize(&dir)) {
            Some(d) => d,
            None => continue,
        };
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

/// `~/.claude`, or `/root/.claude` when there is no home — the fallback Go's
/// `DefaultClaudeConfigDir` uses.
fn default_claude_config_dir() -> String {
    paths::home()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".claude")
        .to_string_lossy()
        .into_owned()
}

/// The single dir a run targets: `CLAUDE_CONFIG_DIR` first, then the stored
/// global setting, then the default. The environment comes first because it is
/// what the surrounding environment has already chosen for every subprocess.
fn run_config_dir(stored: &str) -> String {
    if let Ok(env) = std::env::var("CLAUDE_CONFIG_DIR") {
        if let Some(dir) = absolute_dir(&normalize(&env)) {
            return dir;
        }
    }
    if let Some(dir) = absolute_dir(&normalize(stored)) {
        return dir;
    }
    default_claude_config_dir()
}

/// Expand a leading `~` and clean the path, as `NormalizeClaudeConfigDir` does.
/// Blank stays blank so "not set" is distinguishable from a real value.
///
/// This is not tidiness: the dirs are deduplicated by string comparison and
/// recorded on cached rows, so `~/.claude`, `$HOME/.claude` and `$HOME/.claude/`
/// must collapse to one value or the same corpus is attributed to two dirs.
fn normalize(p: &str) -> String {
    let p = p.trim();
    if p.is_empty() {
        return String::new();
    }
    let expanded = if p == "~" {
        paths::home().map(|h| h.to_string_lossy().into_owned())
    } else if let Some(rest) = p.strip_prefix("~/") {
        paths::home().map(|h| h.join(rest).to_string_lossy().into_owned())
    } else {
        None
    };
    clean(&expanded.unwrap_or_else(|| p.to_string()))
}

/// `filepath.Clean` for the subset of paths a config dir can be: collapse
/// repeated separators, resolve `.` and `..`, and drop a trailing separator.
fn clean(p: &str) -> String {
    if p.is_empty() {
        return String::new();
    }
    let rooted = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if let Some(last) = parts.last() {
                    if *last != ".." {
                        parts.pop();
                        continue;
                    }
                }
                if !rooted {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// Absolute paths only. A relative config dir means two different things at
/// once — Agento resolves it against the server's working directory, Claude
/// Code against the subprocess's — so Go drops it and so does this.
fn absolute_dir(p: &str) -> Option<String> {
    if p.is_empty() || !Path::new(p).is_absolute() {
        return None;
    }
    Some(p.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_matches_filepath_clean() {
        assert_eq!(clean("/home/u/.claude/"), "/home/u/.claude");
        assert_eq!(clean("/home//u/./.claude"), "/home/u/.claude");
        assert_eq!(clean("/home/u/x/../.claude"), "/home/u/.claude");
        assert_eq!(clean("/"), "/");
    }

    #[test]
    fn relative_dirs_are_dropped_not_resolved() {
        assert_eq!(absolute_dir("relative/dir"), None);
        assert_eq!(absolute_dir(""), None);
        assert_eq!(
            absolute_dir("/var/lib/claude"),
            Some("/var/lib/claude".to_string())
        );
    }

    #[test]
    fn the_default_dir_leads_and_duplicates_collapse() {
        let home = paths::home().expect("a home directory");
        let default = home.join(".claude").to_string_lossy().into_owned();

        // The same dir spelled three ways must appear once.
        let dirs = claude_config_dirs("~/.claude", &[format!("{default}/"), default.clone()]);
        assert_eq!(dirs, vec![default]);
    }

    #[test]
    fn extra_dirs_follow_the_default() {
        let home = paths::home().expect("a home directory");
        let default = home.join(".claude").to_string_lossy().into_owned();

        let dirs = claude_config_dirs("", &["/var/lib/claude-work".to_string()]);
        assert_eq!(dirs, vec![default, "/var/lib/claude-work".to_string()]);
    }

    #[test]
    fn a_malformed_settings_column_degrades_to_empty() {
        assert!(decode_string_array("not json").is_empty());
        assert!(decode_string_array("").is_empty());
        assert_eq!(decode_string_array(r#"["/a","/b"]"#), vec!["/a", "/b"]);
    }
}

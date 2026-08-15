//! `GET /api/claude-sessions/projects` — the project picker's list.
//!
//! Mirrors `handleListClaudeProjects` (`internal/api/claude_sessions.go`) over
//! `ListProjects` / `projectsFromDiskFiles` (`internal/claudesessions/projects.go`).
//!
//! **Derived from the walk, not from the fallback.** Go serves this from the
//! list its last scan published (`projectsFromDiskFiles` over the files the scan
//! walked) and only falls back to a fresh directory walk before the first scan
//! of the process. The published list is in-memory Go state that Rust cannot
//! read, so this recomputes it — but from `walk_all_disk_files`, the *same*
//! walk the scan derives from, rather than from Go's `walkProjects` fallback.
//! The two disagree by design: the fallback counts distinct session ids and
//! includes a readable-but-empty project directory with a count of zero, which
//! the scan-derived list omits entirely.
//!
//! **Hidden projects are omitted, not flagged**, unless the caller asks. Every
//! picker in the UI should offer only what the figures beside it cover; the one
//! exception is the Data & Analytics settings tab, which cannot let you unhide
//! a project it is not allowed to show you, so it passes `include_hidden=true`
//! and reads the per-project `hidden` flag.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::native::scanner::walk;
use crate::native::{gopath, query, settings};

/// One project directory. Mirrors `claudesessions.ClaudeProject`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaudeProject {
    pub encoded_name: String,
    pub decoded_path: String,
    pub session_count: i64,
    /// Set from the current settings rather than read from disk, and only ever
    /// true in a response that asked for hidden projects.
    pub hidden: bool,
}

/// `projectsFromDiskFiles` plus the handler's hidden filter.
pub fn list(db_path: &Path, include_hidden: bool) -> Result<Vec<ClaudeProject>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    let data_settings = settings::load(&conn);

    let walked = walk::walk_all_disk_files(&data_settings.indexed_config_dirs);

    // Keyed by the *encoded* directory name, so one project worked on under two
    // config dirs folds into one entry — it is one project whichever account
    // opened it. A `BTreeMap` because the result is sorted anyway and the key
    // order makes the count stable.
    let mut by_encoded: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for file in walked.files.values() {
        // Sub-agent transcripts are not sessions: a session that delegated
        // three times would otherwise be counted four.
        if file.is_subagent {
            continue;
        }
        let encoded = encoded_name(&file.file_path.to_string_lossy());
        let entry = by_encoded
            .entry(encoded)
            .or_insert_with(|| (file.project_path.clone(), 0));
        entry.1 += 1;
    }

    let mut projects: Vec<ClaudeProject> = by_encoded
        .into_iter()
        .map(
            |(encoded_name, (decoded_path, session_count))| ClaudeProject {
                encoded_name,
                decoded_path,
                session_count,
                hidden: false,
            },
        )
        .collect();
    // `sortProjects`: by decoded path. Unstable on the Go side, but the key is
    // unique per project so there are no ties to break.
    projects.sort_by(|a, b| a.decoded_path.cmp(&b.decoded_path));

    let hidden = &data_settings.hidden_projects;
    Ok(projects
        .into_iter()
        .filter_map(|mut p| {
            p.hidden = hidden.iter().any(|h| h == &p.decoded_path);
            if p.hidden && !include_hidden {
                return None;
            }
            Some(p)
        })
        .collect())
}

/// `filepath.Base(filepath.Dir(filePath))` — the project directory's own name,
/// which is the encoded form Claude Code writes.
fn encoded_name(file_path: &str) -> String {
    let dir = gopath::dir(file_path);
    dir.rsplit('/').next().unwrap_or(&dir).to_string()
}

/// `include_hidden=true`, and nothing else. Go compares the raw value against
/// the literal string, so `1`, `yes` and `TRUE` all mean false.
pub fn include_hidden(raw_query: &str) -> bool {
    query::value(raw_query, "include_hidden") == "true"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_literal_includes_hidden_projects() {
        assert!(include_hidden("include_hidden=true"));
        assert!(!include_hidden(""));
        assert!(!include_hidden("include_hidden=1"));
        assert!(!include_hidden("include_hidden=TRUE"));
        assert!(!include_hidden("include_hidden=yes"));
        assert!(!include_hidden("include_hidden=false"));
    }

    #[test]
    fn the_encoded_name_is_the_project_directorys_own_name() {
        assert_eq!(
            encoded_name("/home/u/.claude/projects/-home-u-Projects-agento/abc.jsonl"),
            "-home-u-Projects-agento"
        );
        // A sub-agent transcript sits two levels deeper; it is filtered out
        // before this is called, but the answer is still its own directory.
        assert_eq!(
            encoded_name("/home/u/.claude/projects/-p/sess/subagents/agent-1.jsonl"),
            "subagents"
        );
    }

    /// Field order is the Go struct's declaration order, and `hidden` is always
    /// present — it carries no `omitempty`, so a visible project ships `false`.
    #[test]
    fn the_project_shape_is_the_go_struct_order() {
        let body = crate::native::gojson::to_vec(&vec![ClaudeProject {
            encoded_name: "-home-u-x".to_string(),
            decoded_path: "/home/u/x".to_string(),
            session_count: 3,
            hidden: false,
        }])
        .expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            concat!(
                r#"[{"encoded_name":"-home-u-x","decoded_path":"/home/u/x","#,
                r#""session_count":3,"hidden":false}]"#,
                "\n"
            )
        );
    }

    /// The handler builds with `make([]ClaudeProject, 0, …)`, so a corpus with
    /// no visible project is `[]` rather than `null`.
    #[test]
    fn an_empty_result_is_an_empty_array() {
        let body = crate::native::gojson::to_vec(&Vec::<ClaudeProject>::new()).expect("encode");
        assert_eq!(String::from_utf8(body).expect("utf8"), "[]\n");
    }
}

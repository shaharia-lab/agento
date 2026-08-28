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
///
/// **`file_path` is an OS path, not a corpus-canonical one** (#374). It is the
/// transcript's real location on disk, so on Windows it is
/// `C:\Users\u\.claude\projects\-C--Users-u-proj\<id>.jsonl` and both halves of
/// this have to be the target's rules. It used to be `dir.rsplit('/')`, which
/// on Windows finds no `/` at all and answers the *whole directory path* as the
/// project's name — the identity every later lookup keys on. `gopath::base` is
/// what the doc comment always claimed this was.
fn encoded_name(file_path: &str) -> String {
    gopath::base(&gopath::dir(file_path))
}

/// `include_hidden=true`, and nothing else. Go compares the raw value against
/// the literal string, so `1`, `yes` and `TRUE` all mean false.
pub fn include_hidden(raw_query: &str) -> bool {
    query::value(raw_query, "include_hidden") == "true"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config dir with three projects, one of them delegating — so the
    /// sub-agent exclusion, the fold by encoded name and the sort all have
    /// something to do — plus a settings row hiding one of them.
    fn corpus(hidden: &[&str]) -> (tempfile::TempDir, tempfile::NamedTempFile) {
        let dir = tempfile::tempdir().expect("temp dir");
        let projects = dir.path().join("projects");
        for (encoded, sessions) in [("-zzz", 1), ("-aaa", 2), ("-mmm", 1)] {
            let p = projects.join(encoded);
            std::fs::create_dir_all(&p).expect("project dir");
            for i in 0..sessions {
                std::fs::write(p.join(format!("s{encoded}-{i}.jsonl")), "").expect("transcript");
            }
        }
        // A delegated transcript, which must not be counted as a session.
        let sub = projects.join("-aaa").join("s-aaa-0").join("subagents");
        std::fs::create_dir_all(&sub).expect("subagent dir");
        std::fs::write(sub.join("agent-1.jsonl"), "").expect("subagent transcript");

        let db = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(db.path()).expect("open");
        conn.execute_batch(
            "CREATE TABLE user_settings (
                id INTEGER PRIMARY KEY, default_working_dir TEXT NOT NULL DEFAULT '',
                default_model TEXT NOT NULL DEFAULT '', onboarding_complete INTEGER NOT NULL DEFAULT 0,
                appearance_dark_mode INTEGER NOT NULL DEFAULT 0, appearance_font_size INTEGER NOT NULL DEFAULT 0,
                appearance_font_family TEXT NOT NULL DEFAULT '', notification_settings TEXT NOT NULL DEFAULT '{}',
                event_bus_worker_pool_size INTEGER NOT NULL DEFAULT 3, public_url TEXT NOT NULL DEFAULT '',
                hidden_projects TEXT NOT NULL DEFAULT '[]', idle_gap_threshold_minutes INTEGER NOT NULL DEFAULT 0,
                claude_config_dir TEXT NOT NULL DEFAULT '', claude_config_dirs TEXT NOT NULL DEFAULT '[]',
                claude_executable_path TEXT NOT NULL DEFAULT '');",
        )
        .expect("schema");
        // The config dir under test is an *extra* dir; the default one is
        // always indexed too, and on a developer's machine it is real — so the
        // assertions below look for these projects rather than at the length.
        conn.execute(
            "INSERT INTO user_settings (id, hidden_projects, claude_config_dirs) VALUES (1, ?1, ?2)",
            rusqlite::params![
                serde_json::to_string(hidden).expect("json"),
                serde_json::to_string(&[dir.path().to_string_lossy()]).expect("json"),
            ],
        )
        .expect("settings row");
        (dir, db)
    }

    fn decoded(encoded: &str) -> String {
        crate::native::scanner::walk::decode_project_path(encoded)
    }

    fn find<'a>(projects: &'a [ClaudeProject], encoded: &str) -> Option<&'a ClaudeProject> {
        projects.iter().find(|p| p.encoded_name == encoded)
    }

    /// Sessions are counted per project, delegated transcripts are not sessions,
    /// and the result is sorted by decoded path.
    #[test]
    fn projects_are_counted_and_sorted_the_way_a_scan_publishes_them() {
        let (_dir, db) = corpus(&[]);
        let projects = list(db.path(), false).expect("projects");

        assert_eq!(find(&projects, "-aaa").map(|p| p.session_count), Some(2));
        assert_eq!(find(&projects, "-mmm").map(|p| p.session_count), Some(1));
        assert_eq!(
            find(&projects, "-zzz").map(|p| p.session_count),
            Some(1),
            "the sub-agent transcript must not be counted as a session"
        );
        assert!(
            find(&projects, "subagents").is_none(),
            "a subagents/ directory is not a project"
        );

        let paths: Vec<&str> = projects.iter().map(|p| p.decoded_path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "sorted by decoded path");

        assert!(projects.iter().all(|p| !p.hidden));
    }

    /// A hidden project is omitted entirely, and returned flagged only when the
    /// caller asks — the settings tab's one exception.
    #[test]
    fn a_hidden_project_is_omitted_unless_asked_for() {
        let hidden = decoded("-mmm");
        let (_dir, db) = corpus(&[&hidden]);

        let visible = list(db.path(), false).expect("projects");
        assert!(
            find(&visible, "-mmm").is_none(),
            "a hidden project must not reach a picker"
        );
        assert!(find(&visible, "-aaa").is_some());

        let all = list(db.path(), true).expect("projects");
        let flagged = find(&all, "-mmm").expect("hidden project when asked for");
        assert!(flagged.hidden);
        assert!(!find(&all, "-aaa").expect("visible project").hidden);
    }

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

    /// The same rule under the Windows path rules (#374).
    ///
    /// `encoded_name` dispatches on the target, so on a Unix host the
    /// composition is asserted through the Windows functions directly and the
    /// call itself only on Windows — which is what the `windows_rules` CI job
    /// is for. The third assertion is what the code did before: `rsplit('/')`
    /// finds no separator in a Windows directory and answers the whole path, so
    /// every project's identity would have been its own absolute path.
    #[test]
    fn the_encoded_name_is_the_directorys_own_name_under_the_windows_rules() {
        let file = r"C:\Users\u\.claude\projects\-C--Users-u-Projects-agento\abc.jsonl";
        let dir = gopath::dir_windows(file);
        assert_eq!(
            dir,
            r"C:\Users\u\.claude\projects\-C--Users-u-Projects-agento"
        );
        assert_eq!(gopath::base_windows(&dir), "-C--Users-u-Projects-agento");
        assert_eq!(dir.rsplit('/').next(), Some(dir.as_str()));

        #[cfg(windows)]
        assert_eq!(encoded_name(file), "-C--Users-u-Projects-agento");
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

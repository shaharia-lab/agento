//! Enumerating what is on disk, ported from the walk half of
//! `internal/claudesessions/scanner.go`.
//!
//! The walk produces two things, and the second is the one that matters:
//! the set of transcripts found, and **which directories were actually listed
//! end to end**. Everything about deletion safety hangs on the latter.
//!
//! ## "No file on disk" and "we could not look" are different answers
//!
//! The diff can only tell that a cached row has no matching file. If the
//! directory that row came from could not be listed — an unplugged drive, a
//! permission change — that is not evidence the session is gone, and deleting
//! on it would wipe an account's whole corpus, taking `custom_title` and
//! `is_favorite` (the two user-owned columns a rescan deliberately preserves)
//! with it. So a dir that failed to list is left out of [`DiskWalk::walked`],
//! and the delete pass skips every row belonging to it.
//!
//! Three outcomes, and the middle one is easy to miss:
//!
//! | what happened | walked? | protected? |
//! |---|---|---|
//! | listed end to end | yes | — |
//! | config dir itself unreadable | **no** | the whole dir |
//! | listed, but some project dirs failed | yes | those projects only |
//!
//! A config dir that exists but has no `projects/` is the case that looks like
//! a failure and is not: it has genuinely never run a session, so it walked
//! fine and contributed nothing, and its (nonexistent) rows are safe to
//! reconcile. Protection is per **project** rather than per config dir so one
//! root-owned directory does not stop every *other* project's genuinely
//! deleted transcripts from ever being reconciled away.
//!
//! ## One session, however many dirs hold a copy
//!
//! The ordinary way to set up a second Claude account is to copy the first
//! config dir, which duplicates every session id under the same project paths.
//! Indexing both would double that corpus's tokens and cost in every total,
//! and — because `claude_session_cache` is keyed on `(session_id,
//! project_path)` while `file_path` is only a non-unique index — would leave
//! the losing path permanently classified as an insert, re-firing a discovery
//! event on every scan. [`claim_session`] gives each session to the first dir
//! that has it, which is why the caller must pass dirs **default-first**.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};

const JSONL_EXT: &str = ".jsonl";

/// One transcript found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskFile {
    /// For a sub-agent file this is the **parent's** id, matching the column
    /// `claude_subagent_cache` keys on.
    pub session_id: String,
    pub project_path: String,
    pub file_path: PathBuf,
    pub mtime: DateTime<Utc>,
    pub is_subagent: bool,
    pub agent_id: String,
    pub parent_file_path: PathBuf,
    pub config_dir: String,
}

/// The result of walking every configured config dir.
#[derive(Debug, Default)]
pub struct DiskWalk {
    /// Every transcript found, keyed by its path.
    pub files: HashMap<PathBuf, DiskFile>,
    /// The config dirs that produced a complete listing.
    ///
    /// Not bookkeeping: a cached row whose config dir is absent from this set
    /// must be excluded from the delete pass.
    pub walked: HashSet<String>,
    /// Directory paths whose contents could not be fully listed. Rows beneath
    /// them are excluded from the delete pass.
    pub protected: Vec<PathBuf>,
}

/// Walks every configured config dir into one set.
///
/// Failure is isolated per dir on purpose: a dir that cannot be listed is
/// skipped and left out of `walked`, so the rest of the scan proceeds and that
/// dir's rows are protected rather than deleted.
///
/// `dirs` must be **default-first** — that is what makes [`claim_session`]
/// deterministic about which copy of a duplicated session wins.
pub fn walk_all_disk_files(dirs: &[String]) -> DiskWalk {
    let mut walk = DiskWalk::default();
    // Tracks which (session, project) pair a config dir already claimed, so a
    // corpus copied between dirs is indexed once.
    let mut claimed: HashMap<String, String> = HashMap::new();

    for dir in dirs {
        match walk_one_dir(dir, &mut walk.files, &mut claimed) {
            DirOutcome::Complete => {
                walk.walked.insert(dir.clone());
            }
            DirOutcome::Unreadable => {
                // The config dir itself could not be listed; protect all of it.
                walk.protected.push(PathBuf::from(dir));
            }
            DirOutcome::Partial(failed) => {
                walk.walked.insert(dir.clone());
                walk.protected.extend(failed);
            }
        }
    }
    walk
}

/// What listing one config dir produced.
enum DirOutcome {
    /// Listed end to end. Its rows may be reconciled.
    Complete,
    /// The config dir itself could not be listed. Every row under it is
    /// protected.
    Unreadable,
    /// Listed, but these project directories failed. Only they are protected.
    Partial(Vec<PathBuf>),
}

fn walk_one_dir(
    dir: &str,
    on_disk: &mut HashMap<PathBuf, DiskFile>,
    claimed: &mut HashMap<String, String>,
) -> DirOutcome {
    let projects_dir = Path::new(dir).join("projects");
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(entries) => entries,
        Err(e) => {
            // A config dir that exists but has no projects/ has genuinely never
            // run a session: it walked fine and contributed nothing, and its
            // (nonexistent) rows are safe to reconcile. A config dir that is
            // itself missing or unreadable is a different thing entirely — the
            // user may have unplugged a drive — and must protect its rows.
            if e.kind() == std::io::ErrorKind::NotFound && Path::new(dir).is_dir() {
                return DirOutcome::Complete;
            }
            log::warn!("claude sessions: skipping unreadable config dir {dir}: {e}");
            return DirOutcome::Unreadable;
        }
    };

    let mut failed = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !collect_project_disk_files(dir, &projects_dir, &name, on_disk, claimed) {
            failed.push(projects_dir.join(&name));
        }
    }

    if failed.is_empty() {
        DirOutcome::Complete
    } else {
        DirOutcome::Partial(failed)
    }
}

/// Decides whether a config dir may index a `(session, project)` pair,
/// recording the winner so later dirs lose.
///
/// Returns true for the owner — the first dir to ask, or the same dir asking
/// again.
pub fn claim_session(claimed: &mut HashMap<String, String>, key: String, dir: &str) -> bool {
    match claimed.get(&key) {
        None => {
            claimed.insert(key, dir.to_string());
            true
        }
        Some(owner) => owner == dir,
    }
}

/// Collects one project directory. Returns false when it could not be listed,
/// which the caller turns into protection for that project.
fn collect_project_disk_files(
    config_dir: &str,
    projects_dir: &Path,
    dir_name: &str,
    on_disk: &mut HashMap<PathBuf, DiskFile>,
    claimed: &mut HashMap<String, String>,
) -> bool {
    let project_path = decode_project_path(dir_name);
    let project_dir = projects_dir.join(dir_name);

    let entries = match std::fs::read_dir(&project_dir) {
        Ok(entries) => entries.flatten().collect::<Vec<_>>(),
        Err(e) => {
            // A project dir that cannot be listed drops every file under it
            // from the walk, which the diff reads as "deleted" — so it is
            // reported as an incomplete listing and its rows are protected.
            log::warn!(
                "claude sessions: skipping unreadable project dir {}: {e}",
                project_dir.display()
            );
            return false;
        }
    };

    // Session ids are needed before their sibling directories can be matched,
    // and directory listings give no ordering guarantee, so the flat files are
    // collected first.
    let mut session_ids: HashSet<String> = HashSet::new();
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() || !name.ends_with(JSONL_EXT) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let session_id = name.trim_end_matches(JSONL_EXT).to_string();

        if !claim_session(
            claimed,
            format!("{session_id}\u{0}{project_path}"),
            config_dir,
        ) {
            // Another config dir already indexed this exact session. Skipping
            // it here also keeps its sub-agent directory out, since the second
            // pass only descends into ids collected in the first.
            continue;
        }

        session_ids.insert(session_id.clone());
        let file_path = project_dir.join(&name);
        on_disk.insert(
            file_path.clone(),
            DiskFile {
                session_id,
                project_path: project_path.clone(),
                file_path,
                mtime: mtime_utc(&meta),
                is_subagent: false,
                agent_id: String::new(),
                parent_file_path: PathBuf::new(),
                config_dir: config_dir.to_string(),
            },
        );
    }

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() || !session_ids.contains(&name) {
            continue;
        }
        collect_subagent_disk_files(config_dir, &project_dir, &name, &project_path, on_disk);
    }
    true
}

/// Emits one entry per sub-agent transcript under
/// `<project_dir>/<session_id>/subagents/`.
///
/// Claude Code moved delegated work out of the parent JSONL into this
/// directory; a session with no such directory simply contributes nothing.
fn collect_subagent_disk_files(
    config_dir: &str,
    project_dir: &Path,
    session_id: &str,
    project_path: &str,
    on_disk: &mut HashMap<PathBuf, DiskFile>,
) {
    let subagents_dir = project_dir.join(session_id).join("subagents");
    let Ok(entries) = std::fs::read_dir(&subagents_dir) else {
        return;
    };
    let parent_file_path = project_dir.join(format!("{session_id}{JSONL_EXT}"));

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() || !name.ends_with(JSONL_EXT) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let file_path = subagents_dir.join(&name);
        on_disk.insert(
            file_path.clone(),
            DiskFile {
                // The parent's id: `claude_subagent_cache` keys on
                // `(parent_session_id, agent_id)`.
                session_id: session_id.to_string(),
                project_path: project_path.to_string(),
                file_path,
                mtime: mtime_utc(&meta),
                is_subagent: true,
                agent_id: name.trim_end_matches(JSONL_EXT).to_string(),
                parent_file_path: parent_file_path.clone(),
                config_dir: config_dir.to_string(),
            },
        );
    }
}

/// Normalized to UTC, because the stored bounds are compared as text and
/// lexical order matches chronological order only while every value carries the
/// same zone suffix.
fn mtime_utc(meta: &std::fs::Metadata) -> DateTime<Utc> {
    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH).into()
}

/// Turns Claude Code's dash-encoded project directory name back into a path.
///
/// The encoding is lossy: `/` and `.` both became `-`, so the only way back is
/// to walk the filesystem and prefer whichever split actually exists. Segments
/// accumulate until one resolves, then the walk advances.
///
/// This mirrors `DecodeProjectPath` exactly, because `project_path` is half of
/// `claude_session_cache`'s primary key — a different answer here writes rows
/// under a different key than Go does. Four details are load-bearing and each
/// was got wrong once in this port:
///
/// * **Only one leading `-` is stripped.** `--claude` is a hidden directory,
///   and the second dash is what `find_existing_dir`'s dot-prefixed variant
///   recovers.
/// * **Empty tokens are skipped**, not folded into the segment: consecutive
///   hyphens come from an encoded `.` or `/` and the next token continues the
///   segment as if they were not there.
/// * **Only directories match.** A file of the right name is not a path
///   component.
/// * **The fallback tests the final result**, not whether anything resolved
///   along the way; a partial resolution that lands nowhere returns the raw
///   encoded name, which is what the sessions list then stores.
pub fn decode_project_path(encoded: &str) -> String {
    // TrimPrefix, not trim_start_matches: exactly one dash.
    let trimmed = encoded.strip_prefix('-').unwrap_or(encoded);

    let mut current_path = String::new();
    let mut current_segment = String::new();

    for token in trimmed.split('-') {
        // Empty tokens come from consecutive hyphens. Skipping them lets the
        // next token continue the segment, and the dot-prefixed variant below
        // covers the hidden directories that produced them.
        if token.is_empty() {
            continue;
        }

        if current_segment.is_empty() {
            current_segment = token.to_string();
        } else {
            current_segment.push('-');
            current_segment.push_str(token);
        }

        // Greedily advance when the accumulated segment names a real directory.
        if let Some(next) = find_existing_dir(&current_path, &current_segment) {
            current_path = next;
            current_segment.clear();
        }
    }

    let result = if current_segment.is_empty() {
        current_path
    } else {
        format!("{current_path}/{current_segment}")
    };

    if Path::new(&result).exists() {
        result
    } else {
        encoded.to_string()
    }
}

/// Whether `parent/segment` or `parent/.segment` is an existing **directory**.
///
/// The dot-prefixed variant recovers hidden directories such as `.claude`,
/// which Claude Code encodes with the same `-` it uses for a path separator.
fn find_existing_dir(parent: &str, segment: &str) -> Option<String> {
    [segment.to_string(), format!(".{segment}")]
        .into_iter()
        .map(|name| format!("{parent}/{name}"))
        .find(|candidate| Path::new(candidate).is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Builds `<root>/<dir>/projects/<project>/<session>.jsonl` and returns the
    /// config dir.
    fn config_dir_with(root: &Path, name: &str, project: &str, sessions: &[&str]) -> String {
        let dir = root.join(name);
        let project_dir = dir.join("projects").join(project);
        fs::create_dir_all(&project_dir).unwrap();
        for s in sessions {
            fs::write(project_dir.join(format!("{s}.jsonl")), "{}\n").unwrap();
        }
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn every_configured_dir_contributes() {
        let tmp = tempfile::tempdir().unwrap();
        let a = config_dir_with(tmp.path(), "a", "-home-x", &["s1"]);
        let b = config_dir_with(tmp.path(), "b", "-home-y", &["s2"]);

        let walk = walk_all_disk_files(&[a.clone(), b.clone()]);
        assert_eq!(walk.files.len(), 2);
        assert!(walk.walked.contains(&a) && walk.walked.contains(&b));
        assert!(walk.protected.is_empty());
    }

    #[test]
    fn a_session_copied_between_dirs_is_indexed_once_by_the_first() {
        // Copying the config dir is the ordinary way to set up a second
        // account; both copies carry the same ids under the same project.
        let tmp = tempfile::tempdir().unwrap();
        let default = config_dir_with(tmp.path(), "default", "-home-x", &["s1"]);
        let second = config_dir_with(tmp.path(), "second", "-home-x", &["s1"]);

        let walk = walk_all_disk_files(&[default.clone(), second.clone()]);
        assert_eq!(walk.files.len(), 1, "one session, not two");
        let owner = &walk.files.values().next().unwrap().config_dir;
        assert_eq!(owner, &default, "dirs are walked default-first");
    }

    #[test]
    fn a_missing_config_dir_is_protected_rather_than_walked() {
        let tmp = tempfile::tempdir().unwrap();
        let present = config_dir_with(tmp.path(), "present", "-home-x", &["s1"]);
        let absent = tmp.path().join("unplugged").to_string_lossy().into_owned();

        let walk = walk_all_disk_files(&[present.clone(), absent.clone()]);
        assert!(walk.walked.contains(&present));
        assert!(
            !walk.walked.contains(&absent),
            "an unplugged drive must not be reconciled"
        );
        assert!(walk.protected.contains(&PathBuf::from(&absent)));
    }

    #[test]
    fn a_present_dir_without_projects_walked_fine_and_is_reconcilable() {
        // The case that looks like a failure and is not: it has genuinely never
        // run a session.
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let empty = empty.to_string_lossy().into_owned();

        let walk = walk_all_disk_files(std::slice::from_ref(&empty));
        assert!(walk.walked.contains(&empty));
        assert!(walk.protected.is_empty());
        assert!(walk.files.is_empty());
    }

    #[test]
    fn sub_agent_transcripts_are_collected_under_their_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = config_dir_with(tmp.path(), "a", "-home-x", &["s1"]);
        let subagents = Path::new(&dir)
            .join("projects")
            .join("-home-x")
            .join("s1")
            .join("subagents");
        fs::create_dir_all(&subagents).unwrap();
        fs::write(subagents.join("agent-1.jsonl"), "{}\n").unwrap();

        let walk = walk_all_disk_files(&[dir]);
        assert_eq!(walk.files.len(), 2);
        let sub = walk.files.values().find(|f| f.is_subagent).unwrap();
        assert_eq!(sub.agent_id, "agent-1");
        assert_eq!(sub.session_id, "s1", "the parent's id, not the agent's");
        assert!(sub.parent_file_path.ends_with("s1.jsonl"));
    }

    #[test]
    fn a_sub_agent_dir_without_a_claimed_parent_is_not_descended_into() {
        // The second dir loses the session, so its sub-agents must not sneak in
        // through the second pass.
        let tmp = tempfile::tempdir().unwrap();
        let default = config_dir_with(tmp.path(), "default", "-home-x", &["s1"]);
        let second = config_dir_with(tmp.path(), "second", "-home-x", &["s1"]);
        let subagents = Path::new(&second)
            .join("projects")
            .join("-home-x")
            .join("s1")
            .join("subagents");
        fs::create_dir_all(&subagents).unwrap();
        fs::write(subagents.join("agent-9.jsonl"), "{}\n").unwrap();

        let walk = walk_all_disk_files(&[default, second]);
        assert!(
            !walk.files.values().any(|f| f.is_subagent),
            "the losing dir's sub-agents come with the session it lost"
        );
    }

    #[test]
    fn claiming_is_first_dir_wins_and_is_idempotent_for_the_owner() {
        let mut claimed = HashMap::new();
        assert!(claim_session(&mut claimed, "k".into(), "a"));
        assert!(
            claim_session(&mut claimed, "k".into(), "a"),
            "same dir again"
        );
        assert!(!claim_session(&mut claimed, "k".into(), "b"));
    }

    #[test]
    fn an_unresolvable_project_name_is_kept_as_written() {
        assert_eq!(
            decode_project_path("-nowhere-at-all-really"),
            "-nowhere-at-all-really"
        );
        // A name that was never encoded passes through untouched.
        assert_eq!(decode_project_path("plain"), "plain");
    }

    #[test]
    fn only_one_leading_dash_is_stripped_so_hidden_dirs_survive() {
        // `--claude` is `/.claude`: the first dash is the root, the second is
        // the dot. Stripping both loses the hidden directory.
        let tmp = tempfile::tempdir().unwrap();
        let hidden = tmp.path().join(".claude");
        std::fs::create_dir_all(&hidden).unwrap();

        let root = tmp.path().to_string_lossy().replace('/', "-");
        assert_eq!(
            decode_project_path(&format!("{root}--claude")),
            hidden.to_string_lossy().into_owned()
        );
    }

    #[test]
    fn a_file_of_the_right_name_is_not_a_path_component() {
        // Only directories may resolve a segment.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notadir"), "x").unwrap();

        let root = tmp.path().to_string_lossy().replace('/', "-");
        let encoded = format!("{root}-notadir-deeper");
        assert_eq!(
            decode_project_path(&encoded),
            encoded,
            "nothing resolved, so the raw name is kept"
        );
    }

    #[test]
    fn a_partial_resolution_that_lands_nowhere_keeps_the_raw_name() {
        // The fallback tests the *final* result, not whether anything resolved
        // along the way.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("real")).unwrap();

        let root = tmp.path().to_string_lossy().replace('/', "-");
        let encoded = format!("{root}-real-missing-deeper");
        assert_eq!(decode_project_path(&encoded), encoded);
    }

    #[test]
    fn a_project_name_resolves_against_directories_that_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("my-project").join("sub");
        fs::create_dir_all(&nested).unwrap();

        // The dashes in "my-project" are ambiguous; resolution prefers the
        // segment that exists on disk.
        let encoded = format!(
            "-{}",
            nested
                .to_string_lossy()
                .trim_start_matches('/')
                .replace('/', "-")
        );
        assert_eq!(
            decode_project_path(&encoded),
            nested.to_string_lossy().into_owned()
        );
    }
}

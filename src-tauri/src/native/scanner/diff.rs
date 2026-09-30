//! What changed since the last scan, ported from `diffDiskAndCache` in
//! `internal/claudesessions/scanner.go`.
//!
//! The diff turns "what is on disk" plus "what is cached" into three lists:
//! files to insert, files to re-read, and rows to expire. Two of its rules
//! exist because of failures that were shipped once.
//!
//! ## A path that moved is an update, not a discovery (#245)
//!
//! Rows are keyed on `(session_id, project_path)` — or, for a sub-agent, on
//! `(parent_session_id, agent_id)` — while `file_path` is only a non-unique
//! index. So the *same row* can legitimately arrive under a new path: an
//! unmounted drive hands a duplicated session to the surviving copy, and the
//! claim shifts. Classifying that as an insert re-fires a discovery event for
//! a session that has been cached for months, and makes the scan log call it
//! new.
//!
//! So the cache is indexed **twice**: by path, and by row key. A file whose
//! path is unknown but whose key exists is an update. The row exists; only its
//! path moved. The upsert conflicts on the primary key, which moves the row.
//!
//! The diff also indexes the files on disk by key, so a cached path whose key
//! is still on disk somewhere is a move and never an expiry (#705). It does
//! not lean on the expiry's `UPDATE … WHERE file_path = old` matching nothing
//! once the upsert has moved the row; a claim shift whose new-path read fails
//! keeps its row live rather than expiring it.
//!
//! This is a correctness fix for what `is_new` *means*, not a performance one:
//! the insight worker subscribes to discovered and updated alike, and the
//! scanner re-reads an updated transcript exactly as it re-reads a new one, so
//! the work done is the same either way.
//!
//! ## A row is only expired if we actually looked
//!
//! [`row_reconcilable`] is the guard. "No file on disk" and "we could not look"
//! are indistinguishable here, and only one of them means the session is gone.
//!
//! ## A vanished transcript expires its row; it does not delete it (#705)
//!
//! Claude Code removes transcripts after `cleanupPeriodDays`, and the row holds
//! what the transcript no longer can: cost, tokens, the user's rename and
//! favourite, the PR links. So a row with no file behind it is stamped
//! `transcript_expired_at`, once — an already-expired row is not listed again,
//! so the stamp keeps the time the file was first found missing. An expired
//! row whose file is back on disk is re-read even when the mtime is unchanged
//! (`cp -p`), because the re-read is what clears the stamp.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::walk::{DiskFile, DiskWalk};

/// One cached row, in the columns the diff needs.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedEntry {
    pub file_path: PathBuf,
    pub mtime: DateTime<Utc>,
    pub is_subagent: bool,
    pub config_dir: String,
    /// The parent's id for a sub-agent row, matching the stored column.
    pub session_id: String,
    pub project_path: String,
    pub agent_id: String,
    /// `transcript_expired_at IS NOT NULL`.
    pub expired: bool,
}

/// The identity of a row, which is **not** its path.
///
/// The two tables key differently, so the `is_subagent` prefix keeps the
/// populations disjoint: a sub-agent row carries the *parent's* session id and
/// would otherwise alias the parent's own key.
pub fn row_key(is_subagent: bool, session_id: &str, project_path: &str, agent_id: &str) -> String {
    if is_subagent {
        format!("s\u{0}{session_id}\u{0}{agent_id}")
    } else {
        format!("p\u{0}{session_id}\u{0}{project_path}")
    }
}

impl CachedEntry {
    pub fn key(&self) -> String {
        row_key(
            self.is_subagent,
            &self.session_id,
            &self.project_path,
            &self.agent_id,
        )
    }
}

impl DiskFile {
    pub fn key(&self) -> String {
        row_key(
            self.is_subagent,
            &self.session_id,
            &self.project_path,
            &self.agent_id,
        )
    }
}

/// What one scan has to do.
#[derive(Debug, Default, PartialEq)]
pub struct DiskDiff {
    /// Paths never seen before, whose row does not exist either.
    pub to_insert: Vec<PathBuf>,
    /// Paths whose file changed, or whose row exists under a different path.
    pub to_update: Vec<PathBuf>,
    /// Live cached rows with no file behind them — by path or by key — in
    /// directories we could list.
    pub to_expire: Vec<CachedEntry>,
}

/// Classifies every on-disk file and every cached row.
///
/// `default_config_dir` is what a blank `config_dir` means: the column was
/// added by a migration that could not backfill it, because the home directory
/// is not a SQL constant.
pub fn diff_disk_and_cache(
    on_disk: &HashMap<PathBuf, DiskFile>,
    cached: &HashMap<PathBuf, CachedEntry>,
    walk: &DiskWalk,
    default_config_dir: &str,
) -> DiskDiff {
    let mut diff = DiskDiff::default();

    // The second view: the cache by row identity rather than by path.
    let by_key: HashSet<String> = cached.values().map(CachedEntry::key).collect();
    // The third: the files on disk by row identity, which is what tells a
    // claim shift's old path (a move) from a vanished transcript (an expiry).
    let on_disk_keys: HashSet<String> = on_disk.values().map(DiskFile::key).collect();

    for (path, file) in on_disk {
        match cached.get(path) {
            None => {
                if by_key.contains(&file.key()) {
                    // The row exists; only its path moved.
                    diff.to_update.push(path.clone());
                } else {
                    diff.to_insert.push(path.clone());
                }
            }
            Some(entry) => {
                // An expired row back on disk is re-read even at an unchanged
                // mtime: the upsert is what clears its stamp.
                if entry.mtime != file.mtime || entry.expired {
                    diff.to_update.push(path.clone());
                }
            }
        }
    }

    for (path, entry) in cached {
        if on_disk.contains_key(path) || entry.expired {
            continue;
        }
        if on_disk_keys.contains(&entry.key()) {
            // A claim shift: the row moves with the upsert.
            continue;
        }
        if row_reconcilable(entry, walk, default_config_dir) {
            diff.to_expire.push(entry.clone());
        }
    }

    // The maps are unordered; sorting makes a scan's work deterministic, which
    // matters for the batching writer's progress reporting and for tests.
    diff.to_insert.sort();
    diff.to_update.sort();
    diff.to_expire.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    diff
}

/// Whether a row with no file behind it may be expired.
///
/// Two ways to fail: the row's config dir was never listed, or its file sits
/// under a project directory that could not be read. Either way the absence is
/// not evidence.
pub fn row_reconcilable(entry: &CachedEntry, walk: &DiskWalk, default_config_dir: &str) -> bool {
    let dir = if entry.config_dir.is_empty() {
        default_config_dir
    } else {
        &entry.config_dir
    };

    if !walk.walked.contains(dir) {
        return false;
    }
    !walk
        .protected
        .iter()
        .any(|protected| is_under(&entry.file_path, protected))
}

/// Path-prefix containment, on directory boundaries — `/a/bc` is not under
/// `/a/b`.
fn is_under(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn disk(path: &str, session: &str, project: &str, mtime: &str, dir: &str) -> DiskFile {
        DiskFile {
            session_id: session.into(),
            project_path: project.into(),
            file_path: PathBuf::from(path),
            mtime: utc(mtime),
            is_subagent: false,
            agent_id: String::new(),
            parent_file_path: PathBuf::new(),
            config_dir: dir.into(),
        }
    }

    fn cached(path: &str, session: &str, project: &str, mtime: &str, dir: &str) -> CachedEntry {
        CachedEntry {
            file_path: PathBuf::from(path),
            mtime: utc(mtime),
            is_subagent: false,
            config_dir: dir.into(),
            session_id: session.into(),
            project_path: project.into(),
            agent_id: String::new(),
            expired: false,
        }
    }

    fn expired(path: &str, session: &str, project: &str, mtime: &str, dir: &str) -> CachedEntry {
        CachedEntry {
            expired: true,
            ..cached(path, session, project, mtime, dir)
        }
    }

    fn walked(dirs: &[&str]) -> DiskWalk {
        DiskWalk {
            files: HashMap::new(),
            walked: dirs.iter().map(|d| d.to_string()).collect(),
            protected: Vec::new(),
        }
    }

    fn maps(
        disk_files: Vec<DiskFile>,
        cached_rows: Vec<CachedEntry>,
    ) -> (HashMap<PathBuf, DiskFile>, HashMap<PathBuf, CachedEntry>) {
        (
            disk_files
                .into_iter()
                .map(|f| (f.file_path.clone(), f))
                .collect(),
            cached_rows
                .into_iter()
                .map(|c| (c.file_path.clone(), c))
                .collect(),
        )
    }

    #[test]
    fn the_four_base_cases() {
        let (on_disk, cache) = maps(
            vec![
                disk("/d/new.jsonl", "new", "/p", "2026-03-01T00:00:00Z", "/d"),
                disk("/d/mod.jsonl", "mod", "/p", "2026-03-02T00:00:00Z", "/d"),
                disk("/d/same.jsonl", "same", "/p", "2026-03-01T00:00:00Z", "/d"),
            ],
            vec![
                cached("/d/mod.jsonl", "mod", "/p", "2026-03-01T00:00:00Z", "/d"),
                cached("/d/same.jsonl", "same", "/p", "2026-03-01T00:00:00Z", "/d"),
                cached("/d/gone.jsonl", "gone", "/p", "2026-03-01T00:00:00Z", "/d"),
            ],
        );

        let diff = diff_disk_and_cache(&on_disk, &cache, &walked(&["/d"]), "/default");
        assert_eq!(diff.to_insert, vec![PathBuf::from("/d/new.jsonl")]);
        assert_eq!(diff.to_update, vec![PathBuf::from("/d/mod.jsonl")]);
        // A vanished transcript expires its row rather than deleting it (#705).
        assert_eq!(diff.to_expire.len(), 1);
        assert_eq!(diff.to_expire[0].session_id, "gone");
    }

    #[test]
    fn a_claim_shift_is_an_update_rather_than_a_discovery() {
        // #245: the same session, now owned by a different config dir. Its row
        // has been cached for months; calling it new re-fires a discovery.
        let (on_disk, cache) = maps(
            vec![disk(
                "/second/s1.jsonl",
                "s1",
                "/p",
                "2026-03-01T00:00:00Z",
                "/second",
            )],
            vec![cached(
                "/default/s1.jsonl",
                "s1",
                "/p",
                "2026-03-01T00:00:00Z",
                "/default",
            )],
        );

        let diff = diff_disk_and_cache(&on_disk, &cache, &walked(&["/default", "/second"]), "/x");
        assert!(diff.to_insert.is_empty(), "the row already existed");
        assert_eq!(diff.to_update, vec![PathBuf::from("/second/s1.jsonl")]);
        // Changed deliberately by #705: the old path used to be listed for the
        // delete pass, which then matched nothing because the upsert had moved
        // the row. It is now excluded by key — stricter, not weaker: a claim
        // shift is a move, and must never be stamped expired, even when the
        // new path's read fails and the row is not moved this scan.
        assert!(diff.to_expire.is_empty());
    }

    #[test]
    fn an_already_expired_row_is_not_listed_again() {
        // The stamp keeps the time the transcript was first found missing.
        let (on_disk, cache) = maps(
            vec![],
            vec![expired(
                "/d/gone.jsonl",
                "gone",
                "/p",
                "2026-03-01T00:00:00Z",
                "/d",
            )],
        );
        let diff = diff_disk_and_cache(&on_disk, &cache, &walked(&["/d"]), "/default");
        assert_eq!(diff, DiskDiff::default());
    }

    #[test]
    fn an_expired_row_back_on_disk_is_re_read_at_an_unchanged_mtime() {
        // `cp -p` restores the original mtime; only the re-read un-expires it.
        let (on_disk, cache) = maps(
            vec![disk("/d/s.jsonl", "s", "/p", "2026-03-01T00:00:00Z", "/d")],
            vec![expired(
                "/d/s.jsonl",
                "s",
                "/p",
                "2026-03-01T00:00:00Z",
                "/d",
            )],
        );
        let diff = diff_disk_and_cache(&on_disk, &cache, &walked(&["/d"]), "/default");
        assert_eq!(diff.to_update, vec![PathBuf::from("/d/s.jsonl")]);
        assert!(diff.to_insert.is_empty());
        assert!(diff.to_expire.is_empty());
    }

    #[test]
    fn an_expired_row_whose_key_reappears_elsewhere_is_an_update() {
        // A DB-only session re-pointed from another config dir: the upsert
        // moves it to the new path and clears its stamp.
        let (on_disk, cache) = maps(
            vec![disk(
                "/second/s1.jsonl",
                "s1",
                "/p",
                "2026-03-01T00:00:00Z",
                "/second",
            )],
            vec![expired(
                "/default/s1.jsonl",
                "s1",
                "/p",
                "2026-03-01T00:00:00Z",
                "/default",
            )],
        );
        let diff = diff_disk_and_cache(&on_disk, &cache, &walked(&["/default", "/second"]), "/x");
        assert_eq!(diff.to_update, vec![PathBuf::from("/second/s1.jsonl")]);
        assert!(diff.to_insert.is_empty());
        assert!(diff.to_expire.is_empty());
    }

    #[test]
    fn a_row_from_a_dir_that_was_not_walked_is_never_deleted() {
        // The unplugged-drive case: absence is not evidence.
        let (on_disk, cache) = maps(
            vec![],
            vec![cached(
                "/unplugged/s1.jsonl",
                "s1",
                "/p",
                "2026-03-01T00:00:00Z",
                "/unplugged",
            )],
        );
        let diff = diff_disk_and_cache(&on_disk, &cache, &walked(&["/default"]), "/default");
        assert!(diff.to_expire.is_empty());
    }

    #[test]
    fn a_blank_config_dir_means_the_default_one() {
        // Migration 27 could not backfill it: the home directory is not a SQL
        // constant.
        let (on_disk, cache) = maps(
            vec![],
            vec![cached(
                "/default/projects/p/s1.jsonl",
                "s1",
                "/p",
                "2026-03-01T00:00:00Z",
                "",
            )],
        );
        let diff = diff_disk_and_cache(&on_disk, &cache, &walked(&["/default"]), "/default");
        assert_eq!(
            diff.to_expire.len(),
            1,
            "reconcilable against the default dir"
        );

        let diff = diff_disk_and_cache(&on_disk, &cache, &walked(&["/other"]), "/default");
        assert!(diff.to_expire.is_empty(), "the default dir was not walked");
    }

    #[test]
    fn a_protected_project_shields_only_its_own_rows() {
        // One root-owned directory must not stop every other project's deleted
        // transcripts from ever being reconciled.
        let mut walk = walked(&["/d"]);
        walk.protected.push(PathBuf::from("/d/projects/locked"));

        let (on_disk, cache) = maps(
            vec![],
            vec![
                cached(
                    "/d/projects/locked/a.jsonl",
                    "a",
                    "/p",
                    "2026-03-01T00:00:00Z",
                    "/d",
                ),
                cached(
                    "/d/projects/open/b.jsonl",
                    "b",
                    "/q",
                    "2026-03-01T00:00:00Z",
                    "/d",
                ),
            ],
        );
        let diff = diff_disk_and_cache(&on_disk, &cache, &walk, "/default");
        assert_eq!(diff.to_expire.len(), 1);
        assert_eq!(diff.to_expire[0].session_id, "b");
    }

    #[test]
    fn a_sub_agent_key_cannot_alias_its_parent() {
        // Both carry the parent's session id; only the prefix keeps them apart.
        assert_ne!(
            row_key(false, "s1", "/p", ""),
            row_key(true, "s1", "/p", "agent-1")
        );
        // And two sub-agents of one parent are distinct rows.
        assert_ne!(
            row_key(true, "s1", "/p", "agent-1"),
            row_key(true, "s1", "/p", "agent-2")
        );
    }
}

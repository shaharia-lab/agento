//! Applying a scan's changes: read in parallel, write in batches. Ported from
//! `internal/claudesessions/scan_apply.go`.
//!
//! The two halves have opposite shapes. **Reading** is I/O plus JSON decoding —
//! 17.2 seconds for 1,671 transcripts single-threaded on the reference machine
//! — and parallelizes well. **Writing** does not parallelize at all, because
//! SQLite serializes writers; but a transaction per file does not help either,
//! since a full re-read of 5,000 sessions would be 5,000 commits, each with its
//! own fsync.
//!
//! So: a bounded pool of readers, and one writer draining them in batches. The
//! triggers for a full re-read are routine rather than exotic — any pricing
//! rate edit, any idle-threshold change — so this is the difference between a
//! settings save costing seconds and costing minutes.
//!
//! Three failure rules, each chosen so one bad file cannot cost the rest:
//!
//! * **An unreadable transcript is not fatal.** A file being appended to right
//!   now, or one the user cannot read, is logged and skipped; the scan
//!   continues for every other session.
//! * **A batch that fails to commit is dropped, not retried.** The rows carry
//!   each file's mtime, so an uncommitted file still looks changed to the next
//!   diff and is re-read then. Retrying here risks looping on a persistent
//!   error while the user waits.
//! * **Notifications follow the writes.** A dropped batch emits none, and one
//!   session with N changed sub-agents produces one notification rather than
//!   N+1 — on a first scan that fan-out overflows the worker queue.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;

use rusqlite::Connection;

use crate::native::pricing::Resolver;
use crate::native::sessions::summary::SessionSummary;

use super::diff::CachedEntry;
use super::store::{self, SubagentMeta};
use super::summary_file::{read_session_summary, read_subagent_summary};
use super::walk::DiskFile;

/// How many files one write transaction covers.
///
/// Large enough that the per-commit cost is amortized to nothing, small enough
/// that a failure loses a bounded amount of work and that a reader is never
/// blocked long behind the writer.
pub const SCAN_BATCH_SIZE: usize = 100;

/// Bounds the reader pool.
///
/// One less than the available parallelism, floored at 2 and capped at 8: this
/// runs on the user's own laptop while they are working, and a pool wide enough
/// to saturate every core reading a multi-gigabyte corpus is felt as the
/// machine getting slower. The cap is where the returns flatten anyway — past
/// it the work is bound by the single writer and by the page cache, not by
/// decode throughput.
pub fn scan_readers() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    cores.saturating_sub(1).clamp(2, 8)
}

/// `claude_session_cache`'s primary key: `(session_id, project_path)`.
///
/// Named rather than spelled out at each use so the pairing is legible and so
/// the type cannot quietly become a `String` again.
type SessionKey = (String, String);

/// One file to re-read, with what the diff already knows about it.
#[derive(Debug, Clone)]
pub struct ScanUnit {
    pub file: DiskFile,
    pub is_new: bool,
}

/// A decoded transcript on its way to the writer.
struct ScanResult {
    unit: ScanUnit,
    /// `None` when the file could not be read, or yielded no row. Counted as
    /// done, written as nothing.
    summary: Option<SessionSummary>,
    meta: SubagentMeta,
}

/// What one session's change should be announced as.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub session_id: String,
    /// The other half of the cache key, and the reason it is here (#408).
    ///
    /// Go's event payload carried the id and the path only, because its one
    /// subscriber — the insight worker — keyed on the id alone. That is the
    /// #362 defect: `claude_session_cache` is keyed on
    /// `(session_id, project_path)`, so a session id under two project paths is
    /// two rows with two transcripts, and an announcement that cannot tell them
    /// apart is an announcement one of them never gets.
    pub project_path: String,
    pub file_path: PathBuf,
    /// True only for a genuinely new row. A claim shift is an update, and a
    /// sub-agent is never a discovery in its own right.
    pub is_new: bool,
}

/// The outcome of applying one scan.
#[derive(Debug, Default, PartialEq)]
pub struct ApplyOutcome {
    pub rows_written: usize,
    pub rows_deleted: usize,
    /// Files that could not be read, or produced no row.
    pub skipped: usize,
    /// One per session, however many of its files changed.
    pub notifications: Vec<Notification>,
}

/// Reads every unit in parallel and writes the results in batches.
///
/// Deletes run **after** every write, which is what makes a claim shift safe:
/// the row is upserted under its new path first, and only then is the stale
/// path reconciled away.
pub fn apply_changes(
    conn: &mut Connection,
    units: Vec<ScanUnit>,
    to_delete: &[CachedEntry],
    resolver: Option<&Resolver>,
    idle_gap_ms: i64,
    mut progress: impl FnMut(usize, usize),
) -> ApplyOutcome {
    let total = units.len();
    progress(0, total);

    let mut outcome = ApplyOutcome::default();
    // Keyed by the cache's own key so a session with several changed files is
    // announced once, and two sessions sharing an id are announced twice.
    // Ordered so the announcement order is deterministic.
    let mut pending: BTreeMap<SessionKey, Notification> = BTreeMap::new();

    let (result_tx, result_rx) = mpsc::sync_channel::<ScanResult>(SCAN_BATCH_SIZE);
    // A rendezvous channel: a reader takes the next unit only when it is free,
    // so the queue cannot run ahead of the pool. Both ends are created outside
    // the scope so the borrow the readers share outlives them.
    let (work_tx, work_rx) = mpsc::sync_channel::<ScanUnit>(0);
    let work_rx = std::sync::Mutex::new(work_rx);

    std::thread::scope(|scope| {
        for _ in 0..scan_readers().min(total.max(1)) {
            let result_tx = result_tx.clone();
            let work_rx = &work_rx;
            scope.spawn(move || {
                loop {
                    // The lock spans the wait for the next unit, so only one
                    // reader queues for work at a time — but it is released
                    // before the read, which is where all the time goes.
                    // Handing out a unit is nanoseconds against decoding a
                    // transcript, so the readers still overlap fully.
                    let unit = {
                        let rx = work_rx.lock().expect("work queue");
                        rx.recv()
                    };
                    let Ok(unit) = unit else { return };
                    if result_tx
                        .send(read_unit(unit, resolver, idle_gap_ms))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        drop(result_tx);

        scope.spawn(move || {
            for unit in units {
                if work_tx.send(unit).is_err() {
                    return;
                }
            }
        });

        // The writer runs on this thread: SQLite serializes writers anyway, and
        // keeping it here means the scan is finished when this function returns.
        let mut batch: Vec<ScanResult> = Vec::with_capacity(SCAN_BATCH_SIZE);
        let mut done = 0usize;

        for result in result_rx {
            if result.summary.is_none() {
                outcome.skipped += 1;
                done += 1;
                progress(done, total);
                continue;
            }
            batch.push(result);
            if batch.len() >= SCAN_BATCH_SIZE {
                done += flush(conn, &mut batch, &mut outcome, &mut pending);
                progress(done, total);
            }
        }
        if !batch.is_empty() {
            done += flush(conn, &mut batch, &mut outcome, &mut pending);
            progress(done, total);
        }
    });

    // The delete pass is a single transaction that aborts wholesale on the
    // first error, unlike the batched writes: a partial reconciliation is worse
    // than none, since the rows it would leave behind look deleted to nothing.
    if !to_delete.is_empty() {
        match run_deletes(conn, to_delete) {
            Ok(n) => outcome.rows_deleted = n,
            Err(e) => log::warn!("claude sessions: delete pass failed, leaving rows: {e}"),
        }
    }

    outcome.notifications = pending.into_values().collect();
    outcome
}

/// Reads one unit. Touches no database, which is what makes it parallel-safe.
fn read_unit(unit: ScanUnit, resolver: Option<&Resolver>, idle_gap_ms: i64) -> ScanResult {
    let file = &unit.file;
    let (summary, meta) = if file.is_subagent {
        match read_subagent_summary(
            &file.session_id,
            &file.project_path,
            &file.file_path,
            resolver,
            idle_gap_ms,
        ) {
            Ok(Some(s)) => {
                // The sidecar is read only once the transcript itself was
                // readable; it is a label for data that has to exist.
                let meta = store::read_subagent_meta(&file.file_path);
                (Some(s), meta)
            }
            Ok(None) => (None, SubagentMeta::default()),
            Err(e) => {
                log::warn!(
                    "claude sessions: skipping unreadable sub-agent {}: {e}",
                    file.file_path.display()
                );
                (None, SubagentMeta::default())
            }
        }
    } else {
        match read_session_summary(
            &file.session_id,
            &file.project_path,
            &file.file_path,
            resolver,
            idle_gap_ms,
        ) {
            Ok(s) => (s, SubagentMeta::default()),
            Err(e) => {
                log::warn!(
                    "claude sessions: skipping unreadable transcript {}: {e}",
                    file.file_path.display()
                );
                (None, SubagentMeta::default())
            }
        }
    };

    ScanResult {
        unit,
        summary,
        meta,
    }
}

/// Commits one batch, returning how many files it accounted for.
///
/// A batch that fails is logged and dropped; its files still carry their old
/// mtimes, so the next diff re-reads them.
fn flush(
    conn: &mut Connection,
    batch: &mut Vec<ScanResult>,
    outcome: &mut ApplyOutcome,
    pending: &mut BTreeMap<SessionKey, Notification>,
) -> usize {
    let count = batch.len();
    let items = std::mem::take(batch);

    match write_batch(conn, &items) {
        Ok(()) => {
            outcome.rows_written += count;
            for item in &items {
                record_pending(pending, item);
            }
        }
        Err(e) => {
            // Not retried, and deliberately not notified either: nothing
            // changed, so announcing a change would be a lie.
            log::warn!("claude sessions: dropping a batch of {count} rows: {e}");
            outcome.skipped += count;
        }
    }
    count
}

fn write_batch(conn: &mut Connection, items: &[ScanResult]) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for item in items {
        let Some(summary) = &item.summary else {
            continue;
        };
        let file = &item.unit.file;
        if file.is_subagent {
            store::upsert_subagent_row(&tx, file, summary, &item.meta)?;
        } else {
            store::insert_cache_row(&tx, file, summary)?;
            // The session row and its pull requests go together: the row
            // carries the file's mtime, so a PR write failing after the row
            // committed would leave the file looking unchanged to the next diff
            // and the PR rows would never be rebuilt.
            store::replace_pr_rows(&tx, summary)?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Records one session's notification.
///
/// A sub-agent is recorded against its **parent's** id and path, always as an
/// update, and never overwrites an entry already present — so the parent's own
/// record, which may be a discovery, wins regardless of write order.
///
/// **Keyed on `(session_id, project_path)`, not on the id** (#408): the pair is
/// the cache's primary key, so collapsing on the id alone would announce one of
/// a duplicated id's two sessions and silently swallow the other — including
/// its `is_new`, and including the parent path a sub-agent row resolves to.
fn record_pending(pending: &mut BTreeMap<SessionKey, Notification>, item: &ScanResult) {
    let file = &item.unit.file;
    let (path, is_new) = if file.is_subagent {
        (file.parent_file_path.clone(), false)
    } else {
        (file.file_path.clone(), item.unit.is_new)
    };

    pending
        .entry((file.session_id.clone(), file.project_path.clone()))
        .or_insert(Notification {
            session_id: file.session_id.clone(),
            project_path: file.project_path.clone(),
            file_path: path,
            is_new,
        });
}

fn run_deletes(conn: &mut Connection, to_delete: &[CachedEntry]) -> Result<usize, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for entry in to_delete {
        store::delete_cached_file(&tx, entry)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(to_delete.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reader_pool_stays_within_its_bounds() {
        let n = scan_readers();
        assert!((2..=8).contains(&n), "got {n}");
    }

    #[test]
    fn a_sub_agent_never_overwrites_its_parents_discovery() {
        // Whichever order the batch happens to write them in, the session is
        // announced once, as what the parent made it.
        let mut pending = BTreeMap::new();
        let parent = result("s1", "/d/s1.jsonl", false, true);
        let sub = result("s1", "/d/s1/subagents/a.jsonl", true, false);

        record_pending(&mut pending, &sub);
        record_pending(&mut pending, &parent);
        assert_eq!(pending.len(), 1);
        let n = &pending[&key("s1", "/p")];
        assert_eq!(
            n.file_path,
            PathBuf::from("/d/s1.jsonl"),
            "a sub-agent is announced against its parent"
        );
        assert!(
            !n.is_new,
            "the sub-agent got there first and it is an update"
        );

        // The other order: the parent's discovery is what sticks.
        let mut pending = BTreeMap::new();
        record_pending(&mut pending, &parent);
        record_pending(&mut pending, &sub);
        assert!(pending[&key("s1", "/p")].is_new);
    }

    /// The collapse key is the cache's key, not the session id (#408).
    ///
    /// Copying a `~/.claude` to set up a second account gives one session id two
    /// project paths, two transcripts and two cache rows. Keyed on the id alone
    /// this announces one of them and silently swallows the other, so its
    /// insight is never computed from the event path — the defect #362 fixed in
    /// the schema and the sessions list, in its third form.
    #[test]
    fn one_session_id_under_two_projects_is_two_notifications() {
        let mut pending = BTreeMap::new();
        record_pending(
            &mut pending,
            &result_in("/a", "s1", "/a/s1.jsonl", false, true),
        );
        record_pending(
            &mut pending,
            &result_in("/b", "s1", "/b/s1.jsonl", false, true),
        );

        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending
                .values()
                .map(|n| (n.project_path.as_str(), n.file_path.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("/a", PathBuf::from("/a/s1.jsonl")),
                ("/b", PathBuf::from("/b/s1.jsonl")),
            ],
        );
    }

    #[test]
    fn several_sub_agents_collapse_to_one_notification() {
        // A first scan of a session with N sub-agents would otherwise enqueue
        // N+1 items that each re-read all N+1 files.
        let mut pending = BTreeMap::new();
        for i in 0..5 {
            record_pending(
                &mut pending,
                &result("s1", &format!("/d/s1/subagents/a{i}.jsonl"), true, false),
            );
        }
        assert_eq!(pending.len(), 1);
    }

    fn key(session: &str, project: &str) -> SessionKey {
        (session.to_string(), project.to_string())
    }

    fn result(session: &str, path: &str, is_subagent: bool, is_new: bool) -> ScanResult {
        result_in("/p", session, path, is_subagent, is_new)
    }

    fn result_in(
        project: &str,
        session: &str,
        path: &str,
        is_subagent: bool,
        is_new: bool,
    ) -> ScanResult {
        ScanResult {
            unit: ScanUnit {
                file: DiskFile {
                    session_id: session.into(),
                    project_path: project.into(),
                    file_path: PathBuf::from(path),
                    mtime: chrono::DateTime::UNIX_EPOCH,
                    is_subagent,
                    agent_id: if is_subagent {
                        "a".into()
                    } else {
                        String::new()
                    },
                    parent_file_path: PathBuf::from("/d/s1.jsonl"),
                    config_dir: "/d".into(),
                },
                is_new,
            },
            summary: Some(SessionSummary::default()),
            meta: SubagentMeta::default(),
        }
    }
}

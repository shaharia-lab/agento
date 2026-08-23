//! The loop the processors were always waiting for — ported from
//! `internal/claudesessions/insight_worker.go`.
//!
//! `insights/mod.rs` said, of the pure pipeline beside this file: *"When the
//! storage layer moves, the worker is a loop around this."* The storage layer
//! moved (#274, #278, #388) and the loop was never written, so `session_insights`
//! had exactly one writer left — `scan.rs`'s `UPDATE … SET processor_version = 0`,
//! which queues rows for a worker that never came. On a fresh install the table
//! stayed empty through a full scan of the corpus and Insights reported "0
//! sessions analysed"; on an install migrated from the Go server it silently
//! stopped gaining rows at the cut-over. This is that loop (#408).
//!
//! ## Why it is a thread and not a tokio task
//!
//! Everything it does is blocking: `rusqlite` calls with a five-second
//! `busy_timeout`, and transcript reads measured in seconds for a full corpus.
//! `db::blocking` exists so a *tokio worker* never parks on either, but the
//! cheaper answer for something that is blocking end to end and owns its own
//! schedule is to not be on the runtime at all — which is exactly what
//! `scan::ensure_scan` already does with `std::thread::spawn`. So there is no
//! `db::blocking` here and no `async` anywhere in this module.
//!
//! ## Why one worker rather than Go's pool of four
//!
//! Go runs four goroutines each upserting on its own connection. SQLite
//! serializes writers, so four writers is four threads taking turns on one
//! lock — and it makes the *dedup* a concurrency problem, which is precisely
//! what `insight_worker.go` got wrong (see below).
//!
//! This has the shape `scanner/apply.rs` already proved on the same workload: a
//! bounded pool reading transcripts in parallel, and **one** writer draining
//! them in batches under a single transaction. Reading is where all the time
//! goes and it parallelizes; writing does not parallelize at all. Dedup then
//! stops being a race and becomes a `BTreeSet` key.
//!
//! ## The dedup key is the whole cache key
//!
//! `tryProcess` dedups on `sessionID` alone via a `sync.Map`. That is the #362
//! family of bug in its third form: a session id living under two project paths
//! is two sessions with two transcripts, so queueing both and dropping the
//! second is a session that never gets an insight — not a duplicate avoided.
//! `Pending` is ordered on `(session_id, project_path, file_path)` and the batch
//! collects into a `BTreeSet`, so the pair is the identity everywhere.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rusqlite::Connection;

use super::processors::{self, SessionInsight, CURRENT_PROCESSOR_VERSION};
use super::store::{self, Pending};
use crate::native::{db, pricing::Resolver, settings};

/// How often the worker looks for rows a version bump or an idle-threshold
/// change left behind. Go's `insightWorkerRescanInterval`.
///
/// It is the *only* thing that notices a `CURRENT_PROCESSOR_VERSION` bump: the
/// scan's staleness markers cover the scanner version, the pricing revision and
/// the idle threshold, and deliberately not this one — a processor-only bump
/// must not force a full transcript re-read of `claude_session_cache`, whose
/// rows are unaffected by it.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How many sessions one write transaction covers, matching
/// `apply::SCAN_BATCH_SIZE`'s reasoning: large enough to amortize the commit,
/// small enough that a failure loses a bounded amount of work.
const BATCH_SIZE: usize = 100;

/// The queue's capacity. Go's `insightWorkerQueueSize`.
///
/// It bounds how far the announcements may run ahead of the worker, and it is
/// deliberately much smaller than a first scan: 100 slots against a corpus of
/// 1,145 on the reference machine. What happens to the rest is [`SWEEP_REQUESTED`].
const QUEUE_SIZE: usize = 100;

/// The process-wide queue, in the shape `scan::state` and `chat::live` use.
static QUEUE: OnceLock<SyncSender<Pending>> = OnceLock::new();

/// Set when [`enqueue`] could not deliver something, and cleared by the worker
/// as it starts the sweep that covers it.
///
/// **This is what makes a full queue harmless rather than a five-minute hole.**
/// Go drops the overflow with a warning per item and waits for the next
/// `rescanOutdated`, which on a first scan means ~1,045 warning lines and an
/// Insights view that stays empty for five minutes — the exact symptom #408 was
/// filed about, reproduced by the fix for it. One flag turns the whole overflow
/// into a single sweep, which is the cheaper answer as well as the faster one:
/// the sweep queries for precisely the rows that still need work, so it does not
/// matter which of them were dropped.
///
/// Cleared **before** the sweep runs, never after, so an announcement arriving
/// mid-sweep sets it again and gets its own pass rather than being swallowed by
/// the one already in flight.
static SWEEP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Hand sessions to the worker. Never blocks, and is a no-op before [`start`].
///
/// The no-op matters: `scan.rs` calls this from the boot scan, which races the
/// worker's own startup, and a scan whose enqueue landed nowhere is still
/// covered by the sweep [`start`] runs first thing.
pub fn enqueue(items: impl IntoIterator<Item = Pending>) {
    let Some(tx) = QUEUE.get() else { return };
    let overflowed = offer(tx, items);
    if overflowed > 0 {
        SWEEP_REQUESTED.store(true, Ordering::Release);
        log::debug!(
            "insights: {overflowed} announcements overflowed the queue, requesting a sweep"
        );
    }
}

/// Offer every item to the queue without blocking, returning how many did not
/// fit.
///
/// Split out from [`enqueue`] only so it can be tested: `QUEUE` is a
/// process-wide `OnceLock` that only [`start`] fills, and a unit test that
/// filled it would leave a worker thread running for the rest of the binary.
fn offer(tx: &SyncSender<Pending>, items: impl IntoIterator<Item = Pending>) -> usize {
    items
        .into_iter()
        .filter(|item| tx.try_send(item.clone()).is_err())
        .count()
}

/// Start the worker. Call once, after the migrations have run.
pub fn start(db_path: PathBuf) {
    let (tx, rx) = mpsc::sync_channel::<Pending>(QUEUE_SIZE);
    if QUEUE.set(tx).is_err() {
        log::warn!("insights: worker already started");
        return;
    }

    std::thread::spawn(move || run(&db_path, rx));
}

/// The worker loop: sweep, then drain the queue, forever.
///
/// `recv_timeout` is both halves at once — it delivers enqueued work
/// immediately and falls out every [`RESCAN_INTERVAL`] to sweep, with no
/// separate ticker thread and no way for the two to run concurrently.
fn run(db_path: &Path, rx: Receiver<Pending>) {
    // The sweep runs first, before any event can arrive, so a fresh install
    // gets its whole corpus processed without waiting for a scan to report
    // anything — which is the state the issue was filed about.
    sweep(db_path);

    loop {
        let mut batch: BTreeSet<Pending> = BTreeSet::new();

        match rx.recv_timeout(RESCAN_INTERVAL) {
            Ok(item) => {
                batch.insert(item);
                // Take whatever else is already queued, so a scan that
                // announced 300 sessions is a handful of transactions rather
                // than 300.
                while batch.len() < BATCH_SIZE {
                    match rx.try_recv() {
                        Ok(item) => {
                            batch.insert(item);
                        }
                        Err(_) => break,
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                sweep(db_path);
                continue;
            }
            // Every sender is gone, which cannot happen while the static holds
            // one — but exiting is the only correct answer if it ever does.
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        process_batch(db_path, batch);

        // Whatever `enqueue` could not deliver, picked up now rather than at
        // the next five-minute tick. The swap clears the flag before the sweep
        // begins, so an overflow during it is not lost.
        if SWEEP_REQUESTED.swap(false, Ordering::AcqRel) {
            sweep(db_path);
        }
    }
}

/// `rescanOutdated`: everything with no insight row or an outdated one.
///
/// Processed directly rather than pushed through the queue, which Go does and
/// this deliberately does not: a corpus of 2,000 unprocessed sessions against a
/// 100-slot channel would drop 1,900 of them and report each drop, then find
/// them again five minutes later. The queue is for the scan's incremental
/// announcements; the sweep already has the whole list in hand.
fn sweep(db_path: &Path) {
    let conn = match db::open_read_only(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("insights: cannot read the database to find outdated rows: {e}");
            return;
        }
    };
    let pending = match store::needs_processing(&conn, CURRENT_PROCESSOR_VERSION) {
        Ok(pending) => pending,
        Err(e) => {
            log::warn!("insights: failed to list sessions needing processing: {e}");
            return;
        }
    };
    drop(conn);

    if pending.is_empty() {
        return;
    }
    log::info!("insights: reprocessing {} outdated sessions", pending.len());

    for chunk in pending.chunks(BATCH_SIZE) {
        process_batch(db_path, chunk.iter().cloned().collect());
    }
}

/// Read every session in the batch in parallel, then write the results in one
/// transaction.
fn process_batch(db_path: &Path, batch: BTreeSet<Pending>) {
    let items: Vec<Pending> = batch
        .into_iter()
        // `rescanOutdated` skips a row with no file path, and so does this:
        // there is no transcript to compute from, and it is the shape a cache
        // row can never legitimately have.
        .filter(|item| !item.file_path.is_empty())
        .collect();
    if items.is_empty() {
        return;
    }

    // Read once per batch rather than once per session: a settings save landing
    // mid-batch must not judge two sessions of one corpus by different rules,
    // which is the same reason `processors::Ctx` documents for reading it once
    // per run.
    let (idle_gap_ms, resolver) = match db::open_read_only(db_path) {
        Ok(conn) => (
            settings::load(&conn).idle_gap_ms,
            Resolver::load(&conn).ok(),
        ),
        Err(e) => {
            log::warn!("insights: cannot read settings, skipping a batch: {e}");
            return;
        }
    };

    let computed = read_in_parallel(&items, idle_gap_ms, resolver.as_ref());
    if computed.is_empty() {
        return;
    }

    let mut conn = match db::open_read_write(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("insights: cannot open the database to store a batch: {e}");
            return;
        }
    };
    match write_batch(&mut conn, &computed) {
        Ok(n) => log::debug!("insights: stored {n} session insights"),
        // Dropped rather than retried, for `apply.rs`'s reason: the rows keep
        // their old `processor_version` (or none), so they still look
        // unprocessed to the next sweep and are recomputed then. Retrying here
        // risks looping on a persistent error.
        Err(e) => log::warn!("insights: failed to store a batch, dropping it: {e}"),
    }
}

/// One computed insight and the project path it belongs to.
struct Computed {
    project_path: String,
    insight: SessionInsight,
}

/// The reader pool, in `scanner/apply.rs`'s shape and for its reasons: decoding
/// a transcript is I/O plus JSON and parallelizes; the bound keeps the machine
/// usable while the user is working.
fn read_in_parallel(
    items: &[Pending],
    idle_gap_ms: i64,
    resolver: Option<&Resolver>,
) -> Vec<Computed> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out = Mutex::new(Vec::with_capacity(items.len()));

    std::thread::scope(|scope| {
        for _ in 0..crate::native::scanner::apply::scan_readers().min(items.len().max(1)) {
            let next = &next;
            let out = &out;
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(item) = items.get(i) else { return };

                let files = processors::session_files(
                    &item.session_id,
                    std::path::Path::new(&item.file_path),
                );
                let ctx = processors::Ctx {
                    idle_gap_ms,
                    resolver,
                };
                match processors::run(&item.session_id, &files, &ctx) {
                    Ok(insight) => out
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(Computed {
                            project_path: item.project_path.clone(),
                            insight,
                        }),
                    // One unreadable transcript must not cost the batch, which
                    // is `apply.rs`'s first failure rule. The row keeps its old
                    // version and the next sweep tries again.
                    Err(e) => log::warn!(
                        "insights: skipping {} at {}: {e}",
                        item.session_id,
                        item.file_path
                    ),
                }
            });
        }
    });

    out.into_inner().unwrap_or_else(|e| e.into_inner())
}

fn write_batch(conn: &mut Connection, computed: &[Computed]) -> Result<usize, String> {
    let scanned_at = store::scanned_at_now();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for item in computed {
        store::upsert(&tx, &item.insight, &item.project_path, &scanned_at)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(computed.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dedup rule, stated as the property rather than as the code.
    ///
    /// Go's `sync.Map` keyed on the session id would collapse these to one and
    /// silently drop a whole session's insight — #362 in its third form. This
    /// asserts a `BTreeSet` of `Pending` does not.
    #[test]
    fn dedup_keeps_one_session_id_under_two_projects() {
        let a = Pending {
            session_id: "s1".into(),
            project_path: "/a".into(),
            file_path: "/a/s1.jsonl".into(),
        };
        let b = Pending {
            project_path: "/b".into(),
            file_path: "/b/s1.jsonl".into(),
            ..a.clone()
        };

        let batch: BTreeSet<Pending> = [a.clone(), b.clone(), a.clone()].into_iter().collect();

        assert_eq!(batch.len(), 2, "the pair is the identity, not the id");
        assert_eq!(batch.into_iter().collect::<Vec<_>>(), vec![a, b]);
    }

    /// `enqueue` before `start` must not panic — the boot scan races the
    /// worker's own startup, and the sweep covers whatever was dropped.
    ///
    /// Deliberately does not call `start`: `QUEUE` is a process-wide
    /// `OnceLock`, so a test that started the worker would leave a live thread
    /// polling a path for the rest of the run and make every other test in this
    /// binary order-dependent.
    #[test]
    fn enqueue_before_start_is_a_no_op() {
        enqueue([Pending {
            session_id: "s1".into(),
            project_path: "/a".into(),
            file_path: "/a/s1.jsonl".into(),
        }]);
    }

    fn pending(session: &str) -> Pending {
        Pending {
            session_id: session.into(),
            project_path: "/a".into(),
            file_path: format!("/a/{session}.jsonl"),
        }
    }

    /// A first scan announces far more sessions than the queue holds, and the
    /// overflow is what [`SWEEP_REQUESTED`] exists for.
    ///
    /// Go warns once per dropped item and waits for the next five-minute
    /// rescan; on the reference corpus that is ~1,045 log lines and an Insights
    /// view that stays empty for five minutes — the symptom the issue is named
    /// after, reproduced by the fix for it. Asserting the overflow is *counted*
    /// is what makes the flag reachable, and the flag is what makes a full
    /// queue cost one sweep instead.
    ///
    /// Note the receiver is held: dropping it makes every `try_send` fail as
    /// disconnected, which would pass this test for the wrong reason.
    #[test]
    fn everything_past_the_queues_capacity_is_reported_as_overflow() {
        let (tx, rx) = mpsc::sync_channel::<Pending>(2);
        let announced: Vec<Pending> = (0..5).map(|i| pending(&format!("s{i}"))).collect();

        assert_eq!(offer(&tx, announced.clone()), 3);
        assert_eq!(
            std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>(),
            announced[..2].to_vec(),
            "the queue keeps the first arrivals; the sweep covers the rest",
        );
    }

    /// Everything fitting is not an overflow, so a scan announcing a handful of
    /// changed sessions must not trigger a full-corpus sweep.
    #[test]
    fn a_queue_with_room_reports_no_overflow() {
        let (tx, _rx) = mpsc::sync_channel::<Pending>(4);
        assert_eq!(offer(&tx, (0..3).map(|i| pending(&format!("s{i}")))), 0);
    }
}

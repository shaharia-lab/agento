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
//! **That `std::thread::spawn` is pinned by `tests/insights_worker.rs`** (#447),
//! not by anything here: the property is a claim about where [`start`] puts the
//! loop, so only a test driving `start` itself can falsify it. That binary may
//! hold exactly one such test, because [`QUEUE`] is a process-wide `OnceLock`;
//! its own header says so.
//!
//! ## `run` and `run_once`
//!
//! [`run`] is the boot sweep plus `loop { run_once(..) }`, and the split is a
//! pure extraction (#447). The whole of the loop's behaviour — `recv_timeout`,
//! the batch accumulation, the [`BATCH_SIZE`] cutoff, the [`SWEEP_REQUESTED`]
//! follow-up — lives in [`run_once`], which returns a [`Pass`] so a unit test
//! can drive **one deterministic pass** and assert which arm it took. Polling a
//! started worker cannot give that, and every one of those steps was previously
//! covered only by the unit tests on [`offer`].
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

use super::index::DocAccumulator;
use super::processors::{self, SessionInsight, CURRENT_PROCESSOR_VERSION};
use super::store::{self, Pending, Scope};
use crate::native::{db, pricing::Resolver, search, settings};

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
///
/// Since #435 it also bounds **memory**, and the two constants that decide that
/// live in different files: a batch holds one `search::SearchDoc` per session
/// until the write, each up to `normalize::SESSION_CAP` (512 KiB), so the peak
/// is ~51 MB of document text plus each reader thread's `Vec<Event>` for the
/// transcript it is decoding. That is comfortable, and it is a product of two
/// numbers neither of which mentions the other — so raising either wants a
/// glance at this line. Bounding a batch by accumulated document bytes rather
/// than by row count is the principled version, and is not worth it while the
/// product is this far inside the budget.
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
/// The body is [`run_once`] so a test can drive **one deterministic pass** —
/// see its header for what that buys and what it deliberately does not.
fn run(db_path: &Path, rx: Receiver<Pending>) {
    // The sweep runs first, before any event can arrive, so a fresh install
    // gets its whole corpus processed without waiting for a scan to report
    // anything — which is the state the issue was filed about.
    sweep(db_path);

    while run_once(db_path, &rx, RESCAN_INTERVAL) != Pass::Disconnected {}
}

/// What one pass of [`run`] did.
///
/// It exists so a unit test can assert which arm a pass took — the batch
/// accumulation and the `BATCH_SIZE` cutoff are otherwise observable only as
/// "the rows eventually appear", which is true of any number of passes.
/// Deliberately private and deliberately narrow: it is not a contract, it is
/// what the tests in this file assert on, and the follow-up sweep is asserted
/// through its **effect** on the database rather than by growing a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// A batch came off the queue and was processed. `committed` is
    /// [`process_batch`]'s answer, which carries its own careful definition of
    /// failure.
    Batch { size: usize, committed: bool },
    /// `recv_timeout` expired into the periodic sweep.
    Swept,
    /// Every sender is gone, so [`run`] returns.
    Disconnected,
}

/// One pass: take a batch off the queue (or time out into a sweep), process it,
/// then honour a requested sweep.
///
/// `recv_timeout` is both halves of the schedule at once — it delivers enqueued
/// work immediately and falls out every `timeout` to sweep, with no separate
/// ticker thread and no way for the two to run concurrently.
///
/// **`timeout` is a parameter only so a test does not wait five minutes.**
/// Production has exactly one value for it, [`RESCAN_INTERVAL`], passed by
/// [`run`]; nothing else should ever pass another.
///
/// This being callable does **not** make "the worker's database work is off the
/// runtime" testable — a test choosing to call it from a `tokio::spawn` is
/// still the test choosing where the work runs. What puts that property under
/// test is [`start`]'s `std::thread::spawn`, and the thing that drives it is
/// `tests/insights_worker.rs`.
fn run_once(db_path: &Path, rx: &Receiver<Pending>, timeout: Duration) -> Pass {
    let mut batch: BTreeSet<Pending> = BTreeSet::new();

    match rx.recv_timeout(timeout) {
        Ok(item) => {
            batch.insert(item);
            // Take whatever else is already queued, so a scan that announced
            // 300 sessions is a handful of transactions rather than 300.
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
            return Pass::Swept;
        }
        // Every sender is gone, which cannot happen while the static holds
        // one — but exiting is the only correct answer if it ever does.
        Err(mpsc::RecvTimeoutError::Disconnected) => return Pass::Disconnected,
    }

    // The queue is the incremental path, so a row for a pair may exist. The
    // answer is reported rather than acted on: only a rebuild needs to know
    // whether a batch committed, and a rebuild is [`sweep`]'s alone.
    let size = batch.len();
    let committed = process_batch(db_path, batch, Write::Replace);

    // Whatever `enqueue` could not deliver, picked up now rather than at the
    // next five-minute tick. The swap clears the flag before the sweep begins,
    // so an overflow during it is not lost.
    if SWEEP_REQUESTED.swap(false, Ordering::AcqRel) {
        sweep(db_path);
    }

    Pass::Batch { size, committed }
}

/// `rescanOutdated`: everything with no insight row or an outdated one — **or
/// the whole corpus, when the search index needs rebuilding** (#435).
///
/// Processed directly rather than pushed through the queue, which Go does and
/// this deliberately does not: a corpus of 2,000 unprocessed sessions against a
/// 100-slot channel would drop 1,900 of them and report each drop, then find
/// them again five minutes later. The queue is for the scan's incremental
/// announcements; the sweep already has the whole list in hand.
///
/// ## The two versions are separate, and only one of them widens the scope
///
/// `CURRENT_PROCESSOR_VERSION` selects rows whose *insight* is stale;
/// [`search::SEARCH_INDEX_VERSION`] selects rows whose *index text* is, which is
/// every row at once because the constant is not stored per row. A bump to the
/// second therefore reads every transcript again — through **this** thread, and
/// **not** through the scanner: no staleness marker is touched, so
/// `claude_session_cache.file_mtime` is left exactly as it was and the scan does
/// not re-read a thing. That is `CURRENT_PROCESSOR_VERSION`'s own separation,
/// applied to the second version constant.
///
/// The new version is stamped **after** the rebuild, and only if every batch in
/// it committed. Stamping first would leave a partially indexed corpus that
/// nothing ever finishes, because the mismatch driving the work would be gone.
///
/// ## A rebuild empties the index first, and that is a cost decision
///
/// `search::delete` is a **scan** of the FTS5 content table, so re-indexing a
/// corpus with `search::replace` per session is quadratic — 1,178 scans of a
/// table that grows with every insert, measured at 11–76 s of pure delete
/// depending on document size, all of it inside a write transaction competing
/// with the boot scan that `lib.rs` starts two lines later. `delete_all` once
/// plus [`search::insert`] is the same end state for one scan-free `DELETE`.
///
/// It has a second effect worth having: after emptying, a session the rebuild
/// **skips** has no row at all, where per-session replacement would have left
/// its *previous* row in place — carrying text in the format this build has just
/// declared it disagrees with, under a version stamp saying otherwise. Missing
/// is a recoverable state that `needs_processing` can still describe; silently
/// stale is not.
///
/// The cost is that search answers nothing for the length of a rebuild. That is
/// accepted rather than overlooked: the rows it would otherwise return were
/// produced by a reader this build no longer agrees with, which is what the
/// version bump means. `delete_all`'s own header records the same intent.
fn sweep(db_path: &Path) {
    let conn = match db::open_read_only(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("insights: cannot read the database to find outdated rows: {e}");
            return;
        }
    };
    let indexed_version = search::stored_version(&conn).unwrap_or_else(|e| {
        // Treated as current rather than as 0: a read failure that answered
        // "nothing indexed" would rebuild the entire corpus on every sweep for
        // as long as the failure lasts.
        log::warn!("insights: cannot read search_index_version: {e}");
        search::SEARCH_INDEX_VERSION
    });
    let rebuild = indexed_version != search::SEARCH_INDEX_VERSION;
    let scope = if rebuild {
        Scope::Everything
    } else {
        Scope::Outdated
    };
    let pending = match store::select_pending(&conn, CURRENT_PROCESSOR_VERSION, scope) {
        Ok(pending) => pending,
        Err(e) => {
            log::warn!("insights: failed to list sessions needing processing: {e}");
            return;
        }
    };
    drop(conn);

    if pending.is_empty() {
        // Nothing to index, so the version is trivially reached. Recording it
        // here is what stops an empty corpus rebuilding on every five-minute
        // tick for the life of the process.
        if rebuild {
            record_index_version(db_path);
        }
        return;
    }
    if rebuild {
        log::info!(
            "search: rebuilding the index for {} sessions (stored version {indexed_version}, \
             this build writes {})",
            pending.len(),
            search::SEARCH_INDEX_VERSION,
        );
    } else {
        log::info!("insights: reprocessing {} outdated sessions", pending.len());
    }

    // Emptied once, before the first batch, so the batches can insert without
    // paying `search::delete`'s scan each — see the header. A failure here
    // abandons the rebuild rather than proceeding: inserting into a table that
    // still holds the old rows would duplicate every pair, and FTS5 has no
    // unique index to refuse it.
    if rebuild {
        match db::open_read_write(db_path).and_then(|conn| search::delete_all(&conn)) {
            Ok(()) => {}
            Err(e) => {
                log::warn!("search: cannot clear the index to rebuild it: {e}");
                return;
            }
        }
    }

    let mut every_batch_committed = true;
    for chunk in pending.chunks(BATCH_SIZE) {
        let write = if rebuild {
            Write::Insert
        } else {
            Write::Replace
        };
        every_batch_committed &= process_batch(db_path, chunk.iter().cloned().collect(), write);
    }

    if rebuild && every_batch_committed {
        record_index_version(db_path);
    }
}

/// How a batch puts a session's document into the index.
///
/// The two differ only in whether the scan-costing delete runs first, and
/// choosing wrongly is silent in both directions: `Insert` against a populated
/// table duplicates rows, `Replace` during a rebuild is the quadratic cost
/// [`sweep`]'s header describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    /// The incremental path: a row for this pair may already exist.
    Replace,
    /// The rebuild path, immediately after [`search::delete_all`], where no row
    /// for any pair exists.
    Insert,
}

/// Stamp `claude_cache_metadata.search_index_version`.
///
/// Best-effort, like every marker `scan.rs` records: losing it costs one
/// redundant rebuild at the next sweep, where failing the sweep costs the
/// corpus its insights too.
fn record_index_version(db_path: &Path) {
    let result = db::open_read_write(db_path)
        .and_then(|conn| search::record_version(&conn, search::SEARCH_INDEX_VERSION));
    match result {
        Ok(()) => log::info!(
            "search: index rebuilt at version {}",
            search::SEARCH_INDEX_VERSION
        ),
        Err(e) => log::warn!("search: failed to record search_index_version: {e}"),
    }
}

/// Read every session in the batch in parallel, then write the results in one
/// transaction.
///
/// Answers whether the batch got as far as the database agreeing with it, which
/// [`sweep`] needs in order to decide whether a rebuild may stamp its version.
///
/// **`false` means a *database* failure, never an unreadable transcript.** That
/// distinction is the whole value of the return: a real corpus always contains
/// some session whose file has been deleted, truncated or replaced since it was
/// cached, and `read_in_parallel` skips those by design. Counting a skip as
/// failure would mean the version is never recorded on any real machine, so
/// every five-minute sweep would rebuild the entire index again — for ever, at
/// full corpus cost, with nothing in the log to say why. A batch with nothing
/// readable in it is therefore `true`: nothing was committed because there was
/// nothing to commit.
///
/// What is left blocking a stamp is exactly the set of things that would make
/// the rebuild genuinely incomplete and are worth retrying: the settings read,
/// opening the database for writing, and the batch transaction itself.
fn process_batch(db_path: &Path, batch: BTreeSet<Pending>, write: Write) -> bool {
    let items: Vec<Pending> = batch
        .into_iter()
        // `rescanOutdated` skips a row with no file path, and so does this:
        // there is no transcript to compute from, and it is the shape a cache
        // row can never legitimately have.
        .filter(|item| !item.file_path.is_empty())
        .collect();
    if items.is_empty() {
        return true;
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
            return false;
        }
    };

    let mut computed = read_in_parallel(&items, idle_gap_ms, resolver.as_ref());
    if computed.is_empty() {
        // Every transcript in this batch was unreadable. Nothing to commit, and
        // **not a failure** — see the note on the return value: an unreadable
        // transcript is a skip by design, not an error to block a rebuild on.
        return true;
    }

    let mut conn = match db::open_read_write(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("insights: cannot open the database to store a batch: {e}");
            return false;
        }
    };
    match write_batch(&mut conn, &mut computed, write) {
        Ok(n) => {
            log::debug!("insights: stored {n} session insights and search rows");
            true
        }
        // Dropped rather than retried, for `apply.rs`'s reason: the rows keep
        // their old `processor_version` (or none), so they still look
        // unprocessed to the next sweep and are recomputed then. Retrying here
        // risks looping on a persistent error.
        Err(e) => {
            log::warn!("insights: failed to store a batch, dropping it: {e}");
            false
        }
    }
}

/// One computed insight, its search document, and the project path both belong
/// to.
///
/// The two travel together from the read to the write because they are written
/// together — see [`write_batch`].
struct Computed {
    project_path: String,
    insight: SessionInsight,
    doc: search::SearchDoc,
}

/// The reader pool, in `scanner/apply.rs`'s shape and for its reasons: decoding
/// a transcript is I/O plus JSON and parallelizes; the bound keeps the machine
/// usable while the user is working.
///
/// It reuses `scan_readers()` rather than a bound of its own, which means a
/// sweep overlapping a scan can put **twice** that many readers on the disk.
/// Accepted rather than coordinated: the two only overlap on a boot that both
/// finds a stale corpus and has outdated insights, the overlap is measured in
/// seconds (11s for 1,145 sessions against ~2s for the scan on the reference
/// machine), and a semaphore shared between two subsystems is real coupling for
/// a transient. If it ever needs fixing, halve this pool rather than gating the
/// scan — the scan is what the user is waiting for and this is not.
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
                // The search document is built from the very same decode the
                // processors consume — that is what makes indexing cost no
                // additional file I/O. See `insights::index`.
                let mut doc = DocAccumulator::new();
                match processors::run(&item.session_id, &files, &ctx, &mut doc) {
                    Ok(insight) => out
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(Computed {
                            project_path: item.project_path.clone(),
                            doc: doc.into_doc(&item.session_id, &item.project_path),
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

/// One transaction covering both writes, for every session in the batch.
///
/// **The `session_insights` row and the `session_search` row commit or fail
/// together**, which is the acceptance criterion and also the only arrangement
/// that stays consistent: `processor_version` is what tells the next sweep a
/// session is done, so an index write that failed after the insight row landed
/// would leave that session permanently unindexed with nothing reporting it.
/// `search::replace` is itself a delete followed by an insert and is *not*
/// atomic on its own — this transaction is what makes it so, which is exactly
/// why `search/mod.rs` opens no connection of its own.
///
/// The title is read here rather than carried from the read pass because it
/// lives in the cache row, not the transcript. Reading it inside the transaction
/// buys **batch-internal consistency** — every session in one batch sees one
/// state of the cache — and deliberately not freshness: this is `BEGIN
/// DEFERRED`, and a rename landing an instant after the commit is invisible
/// either way. A rename does not re-index at all; `store::display_title` records
/// why.
///
/// `?` on either write aborts the whole batch, and the `Transaction`'s `Drop`
/// rolls it back — so a failure leaves neither table changed for any session in
/// it, and every one of them still looks unprocessed to the next sweep.
///
/// `computed` is taken by `&mut` so the title can be filled **in place**. A
/// `SearchDoc` carries up to `normalize::SESSION_CAP` of text, and a batch is a
/// hundred of them, so building a titled copy per row would clone tens of
/// megabytes per batch to add a few dozen bytes to each.
fn write_batch(
    conn: &mut Connection,
    computed: &mut [Computed],
    write: Write,
) -> Result<usize, String> {
    let scanned_at = store::scanned_at_now();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for item in computed.iter_mut() {
        store::upsert(&tx, &item.insight, &item.project_path, &scanned_at)?;
        let title = store::display_title(&tx, &item.doc.session_id, &item.doc.project_path);
        item.doc.title = super::index::normalize_title(&title);
        match write {
            Write::Replace => search::replace(&tx, &item.doc)?,
            Write::Insert => search::insert(&tx, &item.doc)?,
        }
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

    // ─── the indexing pipeline (#435) ────────────────────────────────────────
    //
    // These drive `process_batch` and `sweep` against a real database and real
    // transcript files, because every property below is about what the *rows*
    // say after a pass — which is the same reason `scanner/` is verified against
    // stored rows rather than a response.

    use rusqlite::Connection;

    /// A migrated database in `dir`, plus its path.
    ///
    /// `ensure_database` rather than `open_read_write`, which does **not** carry
    /// `SQLITE_OPEN_CREATE` — the app's startup path is the only thing that
    /// creates the file, and these tests take the same route it does.
    fn fixture_db(dir: &tempfile::TempDir) -> PathBuf {
        let db_path = dir.path().join("agento.db");
        let mut conn = db::ensure_database(&db_path).expect("create");
        crate::native::migrate::apply(&mut conn).expect("migrations");
        db_path
    }

    /// Write a two-message transcript and register it as a cache row.
    ///
    /// The text is the fixture's whole point, so each session gets its own
    /// distinctive word to search for.
    fn seed_session(
        dir: &tempfile::TempDir,
        db_path: &Path,
        session_id: &str,
        project_path: &str,
        word: &str,
    ) -> PathBuf {
        let file = dir.path().join(format!(
            "{session_id}-{}.jsonl",
            project_path.replace('/', "_")
        ));
        let lines = [
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-01-01T00:00:00Z",
                "message": {"role": "user", "content": format!("please fix the {word} problem")},
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-01-01T00:01:00Z",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "looking at it now"}],
                },
            }),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&file, body).expect("write transcript");

        let conn = db::open_read_write(db_path).expect("open");
        conn.execute(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity)
             VALUES (?1, ?2, ?3, '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00',
                     '2026-01-01 00:00:00+00:00')",
            rusqlite::params![session_id, project_path, file.to_string_lossy()],
        )
        .expect("cache row");
        file
    }

    fn pending_for(session_id: &str, project_path: &str, file: &Path) -> Pending {
        Pending {
            session_id: session_id.into(),
            project_path: project_path.into(),
            file_path: file.to_string_lossy().into_owned(),
        }
    }

    fn indexed_pairs(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT session_id, project_path FROM session_search ORDER BY 1, 2")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    }

    fn mtimes(conn: &Connection) -> Vec<(String, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT session_id, project_path, file_mtime
                   FROM claude_session_cache ORDER BY 1, 2",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    }

    /// The headline acceptance criterion: one pass over an announced session
    /// leaves a *searchable* row, not merely a row.
    ///
    /// Asserted through `search::search` rather than by counting rows, because a
    /// row holding the wrong columns — a title in `user_text`, or empty text —
    /// counts exactly the same and answers nothing.
    #[test]
    fn one_pass_over_an_announced_session_leaves_a_searchable_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let file = seed_session(&dir, &db_path, "s1", "/a", "pagination");

        assert!(process_batch(
            &db_path,
            [pending_for("s1", "/a", &file)].into_iter().collect(),
            Write::Replace,
        ));

        let conn = db::open_read_only(&db_path).expect("open");
        let hits = search::search(&conn, "\"pagination\"", 10).expect("search");
        assert_eq!(
            hits.iter()
                .map(|h| (h.session_id.as_str(), h.project_path.as_str()))
                .collect::<Vec<_>>(),
            vec![("s1", "/a")],
        );
        // The assistant's side of the conversation is indexed too, in its own
        // column — a document built from user turns alone would pass the check
        // above.
        assert_eq!(
            search::search(&conn, "\"looking at it now\"", 10)
                .expect("search")
                .len(),
            1,
        );
    }

    /// The title comes from the cache row, and follows the display precedence
    /// the UI uses — the user's own rename first.
    #[test]
    fn the_indexed_title_is_the_cache_rows_display_title() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let file = seed_session(&dir, &db_path, "s1", "/a", "pagination");
        let conn = db::open_read_write(&db_path).expect("open");
        conn.execute(
            "UPDATE claude_session_cache
                SET custom_title = 'quarterly rollup', native_title = 'ignored'",
            [],
        )
        .expect("rename");
        drop(conn);

        assert!(process_batch(
            &db_path,
            [pending_for("s1", "/a", &file)].into_iter().collect(),
            Write::Replace,
        ));

        let conn = db::open_read_only(&db_path).expect("open");
        let title: String = conn
            .query_row("SELECT title FROM session_search", [], |r| r.get(0))
            .expect("title");
        assert_eq!(title, "quarterly rollup");
    }

    /// The rest of the precedence, which the `custom_title` test above cannot
    /// reach: native beats AI beats the first prompt, and a session with none of
    /// them is indexed untitled rather than not at all.
    #[test]
    fn the_indexed_title_falls_back_through_the_display_precedence() {
        for (columns, expected) in [
            ("native_title = 'native', ai_title = 'ai'", "native"),
            ("ai_title = 'ai'", "ai"),
            (
                "preview = 'the first thing they typed'",
                "the first thing they typed",
            ),
            ("preview = ''", ""),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = fixture_db(&dir);
            let file = seed_session(&dir, &db_path, "s1", "/a", "pagination");
            let conn = db::open_read_write(&db_path).expect("open");
            conn.execute(&format!("UPDATE claude_session_cache SET {columns}"), [])
                .expect("set titles");
            drop(conn);

            assert!(process_batch(
                &db_path,
                [pending_for("s1", "/a", &file)].into_iter().collect(),
                Write::Replace,
            ));

            let conn = db::open_read_only(&db_path).expect("open");
            let title: String = conn
                .query_row("SELECT title FROM session_search", [], |r| r.get(0))
                .expect("title");
            assert_eq!(title, expected, "for {columns}");
        }
    }

    /// A rebuild must not leave a session carrying text this build's indexer no
    /// longer produces.
    ///
    /// The failure it guards is silent and looks healthy: with per-session
    /// replacement, a session the rebuild **skips** keeps its previous row — old
    /// format — while the version stamp says the whole index is current, so
    /// nothing ever revisits it. Emptying first makes a skip visible as a
    /// *missing* row instead, which is a state `needs_processing` can still
    /// describe.
    ///
    /// The marker row here stands in for "indexed by an older build": it is
    /// present before the rebuild, belongs to a session the rebuild cannot read,
    /// and must be gone afterwards.
    #[test]
    fn a_rebuild_leaves_no_row_from_the_previous_index_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        seed_session(&dir, &db_path, "good", "/a", "pagination");

        let conn = db::open_read_write(&db_path).expect("open");
        // Still cached, but its transcript cannot be read — the shape an
        // unmounted drive or a permissions change leaves, which `scan.rs`
        // deliberately protects from deletion.
        conn.execute(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity)
             VALUES ('unreadable', '/a', ?1, '2026-01-01 00:00:00+00:00',
                     '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00')",
            rusqlite::params![dir.path().join("absent.jsonl").to_string_lossy()],
        )
        .expect("cache row");
        search::replace(
            &conn,
            &search::SearchDoc {
                session_id: "unreadable".into(),
                project_path: "/a".into(),
                user_text: "obsoletemarker".into(),
                ..Default::default()
            },
        )
        .expect("seed a previous-version row");
        search::record_version(&conn, search::SEARCH_INDEX_VERSION - 1).expect("age");
        drop(conn);

        sweep(&db_path);

        let conn = db::open_read_only(&db_path).expect("open");
        assert!(
            search::search(&conn, "\"obsoletemarker\"", 10)
                .expect("search")
                .is_empty(),
            "text from the previous index version survived the rebuild",
        );
        assert_eq!(indexed_pairs(&conn), vec![("good".into(), "/a".into())]);
    }

    /// The rebuild path must not leave two rows for one pair.
    ///
    /// It inserts without deleting, which is only sound because the index was
    /// emptied first — and FTS5 has no unique index to refuse a duplicate, so
    /// getting this wrong doubles every hit silently rather than failing.
    #[test]
    fn a_rebuild_does_not_duplicate_a_pair_that_was_already_indexed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        seed_session(&dir, &db_path, "s1", "/a", "pagination");

        // Index it once at the current version, then force a rebuild over it.
        sweep(&db_path);
        let conn = db::open_read_write(&db_path).expect("open");
        search::record_version(&conn, search::SEARCH_INDEX_VERSION - 1).expect("age");
        drop(conn);

        sweep(&db_path);

        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(indexed_pairs(&conn), vec![("s1".into(), "/a".into())]);
        assert_eq!(
            search::search(&conn, "\"pagination\"", 10)
                .expect("search")
                .len(),
            1,
            "the pair was indexed twice, so every hit for it is doubled",
        );
    }

    /// The #362-family assertion, in its fifth form: one session id under two
    /// project paths is two transcripts and must be two independent index rows.
    ///
    /// Keyed on the id alone, `search::replace`'s delete would remove both and
    /// the batch would end with one row holding the second transcript's text —
    /// a session silently missing from the index with nothing to report it.
    #[test]
    fn one_session_id_under_two_projects_gets_two_index_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let a = seed_session(&dir, &db_path, "s1", "/a", "kestrel");
        let b = seed_session(&dir, &db_path, "s1", "/b", "petrel");

        assert!(process_batch(
            &db_path,
            [pending_for("s1", "/a", &a), pending_for("s1", "/b", &b)]
                .into_iter()
                .collect(),
            Write::Replace,
        ));

        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(
            indexed_pairs(&conn),
            vec![("s1".into(), "/a".into()), ("s1".into(), "/b".into())],
        );
        // …and each carries its own transcript's text, which a shared row could
        // not.
        for (word, project) in [("kestrel", "/a"), ("petrel", "/b")] {
            let hits = search::search(&conn, &format!("\"{word}\""), 10).expect("search");
            assert_eq!(
                hits.iter()
                    .map(|h| h.project_path.as_str())
                    .collect::<Vec<_>>(),
                vec![project],
                "{word} must belong to {project} alone",
            );
        }
    }

    /// Bumping `SEARCH_INDEX_VERSION` rebuilds every row **without** the scanner
    /// re-reading anything.
    ///
    /// The `file_mtime` assertion is the whole point and is easy to lose: a
    /// rebuild driven by the scanner's staleness markers would also produce a
    /// correct index, and would additionally re-read the entire corpus from
    /// disk. Zeroed mtimes are what that looks like, so they are compared before
    /// and after.
    ///
    /// The stored version is set to a value that is not `SEARCH_INDEX_VERSION`
    /// rather than to a literal, so bumping the constant does not turn this into
    /// a test of nothing.
    #[test]
    fn a_version_bump_reindexes_without_touching_the_scanners_mtimes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let file = seed_session(&dir, &db_path, "s1", "/a", "pagination");

        // A first pass, at the current version.
        sweep(&db_path);
        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(indexed_pairs(&conn).len(), 1);
        assert_eq!(
            search::stored_version(&conn).expect("version"),
            search::SEARCH_INDEX_VERSION,
            "a completed sweep stamps the version it indexed at",
        );
        let before = mtimes(&conn);
        drop(conn);

        // Now pretend this build's indexer disagrees with what is stored, and
        // empty the index the way a rebuild's first act would not: if the sweep
        // does nothing, the rows stay gone and the assertion below fails.
        let conn = db::open_read_write(&db_path).expect("open");
        search::record_version(&conn, search::SEARCH_INDEX_VERSION - 1).expect("age the version");
        search::delete_all(&conn).expect("empty the index");
        drop(conn);

        sweep(&db_path);

        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(
            indexed_pairs(&conn),
            vec![("s1".into(), "/a".into())],
            "a version mismatch must reindex every session",
        );
        assert_eq!(
            search::stored_version(&conn).expect("version"),
            search::SEARCH_INDEX_VERSION,
            "and stamp the new version once the rebuild covered the corpus",
        );
        assert_eq!(
            mtimes(&conn),
            before,
            "a search rebuild must not make the scanner re-read a transcript",
        );
        // The insight row is current throughout, which is what makes the
        // ordinary `needs_processing` scope unable to express this rebuild.
        let stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_insights WHERE processor_version < ?1",
                rusqlite::params![CURRENT_PROCESSOR_VERSION],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(stale, 0);
        let _ = file;
    }

    /// An ordinary sweep, with the version already current, must not rebuild.
    ///
    /// Without the equality check this would re-index the whole corpus every
    /// five minutes forever — correct output, and a permanent full-corpus read
    /// nothing would ever report.
    #[test]
    fn a_sweep_at_the_current_version_reindexes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        seed_session(&dir, &db_path, "s1", "/a", "pagination");

        sweep(&db_path);
        // Emptying the index without moving the version is how "did the second
        // sweep decide to rebuild?" becomes observable at all.
        let conn = db::open_read_write(&db_path).expect("open");
        search::delete_all(&conn).expect("empty the index");
        drop(conn);

        sweep(&db_path);

        let conn = db::open_read_only(&db_path).expect("open");
        assert!(
            indexed_pairs(&conn).is_empty(),
            "nothing was stale, so nothing should have been re-read",
        );
    }

    /// An empty corpus still reaches the new version, rather than asking for a
    /// rebuild on every five-minute tick for the life of the process.
    #[test]
    fn a_rebuild_over_an_empty_corpus_still_records_the_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let conn = db::open_read_write(&db_path).expect("open");
        search::record_version(&conn, search::SEARCH_INDEX_VERSION - 1).expect("age");
        drop(conn);

        sweep(&db_path);

        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(
            search::stored_version(&conn).expect("version"),
            search::SEARCH_INDEX_VERSION,
        );
    }

    /// The two writes are one transaction: if the index write fails, the insight
    /// row must not survive either.
    ///
    /// Injected by dropping `session_search` before the batch, which makes
    /// `search::replace` fail on a real `no such table` rather than on a
    /// test-only hook — so this exercises the same `?` a genuine FTS5 error
    /// would take.
    ///
    /// The consequence being pinned is not tidiness: `processor_version` is what
    /// tells the next sweep a session is done, so an insight row that outlived a
    /// failed index write would mark that session complete and it would never be
    /// indexed again.
    #[test]
    fn a_failed_index_write_rolls_back_the_insight_row_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let file = seed_session(&dir, &db_path, "s1", "/a", "pagination");

        let conn = db::open_read_write(&db_path).expect("open");
        conn.execute("DROP TABLE session_search", [])
            .expect("drop the index");
        drop(conn);

        assert!(
            !process_batch(
                &db_path,
                [pending_for("s1", "/a", &file)].into_iter().collect(),
                Write::Replace,
            ),
            "a batch that could not commit must report so",
        );

        let conn = db::open_read_only(&db_path).expect("open");
        let insights: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_insights", [], |r| r.get(0))
            .expect("count");
        assert_eq!(
            insights, 0,
            "the insight row committed without its index row",
        );
        // …and the session is therefore still pending, which is what makes the
        // failure recoverable rather than permanent.
        assert_eq!(
            store::needs_processing(&conn, CURRENT_PROCESSOR_VERSION)
                .expect("needs_processing")
                .len(),
            1,
        );
    }

    /// An unreadable transcript is a **skip, not a failure**, and the batch says
    /// so.
    ///
    /// This looks like the lenient answer and is the strict one. Every real
    /// corpus holds a session whose file has since been deleted or truncated —
    /// `insights_live.rs` allows a 10% shortfall for exactly that — so if a skip
    /// counted as failure, [`sweep`] would never stamp the version on any real
    /// machine and would rebuild the whole index every five minutes for the life
    /// of the process. Correct output, unbounded cost, and nothing in the log
    /// naming the cause.
    #[test]
    fn an_unreadable_transcript_is_a_skip_rather_than_a_batch_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let missing = dir.path().join("absent.jsonl");

        assert!(process_batch(
            &db_path,
            [pending_for("s1", "/a", &missing)].into_iter().collect(),
            Write::Replace,
        ));
    }

    /// …and the consequence, end to end: a corpus containing an unreadable
    /// transcript still finishes its rebuild.
    ///
    /// The second sweep is what makes this an assertion about *termination*
    /// rather than about one pass: with the skip counted as a failure the
    /// version stays behind, so the second sweep rebuilds all over again.
    #[test]
    fn a_rebuild_completes_over_a_corpus_with_an_unreadable_transcript() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        seed_session(&dir, &db_path, "good", "/a", "pagination");
        // A cache row whose transcript does not exist — the ordinary shape of a
        // session deleted from disk since it was scanned.
        let conn = db::open_read_write(&db_path).expect("open");
        conn.execute(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity)
             VALUES ('gone', '/a', ?1, '2026-01-01 00:00:00+00:00',
                     '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00')",
            rusqlite::params![dir.path().join("absent.jsonl").to_string_lossy()],
        )
        .expect("cache row");
        search::record_version(&conn, search::SEARCH_INDEX_VERSION - 1).expect("age");
        drop(conn);

        sweep(&db_path);

        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(
            search::stored_version(&conn).expect("version"),
            search::SEARCH_INDEX_VERSION,
            "one unreadable transcript must not stop the rebuild concluding",
        );
        assert_eq!(indexed_pairs(&conn), vec![("good".into(), "/a".into())]);
        drop(conn);

        // Termination: with the version stamped, a second sweep has nothing to
        // rebuild. Emptying the index first is what makes that observable.
        let conn = db::open_read_write(&db_path).expect("open");
        search::delete_all(&conn).expect("empty");
        drop(conn);

        sweep(&db_path);

        let conn = db::open_read_only(&db_path).expect("open");
        assert!(
            indexed_pairs(&conn).is_empty(),
            "the rebuild repeated, so it would repeat every five minutes for ever",
        );
    }

    /// A batch that could not open the database for writing **is** a failure,
    /// so the two arms of the return value are pinned against each other rather
    /// than only the lenient one being asserted.
    #[test]
    fn a_database_failure_is_a_batch_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let file = seed_session(&dir, &db_path, "s1", "/a", "pagination");
        std::fs::remove_file(&db_path).expect("remove the database");

        assert!(!process_batch(
            &db_path,
            [pending_for("s1", "/a", &file)].into_iter().collect(),
            Write::Replace,
        ));
    }

    // ─── the loop itself (#447) ──────────────────────────────────────────────
    //
    // Everything below drives `run_once`, which is `run`'s whole body. Until it
    // was split out, `recv_timeout` → batch accumulation → the `BATCH_SIZE`
    // cutoff → the `SWEEP_REQUESTED` follow-up were reachable from no test at
    // all; only `offer` was covered.
    //
    // The contended-write-lock test that used to sit here has moved to
    // `tests/insights_worker.rs`, because the property it is named for is a
    // claim about `start` and only a test driving `start` can falsify it.

    /// Serialises the tests that drive [`run_once`].
    ///
    /// [`SWEEP_REQUESTED`] is process-wide and a pass **clears** it, so two of
    /// these running concurrently would steal each other's flag — the sweep
    /// test would see it already consumed, and a queue-path test would take an
    /// unasked-for sweep that indexes its fixture for the wrong reason. Each
    /// test also stores `false` on entry, so none of them depends on another's
    /// leftovers either.
    fn run_once_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// #435's first acceptance criterion, at last driven through the code that
    /// implements it: a session announced by a scan is searchable **after one
    /// queue pass**.
    ///
    /// Every other test in this file calls `process_batch` directly, which
    /// skips `recv_timeout` and the accumulation entirely. Here the only input
    /// is a `Pending` on a channel, exactly as `enqueue` delivers one.
    ///
    /// The flag is cleared first and the arm is asserted, which together are
    /// what make this a test *of the queue*: a pass that swept would index the
    /// same session for a completely different reason and look identical.
    #[test]
    fn one_pass_over_a_queued_session_leaves_a_searchable_row() {
        let _serialised = run_once_lock();
        SWEEP_REQUESTED.store(false, Ordering::Release);

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let file = seed_session(&dir, &db_path, "s1", "/a", "pagination");

        let (tx, rx) = mpsc::sync_channel::<Pending>(QUEUE_SIZE);
        tx.send(pending_for("s1", "/a", &file)).expect("announce");

        assert_eq!(
            run_once(&db_path, &rx, Duration::from_millis(50)),
            Pass::Batch {
                size: 1,
                committed: true
            },
            "the item must arrive as a batch, not as a timed-out sweep",
        );

        let conn = db::open_read_only(&db_path).expect("open");
        let hits = search::search(&conn, "\"pagination\"", 10).expect("search");
        assert_eq!(
            hits.iter()
                .map(|h| (h.session_id.as_str(), h.project_path.as_str()))
                .collect::<Vec<_>>(),
            vec![("s1", "/a")],
            "one pass over an announced session must leave a searchable row",
        );
    }

    /// The accumulation stops at [`BATCH_SIZE`], and the remainder is taken by
    /// the next pass rather than dropped.
    ///
    /// Both halves matter and they fail in opposite directions: no cutoff makes
    /// one transaction unbounded in size (and, since #435, in memory — see
    /// `BATCH_SIZE`'s own header), while a cutoff that consumed the rest would
    /// silently lose every announcement past the hundredth.
    ///
    /// The transcripts do not exist, so nothing is committed; this is a test of
    /// how many items one pass *takes*, which `Pass::size` reports directly.
    #[test]
    fn a_pass_takes_at_most_batch_size_and_the_rest_waits_for_the_next() {
        let _serialised = run_once_lock();
        SWEEP_REQUESTED.store(false, Ordering::Release);

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);

        const EXTRA: usize = 5;
        let (tx, rx) = mpsc::sync_channel::<Pending>(BATCH_SIZE + EXTRA);
        for i in 0..BATCH_SIZE + EXTRA {
            tx.send(pending(&format!("s{i:04}"))).expect("announce");
        }

        assert!(
            matches!(
                run_once(&db_path, &rx, Duration::from_millis(50)),
                Pass::Batch {
                    size: BATCH_SIZE,
                    ..
                }
            ),
            "one pass must not take more than BATCH_SIZE",
        );
        assert!(
            matches!(
                run_once(&db_path, &rx, Duration::from_millis(50)),
                Pass::Batch { size: EXTRA, .. }
            ),
            "the remainder must be taken by the next pass, not dropped",
        );
    }

    /// A pass with [`SWEEP_REQUESTED`] set runs the sweep and leaves the flag
    /// clear.
    ///
    /// Asserted through the sweep's **effect**: `swept` is never announced, so
    /// the only thing that can index it is the follow-up sweep. A `bool` on
    /// [`Pass`] would have been cheaper and would have pinned the branch rather
    /// than the work.
    ///
    /// This is what makes a full queue cost one sweep instead of a five-minute
    /// hole — `SWEEP_REQUESTED`'s own header records why that matters, and
    /// `everything_past_the_queues_capacity_is_reported_as_overflow` covers the
    /// half that *sets* the flag.
    #[test]
    fn a_requested_sweep_runs_at_the_end_of_the_pass_and_clears_the_flag() {
        let _serialised = run_once_lock();
        SWEEP_REQUESTED.store(false, Ordering::Release);

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let queued = seed_session(&dir, &db_path, "queued", "/a", "pagination");
        // Cached, needing an insight row, and deliberately never announced —
        // the shape of an announcement `enqueue` had to drop.
        seed_session(&dir, &db_path, "swept", "/a", "overflowed");

        let (tx, rx) = mpsc::sync_channel::<Pending>(QUEUE_SIZE);
        tx.send(pending_for("queued", "/a", &queued))
            .expect("announce");
        SWEEP_REQUESTED.store(true, Ordering::Release);

        assert_eq!(
            run_once(&db_path, &rx, Duration::from_millis(50)),
            Pass::Batch {
                size: 1,
                committed: true
            },
        );

        assert!(
            !SWEEP_REQUESTED.load(Ordering::Acquire),
            "the flag must be cleared, or every later pass sweeps for ever",
        );
        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(
            search::search(&conn, "\"overflowed\"", 10)
                .expect("search")
                .len(),
            1,
            "a session that was only ever dropped from the queue must be \
             picked up by the sweep the overflow requested",
        );
    }

    /// Every sender gone ends the loop, which is the one arm `run`'s `while`
    /// condition reads.
    #[test]
    fn a_disconnected_queue_ends_the_loop() {
        let _serialised = run_once_lock();
        SWEEP_REQUESTED.store(false, Ordering::Release);

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let (tx, rx) = mpsc::sync_channel::<Pending>(1);
        drop(tx);

        assert_eq!(
            run_once(&db_path, &rx, Duration::from_millis(50)),
            Pass::Disconnected,
        );
    }

    /// …and an idle queue times out into the periodic sweep rather than
    /// spinning.
    #[test]
    fn an_idle_queue_times_out_into_a_sweep() {
        let _serialised = run_once_lock();
        SWEEP_REQUESTED.store(false, Ordering::Release);

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        seed_session(&dir, &db_path, "s1", "/a", "pagination");

        // Held, so the channel is idle rather than disconnected.
        let (_tx, rx) = mpsc::sync_channel::<Pending>(1);

        assert_eq!(
            run_once(&db_path, &rx, Duration::from_millis(50)),
            Pass::Swept,
        );
        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(indexed_pairs(&conn), vec![("s1".into(), "/a".into())]);
    }
}

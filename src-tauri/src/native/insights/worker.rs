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

    let mut every_batch_committed = true;
    for chunk in pending.chunks(BATCH_SIZE) {
        every_batch_committed &= process_batch(db_path, chunk.iter().cloned().collect());
    }

    if rebuild && every_batch_committed {
        record_index_version(db_path);
    }
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
fn process_batch(db_path: &Path, batch: BTreeSet<Pending>) -> bool {
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
    match write_batch(&mut conn, &mut computed) {
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
/// lives in the cache row, not the transcript. Inside the transaction, so it
/// cannot be a value from before a concurrent rename.
///
/// `?` on either write aborts the whole batch, and the `Transaction`'s `Drop`
/// rolls it back — so a failure leaves neither table changed for any session in
/// it, and every one of them still looks unprocessed to the next sweep.
///
/// `computed` is taken by `&mut` so the title can be filled **in place**. A
/// `SearchDoc` carries up to `normalize::SESSION_CAP` of text, and a batch is a
/// hundred of them, so building a titled copy per row would clone tens of
/// megabytes per batch to add a few dozen bytes to each.
fn write_batch(conn: &mut Connection, computed: &mut [Computed]) -> Result<usize, String> {
    let scanned_at = store::scanned_at_now();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for item in computed.iter_mut() {
        store::upsert(&tx, &item.insight, &item.project_path, &scanned_at)?;
        let title = store::display_title(&tx, &item.doc.session_id, &item.doc.project_path);
        item.doc.title = super::index::normalize_title(&title);
        search::replace(&tx, &item.doc)?;
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
        ));

        let conn = db::open_read_only(&db_path).expect("open");
        let title: String = conn
            .query_row("SELECT title FROM session_search", [], |r| r.get(0))
            .expect("title");
        assert_eq!(title, "quarterly rollup");
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
                [pending_for("s1", "/a", &file)].into_iter().collect()
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
        ));
    }

    /// The #366 pattern (`tests/scheduled_run.rs`, `tests/chat_turn.rs`), with
    /// indexing active.
    ///
    /// The worker is a plain OS thread and deliberately holds no runtime handle
    /// — its header says so — so what this pins is that the property survives
    /// the extra database work #435 puts inside the batch transaction. A batch
    /// now writes `session_insights` *and* an FTS5 row per session while holding
    /// one write lock for longer, so "the worker's writes are off the runtime"
    /// is a claim worth re-asserting rather than inheriting.
    ///
    /// The shape is the original's: **one** worker thread so a single parked
    /// worker is the whole runtime; a plain OS thread holding the lock, so the
    /// contention comes from outside exactly as the session scanner's batch
    /// writer does; and `last` seeded **before** the spawn, because a starved
    /// ticker is never polled and seeding on the first poll would start the
    /// clock after the stall and measure nothing.
    ///
    /// **Calibrated rather than assumed.** Replacing the `std::thread::spawn`
    /// below with `tokio::spawn` — i.e. putting the batch's rusqlite work on the
    /// single worker, which is the defect this shape exists to catch — takes the
    /// ticker from ~150 advances to **0** and fails the assertion. Note it must
    /// be `tokio::spawn` to falsify it: calling `process_batch` inline in this
    /// async body blocks the *calling* thread, which `block_on` runs the test
    /// future on, and the ticker on the worker keeps going regardless. That
    /// version of the test passes against the defect, which is the trap the
    /// scheduler's copy documents too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_contended_write_lock_does_not_stall_the_runtime() {
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;
        use std::time::Instant;

        /// Long enough that a parked worker is unmistakable, short enough to
        /// stay well inside `open_read_write`'s 5s `busy_timeout` so the batch
        /// itself still commits.
        const HOLD: Duration = Duration::from_millis(1_500);

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let file = seed_session(&dir, &db_path, "s1", "/a", "pagination");

        // The file must already be WAL. Left in the default rollback journal,
        // `open_read_write`'s own `PRAGMA journal_mode=WAL` is a *mode change*
        // needing an exclusive lock and fails outright in about a millisecond
        // instead of waiting on `busy_timeout` — which would make this measure
        // the wrong thing entirely.
        db::open_read_write(&db_path).expect("convert the fixture to WAL");

        let (holding_tx, holding_rx) = mpsc::channel();
        let lock_db = db_path.clone();
        let holder = std::thread::spawn(move || {
            let mut conn = rusqlite::Connection::open(&lock_db).expect("open");
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .expect("begin immediate");
            holding_tx.send(()).expect("signal");
            std::thread::sleep(HOLD);
            tx.rollback().expect("rollback");
        });
        holding_rx.recv().expect("the writer took the lock");

        let worst_gap_ms = Arc::new(AtomicU64::new(0));
        let ticks = Arc::new(AtomicU64::new(0));
        let ticker = {
            let (worst_gap_ms, ticks, mut last) = (
                Arc::clone(&worst_gap_ms),
                Arc::clone(&ticks),
                Instant::now(),
            );
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let now = Instant::now();
                    let gap =
                        u64::try_from(now.duration_since(last).as_millis()).unwrap_or(u64::MAX);
                    worst_gap_ms.fetch_max(gap, Ordering::Relaxed);
                    ticks.fetch_add(1, Ordering::Relaxed);
                    last = now;
                }
            })
        };

        // Exactly how production runs it: a plain thread, never a tokio task.
        let batch_db = db_path.clone();
        let batch = std::thread::spawn(move || {
            process_batch(
                &batch_db,
                [pending_for("s1", "/a", &file)].into_iter().collect(),
            )
        });

        let committed = tokio::task::spawn_blocking(move || batch.join().expect("batch"))
            .await
            .expect("join");
        ticker.abort();
        holder.join().expect("the writer finished");

        assert!(committed, "the batch did not commit");

        let worst = worst_gap_ms.load(Ordering::Relaxed);
        assert!(
            worst < 500,
            "the runtime stalled for {worst} ms while the write lock was held \
             (the hold is {} ms; anything near it means indexing blocked a worker)",
            HOLD.as_millis(),
        );
        // The gap alone would read as healthy if the ticker had simply been
        // cancelled early, so assert it really ran throughout.
        let ticks = ticks.load(Ordering::Relaxed);
        assert!(
            ticks > 50,
            "the ticker only advanced {ticks} times across a {} ms hold",
            HOLD.as_millis(),
        );

        // …and both rows landed, so this is not passing because nothing
        // happened.
        let conn = db::open_read_only(&db_path).expect("open");
        assert_eq!(indexed_pairs(&conn), vec![("s1".into(), "/a".into())]);
    }
}

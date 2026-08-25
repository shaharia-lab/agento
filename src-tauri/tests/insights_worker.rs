//! The insight worker, driven through its **real entry point** (#447).
//!
//! ## This binary may contain exactly one test that calls `worker::start`
//!
//! That is the whole reason the file exists. `start` fills a process-wide
//! `QUEUE` `OnceLock`; a second call logs `insights: worker already started`
//! and **returns without spawning anything**, so a second `start`-driving test
//! in this binary would silently drive the *first* test's worker, against the
//! *first* test's database. `src-tauri/tests/*.rs` is one binary per file, so
//! one file is the unit of "once".
//!
//! It fails loudly rather than quietly if anyone adds one: every assertion here
//! waits on a row appearing in *this* test's database, so a no-op `start` ends
//! at the poll deadline with a named failure rather than a green run. Another
//! test needing `start` needs another file.
//!
//! ## Why this is not a unit test in `worker.rs`
//!
//! The property below is a claim about **where `start` puts the loop**, and a
//! unit test cannot make that claim: `run`/`run_once` are private, so a test
//! inside the module has to spawn the thread itself — at which point the test,
//! not `start`, decides whether the work is on the runtime, and no change to
//! non-test code can falsify it. That is exactly what the version of this test
//! that lived in `worker.rs` said about itself, and #447 is the issue filed to
//! fix it. The shape here is `tests/scheduled_run.rs`'s and
//! `tests/chat_turn.rs`' (#366), for the same reason: drive the real entry
//! point, measure a runtime beside it.

use std::path::{Path, PathBuf};

use agento_lib::native::insights::store::Pending;
use agento_lib::native::insights::worker;
use agento_lib::native::{db, migrate, search};

/// A migrated database in `dir`, plus its path.
///
/// `ensure_database` rather than `open_read_write`, which does **not** carry
/// `SQLITE_OPEN_CREATE` — the app's startup path is the only thing that creates
/// the file, and this takes the same route it does.
///
/// A copy of `worker.rs`'s test helper rather than a widened production
/// visibility: `tests/chat_turn.rs` already carries its own copy of a log sink
/// for the same reason, and adding `pub` to reach a helper from a test is the
/// thing this issue's design deliberately avoided.
fn fixture_db(dir: &Path) -> PathBuf {
    let db_path = dir.join("agento.db");
    let mut conn = db::ensure_database(&db_path).expect("create");
    migrate::apply(&mut conn).expect("migrations");
    db_path
}

/// Write a two-message transcript and register it as a cache row.
///
/// The text is the fixture's whole point, so the session gets a distinctive
/// word to search for. Also a copy of `worker.rs`'s helper.
fn seed_session(
    dir: &Path,
    db_path: &Path,
    session_id: &str,
    project_path: &str,
    word: &str,
) -> PathBuf {
    let file = dir.join(format!(
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

/// Whether the session is searchable yet — a WAL read, which never waits on the
/// writer holding the lock, so polling it cannot itself perturb the measurement.
fn indexed(db_path: &Path) -> bool {
    let Ok(conn) = db::open_read_only(db_path) else {
        return false;
    };
    search::search(&conn, "\"pagination\"", 10)
        .map(|hits| !hits.is_empty())
        .unwrap_or(false)
}

/// The worker's database work runs off the tokio runtime, and a batch meeting a
/// contended write lock still commits (#366, #447).
///
/// **The thing under test is `start`'s `std::thread::spawn`.** Everything the
/// worker does is blocking — `rusqlite` with a five-second `busy_timeout`, plus
/// transcript reads measured in seconds for a full corpus — so a worker on a
/// tokio task parks a runtime worker for the whole of it. Tokio runs one worker
/// per core, so on this one-worker runtime that is the entire runtime, and in
/// production it is the SPA and every SSE stream sharing it.
///
/// **Verified against the defect, which is the only thing that makes this test
/// worth its runtime**: with `start`'s `std::thread::spawn(move || run(..))`
/// changed to `tokio::spawn(async move { run(..) })`, this fails in 1.8 s with
/// *the ticker only advanced 0 times across a 1500 ms hold*. Note **which**
/// assertion catches it: a ticker that is never polled records no gap either,
/// so `worst` stays at 0 and reads as healthy — which is exactly why the tick
/// count is asserted beside it rather than the gap alone. A contention test
/// that passes either way is precisely the mistake #447 exists to fix.
///
/// It also covers the queue path against the real `enqueue`: the session is
/// announced exactly as `scan.rs` announces one, and the assertion is a
/// **searchable** row rather than a row, because a row holding the wrong
/// columns counts the same and answers nothing.
///
/// The rest of the shape is #366's, each part load-bearing:
///
/// - **one worker thread**, so a single parked worker is the whole runtime;
/// - the fixture converted to WAL **before** the contention begins — left on
///   the default rollback journal, `open_read_write`'s own
///   `PRAGMA journal_mode=WAL` is a *mode change* needing an exclusive lock and
///   fails outright in about a millisecond instead of waiting on
///   `busy_timeout`, which would measure the wrong thing entirely;
/// - **a plain OS thread** holds the lock, so the contention comes from outside
///   the runtime exactly as the session scanner's batch writer does;
/// - `last` seeded **before** the work starts, because a starved ticker is
///   never polled and seeding on the first poll would start the clock after the
///   stall and measure nothing.
#[test]
fn the_worker_does_its_database_work_off_the_runtime() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// How long the lock is held. Long enough that a parked worker is
    /// unmistakable, short enough to stay well inside `open_read_write`'s 5s
    /// `busy_timeout` so the worker's write still commits.
    const HOLD: Duration = Duration::from_millis(1_500);

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    let file = seed_session(dir.path(), &db_path, "s1", "/a", "pagination");

    db::open_read_write(&db_path).expect("convert the fixture to WAL");

    // A writer outside the runtime, holding the lock the worker needs.
    let (holding_tx, holding_rx) = std::sync::mpsc::channel();
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

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");

    let worst_gap_ms = Arc::new(AtomicU64::new(0));
    let ticks = Arc::new(AtomicU64::new(0));
    let committed = runtime.block_on(async {
        // The thing that must keep running. It records the **longest** gap
        // between its own ticks, which is what a parked worker shows up as.
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

        // The real entry point, and the real announcement. `start` runs its
        // boot sweep immediately and meets the held lock there; `enqueue` then
        // delivers the same session down the queue, so the pass that indexes it
        // is whichever reaches the lock first — both on the thread `start`
        // chose.
        worker::start(db_path.clone());
        worker::enqueue([Pending {
            session_id: "s1".into(),
            project_path: "/a".into(),
            file_path: file.to_string_lossy().into_owned(),
        }]);

        // Comfortably longer than HOLD plus a batch, and short enough to fail
        // rather than hang. A `start` that no-opped — see the file header —
        // ends here.
        //
        // **`std::thread::sleep`, not `tokio::time::sleep`, and that is not a
        // slip.** This runs on `block_on`'s calling thread rather than on a
        // worker, so blocking it starves nothing, while awaiting would depend
        // on the very runtime under test: on the revert, `run`'s loop owns the
        // only worker and never yields, so nothing is left to fire a timer and
        // an `await` here would wait for ever instead of reporting.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut committed = false;
        while Instant::now() < deadline {
            if indexed(&db_path) {
                committed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        ticker.abort();
        committed
    });

    // **The runtime is torn down before a single assertion runs, and with a
    // deadline.** On the revert, `run` is an endless loop on the only worker
    // and it parks in a blocking `recv_timeout` between passes, so an ordinary
    // drop — which is what `#[tokio::test]` does after the body, including
    // while unwinding from a failed assertion — waits for a thread that will
    // never return. Measured: the test hung indefinitely rather than failing,
    // which in CI is a wedged job instead of a red one. `shutdown_timeout`
    // gives up and leaks the thread, so the assertions below are reached in
    // both directions, which is the whole point of writing them.
    runtime.shutdown_timeout(Duration::from_millis(200));
    holder.join().expect("the writer finished");

    assert!(
        committed,
        "the worker never indexed the announced session — if a second test in \
         this binary calls `worker::start`, that is why (see the file header)",
    );

    let worst = worst_gap_ms.load(Ordering::Relaxed);
    assert!(
        worst < 500,
        "the runtime stalled for {worst} ms while the write lock was held \
         (the hold is {} ms; anything near it means the worker is on the runtime)",
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
}

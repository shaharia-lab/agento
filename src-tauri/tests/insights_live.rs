//! The insight worker against the machine's **real** corpus, on a copy of the
//! database.
//!
//! `#[ignore]`d, like `scan_live.rs` and for the same reason — CI has no
//! `~/.claude` and no `~/.agento/agento.db`. Run it by hand:
//!
//! ```bash
//! cargo test --test insights_live -- --ignored --nocapture
//! ```
//!
//! # Why this exists rather than a fixture
//!
//! #408 is the defect a fixture cannot see. Every processor was already ported,
//! unit-tested and pinned against the rows the Go worker wrote; what was missing
//! was the loop that calls them, and **nothing failed** — the table simply
//! stayed empty while the scan beside it reported success and the summary
//! endpoint answered 200 with zeros. A three-file fixture that gains three rows
//! does not distinguish that from a healthy build, because the bug was never in
//! the per-session computation.
//!
//! So the assertion is a *count over a real corpus*, taken from a copy of the
//! real database with its cache rows intact: the worker must produce an insight
//! row for every cached session, at the current processor version.

use std::path::{Path, PathBuf};

use agento_lib::native::insights::{processors::CURRENT_PROCESSOR_VERSION, store, worker};

fn real_db() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let db = PathBuf::from(home).join(".agento/agento.db");
    db.is_file().then_some(db)
}

fn scalar(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
}

/// Copy the database *and its WAL*, so the copy is not missing recent writes.
fn copy_db(src: &Path, dir: &Path) -> PathBuf {
    let db = dir.join("agento.db");
    std::fs::copy(src, &db).expect("copy the database");
    for ext in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{ext}", src.display()));
        if from.is_file() {
            let _ = std::fs::copy(&from, dir.join(format!("agento.db{ext}")));
        }
    }
    db
}

#[test]
#[ignore = "needs the machine's real ~/.agento database and ~/.claude corpus"]
fn every_cached_session_gains_an_insight_row() {
    let Some(src) = real_db() else {
        eprintln!("skipping: no ~/.agento/agento.db");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let db = copy_db(&src, dir.path());

    // **Migrate the copy, exactly as the app migrates before starting the
    // worker** (`lib.rs`: `migrate::apply` at startup, `worker::start` long
    // after). The installed database is whatever version the last release left,
    // and this test's whole premise is a *copy of that* — so without this the
    // worker runs against a schema no shipped build ever presents it with.
    //
    // Found the hard way, and worth keeping the reason: with the machine's real
    // database at migration 31, `session_search` did not exist, every batch's
    // index write failed, and — because #435 deliberately commits the index row
    // and the insight row in **one transaction** — the insight write rolled back
    // with it. The result was 0 insight rows over 1,178 sessions: this test's own
    // headline failure, reported against a build that is fine, for a schema
    // reason nothing in the message pointed at.
    {
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        agento_lib::native::migrate::apply(&mut conn).expect("migrate the copy");
    }

    let cached = {
        let conn = rusqlite::Connection::open(&db).expect("open");
        scalar(&conn, "SELECT COUNT(*) FROM claude_session_cache")
    };
    assert!(
        cached > 0,
        "the copied database has no cached sessions, so this proves nothing — \
         open the app once against the real database first"
    );

    // Start from nothing rather than from whatever the Go server left behind:
    // the point is that *this* code writes the rows, and a table already full
    // of Go's rows would pass against a worker that does nothing at all. This is
    // the same trap `scan_live.rs` documents for `files_done`.
    let before = {
        let conn = rusqlite::Connection::open(&db).expect("open");
        let before = scalar(&conn, "SELECT COUNT(*) FROM session_insights");
        conn.execute("DELETE FROM session_insights", [])
            .expect("clear");
        before
    };
    eprintln!("corpus: {cached} cached sessions ({before} insight rows discarded)");

    let started = std::time::Instant::now();
    worker::start(db.clone());

    // The boot sweep reads every transcript in the corpus, which is minutes.
    //
    // **Poll the terminal condition, not the row count.** The count only moves
    // when a batch commits, so it sits flat for the whole of each batch's read
    // phase — the first version of this test waited for it to stop changing and
    // declared success at 300 rows of 1,145, mid-sweep. "Nothing is left to
    // process" is the only signal that cannot be satisfied by a pause.
    let mut settled = false;
    for _ in 0..1_800 {
        let conn = rusqlite::Connection::open(&db).expect("open");
        let left = store::needs_processing(&conn, CURRENT_PROCESSOR_VERSION)
            .expect("needs_processing")
            .into_iter()
            .filter(|p| !p.file_path.is_empty())
            .count();
        drop(conn);
        if left == 0 {
            settled = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    eprintln!("worker took {:?} (settled: {settled})", started.elapsed());

    let conn = rusqlite::Connection::open(&db).expect("open");
    let written = scalar(&conn, "SELECT COUNT(*) FROM session_insights");
    eprintln!("insight rows: {written}");

    // **The assertion #408 is about.** Everything else in this file can pass
    // against a build whose worker never runs.
    assert!(
        written > 0,
        "the worker produced no insight rows over a corpus of {cached} sessions — \
         this is exactly the state the issue reports"
    );

    // Every row must be at the current version, or the sweep would keep finding
    // them and the corpus would be reprocessed every five minutes forever.
    let outdated = scalar(
        &conn,
        &format!(
            "SELECT COUNT(*) FROM session_insights WHERE processor_version <> {CURRENT_PROCESSOR_VERSION}"
        ),
    );
    assert_eq!(
        outdated, 0,
        "{outdated} rows were stored at the wrong version"
    );

    // And the sweep must now be empty: a row per cached session, joined on the
    // whole key. A join or an upsert written on `session_id` alone leaves the
    // duplicated ids reporting work forever.
    let still_pending =
        store::needs_processing(&conn, CURRENT_PROCESSOR_VERSION).expect("needs_processing");
    let with_a_transcript: Vec<_> = still_pending
        .iter()
        .filter(|p| !p.file_path.is_empty())
        .collect();
    assert!(
        with_a_transcript.is_empty(),
        "{} sessions still need processing after the sweep settled, e.g. {:?}",
        with_a_transcript.len(),
        &with_a_transcript[..with_a_transcript.len().min(3)],
    );

    // Not an equality: a transcript that cannot be read is skipped by design,
    // and a session whose file vanished between the copy and the read is a
    // legitimate shortfall. A *large* shortfall is not.
    assert!(
        written * 10 >= cached * 9,
        "only {written} of {cached} cached sessions gained an insight row"
    );

    // The figures have to be real. An insight row of all zeros is what a worker
    // that stored `SessionInsight::default()` for everything would leave, and
    // every count assertion above would still pass.
    let with_turns = scalar(
        &conn,
        "SELECT COUNT(*) FROM session_insights WHERE turn_count > 0",
    );
    assert!(
        with_turns * 2 > written,
        "only {with_turns} of {written} rows have a non-zero turn_count — \
         the rows are being stored but not computed"
    );
    let breakdowns = scalar(
        &conn,
        "SELECT COUNT(*) FROM session_insights WHERE tool_breakdown <> '{}'",
    );
    assert!(
        breakdowns > 0,
        "no row has a tool breakdown, over a corpus of {cached} real sessions"
    );

    // ─── the search index (#435) ─────────────────────────────────────────────
    //
    // The same argument as everything above, for the table the same transaction
    // writes: the index is populated by the *worker*, so a build whose indexer
    // never runs leaves `session_search` empty while every count here still
    // passes — and search simply answers nothing, which is indistinguishable
    // from a corpus with nothing in it. That is #408's failure mode, one table
    // to the right.
    let indexed = scalar(&conn, "SELECT COUNT(*) FROM session_search");
    eprintln!("index rows: {indexed}");
    assert_eq!(
        indexed, written,
        "the index and the insights are written in one transaction, so their \
         row counts cannot differ"
    );

    // Rows are not enough: a document whose columns are all empty is a row, and
    // matches nothing. Requiring *most* rows to carry text rather than all of
    // them, because a session really can be a single injected preamble with no
    // indexable content at all.
    let with_text = scalar(
        &conn,
        "SELECT COUNT(*) FROM session_search
          WHERE user_text <> '' OR assistant_text <> '' OR tool_text <> ''",
    );
    assert!(
        with_text * 2 > indexed,
        "only {with_text} of {indexed} index rows carry any text — \
         the rows are being written but not built"
    );

    // …and the text has to be *reachable through FTS5*, which is the only thing
    // any of this is for. A stored column that the tokenizer never indexed would
    // satisfy every assertion above.
    let hits = scalar(
        &conn,
        "SELECT COUNT(*) FROM session_search WHERE session_search MATCH '\"the\"'",
    );
    assert!(
        hits > 0,
        "no session in a corpus of {cached} matches the commonest word in \
         English — the column is stored but not searchable"
    );

    // The version is stamped, so the next boot does not rebuild the whole
    // corpus again.
    assert_eq!(
        agento_lib::native::search::stored_version(&conn).expect("stored_version"),
        agento_lib::native::search::SEARCH_INDEX_VERSION,
        "the index was built but its version was never recorded, so every \
         later sweep rebuilds it"
    );
}

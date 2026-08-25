//! The `session_search` index against the machine's **real** corpus, on a copy
//! of the database — correctness, and the numbers (#439).
//!
//! `#[ignore]`d, like `scan_live.rs` and `insights_live.rs` and for the same
//! reason — CI has no `~/.claude` and no `~/.agento/agento.db`. Run it by hand:
//!
//! ```bash
//! cargo test --test search_live -- --ignored --nocapture
//! ```
//!
//! # Why this exists rather than a fixture
//!
//! Two failures live here that a fixture cannot see, and they are different
//! failures.
//!
//! The first is `insights_live.rs`'s, one table to the right: an index that is
//! built but empty answers nothing, which is indistinguishable from a corpus
//! with nothing in it. A three-file fixture that gains three rows does not
//! distinguish that from a healthy build.
//!
//! The second is the one this file is named for. #433 chose an FTS5 shape and
//! #434 chose three byte caps (8 KB per message, 2 KB per tool result, 512 KB
//! per session) **a priori**, and neither decision has ever met a real corpus.
//! "Should be fast" and "should be about this big" are not measurements: a cap
//! that turns out to bind on every session, or an index dominated by tool
//! output, or a keyed delete that has quietly become the cost of a scan, are all
//! invisible to a fixture whose documents are three sentences long.
//!
//! So this file does both jobs at once, on one copy of the real database:
//!
//! * **Asserts** what must be true of any corpus — strictly, and never
//!   vacuously. Zero cached sessions, zero index rows, a zero-byte index or a
//!   probe query that does not find its own session are failures, not passes.
//! * **Measures** what varies between machines, and *prints* it: cold full-index
//!   build, per-session incremental reindex (both the raw index write and the
//!   whole worker round trip), the orphan sweep, index bytes with a per-column
//!   breakdown, and p50/p95 query latency with filters and facets composed in.
//!
//! # Why there are no timing assertions
//!
//! Every printed duration is developer-hardware-dependent, and a corpus varies
//! by an order of magnitude between machines — this one indexes ~1,200 sessions;
//! a fresh install indexes none. A latency assertion tuned on one of those
//! flakes on the other, and a flaky test is worse than no test. The bounds
//! belong in the pull request that reads the numbers, not in an `assert!`.
//!
//! What *is* asserted is every **invariant** the numbers are measured against:
//! the caps hold on real data, the index is populated, the version is stamped,
//! and a query for known content finds it. Those hold on any corpus, so they can
//! be strict.
//!
//! # Quote the release numbers, not the debug ones
//!
//! The assertions hold in either profile and the default `cargo test` invocation
//! above is the one to run for correctness. The **timings** are worth a
//! `--release` run, because the `bundled` SQLite is a C dependency compiled at
//! the profile's optimization level: a debug build measures a SQLite nobody
//! ships, and every figure it prints is several times the real one.
//!
//! ```bash
//! cargo test --release --test search_live -- --ignored --nocapture
//! ```
//!
//! Expect minutes rather than seconds either way. That is a property of what is
//! being measured, not of the harness — see [`QUERY_ITERATIONS`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use agento_lib::native::db;
use agento_lib::native::insights::{processors::CURRENT_PROCESSOR_VERSION, store, worker};
use agento_lib::native::search::{
    self,
    normalize::{MESSAGE_CAP, SESSION_CAP, TOOL_RESULT_CAP},
    SearchDoc, SEARCH_INDEX_VERSION, SNIPPET_MARK_START,
};
use agento_lib::native::sessions::page::{facets, list_page};
use agento_lib::native::sessions::query::{SessionQuery, Sort};
use agento_lib::native::settings;

/// How long to wait for the boot rebuild. The sweep reads every transcript in
/// the corpus, so this is minutes rather than seconds — the same 6-minute
/// budget `scan_live` and `insights_live` allow themselves.
const BUILD_BUDGET: Duration = Duration::from_secs(6 * 60);
const POLL: Duration = Duration::from_millis(200);

/// Timed runs per query shape, and the floor and ceiling on them.
///
/// The floor exists because a p-value over fewer than three samples is not a
/// distribution, and the **ceiling is in seconds rather than in runs** because
/// this suite measured a page query that takes tens of seconds on a corpus this
/// size. A fixed run count multiplies that into a suite nobody runs twice, and
/// a smaller fixed count would hide the finding by never sampling the slow
/// shapes at all — so a shape stops when it has spent its budget, and the table
/// reports how many samples each figure came from.
///
/// It tightens itself: when a shape is fast, the budget is never reached and
/// every shape gets the full [`QUERY_ITERATIONS`].
const QUERY_ITERATIONS: usize = 15;
const MIN_ITERATIONS: usize = 3;
const SHAPE_BUDGET: Duration = Duration::from_secs(20);

/// Sessions re-indexed one at a time for the incremental measurement. More than
/// one, because a single session's document size is not representative and the
/// figure that matters — `search::delete`'s scan — is the same for all of them.
const INCREMENTAL_SAMPLES: usize = 5;

// ─── locating and copying the corpus ────────────────────────────────────────

/// The installed database, **not** `paths::database_path()`.
///
/// `cargo test` builds with `debug_assertions`, where `paths::data_dir` ignores
/// the environment entirely and answers `~/.agento-desktop-dev` — deliberately,
/// so a dev build cannot collide with an installed Agento. The corpus this test
/// wants is the installed one, so it is named directly, exactly as `scan_live`
/// and `insights_live` name it.
fn real_db() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let db = PathBuf::from(home).join(".agento/agento.db");
    db.is_file().then_some(db)
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

fn scalar(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
}

// ─── measuring ──────────────────────────────────────────────────────────────

/// Nearest-rank percentile over an already-sorted slice — the definition
/// `gateway::usage` uses, so the two report latency the same way.
fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort_unstable();
    percentile(&xs, 50.0)
}

/// Percent-encode one query-string value.
///
/// Hand-rolled rather than pulled from a crate because the whole requirement is
/// three characters wide: `SessionQuery::parse` reads its input through
/// `form_urlencoded::parse`, so a space in `"cargo test"` would otherwise arrive
/// as a `+` and the quotes would end the value.
fn enc(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// How many index rows a single FTS5 term matches.
fn match_count(conn: &Connection, term: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM session_search WHERE session_search MATCH ?1",
        [format!("\"{term}\"")],
        |r| r.get(0),
    )
    .unwrap_or(-1)
}

/// A word from indexed text that is worth probing with: long enough to be
/// distinctive, and made only of characters `unicode61` keeps as one token.
fn candidate_terms(text: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.len() < 8 || word.len() > 40 {
            continue;
        }
        if !word.chars().any(|c| c.is_ascii_alphabetic()) || !word.is_ascii() {
            continue;
        }
        let lower = word.to_ascii_lowercase();
        if !seen.contains(&lower) {
            seen.push(lower);
        }
        if seen.len() >= 24 {
            break;
        }
    }
    seen
}

// ─── the test ───────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs the machine's real ~/.agento database and ~/.claude corpus"]
fn the_index_is_correct_and_measured_over_the_real_corpus() {
    let Some(src) = real_db() else {
        eprintln!("skipping: no ~/.agento/agento.db");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let db = copy_db(&src, dir.path());

    // **Migrate the copy**, exactly as `lib.rs` migrates before starting the
    // worker, and for the reason `insights_live` records: the installed database
    // is whatever version the last release left, and it lags the repository. On
    // the machine this was written against it had no `session_search` table at
    // all — so without this, every batch's index write fails and, because the
    // index row and the insight row commit in one transaction, the insight write
    // rolls back with it. The symptom is zero rows in both tables and nothing in
    // the message pointing at a schema.
    {
        let mut conn = Connection::open(&db).expect("open");
        agento_lib::native::migrate::apply(&mut conn).expect("migrate the copy");
    }

    let cached = {
        let conn = db::open_read_only(&db).expect("open the copy read-only");
        scalar(&conn, "SELECT COUNT(*) FROM claude_session_cache")
    };
    assert!(
        cached > 0,
        "the copied database has no cached sessions, so this proves nothing — \
         open the app once against the real database first"
    );

    // ─── cold build ─────────────────────────────────────────────────────────
    //
    // Force the rebuild rather than relying on the copy's own state. `sweep`
    // rebuilds when the stored version disagrees with this build's, and 0 is
    // what a fresh install and a database that has never been indexed both
    // carry — so writing it is the one lever that makes this a *cold* build on
    // any developer's database, including one whose index is already current.
    //
    // `sweep` itself is private, so the build is driven the only way an
    // integration test can drive it: start the worker and poll the terminal
    // condition. The version stamp is that condition — `sweep` records it last,
    // and only if every batch committed — which is stronger than a row count,
    // because a row count sits flat for the whole of each batch's read phase and
    // a test that waits for it to stop moving declares success mid-sweep.
    {
        let conn = db::open_read_write(&db).expect("open the copy read-write");
        search::record_version(&conn, 0).expect("force a rebuild");
        assert_eq!(
            search::stored_version(&conn).expect("stored_version"),
            0,
            "the rebuild lever did not take"
        );
    }

    let started = Instant::now();
    worker::start(db.clone());

    let mut built = false;
    while started.elapsed() < BUILD_BUDGET {
        let conn = db::open_read_only(&db).expect("open the copy read-only");
        let stamped = search::stored_version(&conn).unwrap_or(0) == SEARCH_INDEX_VERSION;
        drop(conn);
        if stamped {
            built = true;
            break;
        }
        std::thread::sleep(POLL);
    }
    let cold_build = started.elapsed();
    assert!(
        built,
        "the index was not rebuilt within {BUILD_BUDGET:?} — the version was \
         never stamped, which `sweep` does only when every batch committed"
    );

    let conn = db::open_read_write(&db).expect("open the copy read-write");

    // ─── non-vacuity ────────────────────────────────────────────────────────
    //
    // "An identical over an empty result set is not evidence." Everything below
    // this point — every duration, every ratio, every probe — is meaningless if
    // the index is empty, and an empty index is exactly what the failure this
    // file exists for looks like. So the emptiness checks come first and are
    // hard failures.
    let indexed = scalar(&conn, "SELECT COUNT(*) FROM session_search");
    assert!(
        indexed > 0,
        "the rebuild stamped its version over a corpus of {cached} cached \
         sessions but wrote no index rows — search answers nothing, which is \
         indistinguishable from an empty corpus"
    );
    let insights = scalar(&conn, "SELECT COUNT(*) FROM session_insights");
    assert_eq!(
        indexed, insights,
        "the index row and the insight row are written in one transaction, so \
         their counts cannot differ"
    );
    // Not an equality against `cached`: a transcript that cannot be read is
    // skipped by design, and one whose file vanished between the copy and the
    // read is a legitimate shortfall. A *large* shortfall is not.
    assert!(
        indexed * 10 >= cached * 9,
        "only {indexed} of {cached} cached sessions were indexed"
    );

    let with_text = scalar(
        &conn,
        "SELECT COUNT(*) FROM session_search
          WHERE user_text <> '' OR assistant_text <> '' OR tool_text <> ''",
    );
    assert!(
        with_text * 2 > indexed,
        "only {with_text} of {indexed} index rows carry any text — the rows are \
         being written but not built"
    );

    // ─── index size, and where the bytes went ───────────────────────────────
    let bytes = column_bytes(&conn);
    let content_bytes: i64 = bytes.values().sum();
    assert!(
        content_bytes > 0,
        "the index occupies zero bytes over {indexed} rows"
    );
    let index_bytes = shadow_bytes(&conn);
    assert!(
        index_bytes > 0,
        "the FTS5 shadow tables occupy zero bytes over {indexed} rows"
    );

    // **The caps must hold on real data**, which is the half of #434 no unit
    // test can reach: its fixtures are strings it wrote itself. `SESSION_CAP`
    // bounds the three text columns together and is the one that decides how
    // large this index can ever get.
    let widest: i64 = scalar(
        &conn,
        "SELECT MAX(LENGTH(user_text) + LENGTH(assistant_text) + LENGTH(tool_text))
           FROM session_search",
    );
    assert!(
        widest <= SESSION_CAP as i64,
        "a session's indexed text is {widest} bytes, over the {SESSION_CAP}-byte \
         session cap — the builder's accounting does not hold on real input"
    );

    // ─── the incremental path ───────────────────────────────────────────────
    //
    // Two different numbers, and the distinction is the point.
    //
    // `search::replace` is delete-then-insert, and the delete **scans**: FTS5's
    // `xBestIndex` accepts only `rowid` and `MATCH`, so a predicate over the
    // UNINDEXED key columns cannot use an index, and a content-storing table
    // materializes every scanned row to serve it. That cost grows with the
    // corpus, and it is what a rowid side table would remove — so it is measured
    // on its own, against a *full* index, which is the only state in which the
    // figure means anything.
    //
    // The worker round trip is the other number: the whole incremental path a
    // changed session actually takes, transcript read included.
    let samples = sample_docs(&conn, INCREMENTAL_SAMPLES);
    assert!(
        !samples.is_empty(),
        "no indexed session could be read back out of the index"
    );
    let mut deletes = Vec::new();
    let mut inserts = Vec::new();
    for doc in &samples {
        let t = Instant::now();
        search::delete(&conn, &doc.session_id, &doc.project_path).expect("delete");
        deletes.push(t.elapsed());
        let t = Instant::now();
        search::insert(&conn, doc).expect("insert");
        inserts.push(t.elapsed());
    }
    // Delete-then-insert of the same document is exactly `replace`, so the index
    // is back where it started and every count above still holds.
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM session_search"),
        indexed,
        "re-indexing the samples changed the row count — a delete missed its \
         pair, or an insert duplicated one"
    );

    let t = Instant::now();
    let orphans = search::delete_orphans(&conn).expect("delete_orphans");
    let orphan_sweep = t.elapsed();
    assert_eq!(
        orphans, 0,
        "{orphans} index rows have no cache row, immediately after a rebuild \
         that read the cache"
    );

    drop(conn);
    let round_trip = worker_round_trip(&db, &samples[0]);

    // ─── correctness: a query for known content finds its own session ───────
    let conn = db::open_read_only(&db).expect("open the copy read-only");
    // Read from the copy rather than defaulted: the real database carries an
    // `indexed_config_dirs` scope and a hidden-project list, and a search run
    // without them answers over rows the app never shows — which would make both
    // the probe and every latency figure below measure a different query from
    // the one the product runs.
    let settings = settings::load(&conn);
    let probe = find_probe(&conn).expect(
        "no indexed session yielded a distinctive term — the index holds rows \
         but nothing in them is reachable through FTS5",
    );
    eprintln!(
        "probe: {:?} ({} hits) from session {}",
        probe.term, probe.hits, probe.session_id
    );

    let q = SessionQuery::parse(&format!("q={}", enc(&probe.term))).expect("parse the probe query");
    // `q` with no explicit sort resolves to relevance (#437). Asserted rather
    // than assumed, because the whole point of the probe is that the session is
    // found *ranked*, not merely present somewhere in a recency-ordered list.
    assert_eq!(q.sort, Sort::Relevance);
    let page = list_page(&conn, &settings, &q).expect("the probe page");
    let found = page
        .items
        .iter()
        .find(|s| s.session_id == probe.session_id && s.project_path == probe.project_path);
    let found = found.unwrap_or_else(|| {
        panic!(
            "session {} contains {:?} — one of only {} index rows that match it — \
             yet it is not on the first page of {} results",
            probe.session_id,
            probe.term,
            probe.hits,
            page.items.len(),
        )
    });
    // The row matched through the index by construction, so it must carry a
    // snippet: a hit with no snippet means `attach_match_snippets` was reached
    // with the wrong query, or not reached at all.
    assert!(
        found.match_snippet.contains(SNIPPET_MARK_START),
        "the probe's row carries no highlighted snippet: {:?}",
        found.match_snippet
    );

    // ─── report: everything measured before the latency loop ────────────────
    eprintln!("\n── corpus ───────────────────────────────────────────────");
    eprintln!("cached sessions      {cached}");
    eprintln!("indexed sessions     {indexed} ({with_text} carrying text)");

    eprintln!("\n── build ────────────────────────────────────────────────");
    eprintln!(
        "cold full rebuild    {:>9.2?}  ({:.1} ms/session)",
        cold_build,
        cold_build.as_secs_f64() * 1000.0 / indexed as f64
    );
    eprintln!(
        "incremental: delete  {:>9.2?}  median of {INCREMENTAL_SAMPLES} (this is the scan)",
        median(deletes)
    );
    eprintln!(
        "incremental: insert  {:>9.2?}  median of {INCREMENTAL_SAMPLES}",
        median(inserts)
    );
    eprintln!("incremental: worker  {round_trip:>9.2?}  one session, transcript read included");
    eprintln!("orphan sweep         {orphan_sweep:>9.2?}  one pass over the whole index");

    eprintln!("\n── index size ───────────────────────────────────────────");
    eprintln!(
        "shadow tables        {:>9}  ({:.1} KB/session)",
        human(index_bytes),
        index_bytes as f64 / 1024.0 / indexed as f64
    );
    for (column, size) in &bytes {
        eprintln!(
            "  {column:<18} {:>9}  {:>5.1}% of stored text",
            human(*size),
            *size as f64 * 100.0 / content_bytes as f64
        );
    }
    eprintln!(
        "caps                 message {MESSAGE_CAP}, tool result {TOOL_RESULT_CAP}, \
         session {SESSION_CAP}; widest session {widest}"
    );

    // ─── query latency, with filters and facets composed in ─────────────────
    let busiest = busiest_project(&conn);
    let shapes: Vec<(&str, String)> = vec![
        ("single common word", "q=error".to_string()),
        ("single rare word", format!("q={}", enc(&probe.term))),
        ("two words (implicit AND)", "q=test+failure".to_string()),
        ("quoted phrase", format!("q={}", enc("\"cargo test\""))),
        ("as-you-type prefix", "q=implement".to_string()),
        (
            "word + project filter",
            format!("q=error&project={}", enc(&busiest)),
        ),
        (
            "word + numeric + time sort",
            "q=error&messages_min=5&sort=recent".to_string(),
        ),
    ];

    eprintln!("\n── query latency ────────────────────────────────────────");
    eprintln!(
        "{:<26} {:>5} {:>4} {:>10} {:>10} {:>10} {:>10}",
        "shape", "rows", "n", "page p50", "page p95", "facet p50", "facet p95"
    );
    let mut latency = Vec::new();
    for (label, raw) in &shapes {
        let q = SessionQuery::parse(raw).expect("parse a representative query");
        let shape_started = Instant::now();
        let mut page_times = Vec::with_capacity(QUERY_ITERATIONS);
        let mut facet_times = Vec::with_capacity(QUERY_ITERATIONS);
        let mut hits = 0usize;
        // One extra run, whose sample is dropped: the first call through a shape
        // pays for FTS5 opening its shadow tables, a cost no later query of the
        // same session sees and which would otherwise land entirely in the p95.
        for run in 0..=QUERY_ITERATIONS {
            let t = Instant::now();
            let page = list_page(&conn, &settings, &q).expect("page");
            let page_time = t.elapsed();
            let rows = page.items.len();

            let t = Instant::now();
            facets(&conn, &settings, &q).expect("facets");
            let facet_time = t.elapsed();

            if run > 0 {
                page_times.push(page_time);
                facet_times.push(facet_time);
                hits = rows;
            }
            if page_times.len() >= MIN_ITERATIONS && shape_started.elapsed() > SHAPE_BUDGET {
                break;
            }
        }
        page_times.sort_unstable();
        facet_times.sort_unstable();
        // Printed per shape rather than in one table at the end: the slow shapes
        // are slow enough that a silent run reads as a hang.
        eprintln!(
            "{label:<26} {hits:>5} {:>4} {:>10.2?} {:>10.2?} {:>10.2?} {:>10.2?}",
            page_times.len(),
            percentile(&page_times, 50.0),
            percentile(&page_times, 95.0),
            percentile(&facet_times, 50.0),
            percentile(&facet_times, 95.0),
        );
        latency.push((*label, hits, page_times, facet_times));
    }

    // Every shape has to have answered *something*, or the latency figures are
    // the cost of finding nothing. Not per shape — a corpus need not contain the
    // word "failure" — but across the set, where the rare term alone guarantees
    // a hit.
    let answered = latency.iter().filter(|(_, hits, ..)| *hits > 0).count();
    assert!(
        answered > 0,
        "no representative query returned a row, so the latency figures measure \
         an empty index"
    );

    eprintln!(
        "\n`n` is the sample count, bounded by {SHAPE_BUDGET:?} per shape rather \
         than by a run count — see QUERY_ITERATIONS."
    );
    eprintln!();
}

// ─── helpers the test reads its numbers through ─────────────────────────────

fn human(bytes: i64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / (1 << 10) as f64),
        b => format!("{b} B"),
    }
}

/// Stored bytes per indexed column — the answer to "does tool output dominate
/// this index", which is what decides whether #434's caps need moving.
///
/// A `BTreeMap` so the report is in a fixed order whatever the query planner
/// does.
fn column_bytes(conn: &Connection) -> BTreeMap<&'static str, i64> {
    ["title", "user_text", "assistant_text", "tool_text"]
        .into_iter()
        .map(|column| {
            let total = scalar(
                conn,
                &format!("SELECT COALESCE(SUM(LENGTH({column})), 0) FROM session_search"),
            );
            (column, total.max(0))
        })
        .collect()
}

/// Bytes occupied by the FTS5 shadow tables — the index's real footprint,
/// inverted index and stored content together.
///
/// `dbstat` is the right answer and is a **compile-time option**
/// (`SQLITE_ENABLE_DBSTAT_VTAB`) that this build may or may not carry, so it is
/// attempted and fallen back on rather than assumed. The fallback sums the
/// shadow tables' own blobs, which undercounts page overhead but is the same
/// order and, more importantly, is never zero when the index is not empty.
fn shadow_bytes(conn: &Connection) -> i64 {
    let via_dbstat: rusqlite::Result<i64> = conn.query_row(
        "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat WHERE name LIKE 'session_search%'",
        [],
        |r| r.get(0),
    );
    if let Ok(size) = via_dbstat {
        if size > 0 {
            return size;
        }
    }
    let data = scalar(
        conn,
        "SELECT COALESCE(SUM(LENGTH(block)), 0) FROM session_search_data",
    );
    let docsize = scalar(
        conn,
        "SELECT COALESCE(SUM(LENGTH(sz)), 0) FROM session_search_docsize",
    );
    let content: i64 = column_bytes(conn).values().sum();
    data.max(0) + docsize.max(0) + content
}

/// Read whole documents back out of the index, largest first.
///
/// Largest first on purpose: `search::delete`'s cost is a scan that materializes
/// every row it passes, so the sessions worth timing a re-index of are the ones
/// whose documents are big enough for the insert to register beside it.
fn sample_docs(conn: &Connection, n: usize) -> Vec<SearchDoc> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, project_path, title, user_text, assistant_text, tool_text
               FROM session_search
              ORDER BY LENGTH(user_text) + LENGTH(assistant_text) + LENGTH(tool_text) DESC
              LIMIT ?1",
        )
        .expect("prepare the sample read");
    let rows = stmt
        .query_map([n as i64], |r| {
            Ok(SearchDoc {
                session_id: r.get(0)?,
                project_path: r.get(1)?,
                title: r.get(2)?,
                user_text: r.get(3)?,
                assistant_text: r.get(4)?,
                tool_text: r.get(5)?,
            })
        })
        .expect("read the samples");
    rows.filter_map(Result::ok).collect()
}

/// Time one session all the way around the worker's incremental path.
///
/// Dropping its insight row is what makes the session *pending*, which is both
/// how the work is requested and how its completion is observed — `enqueue`
/// answers nothing, so the terminal condition has to be a query. The row
/// reappears as a side effect, which is why the count is asserted afterwards.
fn worker_round_trip(db: &Path, doc: &SearchDoc) -> Duration {
    let file_path: String = {
        let conn = db::open_read_only(db).expect("open the copy read-only");
        conn.query_row(
            "SELECT file_path FROM claude_session_cache
              WHERE session_id = ?1 AND project_path = ?2",
            [&doc.session_id, &doc.project_path],
            |r| r.get(0),
        )
        .unwrap_or_default()
    };
    assert!(
        !file_path.is_empty(),
        "indexed session {} has no cache row to re-read",
        doc.session_id
    );

    {
        let conn = db::open_read_write(db).expect("open the copy read-write");
        conn.execute(
            "DELETE FROM session_insights WHERE session_id = ?1 AND project_path = ?2",
            [&doc.session_id, &doc.project_path],
        )
        .expect("drop one insight row");
    }

    let pending = store::Pending {
        session_id: doc.session_id.clone(),
        project_path: doc.project_path.clone(),
        file_path,
    };
    let started = Instant::now();
    worker::enqueue([pending.clone()]);

    while started.elapsed() < BUILD_BUDGET {
        let conn = db::open_read_only(db).expect("open the copy read-only");
        let still = store::needs_processing(&conn, CURRENT_PROCESSOR_VERSION)
            .expect("needs_processing")
            .into_iter()
            .any(|p| p.session_id == pending.session_id && p.project_path == pending.project_path);
        drop(conn);
        if !still {
            return started.elapsed();
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "the worker never re-processed session {} — the incremental path is not \
         reachable through the queue",
        doc.session_id
    )
}

struct Probe {
    session_id: String,
    project_path: String,
    term: String,
    hits: i64,
}

/// Pick a session and a term from *its own indexed text* that identifies it.
///
/// Chosen at run time rather than hardcoded, because the corpus is whatever this
/// machine has. The term must match **few** rows — comfortably inside one page —
/// so that "the session is on page one under relevance" is a real assertion
/// about ranking rather than a restatement of "the corpus is small". A term
/// matching one row is ideal; up to [`MAX_PROBE_HITS`] is still decisive, since
/// relevance sorts every index hit above every metadata-only one.
fn find_probe(conn: &Connection) -> Option<Probe> {
    const MAX_PROBE_HITS: i64 = 20;
    const SESSIONS_TO_TRY: i64 = 200;

    let mut stmt = conn
        .prepare(
            "SELECT session_id, project_path, user_text
               FROM session_search
              WHERE LENGTH(user_text) > 200
              LIMIT ?1",
        )
        .ok()?;
    let rows = stmt
        .query_map([SESSIONS_TO_TRY], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .ok()?;

    for row in rows.flatten() {
        let (session_id, project_path, text) = row;
        for term in candidate_terms(&text) {
            let hits = match_count(conn, &term);
            if (1..=MAX_PROBE_HITS).contains(&hits) {
                return Some(Probe {
                    session_id,
                    project_path,
                    term,
                    hits,
                });
            }
        }
    }
    None
}

/// The project with the most cached sessions — the filter most likely to be
/// composed with a search in practice, and the one that exercises the clause
/// combination rather than a filter that removes everything.
fn busiest_project(conn: &Connection) -> String {
    conn.query_row(
        "SELECT project_path FROM claude_session_cache
          GROUP BY project_path ORDER BY COUNT(*) DESC LIMIT 1",
        [],
        |r| r.get(0),
    )
    .unwrap_or_default()
}

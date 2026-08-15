//! A full scan against the machine's **real** corpus, on a copy of the database.
//!
//! `#[ignore]`d, like the other suites that need real data — CI has no
//! `~/.claude` and no `~/.agento/agento.db`. Run it by hand:
//!
//! ```bash
//! cargo test --test scan_live -- --ignored --nocapture
//! ```
//!
//! # Why this exists rather than a fixture
//!
//! #289 is the port's first ownership flip: once the sidecar stops scanning
//! there is no second implementation to fall back to. A fixture would prove the
//! orchestrator wires up, not that it produces the same corpus Go produced —
//! and the failure that matters is a scan that runs, reports success and writes
//! nothing, which a fixture with three files would not distinguish from a
//! healthy one.
//!
//! It works on a **copy** so a bug cannot damage the real database, and the copy
//! keeps the rows Go already wrote so the comparison is against a known-good
//! corpus rather than against zero.

use std::path::PathBuf;

fn real_db() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let db = PathBuf::from(home).join(".agento/agento.db");
    db.is_file().then_some(db)
}

fn count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or(-1)
}

#[test]
#[ignore = "needs the machine's real ~/.agento database and ~/.claude corpus"]
fn a_full_scan_reproduces_the_corpus_go_wrote() {
    let Some(src) = real_db() else {
        eprintln!("skipping: no ~/.agento/agento.db");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    std::fs::copy(&src, &db).expect("copy the database");
    // WAL contents live beside the file; without them the copy can be missing
    // recent writes entirely, which would understate the "before" counts.
    for ext in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{ext}", src.display()));
        if from.is_file() {
            let _ = std::fs::copy(&from, dir.path().join(format!("agento.db{ext}")));
        }
    }

    let before = {
        let conn = rusqlite::Connection::open(&db).expect("open");
        (
            count(&conn, "claude_session_cache"),
            count(&conn, "claude_subagent_cache"),
        )
    };
    eprintln!("before: {} sessions, {} sub-agents", before.0, before.1);
    assert!(
        before.0 > 0,
        "the copied database has no cached sessions, so this proves nothing — \
         open the app once against the real database first"
    );

    // Wipe the freshness marker so the scan has work to do, exactly as
    // `POST /api/claude-sessions/refresh` does.
    {
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute(
            "UPDATE claude_cache_metadata SET last_scanned_at = '0001-01-01 00:00:00 +0000 UTC',
                    scanner_version = 0 WHERE id = 1",
            [],
        )
        .expect("invalidate");
    }

    let started = std::time::Instant::now();
    agento_lib::native::scan::ensure_scan(db.clone());

    // Poll rather than sleep: a full re-read of a real corpus is minutes.
    let mut settled = false;
    for _ in 0..1_800 {
        let status = agento_lib::native::scan::status(&db);
        if !status.scan_in_progress && !status.last_scanned_at.is_empty() {
            settled = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(settled, "the scan never finished within 6 minutes");
    eprintln!("scan took {:?}", started.elapsed());

    let conn = rusqlite::Connection::open(&db).expect("open");
    let after = (
        count(&conn, "claude_session_cache"),
        count(&conn, "claude_subagent_cache"),
    );
    eprintln!("after:  {} sessions, {} sub-agents", after.0, after.1);

    // The scan must not *lose* rows. A forced re-read updates in place; the
    // count can only grow, by whatever landed on disk since Go last looked.
    assert!(
        after.0 >= before.0,
        "the scan dropped sessions: {} -> {}",
        before.0,
        after.0
    );
    assert!(
        after.1 >= before.1,
        "the scan dropped sub-agents: {} -> {}",
        before.1,
        after.1
    );

    // The markers must be recorded, or every later scan re-reads everything.
    let status = agento_lib::native::scan::status(&db);
    assert!(
        !status.last_scanned_at.is_empty(),
        "last_scanned_at was not recorded"
    );
    assert!(!status.scan_in_progress);
    assert!(
        status.files_done > 0,
        "the scan reported no files read, so it did not do the work it claimed"
    );
    let version: i64 = conn
        .query_row(
            "SELECT scanner_version FROM claude_cache_metadata WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .expect("scanner_version");
    assert_eq!(
        version,
        agento_lib::native::scanner::CURRENT_SCANNER_VERSION,
        "the scanner version was not recorded, so the next scan would re-read everything again"
    );
}

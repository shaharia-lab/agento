//! The Credentials Checker's worker, driven through its **real entry points**
//! (#603): `sync`, `start`, `stop` and `enqueue`.
//!
//! ## This binary may contain exactly one test that starts the worker
//!
//! The worker is process-wide — one `Mutex<Option<..>>` in `worker.rs` — so two
//! tests in one binary would start, stop and enqueue onto each other's worker,
//! against each other's database. `src-tauri/tests/*.rs` is one binary per file,
//! so the whole lifecycle is one test, in order. `tests/insights_worker.rs`
//! states the same rule for the insights worker.
//!
//! The loop's individual arms are unit-tested in `worker.rs`; what only this
//! file can show is that `start` really spawns a thread that does the work, and
//! that `stop` really ends it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agento_lib::native::security_scan::store::Pending;
use agento_lib::native::security_scan::worker;
use agento_lib::native::{db, migrate};

/// A migrated database in `dir`, plus its path — `ensure_database`, because
/// `open_read_write` does not create the file.
fn fixture_db(dir: &Path) -> PathBuf {
    let db_path = dir.join("agento.db");
    let mut conn = db::ensure_database(&db_path).expect("create");
    migrate::apply(&mut conn).expect("migrations");
    db_path
}

/// A GitHub token built at runtime, so the source holds no literal secret.
fn token(fill: char) -> String {
    format!("gh{}{}", "p_", fill.to_string().repeat(36))
}

/// A transcript whose `Bash` output leaks a token, registered as a cache row.
fn seed_session(dir: &Path, db_path: &Path, session_id: &str, fill: char) -> Pending {
    let file = dir.join(format!("{session_id}.jsonl"));
    let lines = [
        serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "env"}},
            ]},
        }),
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1",
                 "content": format!("GITHUB_TOKEN={}", token(fill))},
            ]},
        }),
    ];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&file, body).expect("write transcript");
    let file_path = file.to_string_lossy().into_owned();

    let conn = db::open_read_write(db_path).expect("open");
    conn.execute(
        "INSERT INTO claude_session_cache
             (session_id, project_path, file_path, file_mtime, start_time, last_activity)
         VALUES (?1, '/a', ?2, '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00',
                 '2026-01-01 00:00:00+00:00')",
        rusqlite::params![session_id, file_path],
    )
    .expect("cache row");
    Pending {
        session_id: session_id.into(),
        project_path: "/a".into(),
        file_path,
    }
}

fn set_enabled(db_path: &Path, enabled: bool) {
    db::open_read_write(db_path)
        .expect("open")
        .execute(
            "UPDATE user_settings SET credentials_checker_enabled = ?1",
            [i64::from(enabled)],
        )
        .expect("setting");
}

fn count(db_path: &Path, sql: &str, session_id: &str) -> i64 {
    let conn = db::open_read_only(db_path).expect("open");
    conn.query_row(sql, [session_id], |row| row.get(0))
        .expect("count")
}

fn findings(db_path: &Path, session_id: &str) -> i64 {
    count(
        db_path,
        "SELECT COUNT(*) FROM credential_findings
          WHERE session_id = ?1 AND rule_id = 'github-pat'",
        session_id,
    )
}

fn scan_states(db_path: &Path, session_id: &str) -> i64 {
    count(
        db_path,
        "SELECT COUNT(*) FROM credential_scan_state WHERE session_id = ?1",
        session_id,
    )
}

/// Poll until `done`, failing with `what` at a deadline rather than hanging.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// The whole lifecycle, in order — see the file header for why it is one test.
#[test]
fn the_worker_follows_the_setting_and_scans_the_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fixture_db(dir.path());
    // `user_settings` is a one-row table the app's first save creates.
    db::open_read_write(&db_path)
        .expect("open")
        .execute("INSERT OR IGNORE INTO user_settings (id) VALUES (1)", [])
        .expect("settings row");
    seed_session(dir.path(), &db_path, "s1", 'a');

    // 1. Off at boot: no thread, nothing touched.
    set_enabled(&db_path, false);
    worker::sync(db_path.clone());
    assert!(!worker::is_running(), "the checker is off");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        scan_states(&db_path, "s1"),
        0,
        "an off checker read nothing"
    );

    // 2. On at boot: the boot sweep scans the corpus with no announcement.
    set_enabled(&db_path, true);
    worker::sync(db_path.clone());
    assert!(worker::is_running());
    wait_for("the boot sweep's finding", || findings(&db_path, "s1") == 1);

    // 3. The incremental path: an announced session is scanned without
    //    waiting for the five-minute sweep.
    let s2 = seed_session(dir.path(), &db_path, "s2", 'b');
    worker::enqueue([s2]);
    wait_for("the announced session's finding", || {
        findings(&db_path, "s2") == 1
    });

    // 4. Switched off: `sync` stops it, an announcement goes nowhere and the
    //    thread scans nothing.
    set_enabled(&db_path, false);
    worker::sync(db_path.clone());
    assert!(!worker::is_running());
    let s3 = seed_session(dir.path(), &db_path, "s3", 'c');
    worker::enqueue([s3]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(scan_states(&db_path, "s3"), 0, "a stopped worker scanned");

    // 5. Switched on again: a fresh sweep picks up what was missed while off.
    set_enabled(&db_path, true);
    worker::sync(db_path.clone());
    wait_for("the restarted sweep's finding", || {
        findings(&db_path, "s3") == 1
    });
    worker::stop();
}

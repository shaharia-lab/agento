//! The scanner end to end, against a scratch database.
//!
//! Walk → diff → apply, over a corpus built in a temp directory. This is where
//! the pieces are checked as a *sequence*: that a first scan inserts and
//! announces discoveries, that a second scan does nothing, that a touched file
//! is re-read as an update, that a removed transcript is reconciled away, and
//! that the two user-owned columns survive all of it.
//!
//! The database here is created by the test. The application's own handle is
//! read-only, and nothing in the app calls these writers — see the module docs
//! on `native::scanner` for why the port stops short of writing for real.

use std::path::{Path, PathBuf};

use agento_lib::native::scanner::apply::{apply_changes, ScanUnit};
use agento_lib::native::scanner::diff::diff_disk_and_cache;
use agento_lib::native::scanner::store::load_cached_entries;
use agento_lib::native::scanner::walk::walk_all_disk_files;
use rusqlite::Connection;

/// The columns the scanner writes, as the migrations define them.
const SCHEMA: &str = "
CREATE TABLE claude_session_cache (
    session_id TEXT NOT NULL, project_path TEXT NOT NULL,
    file_path TEXT NOT NULL, file_mtime DATETIME NOT NULL,
    preview TEXT NOT NULL DEFAULT '',
    start_time DATETIME NOT NULL, last_activity DATETIME NOT NULL,
    message_count INTEGER NOT NULL DEFAULT 0, event_count INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_5m_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
    git_branch TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '',
    cwd TEXT NOT NULL DEFAULT '',
    custom_title TEXT NOT NULL DEFAULT '', is_favorite INTEGER NOT NULL DEFAULT 0,
    native_title TEXT NOT NULL DEFAULT '', ai_title TEXT NOT NULL DEFAULT '',
    agent_name TEXT NOT NULL DEFAULT '', permission_mode TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL DEFAULT '', relocated_cwd TEXT NOT NULL DEFAULT '',
    worktree_name TEXT NOT NULL DEFAULT '', worktree_branch TEXT NOT NULL DEFAULT '',
    original_branch TEXT NOT NULL DEFAULT '',
    compaction_count INTEGER NOT NULL DEFAULT 0, dropped_tokens INTEGER NOT NULL DEFAULT 0,
    input_cost_usd REAL NOT NULL DEFAULT 0, output_cost_usd REAL NOT NULL DEFAULT 0,
    cache_read_cost_usd REAL NOT NULL DEFAULT 0, cache_write_cost_usd REAL NOT NULL DEFAULT 0,
    total_cost_usd REAL NOT NULL DEFAULT 0,
    unpriced_models TEXT NOT NULL DEFAULT '', unpriced_tokens INTEGER NOT NULL DEFAULT 0,
    cost_by_model TEXT NOT NULL DEFAULT '', active_duration_ms INTEGER NOT NULL DEFAULT 0,
    config_dir TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (session_id, project_path)
);
CREATE TABLE claude_subagent_cache (
    parent_session_id TEXT NOT NULL, agent_id TEXT NOT NULL,
    file_path TEXT NOT NULL, file_mtime DATETIME NOT NULL,
    agent_type TEXT NOT NULL DEFAULT '', description TEXT NOT NULL DEFAULT '',
    tool_use_id TEXT NOT NULL DEFAULT '',
    start_time DATETIME, last_activity DATETIME,
    message_count INTEGER NOT NULL DEFAULT 0, event_count INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_5m_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
    model TEXT NOT NULL DEFAULT '',
    input_cost_usd REAL NOT NULL DEFAULT 0, output_cost_usd REAL NOT NULL DEFAULT 0,
    cache_read_cost_usd REAL NOT NULL DEFAULT 0, cache_write_cost_usd REAL NOT NULL DEFAULT 0,
    total_cost_usd REAL NOT NULL DEFAULT 0,
    unpriced_models TEXT NOT NULL DEFAULT '', unpriced_tokens INTEGER NOT NULL DEFAULT 0,
    active_duration_ms INTEGER NOT NULL DEFAULT 0, config_dir TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (parent_session_id, agent_id)
);
CREATE TABLE claude_session_pr (
    session_id TEXT NOT NULL, pr_url TEXT NOT NULL,
    pr_number INTEGER NOT NULL DEFAULT 0, pr_repository TEXT NOT NULL DEFAULT '',
    first_seen_at DATETIME,
    PRIMARY KEY (session_id, pr_url)
);
";

const IDLE_GAP_MS: i64 = 600_000;

/// A minimal but realistic transcript: a user turn, an assistant reply with
/// usage, and a linked PR.
fn transcript(base_minute: u32) -> String {
    format!(
        r#"{{"type":"user","timestamp":"2026-03-15T12:{base_minute:02}:00Z","cwd":"/w","gitBranch":"main","message":{{"role":"user","content":"do the thing"}}}}
{{"type":"assistant","timestamp":"2026-03-15T12:{:02}:30Z","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":"done"}}],"usage":{{"input_tokens":10,"output_tokens":4,"cache_read_input_tokens":2}}}}}}
{{"type":"pr-link","timestamp":"2026-03-15T13:00:00Z","prUrl":"https://github.com/o/r/pull/7","prNumber":7,"prRepository":"o/r"}}
"#,
        base_minute + 1
    )
}

/// Builds `<root>/cfg/projects/-w/<session>.jsonl` for each session.
fn build_corpus(root: &Path, sessions: &[&str]) -> String {
    let cfg = root.join("cfg");
    let project = cfg.join("projects").join("-w");
    std::fs::create_dir_all(&project).unwrap();
    for (i, s) in sessions.iter().enumerate() {
        std::fs::write(project.join(format!("{s}.jsonl")), transcript(i as u32)).unwrap();
    }
    cfg.to_string_lossy().into_owned()
}

fn scratch_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn
}

/// One scan: walk, diff, apply. Returns what the apply reported.
fn scan(
    conn: &mut Connection,
    dirs: &[String],
) -> agento_lib::native::scanner::apply::ApplyOutcome {
    let walk = walk_all_disk_files(dirs);
    let cached = load_cached_entries(conn).unwrap();
    let diff = diff_disk_and_cache(&walk.files, &cached, &walk, "/nonexistent-default");

    let mut units: Vec<ScanUnit> = Vec::new();
    for path in &diff.to_insert {
        units.push(ScanUnit {
            file: walk.files[path].clone(),
            is_new: true,
        });
    }
    for path in &diff.to_update {
        units.push(ScanUnit {
            file: walk.files[path].clone(),
            is_new: false,
        });
    }

    apply_changes(
        conn,
        units,
        &diff.to_delete,
        None,
        IDLE_GAP_MS,
        |_done, _total| {},
    )
}

fn row_count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_first_scan_inserts_every_session_and_announces_each_once() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = build_corpus(tmp.path(), &["s1", "s2"]);
    let mut conn = scratch_db();

    let outcome = scan(&mut conn, std::slice::from_ref(&cfg));

    assert_eq!(outcome.rows_written, 2);
    assert_eq!(outcome.skipped, 0);
    assert_eq!(row_count(&conn, "claude_session_cache"), 2);
    assert_eq!(outcome.notifications.len(), 2);
    assert!(
        outcome.notifications.iter().all(|n| n.is_new),
        "a first scan is all discoveries"
    );

    // The PR rows are written in the same transaction as their session.
    assert_eq!(row_count(&conn, "claude_session_pr"), 2);

    let (preview, model, messages): (String, String, i64) = conn
        .query_row(
            "SELECT preview, model, message_count FROM claude_session_cache WHERE session_id = 's1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(preview, "do the thing");
    assert_eq!(model, "claude-opus-5");
    assert_eq!(messages, 2, "one user turn and one assistant reply");
}

#[test]
fn a_second_scan_with_nothing_changed_does_no_work() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = build_corpus(tmp.path(), &["s1"]);
    let mut conn = scratch_db();

    scan(&mut conn, std::slice::from_ref(&cfg));
    let second = scan(&mut conn, std::slice::from_ref(&cfg));

    assert_eq!(second.rows_written, 0, "nothing changed on disk");
    assert!(second.notifications.is_empty(), "and nothing to announce");
    assert_eq!(row_count(&conn, "claude_session_cache"), 1);
}

#[test]
fn a_rescan_preserves_the_two_user_owned_columns() {
    // custom_title and is_favorite appear in neither the insert list nor the
    // DO UPDATE SET list. A rescan that clobbered them would silently discard
    // the only data in these tables the user typed.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = build_corpus(tmp.path(), &["s1"]);
    let mut conn = scratch_db();
    scan(&mut conn, std::slice::from_ref(&cfg));

    conn.execute(
        "UPDATE claude_session_cache SET custom_title = 'my name for it', is_favorite = 1",
        [],
    )
    .unwrap();

    // Touch the transcript so the file is genuinely re-read.
    let path = Path::new(&cfg).join("projects").join("-w").join("s1.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(
        r#"{"type":"assistant","timestamp":"2026-03-15T12:05:00Z","message":{"role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"more"}]}}
"#,
    );
    std::fs::write(&path, text).unwrap();
    filetime_bump(&path);

    let outcome = scan(&mut conn, std::slice::from_ref(&cfg));
    assert_eq!(outcome.rows_written, 1, "the touched file was re-read");
    assert!(
        !outcome.notifications[0].is_new,
        "a re-read is an update, not a discovery"
    );

    let (title, favorite, messages): (String, i64, i64) = conn
        .query_row(
            "SELECT custom_title, is_favorite, message_count FROM claude_session_cache",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(title, "my name for it");
    assert_eq!(favorite, 1);
    assert_eq!(messages, 3, "and the row did pick up the new turn");
}

#[test]
fn a_removed_transcript_is_reconciled_away_with_its_pull_requests() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = build_corpus(tmp.path(), &["s1", "s2"]);
    let mut conn = scratch_db();
    scan(&mut conn, std::slice::from_ref(&cfg));
    assert_eq!(row_count(&conn, "claude_session_pr"), 2);

    std::fs::remove_file(Path::new(&cfg).join("projects").join("-w").join("s2.jsonl")).unwrap();

    let outcome = scan(&mut conn, std::slice::from_ref(&cfg));
    assert_eq!(outcome.rows_deleted, 1);
    assert_eq!(row_count(&conn, "claude_session_cache"), 1);
    // No foreign key does this for us; the delete pass resolves the session id
    // through the row before removing it.
    assert_eq!(row_count(&conn, "claude_session_pr"), 1);
}

#[test]
fn an_unreadable_config_dir_protects_its_rows_from_the_delete_pass() {
    // The unplugged-drive case, end to end: the rows must survive a scan that
    // could not see their files.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = build_corpus(tmp.path(), &["s1"]);
    let mut conn = scratch_db();
    scan(&mut conn, std::slice::from_ref(&cfg));
    assert_eq!(row_count(&conn, "claude_session_cache"), 1);

    // The dir vanishes, as an unmounted volume does.
    std::fs::remove_dir_all(&cfg).unwrap();

    let outcome = scan(&mut conn, std::slice::from_ref(&cfg));
    assert_eq!(outcome.rows_deleted, 0, "absence is not evidence");
    assert_eq!(row_count(&conn, "claude_session_cache"), 1);
}

#[test]
fn a_sub_agent_is_written_to_its_own_table_and_announced_via_its_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = build_corpus(tmp.path(), &["s1"]);
    let subagents = Path::new(&cfg)
        .join("projects")
        .join("-w")
        .join("s1")
        .join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::write(subagents.join("agent-1.jsonl"), transcript(20)).unwrap();
    std::fs::write(
        subagents.join("agent-1.meta.json"),
        r#"{"agentType":"Explore","description":"map it","toolUseId":"tu_1"}"#,
    )
    .unwrap();

    let mut conn = scratch_db();
    let outcome = scan(&mut conn, std::slice::from_ref(&cfg));

    assert_eq!(row_count(&conn, "claude_subagent_cache"), 1);
    assert_eq!(
        outcome.notifications.len(),
        1,
        "the parent and its sub-agent are one session"
    );

    let (agent_type, description, messages): (String, String, i64) = conn
        .query_row(
            "SELECT agent_type, description, message_count FROM claude_subagent_cache",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(agent_type, "Explore", "from the sidecar");
    assert_eq!(description, "map it");
    assert_eq!(messages, 2);
}

#[test]
fn an_unreadable_transcript_does_not_abort_the_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = build_corpus(tmp.path(), &["s1", "s2"]);
    // A file with no timestamped event produces no row — the same path a read
    // failure takes.
    std::fs::write(
        Path::new(&cfg).join("projects").join("-w").join("s2.jsonl"),
        "not json at all\n",
    )
    .unwrap();

    let mut conn = scratch_db();
    let outcome = scan(&mut conn, std::slice::from_ref(&cfg));

    assert_eq!(outcome.rows_written, 1, "the good session still landed");
    assert_eq!(outcome.skipped, 1);
    assert_eq!(row_count(&conn, "claude_session_cache"), 1);
}

/// Pushes a file's mtime forward, so a same-second rewrite still reads as
/// changed.
fn filetime_bump(path: &PathBuf) {
    // Rewriting through a fresh handle after a moment is enough on every
    // filesystem this runs on; the diff compares the stored mtime exactly.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let text = std::fs::read_to_string(path).unwrap();
    std::fs::write(path, text).unwrap();
}

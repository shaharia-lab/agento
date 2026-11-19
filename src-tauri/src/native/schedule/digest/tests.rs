use super::*;

use chrono::TimeZone;

/// The fire time every fixture renders at, and the window it closes.
fn until() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
}

fn since() -> DateTime<Utc> {
    until() - chrono::Duration::days(WINDOW_DAYS)
}

fn at(day: u32, hour: u32, minute: u32) -> String {
    bound(
        Utc.with_ymd_and_hms(2026, 11, day, hour, minute, 0)
            .unwrap(),
    )
}

fn migrated() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let mut conn = rusqlite::Connection::open(file.path()).expect("open");
    crate::native::migrate::apply(&mut conn).expect("migrate");
    file
}

fn task(conn: &rusqlite::Connection, id: &str, name: &str) {
    conn.execute(
        "INSERT INTO scheduled_tasks (id, name, prompt) VALUES (?1, ?2, 'p')",
        [id, name],
    )
    .expect("task");
}

#[allow(clippy::too_many_arguments)]
fn run(
    conn: &rusqlite::Connection,
    id: &str,
    task_id: &str,
    status: &str,
    trigger: &str,
    started_at: &str,
    chat: &str,
    error: &str,
    tokens: [i64; 4],
) {
    conn.execute(
        "INSERT INTO job_history
            (id, task_id, task_name, status, triggered_by, started_at, chat_session_id,
             error_message, total_input_tokens, total_output_tokens,
             total_cache_creation_tokens, total_cache_read_tokens)
         VALUES (?1, ?2, 'stored name', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            id, task_id, status, trigger, started_at, chat, error, tokens[0], tokens[1], tokens[2],
            tokens[3]
        ],
    )
    .expect("run");
}

fn chat(conn: &rusqlite::Connection, id: &str, sdk: &str) {
    conn.execute(
        "INSERT INTO chat_sessions (id, agent_slug, sdk_session_id, created_at, updated_at)
         VALUES (?1, '', ?2, 't', 't')",
        [id, sdk],
    )
    .expect("chat");
}

fn cached(conn: &rusqlite::Connection, session: &str, project: &str, cost: f64, unpriced: &str) {
    conn.execute(
        "INSERT INTO claude_session_cache
            (session_id, project_path, file_path, file_mtime, start_time, last_activity,
             total_cost_usd, unpriced_models)
         VALUES (?1, ?2, 'f', 't', 't', 't', ?3, ?4)",
        rusqlite::params![session, project, cost, unpriced],
    )
    .expect("cache");
}

fn subagent(conn: &rusqlite::Connection, session: &str, agent: &str, cost: f64, unpriced: &str) {
    conn.execute(
        "INSERT INTO claude_subagent_cache
            (parent_session_id, agent_id, file_path, file_mtime, total_cost_usd, unpriced_models)
         VALUES (?1, ?2, 'f', 't', ?3, ?4)",
        rusqlite::params![session, agent, cost, unpriced],
    )
    .expect("subagent");
}

fn delivery(
    conn: &rusqlite::Connection,
    id: &str,
    job: &str,
    kind: &str,
    status: &str,
    error: &str,
) {
    conn.execute(
        "INSERT INTO job_deliveries (id, job_id, position, type, target, status, error, created_at)
         VALUES (?1, ?2, 0, ?3, '', ?4, ?5, 't')",
        [id, job, kind, status, error],
    )
    .expect("delivery");
}

/// A mixed week: three tasks (two share a name), runs on both sides of both
/// window edges, a continued run on the same session, a cached session stored
/// under two project paths, a sub-agent with an unpriced model, a session
/// the scanner has not reached, three deliveries and one task's counters.
fn mixed_week() -> tempfile::NamedTempFile {
    let file = migrated();
    let conn = rusqlite::Connection::open(file.path()).expect("open");
    task(&conn, "t-a", "Alpha");
    task(&conn, "t-c", "Beta");
    task(&conn, "t-b", "Beta");
    task(&conn, "t-g", "Gamma");
    conn.execute(
        "UPDATE scheduled_tasks SET dropped_event_count = 3, rate_limited_event_count = 1
         WHERE id = 't-g'",
        [],
    )
    .expect("counters");

    chat(&conn, "c1", "s1");
    chat(&conn, "c2", "");
    chat(&conn, "c3", "s3");
    cached(&conn, "s1", "/p/one", 1.25, "");
    cached(&conn, "s1", "/p/two", 1.0, "");
    subagent(&conn, "s1", "a1", 0.5, "mystery-model");

    let z = [0, 0, 0, 0];
    // Just before the window, and exactly at its end: both outside.
    run(
        &conn,
        "j0",
        "t-a",
        "failed",
        "schedule",
        &at(9, 8, 59),
        "c1",
        "early",
        [9, 9, 9, 9],
    );
    run(
        &conn,
        "j6",
        "t-a",
        "failed",
        "schedule",
        &bound(until()),
        "",
        "late",
        [9, 9, 9, 9],
    );
    // Exactly at its start: inside.
    run(
        &conn,
        "j1",
        "t-a",
        "success",
        "schedule",
        &bound(since()),
        "c1",
        "",
        [100, 20, 5, 50],
    );
    run(
        &conn,
        "j2",
        "t-a",
        "failed",
        "manual",
        &at(11, 10, 30),
        "c2",
        "boom\nsecond line",
        z,
    );
    run(
        &conn,
        "j7",
        "t-a",
        "success",
        "reply",
        &at(13, 7, 0),
        "c1",
        "",
        [10, 2, 0, 0],
    );
    run(
        &conn,
        "j3",
        "t-b",
        "success",
        "slack",
        &at(12, 12, 0),
        "c3",
        "",
        [1, 1, 1, 1],
    );
    run(
        &conn,
        "j4",
        "t-c",
        "failed",
        "schedule",
        &at(12, 12, 0),
        "",
        "out of quota",
        z,
    );

    delivery(&conn, "d1", "j1", "slack", "sent", "");
    delivery(
        &conn,
        "d2",
        "j2",
        "fake",
        "failed",
        "fake destination failed\nmore",
    );
    delivery(&conn, "d3", "j3", "telegram", "skipped", "no origin");
    delivery(&conn, "d0", "j0", "fake", "failed", "outside the window");
    drop(conn);
    file
}

fn rendered(path: &Path) -> String {
    let data = gather(path, since(), until()).expect("gather");
    render(&data, since(), until(), Tz::UTC)
}

/// The acceptance criterion's bytes for a mixed week.
#[test]
fn a_mixed_week_renders_byte_for_byte() {
    let file = mixed_week();
    let want = "Weekly digest: 2026-11-09 09:00 to 2026-11-16 09:00 (UTC)

Runs
5 runs across 3 tasks: 2 failed, 3 success.
- Alpha: 3 runs (1 failed, 2 success); by trigger: 1 manual, 1 reply, 1 schedule
- Beta: 1 run (1 success); by trigger: 1 slack
- Beta: 1 run (1 failed); by trigger: 1 schedule

Failed runs
- 2026-11-11 10:30 Alpha: boom
- 2026-11-12 12:00 Beta: out of quota

Cost and tokens
Tokens: 111 input, 23 output, 6 cache write, 51 cache read.
Cost: ≥ $1.75 from 1 priced session.
Not yet priced: 1 session, not scanned yet.
Unpriced models: mystery-model.

Deliveries
3 deliveries: 1 failed, 1 sent, 1 skipped.
- Alpha to fake: fake destination failed

Events not run (running totals since each task was created)
- Gamma: 3 dropped, 1 rate-limited
";
    assert_eq!(rendered(file.path()), want);
}

/// Two tasks with one name sort by id, so the order cannot depend on the
/// order the rows were inserted in (`t-c` went in before `t-b`).
#[test]
fn tasks_with_one_name_are_ordered_by_id() {
    let file = mixed_week();
    let data = gather(file.path(), since(), until()).expect("gather");
    let ids: Vec<&str> = data.tasks.iter().map(|t| t.task_id.as_str()).collect();
    assert_eq!(ids, ["t-a", "t-b", "t-c"]);
}

/// The same database and the same window render the same bytes, every time.
#[test]
fn rendering_is_deterministic() {
    let file = mixed_week();
    let first = rendered(file.path());
    for _ in 0..5 {
        assert_eq!(rendered(file.path()), first);
    }
}

/// Cost is read from stored columns only: emptying the pricing tables, which
/// a re-pricing digest would read, changes nothing.
#[test]
fn editing_the_pricing_changes_nothing() {
    let file = mixed_week();
    let conn = rusqlite::Connection::open(file.path()).expect("open");
    crate::native::pricing_seed::seed(&conn).expect("seed pricing");
    let before = rendered(file.path());
    let priced: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
        .expect("count");
    assert!(priced > 0, "the seed gave no pricing to edit");
    conn.execute_batch("DELETE FROM model_pricing_tier; DELETE FROM model_pricing;")
        .expect("edit pricing");
    drop(conn);
    assert_eq!(rendered(file.path()), before);
}

/// A session the scanner has not reached is counted, never priced at $0, and
/// a priced session under two project paths counts its largest row once.
#[test]
fn an_unscanned_session_is_not_yet_priced_and_a_twice_cached_one_counts_once() {
    let file = mixed_week();
    let cost = gather(file.path(), since(), until()).expect("gather").cost;
    assert_eq!(cost.priced_sessions, 1);
    assert_eq!(cost.unpriced_sessions, 1);
    assert_eq!(cost.usd, 1.75);
    assert!(cost.is_floor());
    assert_eq!(
        cost.unpriced_models.iter().collect::<Vec<_>>(),
        ["mystery-model"]
    );
}

/// With every session priced and no unpriced model, the total is not a floor.
#[test]
fn a_fully_priced_week_is_not_a_floor() {
    let file = migrated();
    let conn = rusqlite::Connection::open(file.path()).expect("open");
    task(&conn, "t1", "Only");
    chat(&conn, "c1", "s1");
    cached(&conn, "s1", "/p", 0.25, "");
    run(
        &conn,
        "j1",
        "t1",
        "success",
        "schedule",
        &at(12, 0, 0),
        "c1",
        "",
        [1, 2, 3, 4],
    );
    drop(conn);
    let text = rendered(file.path());
    assert!(
        text.contains("\nCost: $0.25 from 1 priced session.\n"),
        "{text}"
    );
    assert!(!text.contains("Not yet priced"), "{text}");
    assert!(!text.contains("Unpriced models"), "{text}");
}

/// A week with no runs is one sentence, and the lifetime counters still show.
#[test]
fn an_empty_week_says_so() {
    let file = migrated();
    assert_eq!(
        rendered(file.path()),
        "Weekly digest: 2026-11-09 09:00 to 2026-11-16 09:00 (UTC)\n\nNo task ran in these seven days.\n"
    );
}

/// More failures than the cap list the first ten, oldest first, and count the
/// rest.
#[test]
fn more_than_ten_failures_are_cut_with_a_count() {
    let file = migrated();
    let conn = rusqlite::Connection::open(file.path()).expect("open");
    task(&conn, "t1", "Flaky");
    for i in 0..13u32 {
        let id = format!("j{i:02}");
        run(
            &conn,
            &id,
            "t1",
            "failed",
            "schedule",
            &at(10, i, 0),
            "",
            &format!("e{i}"),
            [0; 4],
        );
    }
    drop(conn);
    let text = rendered(file.path());
    assert!(text.contains("- 2026-11-10 00:00 Flaky: e0\n"), "{text}");
    assert!(
        text.contains("- 2026-11-10 09:00 Flaky: e9\n+3 more\n"),
        "{text}"
    );
    assert!(!text.contains("e10"), "{text}");
}

/// An error is cut to its first line after masking, so a credential on that
/// line is never kept raw. The key is assembled from pieces, so no literal in
/// this file has a credential's shape.
#[test]
fn an_error_keeps_its_first_line_masked() {
    assert_eq!(first_line_masked("first line\nsecond line"), "first line");
    let key = ["AK", "IA", "Q7ZX3MPLR2VN6TWB"].concat();
    let error = format!("upload failed for {key} today\nsecond line");
    let first = first_line_masked(&error);
    assert!(!first.contains(&key), "the key was kept raw");
    assert!(first.starts_with("upload failed for "), "the line was lost");
    assert!(
        !first.contains("second line"),
        "more than one line was kept"
    );
}

/// The acceptance criterion's budget: a digest over 1,000 runs gathers and
/// renders in well under a second.
#[test]
fn a_thousand_runs_gather_and_render_in_under_a_second() {
    let file = migrated();
    let mut conn = rusqlite::Connection::open(file.path()).expect("open");
    let tx = conn.transaction().expect("tx");
    for t in 0..10 {
        task(&tx, &format!("t{t}"), &format!("Task {t}"));
    }
    for i in 0..1000u32 {
        let chat_id = format!("c{i}");
        chat(&tx, &chat_id, &format!("s{i}"));
        if i % 2 == 0 {
            cached(&tx, &format!("s{i}"), "/p", 0.01, "");
        }
        let status = if i % 7 == 0 { "failed" } else { "success" };
        run(
            &tx,
            &format!("j{i}"),
            &format!("t{}", i % 10),
            status,
            "schedule",
            &at(10 + i % 5, i % 24, i % 60),
            &chat_id,
            "boom",
            [1, 1, 1, 1],
        );
        delivery(&tx, &format!("d{i}"), &format!("j{i}"), "fake", "sent", "");
    }
    tx.commit().expect("commit");
    drop(conn);

    let started = std::time::Instant::now();
    let text = rendered(file.path());
    let took = started.elapsed();
    assert!(text.contains("1000 runs across 10 tasks"), "{text}");
    assert!(took < std::time::Duration::from_secs(1), "took {took:?}");
}

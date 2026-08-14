//! Tests for the sessions list that need a database.
//!
//! The metric-vector test here is the third reader of
//! `internal/claudesessions/testdata/session_metric_vectors.json`, after
//! `session_page_test.go` and `frontend/src/lib/sessionMetrics.test.ts`. The
//! fixture exists because a rendered column and the filter that hides its row
//! must not disagree — a session showing $36.30 must not be hidden by "cost at
//! most $40" — and that bug had already happened once before the filtering
//! moved into SQL. Adding a third implementation of those figures without
//! adding a third reader of the fixture would reopen it in a new language.

use rusqlite::Connection;

use super::page::{facets, list_page};
use super::query::{
    build_filter, SessionQuery, Sort, SQL_ACTIVE_DURATION_MS, SQL_COST_USD, SQL_INPUT_TOKENS,
    SQL_MESSAGE_COUNT, SQL_OUTPUT_TOKENS, SQL_TOKENS,
};
use super::summary::SUMMARY_SOURCE;
use crate::native::gojson;
use crate::native::settings::DataSettings;

/// The columns these tests write, which is the subset of the real schema the
/// list reads. Kept in the shape `internal/storage/sqlite.go` produces.
const SCHEMA: &str = "
    CREATE TABLE claude_session_cache (
        session_id TEXT PRIMARY KEY,
        project_path TEXT NOT NULL DEFAULT '',
        preview TEXT NOT NULL DEFAULT '',
        custom_title TEXT NOT NULL DEFAULT '',
        is_favorite INTEGER NOT NULL DEFAULT 0,
        start_time DATETIME NOT NULL,
        last_activity DATETIME NOT NULL,
        message_count INTEGER NOT NULL DEFAULT 0,
        event_count INTEGER NOT NULL DEFAULT 0,
        input_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens INTEGER NOT NULL DEFAULT 0,
        cache_creation_5m_tokens INTEGER NOT NULL DEFAULT 0,
        cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
        git_branch TEXT NOT NULL DEFAULT '',
        model TEXT NOT NULL DEFAULT '',
        cwd TEXT NOT NULL DEFAULT '',
        native_title TEXT NOT NULL DEFAULT '',
        ai_title TEXT NOT NULL DEFAULT '',
        agent_name TEXT NOT NULL DEFAULT '',
        permission_mode TEXT NOT NULL DEFAULT '',
        mode TEXT NOT NULL DEFAULT '',
        relocated_cwd TEXT NOT NULL DEFAULT '',
        worktree_name TEXT NOT NULL DEFAULT '',
        worktree_branch TEXT NOT NULL DEFAULT '',
        original_branch TEXT NOT NULL DEFAULT '',
        compaction_count INTEGER NOT NULL DEFAULT 0,
        dropped_tokens INTEGER NOT NULL DEFAULT 0,
        input_cost_usd REAL NOT NULL DEFAULT 0,
        output_cost_usd REAL NOT NULL DEFAULT 0,
        cache_read_cost_usd REAL NOT NULL DEFAULT 0,
        cache_write_cost_usd REAL NOT NULL DEFAULT 0,
        total_cost_usd REAL NOT NULL DEFAULT 0,
        unpriced_models TEXT NOT NULL DEFAULT '',
        unpriced_tokens INTEGER NOT NULL DEFAULT 0,
        cost_by_model TEXT NOT NULL DEFAULT '',
        active_duration_ms INTEGER NOT NULL DEFAULT 0,
        config_dir TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE claude_subagent_cache (
        parent_session_id TEXT NOT NULL,
        agent_id TEXT NOT NULL,
        model TEXT NOT NULL DEFAULT '',
        input_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens INTEGER NOT NULL DEFAULT 0,
        cache_creation_5m_tokens INTEGER NOT NULL DEFAULT 0,
        cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
        input_cost_usd REAL NOT NULL DEFAULT 0,
        output_cost_usd REAL NOT NULL DEFAULT 0,
        cache_read_cost_usd REAL NOT NULL DEFAULT 0,
        cache_write_cost_usd REAL NOT NULL DEFAULT 0,
        total_cost_usd REAL NOT NULL DEFAULT 0,
        unpriced_models TEXT NOT NULL DEFAULT '',
        unpriced_tokens INTEGER NOT NULL DEFAULT 0,
        active_duration_ms INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (parent_session_id, agent_id)
    );
    CREATE TABLE claude_session_pr (
        session_id TEXT NOT NULL,
        pr_number INTEGER NOT NULL,
        pr_url TEXT NOT NULL,
        pr_repository TEXT NOT NULL DEFAULT '',
        first_seen_at DATETIME NOT NULL,
        PRIMARY KEY (session_id, pr_url)
    );
    CREATE TABLE user_settings (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        hidden_projects TEXT NOT NULL DEFAULT '',
        claude_config_dir TEXT NOT NULL DEFAULT '',
        claude_config_dirs TEXT NOT NULL DEFAULT ''
    );
";

/// One session's stored figures, main thread and delegated.
#[derive(Default, Clone)]
struct TestSession {
    id: &'static str,
    project: &'static str,
    last_activity: &'static str,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    active_ms: i64,
    messages: i64,
    sub_input_tokens: i64,
    sub_output_tokens: i64,
    sub_cost_usd: f64,
    sub_active_ms: i64,
    favorite: bool,
}

fn fixture(sessions: &[TestSession]) -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.execute_batch(SCHEMA).expect("schema");
    for s in sessions {
        let last = if s.last_activity.is_empty() {
            "2026-08-01 12:00:00 +0000 UTC"
        } else {
            s.last_activity
        };
        conn.execute(
            "INSERT INTO claude_session_cache
                (session_id, project_path, start_time, last_activity, message_count,
                 input_tokens, output_tokens, total_cost_usd, active_duration_ms, is_favorite)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                s.id,
                s.project,
                "2026-08-01 10:00:00 +0000 UTC",
                last,
                s.messages,
                s.input_tokens,
                s.output_tokens,
                s.cost_usd,
                s.active_ms,
                i64::from(s.favorite),
            ],
        )
        .expect("insert session");

        if s.sub_input_tokens != 0
            || s.sub_output_tokens != 0
            || s.sub_cost_usd != 0.0
            || s.sub_active_ms != 0
        {
            conn.execute(
                "INSERT INTO claude_subagent_cache
                    (parent_session_id, agent_id, input_tokens, output_tokens,
                     total_cost_usd, active_duration_ms)
                 VALUES (?, 'agent-1', ?, ?, ?, ?)",
                rusqlite::params![
                    s.id,
                    s.sub_input_tokens,
                    s.sub_output_tokens,
                    s.sub_cost_usd,
                    s.sub_active_ms
                ],
            )
            .expect("insert sub-agent");
        }
    }
    conn
}

fn no_settings() -> DataSettings {
    DataSettings::default()
}

#[test]
fn metric_sql_matches_the_shared_cross_language_vectors() {
    #[derive(serde::Deserialize)]
    struct Vectors {
        cases: Vec<Case>,
    }
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        session: Stored,
        expect: Expected,
    }
    #[derive(serde::Deserialize)]
    struct Stored {
        input_tokens: i64,
        output_tokens: i64,
        subagent_input_tokens: i64,
        subagent_output_tokens: i64,
        total_cost_usd: f64,
        subagent_cost_usd: f64,
        active_duration_ms: i64,
        subagent_active_duration_ms: i64,
        message_count: i64,
    }
    #[derive(serde::Deserialize)]
    struct Expected {
        input_tokens: i64,
        output_tokens: i64,
        tokens: i64,
        cost_usd: f64,
        duration_ms: i64,
        duration_minutes: f64,
        messages: i64,
    }

    let raw =
        include_str!("../../../../../internal/claudesessions/testdata/session_metric_vectors.json");
    let vectors: Vectors = serde_json::from_str(raw).expect("parsing the shared vectors");
    assert!(
        !vectors.cases.is_empty(),
        "the shared vectors file declares no cases"
    );

    for (i, case) in vectors.cases.iter().enumerate() {
        let id: &'static str = Box::leak(format!("vec-{i}").into_boxed_str());
        let conn = fixture(&[TestSession {
            id,
            input_tokens: case.session.input_tokens,
            output_tokens: case.session.output_tokens,
            cost_usd: case.session.total_cost_usd,
            active_ms: case.session.active_duration_ms,
            messages: case.session.message_count,
            sub_input_tokens: case.session.subagent_input_tokens,
            sub_output_tokens: case.session.subagent_output_tokens,
            sub_cost_usd: case.session.subagent_cost_usd,
            sub_active_ms: case.session.subagent_active_duration_ms,
            ..Default::default()
        }]);

        let sql = format!(
            "SELECT {SQL_INPUT_TOKENS}, {SQL_OUTPUT_TOKENS}, {SQL_TOKENS}, {SQL_COST_USD}, \
             {SQL_ACTIVE_DURATION_MS}, {SQL_MESSAGE_COUNT}{SUMMARY_SOURCE}\nWHERE c.session_id = ?"
        );
        let (input, output, tokens, cost, duration_ms, messages): (i64, i64, i64, f64, i64, i64) =
            conn.query_row(&sql, [id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .expect("querying metrics");

        let name = &case.name;
        assert_eq!(input, case.expect.input_tokens, "{name}: input tokens");
        assert_eq!(output, case.expect.output_tokens, "{name}: output tokens");
        assert_eq!(tokens, case.expect.tokens, "{name}: tokens");
        assert_eq!(messages, case.expect.messages, "{name}: messages");
        assert!(
            (cost - case.expect.cost_usd).abs() < 1e-9,
            "{name}: cost = {cost}, want {}",
            case.expect.cost_usd
        );
        assert_eq!(duration_ms, case.expect.duration_ms, "{name}: duration");
        let minutes = duration_ms as f64 / 60_000.0;
        assert!(
            (minutes - case.expect.duration_minutes).abs() < 1e-9,
            "{name}: duration = {minutes} min, want {}",
            case.expect.duration_minutes
        );
    }
}

#[test]
fn keyset_pagination_visits_every_row_exactly_once() {
    let sessions: Vec<TestSession> = (0..12)
        .map(|i| TestSession {
            id: Box::leak(format!("s{i:02}").into_boxed_str()),
            // A deliberate tie in the sort column: without the session_id
            // tiebreak these would page against each other, one repeating and
            // one disappearing.
            cost_usd: if i % 3 == 0 { 10.0 } else { i as f64 },
            ..Default::default()
        })
        .collect();
    let conn = fixture(&sessions);

    let mut seen = Vec::new();
    let mut cursor = String::new();
    for _ in 0..10 {
        let q = SessionQuery {
            limit: 5,
            sort: Sort::Cost,
            cursor: cursor.clone(),
            ..Default::default()
        };
        let page = list_page(&conn, &no_settings(), &q).expect("page");
        seen.extend(page.items.iter().map(|s| s.session_id.clone()));
        cursor = page.next_cursor.clone();
        if cursor.is_empty() {
            break;
        }
    }

    seen.sort();
    let mut want: Vec<String> = sessions.iter().map(|s| s.id.to_string()).collect();
    want.sort();
    assert_eq!(seen, want, "every row exactly once");
}

#[test]
fn filters_read_delegated_totals_not_just_the_main_thread() {
    let conn = fixture(&[
        TestSession {
            id: "main-only",
            cost_usd: 10.0,
            ..Default::default()
        },
        TestSession {
            id: "delegated",
            cost_usd: 10.0,
            sub_cost_usd: 26.3,
            ..Default::default()
        },
    ]);

    let q = SessionQuery {
        cost: super::query::NumericRange {
            min: Some(30.0),
            max: None,
        },
        ..Default::default()
    };
    let page = list_page(&conn, &no_settings(), &q).expect("page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].session_id, "delegated");
    // The row's own total agrees with the filter that let it through.
    assert!((page.items[0].total_cost_usd() - 36.3).abs() < 1e-9);
}

#[test]
fn hidden_projects_are_excluded_from_both_the_page_and_the_totals() {
    let conn = fixture(&[
        TestSession {
            id: "visible",
            project: "/home/u/keep",
            cost_usd: 5.0,
            ..Default::default()
        },
        TestSession {
            id: "hidden",
            project: "/home/u/secret",
            cost_usd: 50.0,
            ..Default::default()
        },
    ]);
    let settings = DataSettings {
        hidden_projects: vec!["/home/u/secret".to_string()],
        ..Default::default()
    };

    let q = SessionQuery::default();
    let page = list_page(&conn, &settings, &q).expect("page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].session_id, "visible");

    // The toolbar's counter must describe the same set as the rows below it.
    let f = facets(&conn, &settings, &q).expect("facets");
    assert_eq!(f.total, 1);
    assert!((f.total_cost_usd - 5.0).abs() < 1e-9);
}

#[test]
fn the_facet_total_and_the_page_describe_the_same_predicate() {
    let conn = fixture(&[
        TestSession {
            id: "a",
            cost_usd: 1.0,
            favorite: true,
            ..Default::default()
        },
        TestSession {
            id: "b",
            cost_usd: 2.0,
            ..Default::default()
        },
        TestSession {
            id: "c",
            cost_usd: 3.0,
            favorite: true,
            ..Default::default()
        },
    ]);

    let q = SessionQuery {
        favorites_only: true,
        ..Default::default()
    };
    let page = list_page(&conn, &no_settings(), &q).expect("page");
    let f = facets(&conn, &no_settings(), &q).expect("facets");
    assert_eq!(page.items.len() as i64, f.total);
    assert!((f.total_cost_usd - 4.0).abs() < 1e-9);
    // Options come from the whole corpus, so a toggle never removes itself.
    assert!(f.has_favorites);
}

#[test]
fn an_empty_page_serializes_as_an_empty_array_not_null() {
    let conn = fixture(&[]);
    let page = list_page(&conn, &no_settings(), &SessionQuery::default()).expect("page");
    let json = String::from_utf8(gojson::to_vec(&page).expect("encode")).expect("utf-8");
    assert_eq!(
        json,
        "{\"items\":[],\"next_cursor\":\"\",\"has_more\":false}\n"
    );
}

#[test]
fn the_config_dir_scope_admits_blank_rows() {
    let conn = fixture(&[TestSession {
        id: "legacy",
        ..Default::default()
    }]);
    // Rows written before migration 27 carry a blank config dir and belong to
    // the default dir, so a scope that excluded them would hide real history.
    let settings = DataSettings {
        indexed_config_dirs: vec!["/home/u/.claude".to_string()],
        ..Default::default()
    };
    let page = list_page(&conn, &settings, &SessionQuery::default()).expect("page");
    assert_eq!(page.items.len(), 1);
}

#[test]
fn a_drilldown_window_replaces_the_range_rather_than_narrowing_it() {
    let conn = fixture(&[TestSession {
        id: "inside",
        last_activity: "2026-08-01 12:00:00 +0000 UTC",
        ..Default::default()
    }]);

    let filter = build_filter(
        &SessionQuery::parse("windows=1000-2000&from=2026-01-01T00:00:00Z").expect("parse"),
        &[],
        &[],
    )
    .expect("filter");
    let rendered = filter.where_clause();
    // One window term, bound to its two ends — and nothing from the range.
    // The window's own predicate mentions last_activity, so the check is on
    // the argument count and the *strict* comparison a window uses, not on a
    // substring that both forms share.
    assert!(rendered.contains("c.start_time < ?"), "{rendered}");
    assert!(
        !rendered.contains("c.start_time <= ?"),
        "the from/to range must not survive a drill-down: {rendered}"
    );
    assert_eq!(
        filter.args.len(),
        2,
        "one window binds exactly its two ends"
    );

    let _ = conn;
}

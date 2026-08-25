//! Tests for the sessions list that need a database.
//!
//! The metric-vector test here reads `parity/session_metric_vectors.json`. The
//! fixture exists because a rendered column and the filter that hides its row
//! must not disagree — a session showing $36.30 must not be hidden by "cost at
//! most $40" — and that bug had already happened once before the filtering
//! moved into SQL. Adding a third implementation of those figures without
//! adding a third reader of the fixture would reopen it in a new language.
//!
//! It lived at `internal/claudesessions/testdata/session_metric_vectors.json`
//! and had three readers — `session_page_test.go`,
//! `frontend/src/lib/sessionMetrics.test.ts` and this one. #391 deleted the Go
//! tree and the web frontend, so this is the **only** reader left and the file
//! moved into `parity/` with the rest of the frozen goldens. It is no longer a
//! cross-language check — this app computes these figures in SQL and renders
//! what the API returns, with no second implementation in the UI. What it still
//! pins is the SQL itself against the numbers Go was asserted to produce, which
//! is the whole reason the values are worth keeping now that the generator is
//! gone.

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
        session_id TEXT NOT NULL,
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
        config_dir TEXT NOT NULL DEFAULT '',
        -- The real key, as `internal/storage/sqlite.go` declares it. It was
        -- `session_id TEXT PRIMARY KEY` here, which made the one shape these
        -- tests could not express — a session id under two project paths —
        -- also the one that reached production broken (#344).
        PRIMARY KEY (session_id, project_path)
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
    for (i, s) in sessions.iter().enumerate() {
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
            // The agent id is per row, not a constant: `claude_subagent_cache`
            // is keyed on `(parent_session_id, agent_id)`, so two rows sharing
            // a session id — which the composite session key now admits —
            // would otherwise collide here instead of expressing the shape.
            conn.execute(
                "INSERT INTO claude_subagent_cache
                    (parent_session_id, agent_id, input_tokens, output_tokens,
                     total_cost_usd, active_duration_ms)
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    s.id,
                    format!("agent-{i}"),
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

    let raw = include_str!("../../../../parity/session_metric_vectors.json");
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
    let mut sessions: Vec<TestSession> = (0..12)
        .map(|i| TestSession {
            id: Box::leak(format!("s{i:02}").into_boxed_str()),
            // A deliberate tie in the sort column: without the session_id
            // tiebreak these would page against each other, one repeating and
            // one disappearing.
            cost_usd: if i % 3 == 0 { 10.0 } else { i as f64 },
            ..Default::default()
        })
        .collect();
    // And a tie in the *tiebreak* as well — one id under two project paths,
    // which the table's key permits and which the id-only cursor cannot tell
    // apart. Without this the suite ties only the sort column, and the one
    // thing that can still break the order goes untested (#364).
    for project in ["/home/u/a", "/home/u/b"] {
        sessions.push(TestSession {
            id: "s-dup",
            project,
            cost_usd: 10.0,
            ..Default::default()
        });
    }
    let conn = fixture(&sessions);

    // Every page size, so the boundary is guaranteed to fall between the two
    // rows sharing an id; a fixed size can put the whole pair inside one page.
    for limit in 1..=5 {
        let mut seen = Vec::new();
        let mut cursor = String::new();
        for _ in 0..sessions.len() + 1 {
            let q = SessionQuery {
                limit,
                sort: Sort::Cost,
                cursor: cursor.clone(),
                ..Default::default()
            };
            let page = list_page(&conn, &no_settings(), &q).expect("page");
            seen.extend(
                page.items
                    .iter()
                    .map(|s| format!("{}@{}", s.session_id, s.project_path)),
            );
            cursor = page.next_cursor.clone();
            if cursor.is_empty() {
                break;
            }
        }

        seen.sort();
        let mut want: Vec<String> = sessions
            .iter()
            .map(|s| format!("{}@{}", s.id, s.project))
            .collect();
        want.sort();
        assert_eq!(seen, want, "limit {limit}: every row exactly once");
    }
}

#[test]
fn a_duplicated_session_id_on_a_tied_sort_value_is_still_reachable() {
    // The fixture from #364: `claude_session_cache` is keyed on
    // `(session_id, project_path)`, so one id legitimately yields two rows.
    // When those two also tie on the sort column, a cursor carrying only the
    // id cannot tell them apart — the walk visits `dup@/a`, jumps straight to
    // `other@/c`, and `dup@/b` is never returned by any page, while `facets`
    // counts it with `COUNT(*)` and the toolbar says three.
    let conn = fixture(&[
        TestSession {
            id: "dup",
            project: "/a",
            cost_usd: 5.0,
            ..Default::default()
        },
        TestSession {
            id: "dup",
            project: "/b",
            cost_usd: 5.0,
            ..Default::default()
        },
        TestSession {
            id: "other",
            project: "/c",
            cost_usd: 1.0,
            ..Default::default()
        },
    ]);

    let mut walk = Vec::new();
    let mut cursor = String::new();
    for _ in 0..5 {
        let q = SessionQuery {
            limit: 1,
            sort: Sort::Cost,
            cursor: cursor.clone(),
            ..Default::default()
        };
        let page = list_page(&conn, &no_settings(), &q).expect("page");
        walk.extend(
            page.items
                .iter()
                .map(|s| format!("{}@{}", s.session_id, s.project_path)),
        );
        cursor = page.next_cursor.clone();
        if cursor.is_empty() {
            break;
        }
    }
    assert_eq!(walk, vec!["dup@/b", "dup@/a", "other@/c"]);

    let f = facets(
        &conn,
        &no_settings(),
        &SessionQuery {
            sort: Sort::Cost,
            ..Default::default()
        },
    )
    .expect("facets");
    assert_eq!(
        f.total,
        walk.len() as i64,
        "the toolbar must not count a row the scroll cannot reach"
    );
}

#[test]
fn a_cursor_minted_before_the_project_tiebreak_still_pages() {
    let sessions: Vec<TestSession> = (0..5)
        .map(|i| TestSession {
            id: Box::leak(format!("s-{i}").into_boxed_str()),
            cost_usd: f64::from(i),
            ..Default::default()
        })
        .collect();
    let conn = fixture(&sessions);

    // A scroll in flight when the binary changed carries a cursor with no "p"
    // at all — spelled out here rather than minted, because `Cursor` now always
    // emits the key. It decodes with the field empty (Go leaves a missing field
    // at its zero value, and `#[serde(default)]` is what matches that), and
    // `c.project_path < ''` is never true, so the predicate degrades to the
    // id-only one it was minted under rather than dropping the rest of the
    // scroll.
    let legacy = super::query::base64_url_nopad(br#"{"s":"cost","v":"3","id":"s-3"}"#);
    let page = list_page(
        &conn,
        &no_settings(),
        &SessionQuery {
            limit: 10,
            sort: Sort::Cost,
            cursor: legacy,
            ..Default::default()
        },
    )
    .expect("paging a legacy cursor");
    let ids: Vec<&str> = page.items.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(ids, vec!["s-2", "s-1", "s-0"]);
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
        &conn,
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
}

/// The search answers exactly as it did before #436 on a database with no
/// `session_search` table — which is what this whole fixture is, since `SCHEMA`
/// above is the pre-#436 subset. It is the honest form of the degradation case:
/// `tests_search.rs` can only reach it by dropping a table it created, which
/// leaves the migration behind and could mask a difference between "never
/// existed" and "removed".
///
/// The six metadata columns still match, and nothing errors.
#[test]
fn a_search_still_matches_metadata_with_no_index_table() {
    let conn = fixture(&[
        TestSession {
            id: "wanted",
            project: "/home/u/alpha",
            ..Default::default()
        },
        TestSession {
            id: "other",
            project: "/home/u/beta",
            ..Default::default()
        },
    ]);

    let q = SessionQuery::parse("q=wanted").expect("parse");
    let page = list_page(&conn, &no_settings(), &q).expect("page");
    assert_eq!(
        page.items.iter().map(|s| &s.session_id).collect::<Vec<_>>(),
        vec!["wanted"]
    );
    // And the toolbar's counters agree, over the same predicate.
    assert_eq!(facets(&conn, &no_settings(), &q).expect("facets").total, 1);

    // A multi-word query is still the old substring match: nothing here can
    // answer word-order-independently, and it must not error trying.
    let words = SessionQuery::parse("q=alpha+wanted").expect("parse");
    assert!(list_page(&conn, &no_settings(), &words)
        .expect("page")
        .items
        .is_empty());
}

#[test]
fn a_session_id_under_two_project_paths_keeps_both_rows_side_tables() {
    // `claude_session_cache` is keyed on `(session_id, project_path)`, so
    // copying one Claude account's history onto a machine that already has it
    // — or simply resuming the same session id from two checkouts — puts one
    // id on a page twice. `claude_session_pr` and `claude_subagent_cache` are
    // keyed on the session id alone, so both rows must be handed the *same*
    // entry. Draining the map instead gave the first row everything and every
    // later row an empty list, which renders as "no linked PRs" rather than as
    // an error (#344).
    let conn = fixture(&[
        TestSession {
            id: "dup",
            project: "/home/u/projects/alpha",
            cost_usd: 2.0,
            ..Default::default()
        },
        TestSession {
            id: "dup",
            project: "/home/u/projects/beta",
            cost_usd: 1.0,
            ..Default::default()
        },
    ]);
    conn.execute(
        "INSERT INTO claude_session_pr
            (session_id, pr_number, pr_url, pr_repository, first_seen_at)
         VALUES ('dup', 7, 'https://github.com/o/r/pull/7', 'o/r',
                 '2026-08-01 11:00:00 +0000 UTC')",
        [],
    )
    .expect("insert pr");
    conn.execute(
        "INSERT INTO claude_subagent_cache
            (parent_session_id, agent_id, model, input_tokens, output_tokens,
             total_cost_usd)
         VALUES ('dup', 'agent-9', 'claude-haiku-4-5-20251001', 11, 22, 0.5)",
        [],
    )
    .expect("insert sub-agent");

    let page = list_page(&conn, &no_settings(), &SessionQuery::default()).expect("page");
    assert_eq!(page.items.len(), 2, "both rows are on the page");
    for s in &page.items {
        assert_eq!(s.prs.len(), 1, "row {} lost its linked PRs", s.project_path);
        assert_eq!(s.prs[0].pr_number, 7);
        assert_eq!(
            s.subagent_usage_by_model
                .get("claude-haiku-4-5-20251001")
                .map(|u| u.input_tokens),
            Some(11),
            "row {} lost its per-model delegated usage",
            s.project_path
        );
        assert_eq!(
            s.subagent_cost_by_model
                .get("claude-haiku-4-5-20251001")
                .map(|c| c.total_usd),
            Some(0.5),
            "row {} lost its per-model delegated cost",
            s.project_path
        );
    }
}

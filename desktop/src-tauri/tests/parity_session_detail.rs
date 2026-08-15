//! Live parity for `GET /api/claude-sessions/{id}` and
//! `GET /api/claude-sessions/projects`: diff both against a *running* Go
//! server, byte for byte.
//!
//! Ignored by default: it needs a real Agento instance and its database, and CI
//! has neither.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_session_detail -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Never point this at the instance on :8990.** That is whatever binary the
//! developer installed, which drifts behind the repo — a stale baseline makes a
//! wrong port look verified, and a right one look broken.
//!
//! **No seeding needed, but coverage is not automatic.** The detail re-reads a
//! real transcript, and the machine's corpus already contains every shape that
//! matters — but only if the sessions this diffs are *varied*. The suite
//! therefore walks the list endpoint and picks sessions by property rather than
//! taking the first N: one with sub-agents, one with a custom title, one with a
//! stored cost, one with linked PRs, plus the most recent and the largest. It
//! asserts it found more than a handful, because a run over three trivial
//! sessions would pass while proving very little.
//!
//! The shapes most likely to diverge, and which the picks above are chosen to
//! reach:
//!
//! - a `tool_use` block's `input`, which must arrive with its stored key order
//!   and number spelling (it travels as a `json.RawMessage`);
//! - a session with delegated work, where `subagents`, `subagent_count` and
//!   `subagent_usage` are patched in from the cache;
//! - a session the scanner has priced, where `cost` and `subagent_cost` come
//!   from the cache rather than from the re-read;
//! - the embedded summary, which Go **flattens** into the same object.
//!
//! **Read-only.** These issue GETs and nothing else.

mod parity_common;

use std::collections::BTreeSet;

use parity_common::*;

use agento_lib::native::sessions::{detail, projects};
use agento_lib::native::{gojson, query};

/// Pull the whole list, following the cursor, so the picks below choose from
/// the corpus rather than from its first page.
async fn all_sessions() -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut cursor = String::new();
    loop {
        let path = if cursor.is_empty() {
            "/api/claude-sessions?limit=200".to_string()
        } else {
            format!("/api/claude-sessions?limit=200&cursor={cursor}")
        };
        let body = fetch(&path).await;
        let page: serde_json::Value = serde_json::from_slice(&body).expect("page json");
        let items = page
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        out.extend(items);
        match page.get("next_cursor").and_then(|v| v.as_str()) {
            Some(next) if !next.is_empty() && out.len() < 2_000 => {
                cursor = form_urlencoded::byte_serialize(next.as_bytes()).collect()
            }
            _ => break,
        }
    }
    out
}

fn id_of(session: &serde_json::Value) -> String {
    session
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn nonzero(session: &serde_json::Value, key: &str) -> bool {
    session.get(key).and_then(|v| v.as_i64()).unwrap_or(0) > 0
}

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_session_detail_matches_the_live_go_response() {
    let db_path = live_db();
    let sessions = all_sessions().await;
    assert!(
        sessions.len() >= 5,
        "only {} sessions on the parity instance — too few to exercise the \
         detail's shapes (see this file's header)",
        sessions.len()
    );

    // Pick by property, not by position: each of these reaches a branch the
    // others do not.
    let mut picks: BTreeSet<String> = BTreeSet::new();
    let pick_first = |pred: &dyn Fn(&serde_json::Value) -> bool, picks: &mut BTreeSet<String>| {
        if let Some(s) = sessions.iter().find(|s| pred(s)) {
            picks.insert(id_of(s));
        }
    };
    pick_first(&|s| nonzero(s, "subagent_count"), &mut picks);
    pick_first(
        &|s| s.get("custom_title").and_then(|v| v.as_str()).is_some(),
        &mut picks,
    );
    pick_first(
        &|s| {
            s.get("cost")
                .and_then(|c| c.get("total_usd"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
                > 0.0
        },
        &mut picks,
    );
    pick_first(&|s| s.get("prs").is_some(), &mut picks);
    pick_first(&|s| nonzero(s, "compaction_count"), &mut picks);
    pick_first(&|s| !s.get("is_favorite").is_none(), &mut picks);
    // The most recent, and the busiest, whatever else they are.
    if let Some(s) = sessions.first() {
        picks.insert(id_of(s));
    }
    if let Some(s) = sessions.iter().max_by_key(|s| {
        s.get("event_count")
            .and_then(|v| v.as_i64())
            .unwrap_or_default()
    }) {
        picks.insert(id_of(s));
    }

    assert!(
        picks.len() >= 4,
        "only {} distinct sessions selected — the corpus does not cover enough \
         shapes for this diff to mean much",
        picks.len()
    );

    for id in &picks {
        let go = fetch(&format!("/api/claude-sessions/{id}")).await;
        let native = detail::get(&db_path, id)
            .unwrap_or_else(|e| panic!("native detail for {id}: {e}"))
            .unwrap_or_else(|| panic!("native detail for {id} was not found"));
        assert_identical(
            &format!("session {id}"),
            &go,
            &gojson::to_vec(&native).expect("encode"),
        );
    }
    println!("diffed {} session details", picks.len());
}

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_projects_read_matches_the_live_go_response() {
    let db_path = live_db();

    for (case, include) in [("", false), ("include_hidden=true", true)] {
        let path = if case.is_empty() {
            "/api/claude-sessions/projects".to_string()
        } else {
            format!("/api/claude-sessions/projects?{case}")
        };
        let go = fetch(&path).await;
        assert_eq!(
            projects::include_hidden(case),
            include,
            "the query parser and the case disagree"
        );
        let native = gojson::to_vec(&projects::list(&db_path, include).expect("native projects"))
            .expect("encode");
        assert_identical(&format!("projects?{case}"), &go, &native);
    }

    // Two empty lists diff clean. The corpus this runs against has projects,
    // and if it does not the diff proves nothing.
    let go = fetch("/api/claude-sessions/projects").await;
    let count = String::from_utf8(go)
        .expect("utf8")
        .matches("\"encoded_name\":")
        .count();
    assert!(
        count >= 3,
        "only {count} projects on the parity instance — too few for the sort \
         and the fold-by-encoded-name to be exercised"
    );

    // The parser is Go's, so an unrecognised spelling must not opt in.
    assert!(!projects::include_hidden("include_hidden=1"));
    assert_eq!(
        query::value("include_hidden=true", "include_hidden"),
        "true"
    );
}

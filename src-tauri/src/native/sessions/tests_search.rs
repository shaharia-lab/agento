//! The sessions list's search, over a real `session_search` index (#436).
//!
//! Separate from `tests_db.rs` on purpose, and the separation is itself an
//! assertion. `tests_db`'s fixture is a hand-written subset schema with **no
//! `session_search` table**, so a search driven through it is the degradation
//! case on a database that genuinely lacks the index rather than one this suite
//! dropped it from — `a_search_still_matches_metadata_with_no_index_table` is
//! that test, and it lives over there for that reason. These tests are the other
//! half: they build their database with `migrate::apply`, so the FTS5 table is
//! migration 33's own DDL rather than a second spelling of it that could drift.
//!
//! What is deliberately *not* here: anything about ranking. #436 composes a
//! membership test; the score is #437's, and `native::search`'s own tests
//! already pin it.

use rusqlite::{params, Connection};

use super::page::{facets, list_page};
use super::query::{build_fts_query, SessionQuery};
use crate::native::migrate;
use crate::native::search::{self, SearchDoc};
use crate::native::settings::DataSettings;

/// A migrated database — the schema the app actually runs on.
fn migrated() -> Connection {
    let mut conn = Connection::open_in_memory().expect("in-memory database");
    migrate::apply(&mut conn).expect("apply migrations");
    conn
}

/// One cache row, with the metadata columns the LIKE clause reads.
fn session(
    conn: &Connection,
    session_id: &str,
    project_path: &str,
    preview: &str,
    native_title: &str,
) {
    conn.execute(
        "INSERT INTO claude_session_cache
             (session_id, project_path, file_path, file_mtime, start_time, last_activity,
              preview, native_title)
         VALUES (?1, ?2, ?3, '2026-08-01 10:00:00 +0000 UTC',
                 '2026-08-01 10:00:00 +0000 UTC', '2026-08-01 12:00:00 +0000 UTC', ?4, ?5)",
        params![
            session_id,
            project_path,
            format!("{project_path}/{session_id}.jsonl"),
            preview,
            native_title,
        ],
    )
    .expect("insert session");
}

/// Index one session's content. Hand-populated rather than produced by the
/// worker, which is what makes this suite independent of #435.
fn index(conn: &Connection, session_id: &str, project_path: &str, user_text: &str) {
    search::replace(
        conn,
        &SearchDoc {
            session_id: session_id.to_string(),
            project_path: project_path.to_string(),
            user_text: user_text.to_string(),
            ..Default::default()
        },
    )
    .expect("index");
}

fn query(q: &str) -> SessionQuery {
    SessionQuery::parse(q).expect("parse query")
}

/// The session ids one search returns, in page order.
fn found(conn: &Connection, q: &str) -> Vec<String> {
    list_page(conn, &DataSettings::default(), &query(q))
        .expect("page")
        .items
        .into_iter()
        .map(|s| s.session_id)
        .collect()
}

/// A corpus whose sessions are distinguishable by *where* their text lives:
/// `content-only` is findable only through the index, `metadata-only` only
/// through the LIKE clause, and `unindexed` has no index row at all.
fn corpus() -> Connection {
    let conn = migrated();

    session(&conn, "content-only", "/home/u/alpha", "", "");
    index(
        &conn,
        "content-only",
        "/home/u/alpha",
        "please fix the auth bug in the login handler",
    );

    session(
        &conn,
        "metadata-only",
        "/home/u/beta",
        "a preview mentioning auth",
        "",
    );
    index(&conn, "metadata-only", "/home/u/beta", "unrelated content");

    session(&conn, "unindexed", "/home/u/gamma", "", "auth in the title");

    session(&conn, "quiet", "/home/u/delta", "", "");
    index(&conn, "quiet", "/home/u/delta", "nothing of interest here");

    conn
}

/// The acceptance criterion this issue exists for: all words, any order.
///
/// The old single `LIKE '%fix auth bug%'` could only match the words in the
/// order they were typed and adjacent, so every one of these but the first was
/// a miss.
#[test]
fn a_multi_word_search_matches_all_words_in_any_order() {
    let conn = corpus();

    for q in ["fix+auth+bug", "bug+auth+fix", "auth+bug", "login+auth"] {
        assert_eq!(
            found(&conn, &format!("q={q}")),
            vec!["content-only"],
            "query {q}"
        );
    }
}

/// AND, not OR: one word the session does not contain excludes it.
#[test]
fn every_word_must_match_not_just_one() {
    let conn = corpus();
    assert!(
        found(&conn, "q=auth+quasarflux").is_empty(),
        "a word nothing contains must exclude the session"
    );
}

/// Content is what the index adds, and it is invisible to the six metadata
/// columns — this session's cache row holds none of these words.
#[test]
fn content_is_findable_and_was_not_before() {
    let conn = corpus();
    assert_eq!(found(&conn, "q=handler"), vec!["content-only"]);

    // The same query with the index dropped finds nothing, which is what the
    // list did before #436 — so the assertion above is about the index rather
    // than about some other column happening to hold the word.
    conn.execute("DROP TABLE session_search", [])
        .expect("drop the index");
    assert!(found(&conn, "q=handler").is_empty());
}

/// The LIKE half stays OR'd in, so the three things the index cannot answer
/// still match: the session id and project path (UNINDEXED in `session_search`),
/// and a session the worker has not reached — which on a fresh install is all
/// of them.
#[test]
fn a_session_with_no_index_row_still_matches_on_its_metadata() {
    let conn = corpus();

    // No index row at all.
    assert_eq!(found(&conn, "q=title"), vec!["unindexed"]);
    // Preview, on a session whose indexed content says something else.
    assert_eq!(found(&conn, "q=preview"), vec!["metadata-only"]);
    // The session id itself.
    assert_eq!(found(&conn, "q=metadata-only"), vec!["metadata-only"]);
    // The project path.
    assert_eq!(found(&conn, "q=gamma"), vec!["unindexed"]);
}

/// The two halves are a union, and a query hitting one session through each
/// returns both.
#[test]
fn the_index_and_the_metadata_clause_are_a_union() {
    let conn = corpus();
    let mut ids = found(&conn, "q=auth");
    ids.sort();
    assert_eq!(ids, vec!["content-only", "metadata-only", "unindexed"]);
}

/// With `session_search` gone the list answers exactly as it did before #436 —
/// no error, no 500, and the metadata match is untouched.
#[test]
fn with_the_index_table_dropped_search_answers_as_it_did_before() {
    let conn = corpus();
    conn.execute("DROP TABLE session_search", [])
        .expect("drop the index");

    let mut ids = found(&conn, "q=auth");
    ids.sort();
    assert_eq!(
        ids,
        vec!["metadata-only", "unindexed"],
        "only the six metadata columns are left"
    );
    // And the facets over the same predicate agree rather than erroring.
    let f = facets(&conn, &DataSettings::default(), &query("q=auth")).expect("facets");
    assert_eq!(f.total, 2);
}

/// A `%` or `_` typed by a user is still a literal to the LIKE half. The FTS
/// half cannot see them at all — `unicode61` drops both — so the metadata
/// behaviour is the whole of this, and it is what
/// `search_wildcards_are_escaped_so_they_match_literally` pins at the string
/// level.
#[test]
fn like_wildcards_are_still_literal_with_the_index_present() {
    let conn = migrated();
    session(&conn, "s-percent", "/p", "100% done", "");
    session(&conn, "s-plain", "/p", "1000 things", "");
    index(&conn, "s-percent", "/p", "");
    index(&conn, "s-plain", "/p", "");

    assert_eq!(found(&conn, "q=100%25"), vec!["s-percent"]);
}

/// Every FTS5 metacharacter a user can type reaches the tokenizer as text.
///
/// Each row is chosen so the two readings give **different** answers — a table
/// where the operator and the literal happen to agree would pass against a raw
/// interpolation and prove nothing. `content-only`'s indexed text is `please fix
/// the auth bug in the login handler`, and the `note` on each row is what FTS5
/// would have answered had the characters stayed syntax.
#[test]
fn fts_metacharacters_are_matched_literally_rather_than_as_operators() {
    // The only session in this corpus whose text is reachable through the index.
    let hit = vec!["content-only".to_string()];
    let none: Vec<String> = Vec::new();

    let conn = corpus();
    for (q, want, note) in [
        ("-auth", &hit, "a leading `-` excludes the term: no rows"),
        (
            "%5Ebug",
            &hit,
            "`^` anchors to a column's first token: no rows",
        ),
        ("auth+*", &hit, "a bare `*` is a syntax error"),
        ("(auth", &hit, "an unbalanced paren is a syntax error"),
        ("auth%22", &hit, "an unterminated string is a syntax error"),
        (
            "auth+NOT+quasarflux",
            &none,
            "`NOT` excludes a word this session does not have: one row",
        ),
        ("auth+OR+quasarflux", &none, "`OR` unions: one row"),
        (
            "user_text%3Aauth",
            &none,
            "`:` selects the column this word is in: one row",
        ),
    ] {
        let ids = found(&conn, &format!("q={q}"));
        assert_eq!(&ids, want, "query {q} — as syntax this would be: {note}");
    }
}

/// A user-typed phrase is honoured as a phrase — adjacent, in order.
#[test]
fn a_quoted_phrase_matches_as_a_phrase() {
    let conn = migrated();
    session(&conn, "ordered", "/p", "", "");
    index(&conn, "ordered", "/p", "the auth bug is here");
    session(&conn, "reversed", "/p", "", "");
    index(&conn, "reversed", "/p", "the bug auth is here");

    let mut both = found(&conn, "q=auth+bug");
    both.sort();
    assert_eq!(both, vec!["ordered", "reversed"], "unquoted is AND");

    assert_eq!(
        found(&conn, "q=%22auth+bug%22"),
        vec!["ordered"],
        "quoted is a phrase"
    );
}

/// The trailing token is a prefix term, which is what makes the list narrow as
/// somebody types rather than only on a completed word.
#[test]
fn the_last_word_matches_as_a_prefix() {
    let conn = corpus();
    assert_eq!(found(&conn, "q=hand"), vec!["content-only"]);
    // ...but not a word before it, which is finished.
    assert!(found(&conn, "q=hand+bug").is_empty());
}

/// No string a user can type may make the query error.
///
/// Two assertions per input, and they pin different things:
///
/// 1. **`build_fts_query`'s output is a valid MATCH expression.** This is the
///    acceptance criterion — safety by construction — and it is the one the
///    fallback would otherwise hide: a builder that emitted raw text would fail
///    the probe, degrade to LIKE, and answer 200 with nothing to say the index
///    had stopped being consulted.
/// 2. **`list_page` and `facets` both answer.** Two statements built from the
///    same `Filter`, only one of them on the obvious path — and this is what
///    stays true even if (1) ever regresses.
///
/// The generator is deterministic rather than randomised: a fuzz test that
/// fails one run in fifty and passes on a re-run is worse than no test. Its
/// alphabet is every character FTS5's grammar reads plus the ones that broke a
/// draft of this — a NUL (reachable as `%00` in a query string, and an
/// `unterminated string` error to FTS5's parser) and whitespace.
///
/// (1) runs over every case; (2) runs over the hand-written ones and every
/// string of length ≤2, because `facets` alone is seven statements and the two
/// endpoints together take the exhaustive length-3 sweep from ~1s to ~12s on
/// every `cargo test`. What they cover is the same seam — a `Filter` whose
/// arguments do not line up with its `?`s — and that is a property of the clause
/// rather than of the input.
#[test]
fn no_input_string_can_make_the_search_error() {
    let conn = corpus();
    let settings = DataSettings::default();

    let alphabet: Vec<char> = "\"'-*^():+ \t\u{0}aOR\\%_.".chars().collect();
    let mut inputs: Vec<String> = vec![
        String::new(),
        " ".into(),
        "\u{0}".into(),
        "\"".into(),
        "\"\"".into(),
        "\"\"\"".into(),
        "-".into(),
        "*".into(),
        "^".into(),
        "NEAR".into(),
        "NEAR(a b)".into(),
        "a:b:c".into(),
        "\\".into(),
        "(((((".into(),
        "auth\u{0}bug".into(),
        "\u{feff}\u{200b}".into(),
        "é".repeat(50),
        "a".repeat(2000),
        "\"".repeat(64),
    ];
    // Everything above goes through both assertions.
    let end_to_end = inputs.len();

    // Every string of length 1..=3 over the alphabet, walked as a mixed-radix
    // counter — 22 + 484 + 10,648 cases, exhaustive rather than sampled.
    let n = alphabet.len();
    for len in 1..=3usize {
        let total = n.pow(len as u32);
        for mut code in 0..total {
            let mut s = String::new();
            for _ in 0..len {
                s.push(alphabet[code % n]);
                code /= n;
            }
            inputs.push(s);
        }
    }

    for (i, raw) in inputs.iter().enumerate() {
        let expr = build_fts_query(raw);
        if !expr.is_empty() {
            search::search(&conn, &expr, 1).unwrap_or_else(|e| {
                panic!("{raw:?} built the invalid MATCH expression {expr:?}: {e}")
            });
        }

        if i >= end_to_end && raw.chars().count() > 2 {
            continue;
        }
        // Straight through the query struct rather than through a URL, so a
        // byte percent-encoding would refuse is still exercised.
        let q = SessionQuery {
            search: raw.clone(),
            ..Default::default()
        };
        list_page(&conn, &settings, &q)
            .unwrap_or_else(|e| panic!("list_page errored on {raw:?} (fts {expr:?}): {e}"));
        facets(&conn, &settings, &q)
            .unwrap_or_else(|e| panic!("facets errored on {raw:?} (fts {expr:?}): {e}"));
    }
}

/// The `Filter` is shared, so the toolbar's counters and the rows below them
/// are the same predicate — asserted rather than assumed, because a search that
/// composed into only one of the two would still look right on either half
/// alone.
#[test]
fn facets_agree_with_the_rows_a_search_returns() {
    let conn = corpus();

    for q in [
        "q=auth",
        "q=handler",
        "q=nothing-at-all",
        "q=%22auth+bug%22",
    ] {
        let page = list_page(&conn, &DataSettings::default(), &query(q)).expect("page");
        let f = facets(&conn, &DataSettings::default(), &query(q)).expect("facets");
        assert_eq!(
            f.total,
            page.items.len() as i64,
            "{q}: facets counted {} against {} rows",
            f.total,
            page.items.len()
        );
    }
}

/// A search is one more `Filter` clause, so everything else composes untouched.
/// The project filter, the config-dir scope and a hidden project each narrow a
/// search the same way they narrow an unfiltered list.
#[test]
fn a_search_composes_with_the_other_filters() {
    let conn = migrated();
    session(&conn, "in-alpha", "/home/u/alpha", "", "");
    index(&conn, "in-alpha", "/home/u/alpha", "the auth bug");
    session(&conn, "in-beta", "/home/u/beta", "", "");
    index(&conn, "in-beta", "/home/u/beta", "the auth bug");

    let mut both = found(&conn, "q=auth+bug");
    both.sort();
    assert_eq!(both, vec!["in-alpha", "in-beta"]);

    assert_eq!(
        found(&conn, "q=auth+bug&project=%2Fhome%2Fu%2Falpha"),
        vec!["in-alpha"],
        "the project filter still applies under a search"
    );

    let hidden = DataSettings {
        hidden_projects: vec!["/home/u/beta".to_string()],
        ..Default::default()
    };
    let page = list_page(&conn, &hidden, &query("q=auth+bug")).expect("page");
    assert_eq!(page.items.len(), 1, "a hidden project stays hidden");
    assert_eq!(page.items[0].session_id, "in-alpha");
}

/// Keyset pagination pages a search the way it pages anything else: every
/// matching session appears exactly once across the pages, and none twice.
///
/// Worth its own test because the search clause is the first one whose argument
/// list is longer than one — a cursor bound in the wrong position would page a
/// plausible-looking subset.
#[test]
fn a_search_pages_through_its_whole_result_set() {
    let conn = migrated();
    for i in 0..7 {
        let id = format!("s{i}");
        session(&conn, &id, "/p", "", "");
        index(&conn, &id, "/p", "the auth bug appears here");
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor = String::new();
    for _ in 0..10 {
        let q = query(&format!("q=auth+bug&limit=2&cursor={cursor}"));
        let page = list_page(&conn, &DataSettings::default(), &q).expect("page");
        seen.extend(page.items.into_iter().map(|s| s.session_id));
        cursor = page.next_cursor.clone();
        if !page.has_more {
            break;
        }
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 7, "every match paged exactly once: {seen:?}");
}

/// A session id living under two project paths is two rows (#362 family), and
/// the index keys on the pair — so a search must return the row whose project
/// was indexed and only that one.
#[test]
fn the_search_clause_keys_on_the_pair_not_the_session_id() {
    let conn = migrated();
    session(&conn, "shared", "/home/u/alpha", "", "");
    session(&conn, "shared", "/home/u/beta", "", "");
    index(&conn, "shared", "/home/u/alpha", "the quasarflux incident");
    index(&conn, "shared", "/home/u/beta", "something else entirely");

    let page = list_page(&conn, &DataSettings::default(), &query("q=quasarflux")).expect("page");
    let paths: Vec<String> = page.items.into_iter().map(|s| s.project_path).collect();
    assert_eq!(
        paths,
        vec!["/home/u/alpha".to_string()],
        "the other project's row shares the id but not the content"
    );
}

/// The availability probe has to reject a bad expression against an **empty**
/// index, not just a populated one.
///
/// `usable_fts_query` learns that an expression is invalid by stepping the
/// statement, and it would be sound to imagine SQLite short-circuiting a query
/// over a table with no rows without ever asking FTS5 to parse the MATCH
/// argument. If it did, the probe would accept anything on a fresh install —
/// which is exactly the machine where the index is empty — and the *real* query
/// would then error, turning the fallback this issue is built on into a 500 for
/// the one user it most needs to protect. It does not; pinned here rather than
/// assumed, because it is a property of SQLite rather than of this code.
#[test]
fn the_probe_still_rejects_a_bad_expression_against_an_empty_index() {
    let conn = migrated();
    let mut stmt = conn
        .prepare("SELECT 1 FROM session_search WHERE session_search MATCH ?1 LIMIT 1")
        .expect("prepare");

    assert!(
        stmt.exists(params!["\"unterminated"]).is_err(),
        "an empty index must still parse the MATCH argument"
    );
    assert!(
        !stmt
            .exists(params!["\"auth\""])
            .expect("a valid expression answers"),
        "and a valid one is simply a miss"
    );
}

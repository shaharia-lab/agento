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
//! Ranking arrived with #437 and lives at the bottom of this file: the
//! `relevance` sort, its keyset cursor and the `match_snippet` field. The score
//! itself is still `native::search`'s to pin; what these assert is what the list
//! does with it.

use rusqlite::{params, Connection};

use super::page::{facets, list_page};
use super::query::{
    build_fts_query, Cursor, SessionQuery, Sort, ERR_CURSOR_MISMATCH, RELEVANCE_UNRANKED,
};
use crate::native::migrate;
use crate::native::search::{self, SearchDoc, SNIPPET_MARK_END, SNIPPET_MARK_START};
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

// ---------------------------------------------------------------------------
// #437 — the match on the wire: `match_snippet`, and the `relevance` sort.
// ---------------------------------------------------------------------------

/// Index one session with text in a chosen column, so a fixture can put the
/// same word under different bm25 weights (title 8.0 … tool_text 0.5).
fn index_doc(conn: &Connection, session_id: &str, project_path: &str, doc: SearchDoc) {
    search::replace(
        conn,
        &SearchDoc {
            session_id: session_id.to_string(),
            project_path: project_path.to_string(),
            ..doc
        },
    )
    .expect("index");
}

/// One session per column, all containing the same word — so the only thing
/// separating them is the weight of the column it landed in.
fn weighted_corpus() -> Connection {
    let conn = migrated();
    for (id, doc) in [
        (
            "in-title",
            SearchDoc {
                title: "quasarflux".into(),
                ..Default::default()
            },
        ),
        (
            "in-user",
            SearchDoc {
                user_text: "quasarflux".into(),
                ..Default::default()
            },
        ),
        (
            "in-tool",
            SearchDoc {
                tool_text: "quasarflux".into(),
                ..Default::default()
            },
        ),
    ] {
        session(&conn, id, "/p", "", "");
        index_doc(&conn, id, "/p", doc);
    }
    conn
}

/// Typing a search asks for the best match first, and picking a sort keeps it.
///
/// The order is the whole assertion, so the fixture is built with the word in
/// three differently-weighted columns: title (8.0) beats user text (4.0) beats
/// tool text (0.5). Reverse the sign in `SQL_RELEVANCE` and this comes back
/// exactly inverted — every row still a genuine hit, which is why the ordering
/// has to be asserted rather than the membership.
#[test]
fn a_search_with_no_sort_is_ranked_best_first_and_an_explicit_sort_wins() {
    let conn = weighted_corpus();

    assert_eq!(
        found(&conn, "q=quasarflux"),
        vec!["in-title", "in-user", "in-tool"],
        "no explicit sort under a search is relevance"
    );
    assert_eq!(
        found(&conn, "q=quasarflux&sort=relevance"),
        vec!["in-title", "in-user", "in-tool"],
        "asking for it explicitly is the same order"
    );

    // `recent` is a total tie here — every fixture row shares one
    // `last_activity` — so the tiebreak decides, and it is the row key
    // descending. That is a different order from the ranking, which is the
    // point: an explicit sort is not overridden by the search term.
    assert_eq!(
        found(&conn, "q=quasarflux&sort=recent"),
        vec!["in-user", "in-tool", "in-title"],
        "an explicit sort=recent is respected under a search"
    );
}

/// `relevance` with nothing to rank is the unknown-sort fallback, not an error
/// and not an ordering over a `MATCH` that was never run.
#[test]
fn relevance_without_a_search_term_falls_back_to_recent() {
    for raw in ["sort=relevance", "sort=relevance&q=", "sort=relevance&q=+"] {
        assert_eq!(
            SessionQuery::parse(raw).expect("parse").sort,
            Sort::Recent,
            "{raw}"
        );
    }
    // ...and with a term it is honoured, while no `sort` at all defaults to it.
    assert_eq!(
        SessionQuery::parse("sort=relevance&q=auth")
            .expect("parse")
            .sort,
        Sort::Relevance
    );
    assert_eq!(
        SessionQuery::parse("q=auth").expect("parse").sort,
        Sort::Relevance
    );
    // No search term and no sort is still the list's own default.
    assert_eq!(SessionQuery::parse("").expect("parse").sort, Sort::Recent);
}

/// The snippet is the answer to "why did this row match", so it exists exactly
/// where there is an index hit to point at — and carries the markers #438 splits
/// on rather than markup.
#[test]
fn a_content_match_carries_a_snippet_and_a_metadata_match_does_not() {
    let conn = corpus();
    let page = list_page(&conn, &DataSettings::default(), &query("q=auth")).expect("page");

    let snippets: std::collections::BTreeMap<String, String> = page
        .items
        .iter()
        .map(|s| (s.session_id.clone(), s.match_snippet.clone()))
        .collect();

    let hit = &snippets["content-only"];
    assert!(
        hit.contains(&format!("{SNIPPET_MARK_START}auth{SNIPPET_MARK_END}")),
        "the matched term is wrapped in the sentinels: {hit:?}"
    );
    assert!(
        hit.contains("login handler"),
        "and the surrounding text is the session's own: {hit:?}"
    );
    assert!(
        !hit.contains('<') && !hit.contains("[MATCH]"),
        "never markup: {hit:?}"
    );

    // `metadata-only` matched on its preview and `unindexed` on its title —
    // neither is an index hit, so neither has anything to snippet.
    assert_eq!(snippets["metadata-only"], "");
    assert_eq!(snippets["unindexed"], "");
}

/// A row with no search carries no snippet at all, and the field leaves the
/// wire — which is what keeps every frozen golden byte-identical.
#[test]
fn a_response_that_is_not_a_search_carries_no_match_snippet_key() {
    let conn = corpus();
    for q in ["", "sort=cost", "project=%2Fhome%2Fu%2Falpha"] {
        let page = list_page(&conn, &DataSettings::default(), &query(q)).expect("page");
        let json = String::from_utf8(crate::native::gojson::to_vec(&page).expect("encode"))
            .expect("utf-8");
        assert!(
            !json.contains("match_snippet"),
            "{q:?} must not introduce the key: {json}"
        );
    }
}

/// A session id under two project paths is two rows (#362 family), and only one
/// of them holds the matched text — so the snippet must follow the pair.
///
/// Keyed on the session id alone, the other project's row would carry a
/// highlight quoting text it does not contain, which reads as a correct answer.
#[test]
fn a_snippet_belongs_to_the_pair_not_to_the_session_id() {
    let conn = migrated();
    session(&conn, "shared", "/home/u/alpha", "", "");
    session(&conn, "shared", "/home/u/beta", "", "");
    index(&conn, "shared", "/home/u/alpha", "the quasarflux incident");
    // The other project matches through its *path*, so it is on the page with
    // no index hit of its own.
    index(&conn, "shared", "/home/u/beta", "something else entirely");

    let page = list_page(&conn, &DataSettings::default(), &query("q=quasarflux")).expect("page");
    let by_path: std::collections::BTreeMap<String, String> = page
        .items
        .iter()
        .map(|s| (s.project_path.clone(), s.match_snippet.clone()))
        .collect();

    assert!(by_path["/home/u/alpha"].contains("incident"), "{by_path:?}");
    assert!(
        !by_path.contains_key("/home/u/beta"),
        "the other project's row does not match at all here: {by_path:?}"
    );
}

/// Relevance pages the whole result set exactly once **under deliberate ties**.
///
/// Every session holds identical text, so every bm25 score is identical and the
/// sort key alone cannot order them: the `(session_id, project_path)` tiebreak
/// is the only thing making the order total. Without it a tied page repeats one
/// row and drops another, and both pages still look plausible.
#[test]
fn relevance_pages_through_tied_ranks_exactly_once() {
    let conn = migrated();
    // Same id under two paths, twice over — so the ties run to the *pair*, not
    // just to the rank, which is the #364 rule this shares.
    for i in 0..4 {
        for project in ["/home/u/alpha", "/home/u/beta"] {
            let id = format!("s{i}");
            session(&conn, &id, project, "", "");
            index(&conn, &id, project, "the quasarflux incident recurred");
        }
    }

    let mut seen: Vec<(String, String)> = Vec::new();
    let mut cursor = String::new();
    for _ in 0..20 {
        let q = query(&format!("q=quasarflux&limit=3&cursor={cursor}"));
        assert_eq!(q.sort, Sort::Relevance);
        let page = list_page(&conn, &DataSettings::default(), &q).expect("page");
        seen.extend(
            page.items
                .into_iter()
                .map(|s| (s.session_id, s.project_path)),
        );
        cursor = page.next_cursor.clone();
        if !page.has_more {
            break;
        }
    }

    let total = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 8, "every match paged exactly once: {seen:?}");
    assert_eq!(total, 8, "and none of them twice");
}

/// The cursor carries the sort, so a relevance position cannot be replayed
/// against another ordering — the existing 400, unchanged.
#[test]
fn a_relevance_cursor_round_trips_and_another_sort_refuses_it() {
    let conn = migrated();
    for i in 0..4 {
        let id = format!("s{i}");
        session(&conn, &id, "/p", "", "");
        index(&conn, &id, "/p", "the quasarflux incident");
    }

    let first = list_page(
        &conn,
        &DataSettings::default(),
        &query("q=quasarflux&limit=2"),
    )
    .expect("page");
    assert!(first.has_more);

    let minted = Cursor::decode(&first.next_cursor, Sort::Relevance)
        .expect("decode")
        .expect("some");
    assert_eq!(minted.sort, "relevance");
    // The value is the negated bm25 — a decimal, in the same `v` field every
    // non-time sort already pages on rather than a second field beside it.
    assert!(
        minted.value.parse::<f64>().is_ok(),
        "value {:?} is a decimal",
        minted.value
    );

    assert_eq!(
        Cursor::decode(&first.next_cursor, Sort::Recent).expect_err("mismatch"),
        ERR_CURSOR_MISMATCH
    );
    // ...and through the handler's own path, which is where the 400 comes from.
    let replayed = query(&format!(
        "q=quasarflux&sort=cost&cursor={}",
        first.next_cursor
    ));
    assert_eq!(
        list_page(&conn, &DataSettings::default(), &replayed).expect_err("mismatch"),
        ERR_CURSOR_MISMATCH
    );
}

/// A cursor minted before `p` existed decodes with it empty and pages as it
/// used to — the Go missing-field rule, asserted for the new sort too because a
/// relevance cursor is the one a bookmark is most likely to be carrying.
#[test]
fn a_relevance_cursor_without_the_project_field_still_decodes() {
    let raw = super::query::base64_url_nopad(br#"{"s":"relevance","v":"0.0001","id":"s1"}"#);
    let c = Cursor::decode(&raw, Sort::Relevance)
        .expect("decode")
        .expect("some");
    assert_eq!(c.id, "s1");
    assert_eq!(c.project, "");
}

/// A row that matched only through the six metadata columns has no bm25 score,
/// and must still page.
///
/// It is the `COALESCE` in `SQL_RELEVANCE` that makes this work. Left as SQL
/// NULL those rows would sort last correctly on the *first* page and then
/// vanish, because `NULL < ?` is NULL and the keyset predicate drops them —
/// which is the silent half, visible only from page two.
///
/// Measured on the revert rather than argued: dropping the `COALESCE` fails
/// this test *earlier* than that, at `Invalid column type Null at index 54`,
/// because the sort key is scanned as an `f64`. That is deliberate and worth
/// keeping — the alternative, reading it as `Option<f64>` and defaulting in
/// Rust, would fix the cursor's value and leave the `ORDER BY` and the keyset
/// predicate still comparing against NULL, which is exactly the silent failure.
/// The sentinel belongs in the SQL, where all three read it.
#[test]
fn a_metadata_only_match_still_pages_under_relevance() {
    let conn = migrated();
    // Two content matches, so the first page is entirely ranked rows.
    for id in ["ranked-a", "ranked-b"] {
        session(&conn, id, "/p", "", "");
        index(&conn, id, "/p", "the quasarflux incident");
    }
    // Two that match only on their title, with no index row at all.
    for id in ["plain-a", "plain-b"] {
        session(&conn, id, "/p", "", "quasarflux in the title");
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor = String::new();
    for _ in 0..10 {
        let page = list_page(
            &conn,
            &DataSettings::default(),
            &query(&format!("q=quasarflux&limit=2&cursor={cursor}")),
        )
        .expect("page");
        seen.extend(page.items.into_iter().map(|s| s.session_id));
        cursor = page.next_cursor.clone();
        if !page.has_more {
            break;
        }
    }
    seen.sort();
    assert_eq!(
        seen,
        vec!["plain-a", "plain-b", "ranked-a", "ranked-b"],
        "the unranked rows survive past the first page"
    );

    // And they sort after every ranked row, which is what the sentinel is for.
    // The two ranked rows hold identical text and so tie on bm25, as do the two
    // unranked ones on the sentinel; within each pair the row-key tiebreak
    // decides, descending.
    let all = found(&conn, "q=quasarflux&limit=10");
    assert_eq!(&all[..2], ["ranked-b", "ranked-a"], "{all:?}");
    assert_eq!(&all[2..], ["plain-b", "plain-a"], "{all:?}");
}

/// The sentinel has to be below every score bm25 can produce, or an unranked
/// row would sort *above* a real match.
///
/// `bm25()` is non-positive, so its negation is non-negative and -1 is strictly
/// below all of it. Asserted against real scores rather than argued from the
/// documentation.
#[test]
fn the_unranked_sentinel_is_below_every_real_score() {
    let conn = weighted_corpus();
    let hits = search::search(&conn, r#""quasarflux""#, 10).expect("search");
    assert_eq!(hits.len(), 3);
    for hit in &hits {
        assert!(hit.rank <= 0.0, "bm25 is non-positive: {hit:?}");
        assert!(
            -hit.rank > RELEVANCE_UNRANKED,
            "the negated score outranks the sentinel: {hit:?}"
        );
    }
}

/// With no `session_search` table there is no rank to sort by, and relevance
/// must still answer — as an unranked total order over the row key rather than
/// as an error.
#[test]
fn relevance_degrades_to_an_unranked_order_with_no_index_table() {
    let conn = corpus();
    conn.execute("DROP TABLE session_search", [])
        .expect("drop the index");

    let mut seen: Vec<String> = Vec::new();
    let mut cursor = String::new();
    for _ in 0..10 {
        let page = list_page(
            &conn,
            &DataSettings::default(),
            &query(&format!("q=auth&limit=1&cursor={cursor}")),
        )
        .expect("page");
        seen.extend(page.items.into_iter().map(|s| s.session_id));
        cursor = page.next_cursor.clone();
        if !page.has_more {
            break;
        }
    }
    seen.sort();
    assert_eq!(
        seen,
        vec!["metadata-only", "unindexed"],
        "the metadata half still pages, one row at a time"
    );
}

/// The toolbar's counters and the rows below them are one predicate, and the
/// sort must not move either — `facets` never joins the index, so a search that
/// composed the ranked join into the *filter* rather than beside it would show
/// up here as a disagreement.
#[test]
fn facets_agree_with_the_rows_under_relevance() {
    let conn = corpus();
    for q in [
        "q=auth",
        "q=handler",
        "q=auth&sort=relevance",
        "q=nothing-at-all",
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

/// The fixture the search golden is built from.
///
/// Three sessions holding the same word in three differently-weighted columns,
/// so **no two rows tie on the sort key** — the golden can record only one
/// ordering, so the fixture must only be able to produce one. Their ids and
/// project paths differ too, so the row-key tiebreak is never what decides.
///
/// The page is deliberately larger than the result set: an exhausted page mints
/// no cursor, which keeps the raw bm25 doubles out of the recorded bytes. What
/// this golden pins is the *shape* — where `match_snippet` sits, that it is
/// omitted where there is no index hit, and the ranked order — not SQLite's
/// scoring, which `native::search`'s own tests own.
fn golden_corpus() -> Connection {
    let conn = migrated();
    for (id, project, doc) in [
        (
            "aaa-title",
            "/work/alpha",
            SearchDoc {
                title: "the quasarflux rollout".into(),
                ..Default::default()
            },
        ),
        (
            "bbb-user",
            "/work/beta",
            SearchDoc {
                user_text: "can you explain the quasarflux failure in staging".into(),
                ..Default::default()
            },
        ),
        (
            "ccc-tool",
            "/work/gamma",
            SearchDoc {
                tool_text: "warning: quasarflux limit exceeded on shard 3".into(),
                ..Default::default()
            },
        ),
    ] {
        session(&conn, id, project, "", "");
        index_doc(&conn, id, project, doc);
    }
    // Matches through its project path alone, so it is on the page with no
    // snippet — which is what pins the omit-when-empty half of the field.
    session(&conn, "ddd-path", "/work/quasarflux-notes", "", "");
    conn
}

/// The whole search response, byte for byte.
///
/// This is the one golden in `parity/` that has no Go ancestor —
/// `match_snippet` and `relevance` are Agento's own, so it is hand-written
/// beside the code the way `desktop_routes.json` and
/// `session_metric_vectors.json` are. **A change here is a change to the
/// contract**: edit it deliberately, never by re-recording until the test
/// passes.
///
/// What it pins that no unit test can: the field's *position* — last, after
/// `unpriced_tokens` — the sentinel spellings on the wire
/// (`\u0001` / `\u0002`, which is what a consumer splits on), the ranked order, and that
/// `ddd-path` carries **no `match_snippet` key at all** rather than an empty
/// string, which is the whole reason every frozen golden survived this change.
#[test]
fn the_search_response_matches_the_golden_bytes() {
    let conn = golden_corpus();
    let page = list_page(
        &conn,
        &DataSettings::default(),
        &query("q=quasarflux&limit=10"),
    )
    .expect("page");
    let got =
        String::from_utf8(crate::native::gojson::to_vec(&page).expect("encode")).expect("utf-8");

    let want = include_str!("../../../../parity/claude_sessions_search_golden.json");
    assert_eq!(got, want, "the search response drifted from its golden");
}

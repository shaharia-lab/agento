//! The `session_search` FTS5 index: the rows, and the one ranked read over them.
//!
//! Migration 33 creates a **content-storing** FTS5 table over four searchable
//! columns — `title`, `user_text`, `assistant_text`, `tool_text` — plus the two
//! UNINDEXED key columns. This module is the only thing that writes it and the
//! one place its ranking is spelled.
//!
//! # Everything here keys on `(session_id, project_path)`
//!
//! `claude_session_cache` is keyed on the pair, not on `session_id`: a session
//! id living under two project paths is two legitimate rows, which is what
//! copying a `~/.claude` to set up a second account produces. That has already
//! cost four bugs (#344, #362, #364, #408), two of them written *after* the
//! schema was re-keyed — so "does this key on `session_id` alone?" is a standing
//! question for every new reader and writer of the corpus, and this module is a
//! new one.
//!
//! Concretely: [`replace`] and [`delete`] both take the whole key, and a replace
//! for one pair leaves the other project's row for the same id untouched. That
//! is not incidental — it is pinned by
//! `a_replace_for_one_pair_leaves_the_other_projects_row`.
//!
//! # The store opens nothing, and owns no transaction
//!
//! Every writer here takes a `&Connection` the caller supplies. The indexer
//! (#435) writes the index row and the `session_insights` row in **one**
//! transaction, so the transaction has to belong to it; a store layer that
//! opened its own handle would make that impossible and would also sidestep the
//! `db::open_read_write` pragma rules. `rusqlite::Transaction` derefs to
//! `Connection`, so a caller inside a transaction passes `&tx`.
//!
//! # Two costs to know before #435 and #439
//!
//! **A delete scans.** FTS5 will not index the UNINDEXED key columns and does
//! not accept an index on them, so `DELETE … WHERE session_id = ? AND
//! project_path = ?` is a scan of the backing `%_content` table. That is
//! correct and it is cheap at one row; it is *not* free at corpus scale, where
//! [`replace`] runs once per changed session. Measuring it — and deciding
//! whether a rowid side table earns its keep — is #439's, and this note is the
//! handle for it.
//!
//! **bm25 is negative, and smaller is better.** SQLite's `bm25()` returns the
//! negated score specifically so that a plain `ORDER BY rank` puts the best
//! match first. Sorting descending silently returns the *worst* matches, and
//! every row is still a genuine hit, so nothing about the answer looks wrong.
//!
//! # `rank` is also a magic FTS5 column, and that is a trap
//!
//! Every FTS5 table has a hidden `rank` column of its own, which is bm25 with
//! **all weights 1.0**. [`match_sql`] aliases the weighted expression to `rank`
//! because that is the name a caller wants to order by — and SQLite resolves a
//! bare `ORDER BY rank` against the output alias, so it is the weighted one that
//! sorts. Nothing about a query says which of the two won, and both answers are
//! made of genuine hits in a plausible order, so a regression here is invisible.
//! `the_returned_rank_is_the_weighted_bm25` compares the value against both
//! spellings rather than trusting the resolution rule.

pub mod normalize;

use rusqlite::{params, Connection};

/// The bm25 column weights, in the FTS table's column order.
///
/// A hit in the title outranks the same hit in a tool result by 16×. The
/// ordering is the point rather than the absolute values: a user searching for
/// text they remember means the thing the session is *about* first, what they
/// said second, what the model said third, and the contents of a tool result
/// last — people do remember error messages, which is why tool text is indexed
/// at all rather than dropped.
///
/// There are four weights for six columns, and that is deliberate: `bm25()`
/// defaults any unnamed trailing column to 1.0, and `session_id` /
/// `project_path` are UNINDEXED, so they can never contribute a match whatever
/// weight they carry.
pub const BM25_WEIGHTS: [f64; 4] = [8.0, 4.0, 2.0, 0.5];

/// The ranking expression, spelled once so #436 and #437 cannot drift from the
/// indexer or from each other.
///
/// Kept as a literal rather than built from [`BM25_WEIGHTS`] at runtime: this
/// goes into SQL, and a `format!` into a query string is the shape that later
/// grows a caller-supplied argument. `the_ranking_expression_matches_the_weights`
/// asserts the two agree.
pub const BM25_EXPR: &str = "bm25(session_search, 8.0, 4.0, 2.0, 0.5)";

/// One session's indexable text, as the indexer hands it over.
///
/// The four text fields are already normalized by the time they arrive (#434);
/// nothing here inspects or truncates them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchDoc {
    pub session_id: String,
    pub project_path: String,
    pub title: String,
    pub user_text: String,
    pub assistant_text: String,
    pub tool_text: String,
}

/// One ranked hit, which is the whole cache key plus its score.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub session_id: String,
    pub project_path: String,
    /// `bm25()`'s negated score: **more negative is a better match**.
    pub rank: f64,
}

/// The ranked-match subquery, with one bound parameter for the FTS query string.
///
/// Returns `session_id`, `project_path` and `rank`, so a caller composes it
/// either as a membership test or as a join that carries the score through to
/// an `ORDER BY`:
///
/// ```sql
/// -- membership only (#436)
/// (c.session_id, c.project_path) IN (
///     SELECT session_id, project_path FROM ( <match_sql()> )
/// )
///
/// -- ranked (#437)
/// JOIN ( <match_sql()> ) m
///   ON m.session_id = c.session_id AND m.project_path = c.project_path
/// ORDER BY m.rank
/// ```
///
/// **The parameter is bound, never interpolated.** A user's search text reaches
/// FTS5's own query grammar, where `-`, `OR`, `*`, `^` and `"` are operators; the
/// query layer neutralizes them by quoting each token as it builds the string
/// (#436). Binding is what keeps that a construction rule rather than a
/// sanitization one.
pub fn match_sql() -> &'static str {
    "SELECT session_id AS session_id,
            project_path AS project_path,
            bm25(session_search, 8.0, 4.0, 2.0, 0.5) AS rank
       FROM session_search
      WHERE session_search MATCH ?"
}

/// Insert this session's row, replacing whatever was indexed for the same pair.
///
/// Delete-then-insert rather than an upsert: an FTS5 table has no unique index
/// to conflict on, so `ON CONFLICT` has nothing to name. The two statements are
/// not atomic on their own — the caller's transaction is what makes them one.
pub fn replace(conn: &Connection, doc: &SearchDoc) -> Result<(), String> {
    delete(conn, &doc.session_id, &doc.project_path)?;
    conn.execute(
        "INSERT INTO session_search
             (title, user_text, assistant_text, tool_text, session_id, project_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            doc.title,
            doc.user_text,
            doc.assistant_text,
            doc.tool_text,
            doc.session_id,
            doc.project_path,
        ],
    )
    .map_err(|e| format!("indexing session {}: {e}", doc.session_id))?;
    Ok(())
}

/// Drop one session's row.
///
/// Keyed on the pair. A claim shift moves a transcript to a different path and
/// is an **update**, not a deletion (#245), so the caller decides what is gone;
/// this only removes what it is told to.
pub fn delete(conn: &Connection, session_id: &str, project_path: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM session_search WHERE session_id = ?1 AND project_path = ?2",
        params![session_id, project_path],
    )
    .map_err(|e| format!("removing session {session_id} from the search index: {e}"))?;
    Ok(())
}

/// Empty the index.
///
/// For a `search_index_version` bump, where every row was produced by a reader
/// this build no longer agrees with. `DELETE FROM` rather than `DROP`/`CREATE`
/// so the schema stays exactly what migration 33 built — a rebuilt virtual
/// table is a second place the DDL is spelled, and the two would drift.
pub fn delete_all(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM session_search", [])
        .map_err(|e| format!("clearing the search index: {e}"))?;
    Ok(())
}

/// Run one FTS query and return its hits, best first.
///
/// `query` is FTS5 query syntax, already built and quoted by the caller. The
/// direct read the ranking is pinned against, and the shortest path for a
/// caller that wants the index alone rather than the composed sessions query.
pub fn search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    // `ORDER BY rank` ascending, because bm25 is negated — see the module doc.
    let sql = format!("{} ORDER BY rank LIMIT ?", match_sql());
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("preparing search: {e}"))?;
    let rows = stmt
        .query_map(params![query, limit as i64], |row| {
            Ok(Hit {
                session_id: row.get(0)?,
                project_path: row.get(1)?,
                rank: row.get(2)?,
            })
        })
        .map_err(|e| format!("querying search: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("reading search results: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::migrate;

    /// A database built the way the app builds one: create, then apply every
    /// migration. That is the fresh-install path, and it is also the only proof
    /// that migration 33's SQL is valid SQLite rather than merely valid JSON.
    fn migrated() -> (tempfile::NamedTempFile, Connection) {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        migrate::apply(&mut conn).expect("apply");
        (file, conn)
    }

    fn doc(session_id: &str, project_path: &str) -> SearchDoc {
        SearchDoc {
            session_id: session_id.to_string(),
            project_path: project_path.to_string(),
            ..Default::default()
        }
    }

    fn indexed_pairs(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT session_id, project_path FROM session_search ORDER BY 1, 2")
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query");
        rows.collect::<Result<Vec<_>, _>>().expect("collect")
    }

    /// The table has to exist after a plain migrate, and it has to be the FTS5
    /// one — a `CREATE VIRTUAL TABLE` that silently became something else would
    /// still answer `SELECT`.
    #[test]
    fn the_migration_builds_an_fts5_table() {
        let (_file, conn) = migrated();

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'session_search'",
                [],
                |row| row.get(0),
            )
            .expect("session_search must exist");
        assert!(sql.contains("USING fts5"), "not an FTS5 table: {sql}");
        assert!(
            sql.contains("remove_diacritics 2"),
            "the tokenizer is part of the contract: {sql}"
        );
        assert!(
            sql.contains("session_id UNINDEXED") && sql.contains("project_path UNINDEXED"),
            "the key columns must not be searchable: {sql}"
        );
    }

    /// `search_index_version` reads back 0 on an existing database, which is the
    /// acceptance criterion for the upgrade path: 0 is "nothing indexed", and a
    /// database that predates this migration and a fresh install are the same
    /// case on purpose.
    #[test]
    fn search_index_version_starts_at_zero() {
        let (_file, conn) = migrated();

        conn.execute(
            "INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, '')",
            [],
        )
        .expect("seed the singleton metadata row");

        let version: i64 = conn
            .query_row(
                "SELECT search_index_version FROM claude_cache_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("search_index_version must exist");
        assert_eq!(version, 0);
    }

    /// The upgrade path with **data** in it.
    ///
    /// `migrate::a_database_at_any_earlier_version_upgrades_to_this_one` already
    /// walks every intermediate version, but it applies DDL to an empty database
    /// and asserts on versions — so it cannot see an `ALTER TABLE` that runs
    /// cleanly and loses or rewrites a row. This one seeds the singleton
    /// `claude_cache_metadata` row before the last migration runs, and asserts
    /// both that the row survives untouched and that the added column defaults
    /// rather than needing a backfill.
    ///
    /// The boundary is derived from `expected_version()`, not written as a
    /// literal, so appending migration 34 does not silently turn this into a
    /// fresh-install test that no longer exercises an upgrade at all.
    #[test]
    fn the_migration_applies_to_a_populated_database() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        let last = migrate::expected_version();

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version    INTEGER PRIMARY KEY,
                applied_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .expect("tracking table");
        for m in migrate::migrations().iter().filter(|m| m.version < last) {
            conn.execute_batch(&m.sql)
                .unwrap_or_else(|e| panic!("migration {}: {e}", m.version));
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, '')",
                params![m.version],
            )
            .expect("record");
        }
        conn.execute(
            "INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, 'before')",
            [],
        )
        .expect("seed metadata");
        assert_eq!(migrate::current_version(&conn).expect("version"), last - 1);

        migrate::apply(&mut conn).expect("upgrade must apply");

        assert_eq!(migrate::current_version(&conn).expect("version"), last);
        migrate::verify(&conn).expect("verify");
        let (kept, version): (String, i64) = conn
            .query_row(
                "SELECT last_scanned_at, search_index_version
                   FROM claude_cache_metadata WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the existing row survives");
        assert_eq!(kept, "before", "the migration must not rewrite the row");
        assert_eq!(version, 0, "an upgraded database has nothing indexed");
    }

    /// The rule this module exists to hold: the key is the **pair**. Two
    /// sessions sharing an id under different project paths are two rows, and
    /// re-indexing one must leave the other exactly as it was.
    ///
    /// Keyed on the id alone, `replace` would delete both and the second
    /// project's text would vanish from the index with nothing to report it —
    /// the same shape as #362 and #408.
    #[test]
    fn a_replace_for_one_pair_leaves_the_other_projects_row() {
        let (_file, conn) = migrated();

        let mut first = doc("shared-id", "/home/u/alpha");
        first.title = "alpha original".into();
        let mut second = doc("shared-id", "/home/u/beta");
        second.title = "beta original".into();
        replace(&conn, &first).expect("index alpha");
        replace(&conn, &second).expect("index beta");
        assert_eq!(
            indexed_pairs(&conn),
            vec![
                ("shared-id".to_string(), "/home/u/alpha".to_string()),
                ("shared-id".to_string(), "/home/u/beta".to_string()),
            ]
        );

        let mut rewritten = doc("shared-id", "/home/u/alpha");
        rewritten.title = "alpha rewritten".into();
        replace(&conn, &rewritten).expect("re-index alpha");

        // Still two rows, one per pair — not one, and not three.
        assert_eq!(indexed_pairs(&conn).len(), 2);
        let titles: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT project_path, title FROM session_search ORDER BY 1")
                .expect("prepare");
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("query");
            rows.collect::<Result<Vec<_>, _>>().expect("collect")
        };
        assert_eq!(
            titles,
            vec![
                ("/home/u/alpha".to_string(), "alpha rewritten".to_string()),
                ("/home/u/beta".to_string(), "beta original".to_string()),
            ],
            "beta's row must be untouched"
        );
    }

    /// `delete` takes the whole key too, for the same reason.
    #[test]
    fn a_delete_for_one_pair_leaves_the_other_projects_row() {
        let (_file, conn) = migrated();

        replace(&conn, &doc("shared-id", "/home/u/alpha")).expect("alpha");
        replace(&conn, &doc("shared-id", "/home/u/beta")).expect("beta");

        delete(&conn, "shared-id", "/home/u/alpha").expect("delete alpha");

        assert_eq!(
            indexed_pairs(&conn),
            vec![("shared-id".to_string(), "/home/u/beta".to_string())]
        );
    }

    /// Deleting a pair that was never indexed is a no-op, not an error: the
    /// reconcile runs over sessions whose index row may or may not exist, and a
    /// version rebuild deletes before it inserts.
    #[test]
    fn deleting_an_unindexed_pair_is_a_no_op() {
        let (_file, conn) = migrated();

        replace(&conn, &doc("kept", "/home/u/alpha")).expect("index");
        delete(&conn, "never-indexed", "/home/u/alpha").expect("must not error");
        delete(&conn, "kept", "/home/u/other").expect("right id, wrong path");

        assert_eq!(indexed_pairs(&conn).len(), 1);
    }

    #[test]
    fn delete_all_empties_the_index() {
        let (_file, conn) = migrated();

        replace(&conn, &doc("a", "/p/one")).expect("a");
        replace(&conn, &doc("b", "/p/two")).expect("b");
        delete_all(&conn).expect("clear");

        assert!(indexed_pairs(&conn).is_empty());
        // And it is still usable afterwards — a rebuild inserts straight after.
        replace(&conn, &doc("c", "/p/three")).expect("reindex after clear");
        assert_eq!(indexed_pairs(&conn).len(), 1);
    }

    /// The ranking acceptance criterion: two documents differing **only** in
    /// which column holds the term, and the title hit must come first.
    ///
    /// The documents are otherwise identical so nothing but the weights can
    /// explain the order — bm25 also scores on field length and term rarity, so
    /// two differently-shaped documents would prove nothing about the weights.
    #[test]
    fn a_title_hit_outranks_the_same_hit_in_tool_text() {
        let (_file, conn) = migrated();

        let term = "quasarflux";
        replace(
            &conn,
            &SearchDoc {
                session_id: "in-tool".into(),
                project_path: "/p".into(),
                tool_text: term.into(),
                ..Default::default()
            },
        )
        .expect("index the tool-text doc");
        replace(
            &conn,
            &SearchDoc {
                session_id: "in-title".into(),
                project_path: "/p".into(),
                title: term.into(),
                ..Default::default()
            },
        )
        .expect("index the title doc");

        let hits = search(&conn, term, 10).expect("search");
        assert_eq!(hits.len(), 2, "both documents contain the term");
        assert_eq!(
            hits[0].session_id, "in-title",
            "the title hit must rank first, got {hits:?}"
        );
        assert!(
            hits[0].rank < hits[1].rank,
            "bm25 is negated, so the better match is the smaller number: {hits:?}"
        );
    }

    /// The full weight ordering, not just its two ends — an implementation that
    /// weighted only the title would pass the test above.
    #[test]
    fn the_four_columns_rank_in_weight_order() {
        let (_file, conn) = migrated();

        let term = "quasarflux";
        for (id, build) in [
            ("tool", 3usize),
            ("assistant", 2),
            ("user", 1),
            ("title", 0),
        ] {
            let mut d = doc(id, "/p");
            match build {
                0 => d.title = term.into(),
                1 => d.user_text = term.into(),
                2 => d.assistant_text = term.into(),
                _ => d.tool_text = term.into(),
            }
            replace(&conn, &d).expect("index");
        }

        let order: Vec<String> = search(&conn, term, 10)
            .expect("search")
            .into_iter()
            .map(|h| h.session_id)
            .collect();
        assert_eq!(order, vec!["title", "user", "assistant", "tool"]);
    }

    /// The key columns are UNINDEXED, so a term that appears only in a session
    /// id or a project path is **not** a content hit. The query layer keeps its
    /// six-column LIKE clause precisely because this is true (#436); if these
    /// ever became searchable, every session under `/home/u/agento` would match
    /// a search for "agento".
    #[test]
    fn the_key_columns_are_not_searchable() {
        let (_file, conn) = migrated();

        replace(
            &conn,
            &SearchDoc {
                session_id: "quasarflux".into(),
                project_path: "/home/u/zircondrift".into(),
                title: "unrelated".into(),
                ..Default::default()
            },
        )
        .expect("index");

        assert!(search(&conn, "quasarflux", 10).expect("search").is_empty());
        assert!(search(&conn, "zircondrift", 10).expect("search").is_empty());
        assert_eq!(search(&conn, "unrelated", 10).expect("search").len(), 1);
    }

    /// `remove_diacritics 2` is in the migration for a reason a user notices:
    /// typing `cafe` has to find `café`.
    #[test]
    fn the_tokenizer_folds_diacritics() {
        let (_file, conn) = migrated();

        let mut d = doc("s", "/p");
        d.user_text = "the café outage".into();
        replace(&conn, &d).expect("index");

        assert_eq!(search(&conn, "cafe", 10).expect("search").len(), 1);
        assert_eq!(search(&conn, "café", 10).expect("search").len(), 1);
    }

    /// `ORDER BY rank` must sort by the **weighted** expression, not by FTS5's
    /// own hidden `rank` column — which is bm25 with all weights 1.0 and would
    /// still return every genuine hit, in a plausible order, with nothing to say
    /// the weights had stopped applying.
    ///
    /// Asserted against both spellings computed directly, so it fails whichever
    /// way a future edit breaks the alias resolution.
    #[test]
    fn the_returned_rank_is_the_weighted_bm25() {
        let (_file, conn) = migrated();

        let term = "quasarflux";
        let mut d = doc("s", "/p");
        d.title = format!("{term} in the title");
        d.tool_text = "unrelated filler text making the documents differ".into();
        replace(&conn, &d).expect("index");

        let (weighted, unweighted): (f64, f64) = conn
            .query_row(
                "SELECT bm25(session_search, 8.0, 4.0, 2.0, 0.5),
                        bm25(session_search, 1.0, 1.0, 1.0, 1.0)
                   FROM session_search
                  WHERE session_search MATCH ?1",
                params![term],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("both spellings");
        assert_ne!(
            weighted, unweighted,
            "the fixture must be able to tell the two apart"
        );

        let hits = search(&conn, term, 10).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].rank, weighted,
            "search() must return the weighted score, not FTS5's default rank"
        );
    }

    /// The one place the weights are spelled twice. `BM25_EXPR` goes into SQL
    /// and `BM25_WEIGHTS` is what a caller reads; a drift between them would
    /// make the documented ranking and the executed one different things.
    #[test]
    fn the_ranking_expression_matches_the_weights() {
        let spelled = BM25_WEIGHTS
            .iter()
            .map(|w| format!("{w:.1}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(BM25_EXPR, format!("bm25(session_search, {spelled})"));
        assert!(match_sql().contains(BM25_EXPR));
    }

    /// A search string is **bound**, never interpolated — so FTS5's own
    /// operators in user text cannot change which rows come back. A bare
    /// `"` is a syntax error to FTS5, which surfaces as `Err` rather than as a
    /// wrong answer; the query layer's job (#436) is to quote it before it gets
    /// here, and its fallback depends on this being an error it can see.
    #[test]
    fn a_malformed_query_is_an_error_rather_than_a_wrong_answer() {
        let (_file, conn) = migrated();

        let mut d = doc("s", "/p");
        d.user_text = "authentication failed".into();
        replace(&conn, &d).expect("index");

        assert!(search(&conn, "\"unterminated", 10).is_err());
        // And a quoted phrase is matched literally rather than as syntax.
        assert_eq!(
            search(&conn, "\"authentication failed\"", 10)
                .expect("phrase")
                .len(),
            1
        );
    }

    /// A caller inside a transaction passes `&tx`, which is what the indexer
    /// does — the index row and the `session_insights` row are one commit.
    #[test]
    fn a_write_composes_with_the_callers_transaction() {
        let (_file, mut conn) = migrated();

        let tx = conn.transaction().expect("begin");
        replace(&tx, &doc("rolled-back", "/p")).expect("index inside the tx");
        drop(tx); // rusqlite rolls back on drop.
        assert!(indexed_pairs(&conn).is_empty());

        let tx = conn.transaction().expect("begin");
        replace(&tx, &doc("committed", "/p")).expect("index inside the tx");
        tx.commit().expect("commit");
        assert_eq!(indexed_pairs(&conn).len(), 1);
    }
}

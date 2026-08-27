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
//! # `session_search_key` is the reason a delete no longer scans (#446)
//!
//! FTS5 will not index the UNINDEXED key columns and does not accept an index on
//! them, so `DELETE … WHERE session_id = ? AND project_path = ?` is a **scan** of
//! the backing `%_content` table — `SCAN session_search VIRTUAL TABLE INDEX 0:`,
//! with a content-storing table materializing every row it walks. #439 measured
//! it at **44.57 ms median** against a 188.7 MB / 1,178-session index and found
//! it to be *the whole* of the incremental path's cost, against 11 ms for the
//! insert beside it.
//!
//! Migration 36 adds `session_search_key(session_id, project_path, rowid_ref)`,
//! so [`delete`] resolves the pair to a rowid in an ordinary B-tree and then
//! deletes on `rowid`, which is the one constraint besides `MATCH` that FTS5's
//! `xBestIndex` accepts. That is what makes a per-row rebuild affordable, and
//! per-row rebuilds are what let a skipped session be retried (#446).
//!
//! **Every writer here maintains it, inside the caller's transaction**, and the
//! table's PRIMARY KEY is the uniqueness FTS5 itself cannot offer — so a second
//! row for one pair is now a refusal rather than a duplicate that returns two
//! hits for ever. Two things follow, and both are deliberate:
//!
//! * **A missing key row is not an error.** A database indexed by a build older
//!   than migration 36 has none for rows the backfill could not claim, so
//!   [`delete`] falls back to the scan-shaped predicate rather than silently
//!   leaving a row behind.
//! * **[`delete_orphans`] is still keyed on the cache, not on this table.**
//!   Driving it from the side table would make an index row with no key entry
//!   unreachable for ever; keying both statements on "no cache row for this pair
//!   remains" makes them independently correct, and #439 measured that pass at
//!   35 ms over the whole index — once per scan, not once per session.
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

use rusqlite::{params, Connection, OptionalExtension};

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

/// What this build's indexer produces, compared against
/// **`session_insights.search_index_version`** — one value per row since #446,
/// not the single `claude_cache_metadata` stamp #435 introduced.
///
/// Bump it whenever the *stored text* changes — a different column routing, a
/// changed cap, a normalizer rule that keeps or drops different words. Every row
/// then reads as behind and the worker's sweep re-indexes it (see
/// [`crate::native::insights::worker`]).
///
/// **Per row is what makes a bump resumable and a skip recoverable.** The column
/// is written in the same transaction as the index row, so a session whose
/// transcript could not be read keeps its old value and is retried by every
/// later sweep, and a rebuild interrupted between batches costs the batches it
/// had left rather than the whole corpus. A bump also no longer means "the index
/// answers nothing until the rebuild finishes": each row is replaced in place,
/// so the un-rebuilt part of the corpus keeps answering with its previous text.
///
/// **It is deliberately not one of the scanner's staleness markers**, which is
/// the same separation `CURRENT_PROCESSOR_VERSION` has: rebuilding the index
/// re-reads transcripts *through the worker*, and must not make the scanner
/// re-read `claude_session_cache` — those rows are a function of the transcript
/// and the pricing catalog, neither of which a search-text change touches.
///
/// 0 is reserved for "nothing indexed": migration 36 defaults the column to it,
/// so an upgraded database and a fresh install are the same case, and both
/// trigger a full first index.
pub const SEARCH_INDEX_VERSION: i64 = 1;

/// The ranking expression, spelled once so #436 and #437 cannot drift from the
/// indexer or from each other.
///
/// Kept as a literal rather than built from [`BM25_WEIGHTS`] at runtime: this
/// goes into SQL, and a `format!` into a query string is the shape that later
/// grows a caller-supplied argument. `the_ranking_expression_matches_the_weights`
/// asserts the two agree.
pub const BM25_EXPR: &str = "bm25(session_search, 8.0, 4.0, 2.0, 0.5)";

/// The markers `snippet()` wraps a matched term in, on the wire (#437).
///
/// **U+0001 and U+0002, because they cannot occur in the indexed text.**
/// `normalize::normalize_text` collapses every whitespace *and control*
/// character to a space before a byte is stored — "a control character is never
/// a word, and leaving one in would put it in a snippet and in the stored
/// column" — so these two are unambiguous by construction rather than by
/// convention. A printable sentinel (`[MATCH]`, `**`, a `<mark>` tag) is text a
/// session could genuinely contain, and the consumer splitting on it would then
/// highlight the wrong span with nothing to say so.
///
/// They are deliberately **not HTML**: `match_snippet` carries the transcript's
/// own bytes, and a consumer that received markup would have to decide what is
/// text and what is not. #438 splits on these and renders the spans itself.
///
/// `gojson` escapes both as `\u0001` / `\u0002`, which is ordinary JSON.
pub const SNIPPET_MARK_START: char = '\u{1}';
pub const SNIPPET_MARK_END: char = '\u{2}';

/// The snippet expression, spelled once beside the ranking it accompanies.
///
/// `-1` lets FTS5 pick the column rather than pinning one, so a hit that only
/// appears in a tool result still produces a snippet instead of an empty string
/// from an empty `title`. The UNINDEXED key columns can never be picked — they
/// hold no matches at all. Which column wins a tie is FTS5's business and
/// deliberately not asserted; that a snippet comes back, from a column that
/// contains the term, is.
///
/// The markers go in as `char(1)`/`char(2)` rather than as literals so this
/// stays an ASCII string, and `12` tokens is a phrase-length window: long enough
/// to read as a sentence, short enough that a page of 200 rows is still one
/// line each.
pub const SNIPPET_EXPR: &str = "snippet(session_search, -1, char(1), char(2), '…', 12)";

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
///
/// **This is the rebuild path as well as the incremental one since #446.** It
/// was not: `search_index_version` was one stamp for the whole corpus, so a
/// rebuild emptied the index with [`delete_all`] and then used [`insert`], which
/// is what kept it from paying the old [`delete`]'s scan 1,178 times. The
/// version now lives per row, so a rebuild replaces each row in place — which is
/// affordable only because `session_search_key` turned that delete into a rowid
/// lookup.
pub fn replace(conn: &Connection, doc: &SearchDoc) -> Result<(), String> {
    delete(conn, &doc.session_id, &doc.project_path)?;
    insert(conn, doc)
}

/// Add a row, without first removing one for the same pair.
///
/// **Only safe where the caller knows no row for this pair exists.** FTS5 has no
/// unique index of its own, so a second *index* row is not refused there — but
/// since #446 the accompanying `session_search_key` insert is, on its PRIMARY
/// KEY. So a misuse is now an `Err` rather than a silent duplicate returning two
/// hits for ever, and inside the caller's transaction that `Err` rolls the index
/// row back with it.
///
/// It survives [`replace`] as its second half, and as the shortest way to time
/// the insert on its own (`tests/search_live.rs`).
pub fn insert(conn: &Connection, doc: &SearchDoc) -> Result<(), String> {
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

    // Read straight back off the connection rather than re-querying for it: FTS5
    // assigns the docid on insert and `last_insert_rowid` is how SQLite reports
    // it, where a `SELECT rowid … WHERE session_id = ?` would be the very scan
    // this table exists to remove.
    let rowid = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO session_search_key (session_id, project_path, rowid_ref)
         VALUES (?1, ?2, ?3)",
        params![doc.session_id, doc.project_path, rowid],
    )
    .map_err(|e| {
        format!(
            "recording the index key for session {}: {e}",
            doc.session_id
        )
    })?;
    Ok(())
}

/// Drop one session's row.
///
/// Keyed on the pair. A claim shift moves a transcript to a different path and
/// is an **update**, not a deletion (#245), so the caller decides what is gone;
/// this only removes what it is told to.
///
/// **This used to scan, and since #446 it does not.** FTS5's `xBestIndex`
/// accepts only `rowid` and `MATCH`, so a predicate over the UNINDEXED key
/// columns cannot use an index — SQLite answers `SCAN session_search VIRTUAL
/// TABLE INDEX 0:` — and a content-storing table materializes each scanned row
/// to serve it. #439 measured that at **44.57 ms median** against a 188.7 MB
/// index, growing with the corpus, and found it to be the entire cost of the
/// incremental path.
///
/// `session_search_key` resolves the pair to a rowid in an ordinary B-tree, and
/// `rowid` is the one non-`MATCH` constraint FTS5 does accept — so the delete is
/// a docid lookup. `a_keyed_delete_is_a_rowid_lookup_rather_than_a_scan` pins
/// that through `EXPLAIN QUERY PLAN`, because both spellings return the right
/// rows and only the plan says which one ran.
///
/// **A pair with no key row is not an error.** A database indexed before
/// migration 36 has none for any row the backfill could not claim — a duplicate
/// pair is the reachable case, since `INSERT OR IGNORE` keeps only the first —
/// so the old predicate is the fallback, and the upgrade is a slow path rather
/// than a correctness cliff. The key row is removed either way, so a fallback
/// delete cannot leave a dangling `rowid_ref` behind.
pub fn delete(conn: &Connection, session_id: &str, project_path: &str) -> Result<(), String> {
    let rowid: Option<i64> = conn
        .query_row(
            "SELECT rowid_ref FROM session_search_key
              WHERE session_id = ?1 AND project_path = ?2",
            params![session_id, project_path],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("reading the index key for session {session_id}: {e}"))?;

    match rowid {
        Some(rowid) => conn.execute(
            "DELETE FROM session_search WHERE rowid = ?1",
            params![rowid],
        ),
        None => conn.execute(
            "DELETE FROM session_search WHERE session_id = ?1 AND project_path = ?2",
            params![session_id, project_path],
        ),
    }
    .map_err(|e| format!("removing session {session_id} from the search index: {e}"))?;

    conn.execute(
        "DELETE FROM session_search_key WHERE session_id = ?1 AND project_path = ?2",
        params![session_id, project_path],
    )
    .map_err(|e| format!("removing the index key for session {session_id}: {e}"))?;
    Ok(())
}

/// Empty the index.
///
/// `DELETE FROM` rather than `DROP`/`CREATE` so the schema stays exactly what
/// migration 33 built — a rebuilt virtual table is a second place the DDL is
/// spelled, and the two would drift.
///
/// **No longer part of the rebuild path** (#446): a version bump is now per row
/// and each row is replaced in place, so nothing empties the index to rebuild
/// it. What is left is the tests that need a known-empty index, and any future
/// caller that genuinely wants one — the side table is emptied with it, because
/// a key row pointing at a deleted docid is the one state that would make a
/// later `delete` remove somebody else's row.
pub fn delete_all(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM session_search", [])
        .map_err(|e| format!("clearing the search index: {e}"))?;
    conn.execute("DELETE FROM session_search_key", [])
        .map_err(|e| format!("clearing the search index keys: {e}"))?;
    Ok(())
}

/// Drop index rows whose session is no longer in the cache.
///
/// The exact shape of `insights::store::delete_orphans`, and for exactly its
/// reasons: **keyed on "no cache row for this pair remains", never on a path.**
/// A claim shift moves a transcript to a new `file_path` and is an *update*
/// (#245), so a path-keyed delete would drop a session that is still there.
/// `session_search` carries no `file_path` at all, so path is not even available
/// as a wrong answer.
///
/// It follows that this must run **after** the cache's own delete pass, which is
/// where it inherits that pass's protection for free: a config dir that could
/// not be listed keeps its cache rows, so its index rows are not orphans and are
/// left alone. That is what stops an unmounted drive emptying an account's
/// search index.
///
/// **Both tables are reconciled against the cache, independently** (#446), and
/// that is deliberately not "delete the index rows the key table says are
/// orphans". Driving the index delete off the side table would make an index row
/// with no key entry — an older build's, or a duplicate the backfill's `INSERT
/// OR IGNORE` could not claim — unreachable for ever, which is the one failure a
/// reconcile exists to prevent. Keyed on the cache, each statement is correct
/// whatever the other table holds.
///
/// The cost is [`delete`]'s old one, and it stays: FTS5 accepts no index on the
/// UNINDEXED key columns, so the `NOT EXISTS` runs over the backing `%_content`
/// table. #439 measured that at 35 ms over a 188.7 MB index — one pass per scan,
/// not one per session, which is why it does not want the rowid treatment.
///
/// The count returned is the index's, which is what the caller reports.
pub fn delete_orphans(conn: &Connection) -> Result<usize, String> {
    let removed = conn
        .execute(
            "DELETE FROM session_search
         WHERE NOT EXISTS (
             SELECT 1 FROM claude_session_cache c
              WHERE c.session_id = session_search.session_id
                AND c.project_path = session_search.project_path
         )",
            [],
        )
        .map_err(|e| format!("reconciling orphaned search rows: {e}"))?;
    conn.execute(
        "DELETE FROM session_search_key
         WHERE NOT EXISTS (
             SELECT 1 FROM claude_session_cache c
              WHERE c.session_id = session_search_key.session_id
                AND c.project_path = session_search_key.project_path
         )",
        [],
    )
    .map_err(|e| format!("reconciling orphaned search index keys: {e}"))?;
    Ok(removed)
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

/// The highlighted snippet for each of `session_ids` that matched `query`
/// through the index (#437).
///
/// Returns `(session_id, project_path, snippet)`, so the caller keys on the pair
/// the way everything else here does; a session id under two project paths
/// matches under one of them and gets a snippet for that one alone.
///
/// **One statement per page, not one per row, and not one per match.** The
/// filter (#436) has already reduced the corpus to the rows on the page; this
/// re-runs the same `MATCH` and keeps only the page's ids, so the cost is one
/// more walk of the inverted index — the same walk the filter's membership test
/// performs — rather than a correlated subquery per row. `snippet()` itself is
/// the expensive half and it is evaluated **only for rows that survive the
/// `IN`**, so the highlighting is bounded by the page rather than by the match
/// set. The page cap (200) bounds the `IN` list.
///
/// Filtering on `session_id` alone rather than on the pair is deliberate: FTS5
/// accepts no index on either UNINDEXED column, so neither form narrows the
/// scan, and one placeholder per row keeps the statement half the size. The
/// caller matches the pair on the rows that come back.
///
/// An empty `session_ids` returns no rows without preparing anything: a page
/// with nothing on it has nothing to highlight.
pub fn snippets(
    conn: &Connection,
    query: &str,
    session_ids: &[&str],
) -> Result<Vec<(String, String, String)>, String> {
    if session_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; session_ids.len()].join(", ");
    let sql = format!(
        "SELECT session_id, project_path, {SNIPPET_EXPR}
           FROM session_search
          WHERE session_search MATCH ?1
            AND session_id IN ({placeholders})"
    );
    let mut args: Vec<&str> = Vec::with_capacity(session_ids.len() + 1);
    args.push(query);
    args.extend(session_ids.iter().copied());

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("preparing search snippets: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|e| format!("querying search snippets: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("reading search snippets: {e}"))
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

    fn cache_row(conn: &Connection, session_id: &str, project_path: &str, file_path: &str) {
        conn.execute(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity)
             VALUES (?1, ?2, ?3, '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00',
                     '2026-01-01 00:00:00+00:00')",
            params![session_id, project_path, file_path],
        )
        .expect("cache row");
    }

    /// The reconcile keys on the **pair**, so a session id that also exists
    /// under another project keeps the row for the project still cached.
    ///
    /// Keyed on the id alone this would empty both, and the surviving session
    /// would simply stop being findable — with the cache row, the insight row
    /// and every count still intact, so nothing looks wrong anywhere.
    #[test]
    fn the_reconcile_drops_only_rows_with_no_cache_row() {
        let (_file, conn) = migrated();
        for (session, project) in [("s1", "/a"), ("s1", "/b"), ("gone", "/a")] {
            cache_row(
                &conn,
                session,
                project,
                &format!("{project}/{session}.jsonl"),
            );
            replace(&conn, &doc(session, project)).expect("index");
        }

        conn.execute(
            "DELETE FROM claude_session_cache WHERE session_id = 'gone' OR project_path = '/b'",
            [],
        )
        .expect("reconcile the cache");

        assert_eq!(delete_orphans(&conn).expect("delete_orphans"), 2);
        assert_eq!(indexed_pairs(&conn), vec![("s1".into(), "/a".into())]);
    }

    /// A claim shift moves a transcript and is an **update**, not a deletion
    /// (#245). The cache row survives under its new `file_path`, so the index
    /// row must be left exactly where it is.
    ///
    /// `session_search` carries no `file_path` at all, which is what makes the
    /// wrong formulation unavailable rather than merely unused — but the rule is
    /// worth pinning, because the obvious way to add a path column later would
    /// reintroduce it.
    #[test]
    fn a_moved_transcript_keeps_its_index_row() {
        let (_file, conn) = migrated();
        cache_row(&conn, "s1", "/a", "/old/s1.jsonl");
        replace(&conn, &doc("s1", "/a")).expect("index");

        conn.execute(
            "UPDATE claude_session_cache SET file_path = '/new/s1.jsonl'",
            [],
        )
        .expect("move the file");

        assert_eq!(delete_orphans(&conn).expect("delete_orphans"), 0);
        assert_eq!(indexed_pairs(&conn), vec![("s1".into(), "/a".into())]);
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

    /// `session_insights.search_index_version` reads back 0 for a row nothing
    /// has indexed, which is the acceptance criterion for the upgrade path: 0 is
    /// "nothing indexed", and a database that predates migration 36 and a fresh
    /// install are the same case on purpose.
    #[test]
    fn search_index_version_starts_at_zero() {
        let (_file, conn) = migrated();

        conn.execute(
            "INSERT INTO session_insights (session_id, project_path, scanned_at)
             VALUES ('s1', '/a', '')",
            [],
        )
        .expect("seed an insight row the way an older build would have");

        let version: i64 = conn
            .query_row(
                "SELECT search_index_version FROM session_insights
                  WHERE session_id = 's1' AND project_path = '/a'",
                [],
                |row| row.get(0),
            )
            .expect("search_index_version must exist");
        assert_eq!(version, 0);
    }

    /// The migration these upgrade tests exercise: the one that added
    /// `session_insights.search_index_version` and the `session_search_key`
    /// side table (#446).
    ///
    /// A literal on purpose. Each of the three tests below builds a database at
    /// `SEARCH_KEY_MIGRATION - 1`, seeds rows an older build would have written,
    /// and asserts on what **36** does to them — so the boundary is a property of
    /// what is under test, not of wherever the migration list happens to end.
    const SEARCH_KEY_MIGRATION: i64 = 36;

    /// The upgrade path with **data** in it.
    ///
    /// `migrate::a_database_at_any_earlier_version_upgrades_to_this_one` already
    /// walks every intermediate version, but it applies DDL to an empty database
    /// and asserts on versions — so it cannot see an `ALTER TABLE` that runs
    /// cleanly and loses or rewrites a row. This one seeds a `session_insights`
    /// row **and an index row for it** before the last migration runs, and
    /// asserts three things #446 depends on: the row survives untouched, its new
    /// `search_index_version` **inherits the global stamp** that described the
    /// text already in the index, and migration 36's backfill claimed the
    /// already-indexed pair.
    ///
    /// The middle one is what stops the upgrade being a 35.8 s rebuild for no
    /// reason. Both directions are covered — an install that had stamped a
    /// version carries it forward and indexes nothing; one that had not (the 0
    /// below, seeded by the sibling case) reads as behind and is rebuilt.
    ///
    /// The boundary is [`SEARCH_KEY_MIGRATION`], the migration under test, and it
    /// was `expected_version()` until #490 appended migration 37: read off the
    /// tip, the seed rows land *after* 36 has already run, so the backfill this
    /// asserts on never sees them and the test fails for a change that has
    /// nothing to do with it.
    #[test]
    fn the_migration_applies_to_a_populated_database() {
        // Two installs, differing only in whether the old global stamp had been
        // written: `Some(1)` is an install whose index is current, `None` one
        // that never finished (or never ran) an index.
        for stamped in [Some(SEARCH_INDEX_VERSION), None] {
            let file = tempfile::NamedTempFile::new().expect("temp file");
            let mut conn = Connection::open(file.path()).expect("open");
            let last = SEARCH_KEY_MIGRATION;

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
                "INSERT INTO session_insights (session_id, project_path, scanned_at, turn_count)
                 VALUES ('s1', '/a', '', 7)",
                [],
            )
            .expect("seed an insight row");
            // Indexed by the older build, which had no key table to record.
            conn.execute(
                "INSERT INTO session_search
                     (title, user_text, assistant_text, tool_text, session_id, project_path)
                 VALUES ('old', '', '', '', 's1', '/a')",
                [],
            )
            .expect("seed an index row");
            if let Some(version) = stamped {
                conn.execute(
                    "INSERT INTO claude_cache_metadata (id, last_scanned_at, search_index_version)
                     VALUES (1, 'when', ?1)",
                    params![version],
                )
                .expect("seed the old global stamp");
            }
            assert_eq!(migrate::current_version(&conn).expect("version"), last - 1);

            migrate::apply(&mut conn).expect("upgrade must apply");

            assert_eq!(
                migrate::current_version(&conn).expect("version"),
                migrate::expected_version()
            );
            migrate::verify(&conn).expect("verify");
            let (kept, version): (i64, i64) = conn
                .query_row(
                    "SELECT turn_count, search_index_version
                       FROM session_insights WHERE session_id = 's1' AND project_path = '/a'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("the existing row survives");
            assert_eq!(kept, 7, "the migration must not rewrite the row");
            assert_eq!(
                version,
                stamped.unwrap_or(0),
                "the row must inherit the stamp that described its indexed text \
                 (stamped: {stamped:?})",
            );

            // The backfill: the pre-existing index row now has a key, pointing at
            // its own docid. Without it every session in a later rebuild takes
            // `delete`'s fallback scan.
            let (session, project, rowid_ref): (String, String, i64) = conn
                .query_row(
                    "SELECT session_id, project_path, rowid_ref FROM session_search_key",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("the backfill claimed the indexed pair");
            assert_eq!((session.as_str(), project.as_str()), ("s1", "/a"));
            let docid: i64 = conn
                .query_row("SELECT rowid FROM session_search", [], |row| row.get(0))
                .expect("docid");
            assert_eq!(rowid_ref, docid, "the backfill recorded the wrong docid");
        }
    }

    /// An insight row with **no** index row must not inherit the stamp, however
    /// current the stamp is — it has nothing indexed, so it is behind.
    ///
    /// This is the half the loop above cannot reach, and getting it wrong is
    /// silent in the worst direction: the session would read as indexed for ever
    /// and never be searchable. It is the same hole #446 exists to close,
    /// reintroduced by the migration that closes it.
    #[test]
    fn the_migration_leaves_an_unindexed_session_behind() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        let last = SEARCH_KEY_MIGRATION;

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
            "INSERT INTO session_insights (session_id, project_path, scanned_at)
             VALUES ('never-indexed', '/a', '')",
            [],
        )
        .expect("seed an insight row with nothing in the index");
        conn.execute(
            "INSERT INTO claude_cache_metadata (id, last_scanned_at, search_index_version)
             VALUES (1, 'when', ?1)",
            params![SEARCH_INDEX_VERSION],
        )
        .expect("seed a current global stamp");

        migrate::apply(&mut conn).expect("upgrade must apply");

        let version: i64 = conn
            .query_row(
                "SELECT search_index_version FROM session_insights",
                [],
                |row| row.get(0),
            )
            .expect("version");
        assert_eq!(
            version, 0,
            "a session with no index row inherited the stamp and will never be \
             indexed",
        );
    }

    /// Migration 36 removes a duplicate pair the older schema could not refuse.
    ///
    /// It matters because [`delete`] is keyed on **one** recorded rowid from
    /// here on: the scan-shaped predicate used to remove every row for a pair,
    /// so a duplicate self-healed on the next re-index, and a rowid delete
    /// removes one and leaves the other for ever. The migration is the one place
    /// that can still see both.
    #[test]
    fn the_migration_removes_a_duplicate_pair_the_old_schema_allowed() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        let last = SEARCH_KEY_MIGRATION;

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
        for title in ["first", "second"] {
            conn.execute(
                "INSERT INTO session_search
                     (title, user_text, assistant_text, tool_text, session_id, project_path)
                 VALUES (?1, 'quasarflux', '', '', 'dup', '/a')",
                params![title],
            )
            .expect("seed a duplicate pair");
        }
        conn.execute(
            "INSERT INTO session_search
                 (title, user_text, assistant_text, tool_text, session_id, project_path)
             VALUES ('kept', 'quasarflux', '', '', 'other', '/a')",
            [],
        )
        .expect("seed an ordinary pair");

        migrate::apply(&mut conn).expect("upgrade must apply");

        assert_eq!(
            indexed_pairs(&conn),
            vec![
                ("dup".to_string(), "/a".to_string()),
                ("other".to_string(), "/a".to_string()),
            ],
        );
        assert_eq!(
            search(&conn, "quasarflux", 10).expect("search").len(),
            2,
            "the duplicate still answers twice",
        );
        assert_key_table_agrees(&conn, "after the migration's dedup and backfill");
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
        // And it is still usable afterwards.
        replace(&conn, &doc("c", "/p/three")).expect("reindex after clear");
        assert_eq!(indexed_pairs(&conn).len(), 1);
    }

    // ─── the rowid side table (#446) ─────────────────────────────────────────

    /// Every `(session_id, project_path)` the key table holds, ordered.
    fn key_pairs(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT session_id, project_path FROM session_search_key ORDER BY 1, 2")
            .expect("prepare");
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect")
    }

    /// Every `rowid_ref` must name a row that is actually in the index, and
    /// every indexed row must have exactly one key. Checked as *both*
    /// directions rather than as a count, because two rows and two keys that
    /// point at each other's docids agree on every count and delete the wrong
    /// row.
    fn assert_key_table_agrees(conn: &Connection, where_: &str) {
        assert_eq!(
            key_pairs(conn),
            indexed_pairs(conn),
            "the key table and the index hold different pairs, {where_}",
        );
        let mismatched: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_search_key k
                  WHERE NOT EXISTS (
                      SELECT 1 FROM session_search s
                       WHERE s.rowid = k.rowid_ref
                         AND s.session_id = k.session_id
                         AND s.project_path = k.project_path
                  )",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(
            mismatched, 0,
            "{mismatched} key rows point at the wrong docid, {where_}",
        );
    }

    /// The invariant the whole design rests on, walked through every writer.
    ///
    /// A drift here is silent and severe in both directions: a stale
    /// `rowid_ref` deletes **somebody else's** row, and a missing one leaves a
    /// duplicate that returns two hits for ever. Nothing about a search says
    /// either has happened.
    #[test]
    fn the_key_table_tracks_the_index_through_every_writer() {
        let (_file, conn) = migrated();

        for (session, project) in [("s1", "/a"), ("s1", "/b"), ("gone", "/a")] {
            cache_row(
                &conn,
                session,
                project,
                &format!("{project}/{session}.jsonl"),
            );
            replace(&conn, &doc(session, project)).expect("index");
        }
        assert_key_table_agrees(&conn, "after a replace");

        // A re-replace of the same pair: the docid changes, so a key row left
        // behind is exactly the stale pointer that deletes the wrong row.
        let mut rewritten = doc("s1", "/a");
        rewritten.title = "rewritten".into();
        replace(&conn, &rewritten).expect("re-index");
        assert_key_table_agrees(&conn, "after a re-replace of the same pair");
        assert_eq!(
            indexed_pairs(&conn).len(),
            3,
            "a re-replace duplicated a pair"
        );

        // One pair removed outright.
        delete(&conn, "gone", "/a").expect("delete");
        assert_key_table_agrees(&conn, "after a delete");

        // The reconcile: `/b`'s cache row goes, so its index row and its key row
        // must both go with it.
        conn.execute(
            "DELETE FROM claude_session_cache WHERE project_path = '/b'",
            [],
        )
        .expect("reconcile the cache");
        assert_eq!(delete_orphans(&conn).expect("delete_orphans"), 1);
        assert_key_table_agrees(&conn, "after delete_orphans");

        delete_all(&conn).expect("clear");
        assert_key_table_agrees(&conn, "after delete_all");
        assert!(key_pairs(&conn).is_empty());
    }

    /// The acceptance criterion #446 exists for: the keyed delete is a rowid
    /// lookup, not the `%_content` scan #439 measured at 44.57 ms.
    ///
    /// Asserted through `EXPLAIN QUERY PLAN`, because both spellings delete the
    /// right row and only the plan says which one ran — a regression here is a
    /// pure performance cliff with no visible symptom until a corpus is large
    /// enough for the difference to be seconds.
    ///
    /// **`SCAN` is not the tell, and reading it as one is how this test gets
    /// written wrong.** SQLite prints `SCAN <table> VIRTUAL TABLE INDEX <n>:<s>`
    /// for *every* virtual-table access — it has no idea what the module will
    /// do — so the two plans differ only in `<s>`, which is fts5's own `idxStr`.
    /// `fts5BestIndexMethod` appends one character per constraint it accepted,
    /// and `=` is the one it appends for a rowid equality. So the whole content
    /// of this test is `0:` (nothing accepted; fts5 walks the content table and
    /// filters) versus `0:=` (a docid seek).
    ///
    /// Both are pinned as exact strings, so a future SQLite that respells either
    /// fails loudly here rather than quietly passing a `contains` that no longer
    /// means anything.
    #[test]
    fn a_keyed_delete_is_a_rowid_lookup_rather_than_a_scan() {
        let (_file, conn) = migrated();
        replace(&conn, &doc("s1", "/a")).expect("index");

        let plan = |sql: &str| -> String {
            let mut stmt = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .expect("prepare");
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(3))
                .expect("query");
            rows.collect::<Result<Vec<_>, _>>()
                .expect("collect")
                .join(" | ")
        };

        assert_eq!(
            plan("DELETE FROM session_search WHERE session_id = 'x' AND project_path = 'y'"),
            "SCAN session_search VIRTUAL TABLE INDEX 0:",
            "the fallback is the scan this table exists to avoid",
        );
        assert_eq!(
            plan("DELETE FROM session_search WHERE rowid = 1"),
            "SCAN session_search VIRTUAL TABLE INDEX 0:=",
            "the keyed delete did not reach fts5 as a rowid constraint",
        );
    }

    /// …and the other half of the same claim, from **behaviour** rather than
    /// from a query plan: [`delete`] runs the keyed statement, not the scan.
    ///
    /// The plan test above is about two SQL literals and would go on passing if
    /// `delete` were reverted to the scan — so it pins the *predicate*, and this
    /// pins the *call site*. Two rows for one pair are the one input that tells
    /// them apart: the scan matches on the key columns and removes both, the
    /// rowid delete removes only the one the key table names.
    ///
    /// (The duplicate is unreachable from this module's own API since migration
    /// 36 — `insert`'s key row refuses a second — so it is built by hand, which
    /// is also exactly the state an older build could leave behind.)
    #[test]
    fn a_keyed_delete_targets_the_recorded_rowid() {
        let (_file, conn) = migrated();
        replace(&conn, &doc("s1", "/a")).expect("index");
        let recorded: i64 = conn
            .query_row("SELECT rowid_ref FROM session_search_key", [], |r| r.get(0))
            .expect("the key row");
        conn.execute(
            "INSERT INTO session_search
                 (title, user_text, assistant_text, tool_text, session_id, project_path)
             VALUES ('shadow', '', '', '', 's1', '/a')",
            [],
        )
        .expect("a second row for the pair, as an older build could leave");

        delete(&conn, "s1", "/a").expect("delete");

        let survivors: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT rowid FROM session_search")
                .expect("prepare");
            stmt.query_map([], |r| r.get(0))
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect")
        };
        assert_eq!(
            survivors.len(),
            1,
            "the delete removed both rows, so it ran the key-column scan",
        );
        assert_ne!(
            survivors[0], recorded,
            "the delete removed the wrong row — the recorded docid is the one to go",
        );
    }

    /// A database indexed before migration 36 can hold an index row with no key
    /// entry — the duplicate the backfill's `INSERT OR IGNORE` could not claim.
    /// It must still delete, or the upgrade is a correctness cliff rather than a
    /// slow path.
    #[test]
    fn a_pair_with_no_key_row_still_deletes() {
        let (_file, conn) = migrated();
        replace(&conn, &doc("s1", "/a")).expect("index");
        replace(&conn, &doc("s2", "/a")).expect("index the neighbour");
        conn.execute("DELETE FROM session_search_key WHERE session_id = 's1'", [])
            .expect("forget the key, as an older build would have");

        delete(&conn, "s1", "/a").expect("delete through the fallback");

        assert_eq!(indexed_pairs(&conn), vec![("s2".into(), "/a".into())]);
        assert_key_table_agrees(&conn, "after a fallback delete");
    }

    /// [`insert`] against a pair that is already indexed is now **refused** by
    /// the key table's PRIMARY KEY, where FTS5 alone accepted it silently.
    ///
    /// Inside the caller's transaction — which is where every real caller runs —
    /// the refusal takes the index row with it, so the duplicate never exists.
    #[test]
    fn a_second_insert_for_one_pair_is_refused_rather_than_duplicated() {
        let (_file, mut conn) = migrated();
        replace(&conn, &doc("s1", "/a")).expect("index");

        let tx = conn.transaction().expect("begin");
        let err = insert(&tx, &doc("s1", "/a")).expect_err("a duplicate must be refused");
        assert!(
            err.contains("s1"),
            "the error should name the session: {err}"
        );
        drop(tx);

        assert_eq!(indexed_pairs(&conn), vec![("s1".into(), "/a".into())]);
        assert_key_table_agrees(&conn, "after a refused duplicate");
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

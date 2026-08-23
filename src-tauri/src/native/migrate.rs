//! The schema: what Rust knows about it, and what it is allowed to do to it.
//!
//! # The migrations are not transcribed
//!
//! `MIGRATIONS` is parsed from `desktop/parity/migrations_vectors.json`, which
//! is **generated from Go** (`go test ./internal/storage/
//! -update-migration-vectors`) and asserted against the `migrations` slice by
//! `internal/storage/migrations_vector_test.go`. Adding migration 28 without
//! regenerating fails Go's own test suite.
//!
//! # ...but migration 31 onward is this branch's own (#405)
//!
//! That paragraph describes migrations 1–30, and they are still exactly Go's
//! bytes. It stopped being the whole story with #405, which needs an
//! `api_tokens` table `main`'s server has never heard of. There is no generator
//! left to run — #388 deleted the Go tree — so migration 31 is **authored**
//! here, and the vector file's own `_comment` says so at the top.
//!
//! Two rules follow, and both are asserted below:
//!
//! - **Do not edit 1–30.** They are a frozen record of what Go produced, and
//!   the only thing that still makes the "not transcribed" argument above true.
//! - **Anything appended must be additive.** `main` still ships the Go server
//!   and it opens the same `~/.agento/agento.db`. `applyMigrations` runs only
//!   migrations *newer* than the recorded version, so a database at 31 makes an
//!   `agento web` apply nothing and carry on — it simply never reads the new
//!   table. A migration that *altered* something Go reads would break that
//!   process instead, silently, on a user's machine.
//!
//! Twenty-seven migrations of hand-copied DDL is precisely the kind of thing
//! that agrees on every table anyone happens to check and differs on one column
//! default nobody does — and the failure would surface as a write succeeding
//! against a column that is `NOT NULL DEFAULT ''` on one side and nullable on
//! the other. Embedding the file removes the transcription entirely. It also
//! outlives the Go server: when the sidecar is deleted, this file is the record
//! of what the schema was, the same reason `desktop/parity/` keeps its other
//! vectors.
//!
//! # Rust verifies; it does not apply. Yet.
//!
//! This is the load-bearing part of #274, and it is a deliberate departure from
//! the issue's wording ("Rust owns migration ordering").
//!
//! Go's `applyMigrations` reads the current version **outside** the transaction
//! that applies the next one (`internal/storage/sqlite.go`). Two processes
//! starting together therefore both read the same version, both decide to
//! apply the next migration, and both run its DDL: the loser gets
//! `table already exists`, which is not a
//! conflict it retries but an error that fails `NewSQLiteDB` and takes the whole
//! startup with it. Whichever process loses simply does not come up.
//!
//! So while the Go sidecar is still bundled — and it is, until #278 deletes it —
//! exactly one process may apply migrations, and that process is Go, because it
//! is the one that also creates the database, seeds the pricing catalog and runs
//! the legacy filesystem import. Rust [`verify`]s instead: it reads the version
//! and refuses to serve a database it does not recognise, which sends the
//! request to Go rather than writing through a schema it has guessed at.
//!
//! [`apply`] is written, tested and unused. It is what #278 turns on, in the
//! same change that stops the sidecar from migrating — one commit, one owner,
//! no window in which both do it.
//!
//! # Two directions, two different answers
//!
//! A database **older** than this build is a hard error: some migration this
//! code depends on has not run, so a write would hit a missing column. A
//! database **newer** is also an error here, though Go accepts it silently — Go
//! can afford to, because its queries are written against the schema it shipped
//! with, whereas a Rust build reading a newer file is reading columns it was
//! never compiled against. Both directions return `Err`, and `Err` means the
//! request forwards to the sidecar, which is the implementation that does know
//! the schema in front of it.

use std::sync::OnceLock;

use rusqlite::Connection;

/// One migration, exactly as `internal/storage` applies it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Migration {
    pub version: i64,
    pub sql: String,
}

#[derive(serde::Deserialize)]
struct VectorFile {
    migrations: Vec<Migration>,
}

/// The vector file, embedded at compile time so a build can never disagree with
/// the tree it was built from.
const VECTORS: &str = include_str!("../../../parity/migrations_vectors.json");

/// Every migration, in the order Go applies them.
///
/// # Panics
///
/// Only if the embedded vector file is malformed, which is a build-time fact
/// rather than a runtime one: the same bytes are parsed on every run, and the
/// unit tests below parse them too, so a bad file fails `cargo test` and CI
/// long before it can reach a user.
pub fn migrations() -> &'static [Migration] {
    static PARSED: OnceLock<Vec<Migration>> = OnceLock::new();
    PARSED.get_or_init(|| {
        let file: VectorFile = serde_json::from_str(VECTORS)
            .expect("migrations_vectors.json is embedded and must parse");
        file.migrations
    })
}

/// The version this build was written against — the highest it knows.
pub fn expected_version() -> i64 {
    migrations().last().map(|m| m.version).unwrap_or(0)
}

/// The version recorded in the database.
///
/// Mirrors Go's `currentVersion`: `COALESCE(MAX(version), 0)`, so a database
/// whose `schema_migrations` table exists but is empty reads as 0 rather than
/// failing. A database with no such table at all has never been migrated, and
/// that is an error here rather than a 0 — Go creates the table as its first
/// act, so its absence means this is not an Agento database.
pub fn current_version(conn: &Connection) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )
    .map_err(|e| format!("reading schema version: {e}"))
}

/// Confirm the database is the schema this build writes against.
///
/// Called by every native write before it touches anything. The cost is one
/// indexed aggregate over a table with 27 rows; the alternative is discovering
/// the mismatch as a constraint violation halfway through a transaction.
pub fn verify(conn: &Connection) -> Result<(), String> {
    let want = expected_version();
    let have = current_version(conn)?;
    if have == want {
        return Ok(());
    }
    // Both directions forward to Go, but they are different situations and a
    // log line that says which one saves the next person a bisect.
    if have < want {
        return Err(format!(
            "database is at schema version {have}, this build writes version {want}; \
             the sidecar has not migrated it yet"
        ));
    }
    Err(format!(
        "database is at schema version {have}, newer than this build's {want}; \
         forwarding to the sidecar, which knows the schema in front of it"
    ))
}

/// Apply every pending migration.
///
/// **Not called while the Go sidecar exists** — see the module header. This is
/// the body #278 turns on once the sidecar stops migrating.
///
/// Mirrors Go's `applyMigrations`/`applyMigration`: create the tracking table,
/// read the current version, then run each pending migration and record it.
/// One departure, and it is the reason the Go version cannot be run twice
/// concurrently: the version is re-read **inside** the transaction that applies
/// the next migration, so two processes racing resolve to one applying and the
/// other finding its work already done, instead of one failing on duplicate
/// DDL. `BEGIN IMMEDIATE` takes the write lock up front rather than on first
/// write, which is what makes that re-read authoritative.
pub fn apply(conn: &mut Connection) -> Result<(), String> {
    // `BEGIN IMMEDIATE` takes the write lock up front, so a contended database
    // needs a busy timeout or the loser fails on the lock rather than waiting
    // for it — which would be the same failure this function exists to avoid,
    // one layer down.
    //
    // rusqlite already sets 5000 ms on every `Connection::open`
    // (`InnerConnection::open_with_flags`), so this is **explicitness, not a
    // fix**: it states the value the correctness argument depends on instead of
    // inheriting it from a dependency's default, which a version bump could
    // change without anything here noticing.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("setting busy_timeout: {e}"))?;

    // Outside any transaction, like Go's. Harmless to race: `IF NOT EXISTS`.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .map_err(|e| format!("creating schema_migrations table: {e}"))?;

    for migration in migrations() {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("begin migration {}: {e}", migration.version))?;

        // Re-read under the write lock. Without this, the check and the apply
        // are two steps a second process can interleave.
        let current: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("reading schema version: {e}"))?;
        if migration.version <= current {
            // `tx` is dropped here, and rusqlite's default drop behaviour is
            // rollback — so this releases the write lock rather than leaking it.
            continue;
        }

        tx.execute_batch(&migration.sql)
            .map_err(|e| format!("migration {}: {e}", migration.version))?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![migration.version, super::gotime::now_go_text()],
        )
        .map_err(|e| format!("recording migration {}: {e}", migration.version))?;

        tx.commit()
            .map_err(|e| format!("commit migration {}: {e}", migration.version))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded file has to parse, and it has to be the whole schema.
    /// Hardcoded rather than derived, for the same reason `sqlite_test.go`
    /// hardcodes its version: a count computed from the list agrees with itself
    /// no matter what the list lost.
    #[test]
    fn the_embedded_vector_is_the_whole_schema() {
        let all = migrations();
        assert_eq!(all.len(), 31, "expected 31 migrations");
        assert_eq!(expected_version(), 31);
        for (i, m) in all.iter().enumerate() {
            assert_eq!(
                m.version,
                i as i64 + 1,
                "versions must be contiguous from 1"
            );
            assert!(!m.sql.is_empty(), "migration {} has no SQL", m.version);
        }
    }

    /// **The boundary between Go's migrations and this branch's** (#405).
    ///
    /// 1–30 are the frozen record of what `internal/storage` applied and must
    /// never be edited; 31 onward is authored here, because #388 deleted the
    /// generator. Pinned as a number rather than left implicit so that appending
    /// a migration is a deliberate act with a line to change, and so that a
    /// *rewrite* of one of Go's — the thing that would quietly destroy the "not
    /// transcribed" property — shows up as a failing hash rather than as
    /// nothing at all.
    #[test]
    fn the_migrations_go_generated_are_unchanged() {
        const LAST_GO_MIGRATION: i64 = 30;

        let all = migrations();
        let go: Vec<&Migration> = all
            .iter()
            .filter(|m| m.version <= LAST_GO_MIGRATION)
            .collect();
        assert_eq!(go.len(), LAST_GO_MIGRATION as usize);

        // A digest over the whole Go half, so editing any one of them fails
        // here with a message that says which rule was broken. Update this
        // constant only if the Go tree is restored and regenerates the file.
        let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
        for m in &go {
            hasher.update(m.version.to_string().as_bytes());
            hasher.update(b"\0");
            hasher.update(m.sql.as_bytes());
            hasher.update(b"\0");
        }
        let digest: String = hasher
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            digest, GO_MIGRATIONS_SHA256,
            "migrations 1-{LAST_GO_MIGRATION} are Go's frozen output and must not be \
             edited; append a new version instead"
        );
    }

    /// The digest of migrations 1–30, recorded when #405 appended the first
    /// non-Go migration.
    const GO_MIGRATIONS_SHA256: &str =
        "fb4ae3ab30711f1532444af09913c643a1a662564750fbb81a1b841e333c6da3";

    /// The point of embedding rather than transcribing: the SQL must be Go's,
    /// unreformatted. Spot-check a few things a prettifier would silently
    /// change and a hand-copy would silently drop.
    #[test]
    fn the_sql_is_gos_bytes() {
        let all = migrations();
        assert!(
            all[0].sql.starts_with('\n'),
            "migration 1 keeps its leading newline"
        );
        assert!(all[0]
            .sql
            .contains("model           TEXT NOT NULL DEFAULT 'claude-sonnet-4-6'"));
        // Migration 24's RENAME is the one that makes physical column order
        // differ from declaration order in session_insights.
        assert!(all[23]
            .sql
            .contains("RENAME COLUMN thinking_time_ms TO claude_working_time_ms"));
        // Only 9, 26, 27, 28 and 29 use IF NOT EXISTS; migration 2 must not have
        // acquired one, or a half-applied database would look migrated.
        assert!(!all[1].sql.contains("IF NOT EXISTS"));
    }

    /// Applying the whole list against an empty database must produce a
    /// working schema — which is also the cheapest possible proof that the
    /// embedded SQL is valid SQLite rather than merely well-formed JSON.
    #[test]
    fn applying_every_migration_builds_the_schema() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        conn.execute_batch("PRAGMA foreign_keys=ON")
            .expect("pragma");

        apply(&mut conn).expect("apply");

        assert_eq!(current_version(&conn).expect("version"), 31);
        verify(&conn).expect("verify");

        // A column from the last migration, and the one migration 24 renamed:
        // between them they prove the list ran in order and to the end.
        let cols: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('claude_session_cache') WHERE name = 'config_dir'",
                [],
                |row| row.get(0),
            )
            .expect("config_dir");
        assert_eq!(cols, 1);
        let renamed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_insights') WHERE name = 'claude_working_time_ms'",
                [],
                |row| row.get(0),
            )
            .expect("renamed column");
        assert_eq!(renamed, 1);
        let old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_insights') WHERE name = 'thinking_time_ms'",
                [],
                |row| row.get(0),
            )
            .expect("old column");
        assert_eq!(old, 0, "migration 24 renames rather than adding");
    }

    /// Idempotence, which is what makes a second process safe to run at all.
    #[test]
    fn applying_twice_is_a_no_op() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");

        apply(&mut conn).expect("first");
        apply(&mut conn).expect("second must not fail");
        assert_eq!(current_version(&conn).expect("version"), 31);
    }

    /// The property this whole function exists for, and the one sequential
    /// idempotence does **not** prove: two processes applying at once must both
    /// succeed rather than one failing on duplicate DDL — which is exactly what
    /// Go's runner does, because it reads the version outside the transaction
    /// that applies the next migration.
    #[test]
    fn two_connections_applying_concurrently_both_succeed() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // WAL is set **once, up front** — not per thread.
        //
        // It is persistent in the file, so both connections inherit it. Setting
        // it inside each thread is what made this flake in CI: switching journal
        // mode needs a lock SQLite refuses to *wait* for, so it returns
        // `SQLITE_BUSY` immediately rather than honouring a busy timeout, and
        // the test failed on its own setup instead of on the property it
        // measures. The app never hits this because the mode is already set by
        // the time anything opens the database.
        {
            let conn = Connection::open(&path).expect("open");
            conn.pragma_update(None, "journal_mode", "WAL")
                .expect("wal");
        }

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut conn = Connection::open(&path).expect("open");
                    barrier.wait();
                    apply(&mut conn)
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .expect("thread")
                .expect("both must apply cleanly");
        }

        let conn = Connection::open(&path).expect("open");
        assert_eq!(current_version(&conn).expect("version"), 31);
        // Each migration recorded exactly once — a double-apply would have
        // violated the primary key and failed above, but assert the end state
        // rather than relying on that.
        let recorded: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(recorded, 31);
    }

    #[test]
    fn a_database_behind_this_build_is_refused() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at DATETIME);
             INSERT INTO schema_migrations (version, applied_at) VALUES (26, '');",
        )
        .expect("seed");

        let err = verify(&conn).expect_err("a behind database must not be served");
        assert!(err.contains("has not migrated"), "got: {err}");
    }

    /// Go accepts a newer database silently. Rust must not: its queries were
    /// compiled against this schema, not that one.
    #[test]
    fn a_database_ahead_of_this_build_is_refused() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at DATETIME);
             INSERT INTO schema_migrations (version, applied_at) VALUES (99, '');",
        )
        .expect("seed");

        let err = verify(&conn).expect_err("a newer database must not be served");
        assert!(err.contains("newer than this build"), "got: {err}");
    }

    /// No `schema_migrations` at all is not "version 0" — it is not an Agento
    /// database, and guessing would mean writing into someone else's file.
    #[test]
    fn a_database_with_no_migration_table_is_an_error() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        assert!(current_version(&conn).is_err());
        assert!(verify(&conn).is_err());
    }
}

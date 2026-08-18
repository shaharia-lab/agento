//! Access to the SQLite database the Go server owns.
//!
//! Ported endpoints use the *same* file the sidecar writes, rather than a copy
//! or a second schema: a second source of truth would be a bug factory.
//!
//! ## Two handles, and why the read-only one is still the default
//!
//! [`open_read_only`] is what every ported *read* uses, and it stays that way.
//! `SQLITE_OPEN_READ_ONLY` is cheap insurance: a reader that cannot write
//! cannot corrupt anything by accident, whatever a later edit does to it.
//!
//! [`open_read_write`] (#274) is for the ported writes. Two processes writing
//! one SQLite file is not the hazard it sounds like — it is what WAL plus a
//! busy timeout exist for, and the Go side already runs exactly that
//! configuration. Readers do not block writers, writers serialize against each
//! other, and a lock contended past the timeout surfaces as an error rather
//! than as corruption.
//!
//! **Migrations are the part that genuinely cannot be shared, and this module
//! does not do them.** `applyMigrations` (Go, `internal/storage/sqlite.go`)
//! reads the current version *outside* the transaction that applies the next
//! one, so two processes starting together both decide to apply the same
//! version and the loser's DDL fails — taking its whole startup with it. See
//! `migrate.rs`: Rust verifies the schema version and refuses to serve when it
//! disagrees, and only takes over applying when the sidecar is gone (#278).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Matches the Go side's `busy_timeout=5000`. A checkpoint can briefly lock the
/// database, and failing over that would be worse than waiting.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Open the database for reading.
///
/// The Go server runs in WAL mode, so a reader never blocks on a writer and
/// sees a consistent snapshot. WAL does require the `-shm` file, which is why
/// the URI is opened plainly rather than with `immutable=1`: that flag would
/// skip the WAL entirely and serve stale rows from the main file — every write
/// since the last checkpoint silently missing.
pub fn open_read_only(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("opening {}: {e}", path.display()))?;

    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| format!("setting busy_timeout: {e}"))?;

    Ok(conn)
}

/// Open the database for writing.
///
/// The pragmas mirror `openSQLiteDB` in `internal/storage/sqlite.go` exactly,
/// and each one has to be set here rather than inherited:
///
/// - `journal_mode=WAL` is persistent in the file, so this is a no-op against a
///   database the Go server has opened — but it is *not* a no-op on a fresh
///   one, and asserting it costs nothing.
/// - `busy_timeout` and `foreign_keys` are **per connection**. Go gets away with
///   setting them once because `SetMaxOpenConns(1)` means it only ever has one;
///   every handle opened here needs them again. Missing `foreign_keys=ON` is
///   the quiet one: `ON DELETE CASCADE` on `chat_messages`, `job_history`,
///   `trigger_rules` and `model_pricing_tier` simply would not fire, and a
///   deleted chat would leave its messages behind with nothing to say so.
/// - `synchronous=NORMAL` matches Go's durability trade rather than SQLite's
///   stricter default.
///
/// Deliberately **not** `SQLITE_OPEN_CREATE`: a missing database means the app
/// has never run, and conjuring an empty one here would let a write appear to
/// succeed against a schema that does not exist. Failing sends the request to
/// Go, which creates and migrates it properly.
pub fn open_read_write(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("opening {} for writing: {e}", path.display()))?;

    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| format!("setting busy_timeout: {e}"))?;

    // `journal_mode` answers with the resulting mode, so it is a query rather
    // than a plain execute; the others return nothing.
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(|e| format!("setting journal_mode: {e}"))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(format!("expected WAL journal mode, got {mode}"));
    }

    for pragma in ["PRAGMA foreign_keys=ON", "PRAGMA synchronous=NORMAL"] {
        conn.execute_batch(pragma)
            .map_err(|e| format!("setting {pragma}: {e}"))?;
    }

    Ok(conn)
}

/// Create-if-missing open, for exactly one caller: startup (#278).
///
/// Go's `NewSQLiteDB` created the file, ran the migrations and seeded the
/// pricing catalog before the server listened; with the sidecar gone the shell
/// does the same, once, in `lib.rs`'s setup. Every other open in this module
/// deliberately does **not** create — `open_read_write` on a missing file is
/// an error, pinned by `a_missing_database_is_an_error_not_a_new_file`,
/// because outside startup a missing database means a misresolved path, and
/// writing a fresh schema at a wrong location would hide that.
///
/// The parent directory is created too: on a fresh install `~/.agento` itself
/// does not exist yet, which was likewise Go's job.
pub fn ensure_database(path: &Path) -> Result<Connection, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("creating data dir {}: {e}", dir.display()))?;
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("opening {} for startup: {e}", path.display()))?;

    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| format!("setting busy_timeout: {e}"))?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(|e| format!("setting journal_mode: {e}"))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(format!("expected WAL journal mode, got {mode}"));
    }
    for pragma in ["PRAGMA foreign_keys=ON", "PRAGMA synchronous=NORMAL"] {
        conn.execute_batch(pragma)
            .map_err(|e| format!("setting {pragma}: {e}"))?;
    }
    Ok(conn)
}

/// Run a synchronous database section on the blocking pool.
///
/// **Everything in this module blocks a thread, and `BUSY_TIMEOUT` says for how
/// long.** A lock held by the Go sidecar or by the session scanner's batch
/// writer parks the caller for up to five seconds. On an async worker that is
/// not a slow call, it is a *stalled runtime*: `proxy.rs` puts its native
/// handlers on the blocking pool for exactly this reason — "one slow read cannot
/// stall an SSE stream sharing the runtime" — and the tokio default is one
/// worker per core, so a four-core machine has four to lose.
///
/// **The proxy's cover is narrower than it looks**, which is why this exists.
/// `serve` — the buffered path, every entry in `native::ENDPOINTS` — is on the
/// pool. `serve_stream` is not: it awaits on the worker, and `STREAM_ENDPOINTS`
/// is the chat turn. So the callers here are the streaming turn's own commit
/// plus everything reached from a timer or a webhook rather than a request: the
/// scheduler's executor (#366), which runs three at once, and the trigger
/// dispatcher (#319), which runs ten — against a tokio default of one worker per
/// core.
///
/// `None` means the pool task itself failed, which is a panic inside `f` and
/// nothing else. It is logged here so a panic is never silent, but **what it
/// leaves behind is the caller's problem, not this function's**: a closure
/// holding several writes can panic between them, and only the caller knows what
/// a half-finished section left in the database.
pub async fn blocking<T, F>(what: &'static str, f: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(value) => Some(value),
        Err(e) => {
            log::error!("{what}: a blocking database task failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_database_is_an_error_not_a_new_file() {
        let dir = std::env::temp_dir().join("agento-native-db-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("does-not-exist.db");
        let _ = std::fs::remove_file(&path);

        assert!(open_read_only(&path).is_err());
        // Read-only must not have conjured one, or a first run would answer
        // from an empty schema instead of falling back to Go.
        assert!(!path.exists());
    }

    /// The write handle must not create either. `SQLITE_OPEN_CREATE` is easy to
    /// add "so it works on a fresh machine" and would make a write succeed
    /// against an empty file with no schema — reported to the user as saved.
    #[test]
    fn opening_for_writing_does_not_create_the_database() {
        let dir = std::env::temp_dir().join("agento-native-db-test-rw");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("also-does-not-exist.db");
        let _ = std::fs::remove_file(&path);

        assert!(open_read_write(&path).is_err());
        assert!(!path.exists());
    }

    /// `foreign_keys` is per connection and off by default. Without it the
    /// cascades in the schema silently stop firing, so a deleted chat keeps its
    /// messages and nothing anywhere reports a problem.
    #[test]
    fn the_write_handle_enforces_cascades() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        {
            let conn = Connection::open(file.path()).expect("open");
            conn.execute_batch(
                "CREATE TABLE parent (id TEXT PRIMARY KEY);
                 CREATE TABLE child (
                    id   INTEGER PRIMARY KEY AUTOINCREMENT,
                    pid  TEXT NOT NULL REFERENCES parent(id) ON DELETE CASCADE
                 );
                 INSERT INTO parent (id) VALUES ('p1');
                 INSERT INTO child (pid) VALUES ('p1');",
            )
            .expect("schema");
        }

        let conn = open_read_write(file.path()).expect("open rw");
        conn.execute("DELETE FROM parent WHERE id = 'p1'", [])
            .expect("delete");

        let orphans: i64 = conn
            .query_row("SELECT COUNT(*) FROM child", [], |row| row.get(0))
            .expect("count");
        assert_eq!(orphans, 0, "ON DELETE CASCADE did not fire");
    }

    /// A read handle opened against the same file must still refuse writes,
    /// so the two cannot be confused at a call site.
    #[test]
    fn the_read_handle_still_refuses_to_write() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        {
            let conn = Connection::open(file.path()).expect("open");
            conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY);")
                .expect("schema");
        }

        let conn = open_read_only(file.path()).expect("open ro");
        assert!(conn.execute("INSERT INTO t (id) VALUES (1)", []).is_err());
    }
}

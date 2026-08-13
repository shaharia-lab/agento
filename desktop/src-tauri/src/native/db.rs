//! Read-only access to the SQLite database the Go server owns.
//!
//! Ported endpoints read the *same* file the sidecar writes, rather than a copy
//! or a second schema — phase 3 moves the writes over, and until then a second
//! source of truth would be a bug factory.
//!
//! Read-only is not a stylistic choice. The Go server holds the file open, runs
//! migrations against it and seeds the pricing catalog on every startup; a
//! second writer would race those. `SQLITE_OPEN_READ_ONLY` makes that
//! impossible to get wrong by accident.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

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

    // Matches the Go side's busy_timeout. A checkpoint can briefly lock the
    // database, and failing a read over that would be worse than waiting.
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("setting busy_timeout: {e}"))?;

    Ok(conn)
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
}

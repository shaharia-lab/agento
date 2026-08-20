//! When a scan must re-read everything, ported from the staleness half of
//! `internal/claudesessions/scanner.go`.
//!
//! An incremental scan normally re-reads only what changed on disk. Three
//! things make that wrong, and each forces a **full** re-read of every
//! transcript even though no file moved:
//!
//! | marker | drifts when | why a re-read is the only fix |
//! |---|---|---|
//! | `scanner_version` | the scanner learns to extract something new | cached rows are simply missing the field |
//! | `pricing_rev` | any rate is added or corrected | cost is *stored*, and re-pricing needs each message's own model and timestamp — neither of which the row keeps |
//! | `idle_threshold_ms` | the user changes the idle gap | active durations are stored, computed under the old threshold |
//!
//! The last one is why this exists at all: a user-caused drift cannot be
//! expressed as a version constant, but it makes exactly the same rows stale.
//!
//! **The markers are recorded only after the re-read succeeds.** A scan that
//! fails leaves the drift recorded and retryable, rather than claiming freshness
//! it did not achieve.
//!
//! Invalidation zeroes each cached row's mtime rather than dropping the row.
//! That is deliberate: a zeroed row still exists, so the diff classifies it as
//! an **update** rather than a discovery — no spurious "new session" events for
//! a corpus that has been cached for months — and deletions are still detected.

use rusqlite::Connection;

use super::diff::CachedEntry;
use super::CURRENT_SCANNER_VERSION;

/// The revision a process with no pricing catalog reports.
///
/// It must never be treated as a drift: a process that cannot price anything
/// would otherwise re-read the whole corpus on every scan, forever.
pub const PRICING_REV_UNKNOWN: i64 = -1;

/// What the next scan has to invalidate.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CacheStaleness {
    /// The scanner extracts something the cached rows do not carry.
    pub reader: bool,
    /// The pricing catalog moved.
    pub pricing: bool,
    pub pricing_rev: i64,
    /// The idle-gap threshold moved.
    pub idle: bool,
    pub idle_ms: i64,
}

impl CacheStaleness {
    /// Whether anything at all forces a full re-read.
    pub fn any(&self) -> bool {
        self.reader || self.pricing || self.idle
    }
}

/// Compares the recorded markers against the live ones.
///
/// `live_pricing_rev` is the catalog's current fingerprint, or
/// [`PRICING_REV_UNKNOWN`]; `live_idle_ms` is the configured threshold.
pub fn detect_staleness(
    conn: &Connection,
    live_pricing_rev: i64,
    live_idle_ms: i64,
) -> CacheStaleness {
    let stored_version = stored_i64(conn, "scanner_version");
    let stored_pricing = stored_i64(conn, "pricing_rev");
    let stored_idle = stored_i64(conn, "idle_threshold_ms");

    CacheStaleness {
        reader: stored_version < CURRENT_SCANNER_VERSION,
        // An unpriced process must not loop.
        pricing: live_pricing_rev != PRICING_REV_UNKNOWN && stored_pricing != live_pricing_rev,
        pricing_rev: live_pricing_rev,
        // Zero — unreadable, or a row predating the column — differs from every
        // valid threshold, which is what produces exactly one forced re-read.
        idle: stored_idle != live_idle_ms,
        idle_ms: live_idle_ms,
    }
}

/// One marker column from the single metadata row. Unreadable means zero, which
/// is a value no live marker takes.
fn stored_i64(conn: &Connection, column: &str) -> i64 {
    let sql = format!("SELECT COALESCE({column}, 0) FROM claude_cache_metadata WHERE id = 1");
    conn.query_row(&sql, [], |r| r.get(0)).unwrap_or(0)
}

/// Zeroes every cached row's mtime so the next diff sees each file as modified.
///
/// The rows are **kept**, not dropped: a dropped row would come back as an
/// insert and re-fire a discovery event for a session that has been cached for
/// months, and the delete pass would lose the ability to detect a genuine
/// removal in the same scan.
pub fn invalidate_cached_mtimes(cached: &mut [CachedEntry]) {
    let zero = chrono::DateTime::UNIX_EPOCH;
    for entry in cached.iter_mut() {
        entry.mtime = zero;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A metadata table with the three markers set.
    fn conn_with(version: i64, pricing: i64, idle: i64) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE claude_cache_metadata (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 last_scanned_at DATETIME NOT NULL,
                 scanner_version INTEGER NOT NULL DEFAULT 0,
                 pricing_rev INTEGER NOT NULL DEFAULT 0,
                 idle_threshold_ms INTEGER NOT NULL DEFAULT 600000
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO claude_cache_metadata
                 (id, last_scanned_at, scanner_version, pricing_rev, idle_threshold_ms)
             VALUES (1, '2026-03-01', ?, ?, ?)",
            [version, pricing, idle],
        )
        .unwrap();
        conn
    }

    #[test]
    fn a_corpus_at_the_current_markers_is_fresh() {
        let conn = conn_with(CURRENT_SCANNER_VERSION, 42, 600_000);
        let stale = detect_staleness(&conn, 42, 600_000);
        assert!(!stale.any());
    }

    #[test]
    fn an_older_scanner_version_forces_a_re_read() {
        let conn = conn_with(CURRENT_SCANNER_VERSION - 1, 42, 600_000);
        assert!(detect_staleness(&conn, 42, 600_000).reader);
    }

    #[test]
    fn a_newer_stored_version_does_not() {
        // A downgrade should not thrash the corpus back and forth.
        let conn = conn_with(CURRENT_SCANNER_VERSION + 1, 42, 600_000);
        assert!(!detect_staleness(&conn, 42, 600_000).reader);
    }

    #[test]
    fn a_rate_edit_forces_a_re_read_because_cost_is_stored() {
        let conn = conn_with(CURRENT_SCANNER_VERSION, 42, 600_000);
        assert!(detect_staleness(&conn, 43, 600_000).pricing);
    }

    #[test]
    fn an_unpriced_process_never_reports_pricing_drift() {
        // Otherwise it would re-read the whole corpus on every single scan.
        let conn = conn_with(CURRENT_SCANNER_VERSION, 42, 600_000);
        let stale = detect_staleness(&conn, PRICING_REV_UNKNOWN, 600_000);
        assert!(!stale.pricing);
        assert!(!stale.any());
    }

    #[test]
    fn an_idle_threshold_change_forces_a_re_read() {
        let conn = conn_with(CURRENT_SCANNER_VERSION, 42, 600_000);
        assert!(detect_staleness(&conn, 42, 900_000).idle);
    }

    #[test]
    fn a_missing_metadata_row_reads_as_stale_exactly_once() {
        // Zero is a value no live marker takes, so the first scan re-reads and
        // records; the second is fresh.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE claude_cache_metadata (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 last_scanned_at DATETIME NOT NULL,
                 scanner_version INTEGER NOT NULL DEFAULT 0,
                 pricing_rev INTEGER NOT NULL DEFAULT 0,
                 idle_threshold_ms INTEGER NOT NULL DEFAULT 600000
             );",
        )
        .unwrap();
        let stale = detect_staleness(&conn, 42, 600_000);
        assert!(stale.reader && stale.pricing && stale.idle);
    }

    #[test]
    fn invalidation_keeps_the_rows_and_only_zeroes_their_mtimes() {
        let mut rows = vec![CachedEntry {
            file_path: "/d/s1.jsonl".into(),
            mtime: chrono::Utc::now(),
            is_subagent: false,
            config_dir: "/d".into(),
            session_id: "s1".into(),
            project_path: "/p".into(),
            agent_id: String::new(),
        }];
        invalidate_cached_mtimes(&mut rows);
        assert_eq!(rows.len(), 1, "dropping the row would re-fire a discovery");
        assert_eq!(rows[0].mtime, chrono::DateTime::UNIX_EPOCH);
    }
}

//! The scan Rust owns, and the two endpoints that report on it (#289).
//!
//! Mirrors `Cache.EnsureScan`, `Cache.Invalidate`, `Cache.ScanProgress`,
//! `Cache.ScanInProgress`, `Cache.CostsStale` and `Cache.LastScannedAt`
//! (`internal/claudesessions/cache.go`), over the scanner ported in #270.
//!
//! # This is the port's first ownership flip
//!
//! Every route before this one could forward on doubt: `Err` meant "let the
//! sidecar answer", so a ported route could only ever be as broken as an
//! unported one. **That property does not hold here.** Once the sidecar stops
//! scanning there is no second implementation to fall back to — forwarding
//! `/status` would ask Go about a scan Go is not running, and it would answer
//! `false`/`0`/`0` with complete confidence.
//!
//! So the flip has to be all of a piece: Rust scans, Rust answers, and the Go
//! scanner is switched off in the same change. A half-flip is the one state
//! that is worse than either side of it — two writers on one SQLite file, which
//! is exactly what `native::db` opening read-only was protecting against until
//! #274 made it read-write.
//!
//! # Why the freshness probe goes
//!
//! Answering a read natively used to remove the very thing that kept the corpus
//! fresh, because the Go handler behind it called `EnsureScan` on the way past.
//! `Answer::with_probe` put that back by firing a cheap request at the sidecar
//! afterwards. Now that the scan is here, those call sites call
//! [`ensure_scan`] directly — the same thing Go's own `Cache.List` does, one
//! process earlier. The probe, `PROBE_PATH` and the `Answer.probe` plumbing are
//! deleted rather than left dormant: a probe that still fired would ask a
//! sidecar that no longer scans to start a scan.
//!
//! # One scan at a time, and the request never waits for it
//!
//! `EnsureScan` admits exactly one scan under a short critical section and
//! returns immediately; the scan runs on its own thread. That is deliberate on
//! Go's side and reproduced here — at the target corpus size a scan is minutes,
//! so a first-run user would otherwise get a timeout and an empty list. It is
//! also what makes a double-click on Refresh harmless.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use axum::http::{Method, StatusCode};
use serde::Serialize;

use super::scanner::{apply, diff, staleness, store, walk, CURRENT_SCANNER_VERSION};
use super::writes::{finish, WriteError};

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "scan",
    claims,
    serve,
};

/// Go's `time.Time{}` as the driver renders it. `Invalidate` writes exactly
/// this, and `LastScannedAt` reports "never" for it — so the text is the
/// contract between the two, not an implementation detail.
const GO_ZERO_TIME: &str = "0001-01-01 00:00:00 +0000 UTC";

/// In-memory scan state, process-wide.
///
/// Process-wide rather than per-request for the same reason #276's live-session
/// registry is: `/status` has to observe the scan that `/refresh` started, and
/// they are different requests.
#[derive(Default)]
struct ScanState {
    in_progress: bool,
    files_done: usize,
    files_total: usize,
}

fn state() -> &'static Mutex<ScanState> {
    static STATE: OnceLock<Mutex<ScanState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(ScanState::default()))
}

/// `claudeSessionStatus`. A **struct** in Go, so this order is declaration
/// order rather than alphabetical.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Status {
    pub costs_stale: bool,
    pub scan_in_progress: bool,
    pub files_done: usize,
    pub files_total: usize,
    /// RFC 3339 in UTC, or empty when the cache has never been scanned — which
    /// is what `Invalidate` produces, since it writes the zero time.
    pub last_scanned_at: String,
}

fn claims(method: &Method, path: &str) -> bool {
    match path {
        "/api/claude-sessions/status" => method == Method::GET,
        "/api/claude-sessions/refresh" => method == Method::POST,
        _ => false,
    }
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    match (req.method, req.path) {
        (&Method::GET, "/api/claude-sessions/status") => {
            let body = super::gojson::to_vec(&status(&ctx.db_path))
                .map_err(|e| format!("encoding scan status: {e}"))?;
            Ok(super::Answer::json(body))
        }
        (&Method::POST, "/api/claude-sessions/refresh") => finish(refresh(&ctx.db_path)),
        _ => Err(format!("{} {} is not a scan route", req.method, req.path)),
    }
}

/// `handleGetClaudeSessionStatus`.
///
/// Every field degrades to the "nothing is happening" answer rather than
/// failing: this endpoint is polled by the sessions list, and a 500 there would
/// replace a progress line with an error on a page that is otherwise working.
pub fn status(db_path: &Path) -> Status {
    let (in_progress, files_done, files_total) = {
        let s = state().lock().unwrap_or_else(|e| e.into_inner());
        (s.in_progress, s.files_done, s.files_total)
    };
    let (costs_stale, last_scanned_at) = match super::db::open_read_only(db_path) {
        Ok(conn) => (costs_stale(&conn, db_path), last_scanned_at(&conn)),
        Err(_) => (false, String::new()),
    };
    Status {
        costs_stale,
        scan_in_progress: in_progress,
        files_done,
        files_total,
        last_scanned_at,
    }
}

/// `handleRefreshClaudeSessionCache`: invalidate, then start a scan. **202**,
/// with no body — Go writes the header directly rather than through `writeJSON`.
fn refresh(db_path: &Path) -> Result<super::Answer, WriteError> {
    let conn = super::db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    invalidate(&conn).map_err(WriteError::Fallback)?;
    drop(conn);

    ensure_scan(db_path.to_path_buf());
    Ok(super::Answer::status_only(StatusCode::ACCEPTED))
}

/// `Cache.Invalidate`: stamp `last_scanned_at` with the zero time.
///
/// Note it does **not** clear the cached rows. Invalidating is about the
/// freshness marker; the rows stay and are reconciled by the scan, which is
/// what keeps `custom_title` and `is_favorite` through a refresh.
fn invalidate(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute(
        "INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET last_scanned_at = excluded.last_scanned_at",
        [GO_ZERO_TIME],
    )
    .map(|_| ())
    .map_err(|e| format!("invalidating claude session cache: {e}"))
}

fn last_scanned_at(conn: &rusqlite::Connection) -> String {
    let stored: String = conn
        .query_row(
            "SELECT last_scanned_at FROM claude_cache_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    if stored.is_empty() || stored.starts_with("0001-01-01") {
        return String::new();
    }
    // Go reads the column into a `time.Time` and formats it RFC 3339 in UTC.
    match super::gotime::GoTime::parse_any(&stored) {
        Ok(t) => t
            .instant()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        // Unparseable is `time.Time{}` to Go's driver, and so "never".
        Err(_) => String::new(),
    }
}

/// `Cache.CostsStale` → `pricingChanged()`: the stored catalog fingerprint
/// differs from the live one.
fn costs_stale(conn: &rusqlite::Connection, db_path: &Path) -> bool {
    let live = live_pricing_rev(db_path);
    let idle_ms = super::settings::load(conn).idle_gap_ms;
    staleness::detect_staleness(conn, live, idle_ms).pricing
}

/// The catalog fingerprint the scanner compares against.
///
/// `PRICING_REV_UNKNOWN` on failure rather than a guess: `detect_staleness`
/// treats it as "do not decide", which stops an unpriced process re-reading the
/// whole corpus on every scan.
fn live_pricing_rev(db_path: &Path) -> i64 {
    super::pricing::catalog(db_path)
        .map(|c| c.revision)
        .unwrap_or(staleness::PRICING_REV_UNKNOWN)
}

/// `Cache.EnsureScan`: admit exactly one scan, and return immediately.
///
/// The critical section covers only the flag, never the scan — holding the lock
/// for the scan's duration would make `/status` block on the very thing it
/// exists to report.
pub fn ensure_scan(db_path: PathBuf) {
    {
        let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
        if s.in_progress {
            return;
        }
        s.in_progress = true;
        s.files_done = 0;
        s.files_total = 0;
    }
    std::thread::spawn(move || {
        if let Err(e) = run_scan(&db_path) {
            log::warn!("claude session scan failed: {e}");
        }
        let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
        s.in_progress = false;
    });
}

/// One full pass: walk, diff, re-read what changed, record the markers.
fn run_scan(db_path: &Path) -> Result<(), String> {
    let mut conn = super::db::open_read_write(db_path)?;
    super::migrate::verify(&conn)?;

    let live_pricing = live_pricing_rev(db_path);
    let idle_ms = super::settings::load(&conn).idle_gap_ms;
    let stale = staleness::detect_staleness(&conn, live_pricing, idle_ms);

    let dirs = super::settings::load(&conn).indexed_config_dirs;
    let walked = walk::walk_all_disk_files(&dirs);
    let mut cached: Vec<diff::CachedEntry> =
        store::load_cached_entries(&conn)?.into_values().collect();

    // A version bump, a rate edit or a threshold change leaves every cached row
    // incomplete without any file mtime changing, so the only correct response
    // is to re-read everything. Zeroing the mtimes is how that is expressed —
    // the rows stay, so a re-read is an update rather than a rediscovery.
    if stale.any() {
        staleness::invalidate_cached_mtimes(&mut cached);
    }
    let cached_by_path: std::collections::HashMap<PathBuf, diff::CachedEntry> = cached
        .into_iter()
        .map(|entry| (entry.file_path.clone(), entry))
        .collect();

    let default_dir = super::settings::default_claude_config_dir();
    let changes = diff::diff_disk_and_cache(&walked.files, &cached_by_path, &walked, &default_dir);

    let mut units: Vec<apply::ScanUnit> = Vec::new();
    for path in &changes.to_insert {
        if let Some(file) = walked.files.get(path) {
            units.push(apply::ScanUnit {
                file: file.clone(),
                is_new: true,
            });
        }
    }
    for path in &changes.to_update {
        if let Some(file) = walked.files.get(path) {
            units.push(apply::ScanUnit {
                file: file.clone(),
                is_new: false,
            });
        }
    }

    // A catalog that will not load prices nothing rather than pricing wrongly.
    let resolver = super::pricing::Resolver::load(&conn).ok();
    let outcome = apply::apply_changes(
        &mut conn,
        units,
        &changes.to_delete,
        resolver.as_ref(),
        idle_ms,
        |done, total| {
            let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
            s.files_done = done;
            s.files_total = total;
        },
    );

    record_markers(&conn, live_pricing, idle_ms);
    log::info!(
        "claude session scan complete: {} written, {} deleted, {} skipped",
        outcome.rows_written,
        outcome.rows_deleted,
        outcome.skipped
    );
    Ok(())
}

/// The three staleness markers plus the scan time.
///
/// Written **after** the changes apply, never before: a failed scan must leave
/// the drift recorded so the next one retries it, which is the property that
/// makes a rate edit eventually reach the figures.
fn record_markers(conn: &rusqlite::Connection, pricing_rev: i64, idle_ms: i64) {
    let now = super::gotime::now_go_text();
    let statements: [(&str, rusqlite::types::Value); 4] = [
        (
            "INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET last_scanned_at = excluded.last_scanned_at",
            now.into(),
        ),
        (
            "UPDATE claude_cache_metadata SET scanner_version = ?1 WHERE id = 1",
            CURRENT_SCANNER_VERSION.into(),
        ),
        (
            "UPDATE claude_cache_metadata SET pricing_rev = ?1 WHERE id = 1",
            pricing_rev.into(),
        ),
        (
            "UPDATE claude_cache_metadata SET idle_threshold_ms = ?1 WHERE id = 1",
            idle_ms.into(),
        ),
    ];
    for (sql, value) in statements {
        if let Err(e) = conn.execute(sql, [value]) {
            // Go logs and continues for each of these too: a lost marker costs
            // one redundant re-read, where failing the scan costs the corpus.
            log::warn!("recording scan marker failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_scan_routes_are_claimed() {
        assert!(claims(&Method::GET, "/api/claude-sessions/status"));
        assert!(claims(&Method::POST, "/api/claude-sessions/refresh"));
        // Method-specific: refresh is a POST and status is a GET.
        assert!(!claims(&Method::POST, "/api/claude-sessions/status"));
        assert!(!claims(&Method::GET, "/api/claude-sessions/refresh"));
        // The list and its siblings belong to `native::sessions`.
        assert!(!claims(&Method::GET, "/api/claude-sessions"));
        assert!(!claims(&Method::GET, "/api/claude-sessions/abc"));
    }

    fn migrated() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        super::super::migrate::apply(&mut conn).expect("migrate");
        file
    }

    /// `Invalidate` writes Go's zero time, and that is what "never scanned"
    /// means on the wire — an empty string, not a formatted year 1 date.
    #[test]
    fn invalidating_makes_last_scanned_at_empty_rather_than_year_one() {
        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute(
            "INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, '2026-03-01 10:00:00 +0000 UTC')
             ON CONFLICT(id) DO UPDATE SET last_scanned_at = excluded.last_scanned_at",
            [],
        )
        .expect("seed");
        assert_eq!(last_scanned_at(&conn), "2026-03-01T10:00:00Z");

        invalidate(&conn).expect("invalidate");
        assert_eq!(
            last_scanned_at(&conn),
            "",
            "the zero time is `never`, and must not be formatted as a date"
        );
    }

    /// A cache that has never been written at all is also "never".
    #[test]
    fn an_absent_metadata_row_is_never_scanned() {
        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute("DELETE FROM claude_cache_metadata", [])
            .expect("clear");
        assert_eq!(last_scanned_at(&conn), "");
    }

    /// The status shape is Go's struct, so the order is declaration order and
    /// not the alphabetical order a map would produce.
    #[test]
    fn the_status_shape_is_gos_struct_order() {
        let body = super::super::gojson::to_vec(&Status {
            costs_stale: true,
            scan_in_progress: true,
            files_done: 4,
            files_total: 9,
            last_scanned_at: "2026-03-01T10:00:00Z".to_string(),
        })
        .expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf-8").trim_end(),
            r#"{"costs_stale":true,"scan_in_progress":true,"files_done":4,"files_total":9,"last_scanned_at":"2026-03-01T10:00:00Z"}"#
        );
    }

    /// `EnsureScan` admits one scan, so a double-click cannot start a second
    /// full re-read on top of the first.
    #[test]
    fn a_second_ensure_scan_while_one_runs_is_a_no_op() {
        {
            let mut s = state().lock().expect("lock");
            s.in_progress = true;
            s.files_done = 3;
            s.files_total = 7;
        }
        // Would reset the counters to 0/0 if it had been admitted.
        ensure_scan(PathBuf::from("/nonexistent/agento.db"));
        let s = state().lock().expect("lock");
        assert!(s.in_progress);
        assert_eq!((s.files_done, s.files_total), (3, 7));
        drop(s);
        state().lock().expect("lock").in_progress = false;
    }
}

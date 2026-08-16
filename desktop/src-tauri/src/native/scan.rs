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
    /// When a scan last found **no readable config dir**.
    ///
    /// That path deliberately records no staleness markers — see `run_scan` —
    /// so the gate that admitted it is still open when the next request
    /// arrives, and every request would admit another. Go never notices because
    /// its equivalent is reached from `List`, which is called far less often
    /// than a gated request is here. A fresh install is exactly this state:
    /// `scanner_version = 0` and no `~/.claude` yet.
    ///
    /// This is a rate limit, not a marker: the drift stays recorded and stays
    /// retryable, which is the property the early return exists to preserve.
    no_dirs_at: Option<std::time::Instant>,
    files_done: usize,
    files_total: usize,
    /// Rows the last scan actually wrote.
    ///
    /// Exposed because `files_done` is **not** evidence of work: `apply` counts
    /// a file as done even when its batch fails to commit, so a scan that read
    /// everything and wrote nothing reaches `files_done == files_total`. That is
    /// precisely the failure the live test exists to catch, and it needs a
    /// number that only a successful write moves.
    rows_written: usize,
}

/// Rows the last completed scan wrote.
///
/// Not on [`Status`] — that is a wire shape Go defines and this is not one of
/// its fields. It exists so a test can tell a scan that did the work from one
/// that walked every file and committed nothing, which `files_done` cannot: a
/// file whose batch fails to commit is still counted done.
pub fn last_rows_written() -> usize {
    state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .rows_written
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

    // `force`: the user asked. The TTL check would fire anyway — `invalidate`
    // just zeroed the marker — but saying so here means a later change to the
    // gate cannot quietly turn Refresh into a no-op.
    force_scan(db_path.to_path_buf());
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
    match stored_scan_time(conn) {
        // Go reads the column into a `time.Time` and formats it RFC 3339 in UTC.
        Some(t) => t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        // Never scanned, unreadable, or the zero time — all of which Go reports
        // as an empty string rather than a year-1 date.
        None => String::new(),
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
    super::pricing::revision_of(db_path).unwrap_or(staleness::PRICING_REV_UNKNOWN)
}

/// Clears `in_progress` however the scan thread ends.
///
/// A plain reset at the end of the closure is **not** enough: `thread::spawn`
/// does not run the tail on an unwind, so a panicking scan would leave the flag
/// set for the life of the process — `ensure_scan` would then return at its
/// first check forever and `/status` would report a scan that is not running.
/// The panic path is reachable: `apply.rs` shares a `Mutex` across its reader
/// pool and `expect`s the lock, so one panicking reader cascades. Go is safe
/// here because its equivalent uses `defer`.
///
/// This works only because **every profile unwinds** — `panic = "abort"` runs no
/// destructors, so under it the guard would be a debug-only net and a scan panic
/// would kill the app outright. `Cargo.toml` sets `panic = "unwind"` for release
/// for this reason and for `proxy.rs`'s panic-to-forward, and says so there.
struct ScanGuard;

impl Drop for ScanGuard {
    fn drop(&mut self) {
        state()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_progress = false;
    }
}

/// `Cache.ensureFresh`: scan only when something says the cache is out of date.
///
/// **`ensureFresh`, not `EnsureScan`.** Go's read paths do not start a scan on
/// every request — they ask three questions first, and only then admit one.
/// Porting the admission without the questions makes every list, facet change,
/// dashboard and insights open kick off a full corpus walk, which at the size
/// this file is written for is minutes of work per page view, with the progress
/// counters resetting under the user each time.
///
/// The three questions are Go's, in `cache.go`:
///
/// 1. the last scan is older than [`CACHE_TTL`],
/// 2. the pricing catalog moved, or
/// 3. the idle-gap threshold moved.
///
/// `detect_staleness` already answers the last two; the TTL is the third.
/// `force` is how `/refresh` bypasses all of it — though in practice it has
/// already invalidated `last_scanned_at`, so the TTL check would fire anyway.
pub fn ensure_scan(db_path: PathBuf) {
    ensure_scan_inner(db_path, false)
}

/// `/refresh`: scan whatever the markers say.
pub fn force_scan(db_path: PathBuf) {
    ensure_scan_inner(db_path, true)
}

/// How long to wait before re-admitting a scan that found no readable dir.
///
/// The exact value is deliberately low-stakes: `/refresh` bypasses it, so this
/// is a floor on automatic retries and never a deadline the user is stuck
/// behind. Too long is bounded by a button the UI already has; too short only
/// wastes a cheap walk. Anything from seconds to minutes behaves the same.
///
/// **Known narrower edge, not fixed here:** this covers the no-readable-dir
/// return specifically, not the general case of "a scan that recorded no
/// markers". A `run_scan` that fails before applying — `open_read_write` denied
/// on a read-only filesystem while `open_read_only` still succeeds — leaves the
/// same gate open with no cooldown. It is rarer, it is what Go does too, and
/// naming it beats generalising a rate limit over failure paths that should be
/// loud rather than throttled.
const NO_DIRS_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

fn ensure_scan_inner(db_path: PathBuf, force: bool) {
    if !force && !needs_scan(&db_path) {
        return;
    }
    if !force {
        let s = state().lock().unwrap_or_else(|e| e.into_inner());
        if s.no_dirs_at
            .is_some_and(|at| at.elapsed() < NO_DIRS_COOLDOWN)
        {
            return;
        }
    }
    {
        let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
        if s.in_progress {
            return;
        }
        s.in_progress = true;
        s.files_done = 0;
        s.files_total = 0;
        s.rows_written = 0;
    }
    std::thread::spawn(move || {
        // Created first, so it clears the flag on an unwind as well as a return.
        let _guard = ScanGuard;
        if let Err(e) = run_scan(&db_path) {
            log::warn!("claude session scan failed: {e}");
        }
    });
}

/// Go's `CacheTTL`.
const CACHE_TTL: chrono::TimeDelta = chrono::TimeDelta::hours(1);

/// The `!isFresh() || pricingChanged() || idleThresholdChanged()` decision.
///
/// Unreadable means "scan": a database this cannot open is one whose freshness
/// it cannot vouch for, and Go's `isFresh` returns false on a failed read for
/// the same reason.
fn needs_scan(db_path: &Path) -> bool {
    let Ok(conn) = super::db::open_read_only(db_path) else {
        return true;
    };
    let idle_ms = super::settings::load(&conn).idle_gap_ms;
    let stale = staleness::detect_staleness(&conn, live_pricing_rev(db_path), idle_ms);
    // `ensureFresh` asks only about pricing and the threshold, because a *read*
    // should not pay for a scanner upgrade. But Go's boot scan does not go
    // through `ensureFresh` at all — `StartBackgroundScan` calls `EnsureScan`
    // directly, precisely so an upgraded binary re-reads what its new scanner
    // can extract. Gating the boot scan the same way as a read would have meant
    // a version bump silently waiting up to an hour for the TTL.
    //
    // Including `reader` here answers both: one decision instead of two, and a
    // version bump reaches the figures on whichever surface the user opens
    // first. It cannot loop — `record_markers` writes `scanner_version` under
    // exactly this flag, so it is true for one scan only.
    if stale.reader || stale.pricing || stale.idle {
        return true;
    }
    !is_fresh(&conn)
}

/// `Cache.isFresh`: scanned within [`CACHE_TTL`]. A missing or unparseable
/// marker is stale, which is what `Invalidate` relies on.
fn is_fresh(conn: &rusqlite::Connection) -> bool {
    // The **stored** column, not `last_scanned_at`'s output. That function
    // formats for the wire; routing the freshness decision through it would
    // couple the gate to a display format, so a change there would silently
    // move when scans happen.
    let Some(t) = stored_scan_time(conn) else {
        return false;
    };
    chrono::Utc::now().signed_duration_since(t) < CACHE_TTL
}

/// The stored scan time as an instant, or `None` for never/unreadable — which
/// includes the zero time `Invalidate` writes.
fn stored_scan_time(conn: &rusqlite::Connection) -> Option<chrono::DateTime<chrono::Utc>> {
    let stored: String = conn
        .query_row(
            "SELECT last_scanned_at FROM claude_cache_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .ok()?;
    if stored.is_empty() || stored.starts_with("0001-01-01") {
        return None;
    }
    super::gotime::GoTime::parse_any(&stored)
        .ok()
        .map(|t| t.instant())
}

/// One full pass: walk, diff, re-read what changed, record the markers.
fn run_scan(db_path: &Path) -> Result<(), String> {
    let mut conn = super::db::open_read_write(db_path)?;
    super::migrate::verify(&conn)?;

    let live_pricing = live_pricing_rev(db_path);
    let settings = super::settings::load(&conn);
    let idle_ms = settings.idle_gap_ms;
    let stale = staleness::detect_staleness(&conn, live_pricing, idle_ms);

    let walked = walk::walk_all_disk_files(&settings.indexed_config_dirs);

    // Not one configured dir could be listed. Go stamps `last_scanned_at` and
    // returns **without recording the staleness markers**, which is the part
    // that matters: recording them would claim a re-read that never happened,
    // so a pending version bump or rate edit would be dropped for good after a
    // single transient unreadable `~/.claude` — an unplugged drive looks exactly
    // like a deleted one from here.
    if walked.walked.is_empty() {
        log::warn!(
            "claude sessions: no readable claude config dir, keeping cached rows: {:?}",
            settings.indexed_config_dirs
        );
        record_last_scanned(&conn);
        // Deliberately no markers — see `record_markers`. That leaves the gate
        // open, so remember this happened and stop re-admitting for a while.
        state().lock().unwrap_or_else(|e| e.into_inner()).no_dirs_at =
            Some(std::time::Instant::now());
        return Ok(());
    }

    state().lock().unwrap_or_else(|e| e.into_inner()).no_dirs_at = None;

    let mut cached: Vec<diff::CachedEntry> =
        store::load_cached_entries(&conn)?.into_values().collect();

    // A version bump, a rate edit or a threshold change leaves every cached row
    // incomplete without any file mtime changing, so the only correct response
    // is to re-read everything. Zeroing the mtimes is how that is expressed —
    // the rows stay, so a re-read is an update rather than a rediscovery.
    if stale.any() {
        staleness::invalidate_cached_mtimes(&mut cached);
        // A threshold change is the one kind of staleness that also invalidates
        // the *insights*, and it has to: `turn_count` and every rhythm metric
        // derived from it are computed under the threshold. Without this the
        // insight worker never picks these rows up — its rescan selects on
        // `processor_version < CurrentProcessorVersion` alone — so the insights
        // would sit on the old threshold indefinitely while the sessions list
        // beside them showed the new one.
        if stale.idle {
            match conn.execute("UPDATE session_insights SET processor_version = 0", []) {
                Ok(rows) if rows > 0 => {
                    log::info!("claude sessions: {rows} insights queued for reprocessing")
                }
                Ok(_) => {}
                // Logged and continued, as Go does: losing this costs stale
                // insights, where failing the scan costs the corpus.
                Err(e) => log::warn!(
                    "claude sessions: failed to invalidate insights after idle-threshold change: {e}"
                ),
            }
        }
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

    // `outcome.notifications` is deliberately dropped. Go publishes them on its
    // event bus and the insight worker reacts at once; Rust has no bus, so the
    // desktop app relies on that worker's own 5-minute `rescanOutdated` loop
    // instead. The effect is bounded latency, not lost work — the rows are
    // written and carry their versions, so the loop finds them. An event path is
    // worth building when there is a second consumer; until then a bus with one
    // subscriber behind a 5-minute fallback is machinery for its own sake.
    state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .rows_written = outcome.rows_written;
    record_markers(&conn, &stale);
    log::info!(
        "claude session scan complete: {} written, {} deleted, {} skipped",
        outcome.rows_written,
        outcome.rows_deleted,
        outcome.skipped
    );
    Ok(())
}

/// The staleness markers, plus the scan time.
///
/// Written **after** the changes apply, never before: a failed scan must leave
/// the drift recorded so the next one retries it, which is the property that
/// makes a rate edit eventually reach the figures.
///
/// Each marker is written **only if that marker was stale**, mirroring Go's
/// `cacheStaleness.record`. Stamping all three unconditionally would be wrong in
/// one specific way: `pricing_rev` would be overwritten with
/// `PRICING_REV_UNKNOWN` whenever the catalog failed to load, discarding a
/// perfectly good revision and guaranteeing a re-read next time.
fn record_markers(conn: &rusqlite::Connection, stale: &staleness::CacheStaleness) {
    record_last_scanned(conn);
    if stale.reader {
        record_marker(conn, "scanner_version", CURRENT_SCANNER_VERSION);
    }
    if stale.pricing {
        record_marker(conn, "pricing_rev", stale.pricing_rev);
    }
    if stale.idle {
        record_marker(conn, "idle_threshold_ms", stale.idle_ms);
    }
}

/// `updateLastScanned`. Stamped even on the path where nothing was read, which
/// is what stops an unreadable corpus re-walking on every request.
fn record_last_scanned(conn: &rusqlite::Connection) {
    let now = super::gotime::now_go_text();
    if let Err(e) = conn.execute(
        "INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET last_scanned_at = excluded.last_scanned_at",
        [&now],
    ) {
        log::warn!("failed to update last_scanned_at: {e}");
    }
}

fn record_marker(conn: &rusqlite::Connection, column: &str, value: i64) {
    // The column name is one of three literals above, never user input.
    let sql = format!("UPDATE claude_cache_metadata SET {column} = ?1 WHERE id = 1");
    if let Err(e) = conn.execute(&sql, [value]) {
        // Go logs and continues for each of these too: a lost marker costs one
        // redundant re-read, where failing the scan costs the corpus.
        log::warn!("claude sessions: failed to record {column}: {e}");
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

    /// Serialises the two tests that touch the process-global [`state`].
    ///
    /// They flake against each other otherwise — one clears `in_progress` while
    /// the other is asserting on it, in both directions. The full suite happens
    /// not to hit it today, but `cargo test --lib scan` is the ordinary dev
    /// loop and the odds move with thread count; a flaky test guarding the
    /// port's panic-safety net is the worst place to leave one.
    ///
    /// `unwrap_or_else` rather than `unwrap`: one of these tests panics on
    /// purpose, which poisons the lock.
    fn scan_state_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn seed_scanned_at_for(db: &std::path::Path, conn: &rusqlite::Connection, text: &str) {
        conn.execute(
            "INSERT INTO claude_cache_metadata (id, last_scanned_at, scanner_version,
                                                pricing_rev, idle_threshold_ms)
             VALUES (1, ?1, ?2, ?3, 600000)
             ON CONFLICT(id) DO UPDATE SET
                last_scanned_at = excluded.last_scanned_at,
                scanner_version = excluded.scanner_version,
                pricing_rev = excluded.pricing_rev",
            rusqlite::params![text, CURRENT_SCANNER_VERSION, live_pricing_rev(db)],
        )
        .expect("seed");
    }

    /// A cache scanned inside the TTL, with every marker current, needs nothing.
    ///
    /// This is the half that stops the app doing a corpus walk per request —
    /// and the half whose absence review caught, so it is pinned rather than
    /// assumed.
    #[test]
    fn a_fresh_cache_with_current_markers_does_not_need_a_scan() {
        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let now = super::super::gotime::now_go_text();
        seed_scanned_at_for(file.path(), &conn, &now);
        drop(conn);
        assert!(
            !needs_scan(file.path()),
            "a cache scanned just now, at the current markers, must not rescan"
        );
    }

    /// …and each of the four reasons, on its own, is enough.
    #[test]
    fn every_reason_to_rescan_is_enough_on_its_own() {
        // 1. The TTL expired.
        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        seed_scanned_at_for(file.path(), &conn, "2020-01-01 00:00:00 +0000 UTC");
        drop(conn);
        assert!(needs_scan(file.path()), "an expired TTL must rescan");

        // 2. Never scanned at all — the state `Invalidate` leaves behind.
        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let now = super::super::gotime::now_go_text();
        seed_scanned_at_for(file.path(), &conn, &now);
        invalidate(&conn).expect("invalidate");
        drop(conn);
        assert!(
            needs_scan(file.path()),
            "an invalidated cache must rescan, or Refresh would do nothing"
        );

        // 3. The scanner version moved. `ensureFresh` does *not* ask this, but
        // Go's boot scan re-reads unconditionally, so the gate has to — else a
        // version bump waits up to an hour behind the TTL.
        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let now = super::super::gotime::now_go_text();
        seed_scanned_at_for(file.path(), &conn, &now);
        conn.execute(
            "UPDATE claude_cache_metadata SET scanner_version = ?1 WHERE id = 1",
            [CURRENT_SCANNER_VERSION - 1],
        )
        .expect("age the version");
        drop(conn);
        assert!(
            needs_scan(file.path()),
            "an older scanner version must rescan on any surface, not just at boot"
        );

        // 4. The idle threshold moved.
        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let now = super::super::gotime::now_go_text();
        seed_scanned_at_for(file.path(), &conn, &now);
        conn.execute(
            "UPDATE claude_cache_metadata SET idle_threshold_ms = 999 WHERE id = 1",
            [],
        )
        .expect("move the threshold");
        drop(conn);
        assert!(
            needs_scan(file.path()),
            "a moved idle threshold must rescan"
        );
    }

    /// A database this cannot open is one whose freshness it cannot vouch for.
    #[test]
    fn an_unreadable_database_needs_a_scan() {
        assert!(needs_scan(std::path::Path::new("/nonexistent/agento.db")));
    }

    /// The guard clears the flag however the thread ends — including a panic,
    /// which `thread::spawn` would otherwise leave set for the process lifetime,
    /// wedging every future scan and making `/status` report one forever.
    #[test]
    fn the_scan_guard_clears_the_flag_on_a_panic() {
        let _serialised = scan_state_lock();
        state().lock().expect("lock").in_progress = true;
        let handle = std::thread::spawn(|| {
            let _guard = ScanGuard;
            panic!("a scan blew up");
        });
        assert!(handle.join().is_err(), "the thread should have panicked");
        assert!(
            !state().lock().expect("lock").in_progress,
            "a panicking scan must not leave the scan wedged — this needs \
             panic=unwind in every profile, see Cargo.toml"
        );
    }

    /// The cooldown is the one decision in this file that nothing else pins.
    ///
    /// It exists because the no-readable-dir return records **no markers** — so
    /// the gate that admitted the scan is still open, and without this every
    /// gated request would admit another. A fresh install with no `~/.claude`
    /// is exactly that state. The four properties below are the whole contract:
    /// the drift must survive, the gate must stay open, automatic retries must
    /// be refused, and `/refresh` must still get through.
    #[test]
    fn a_scan_with_no_readable_dir_arms_a_cooldown_without_recording_markers() {
        let _serialised = scan_state_lock();
        // `HOME` is a second global, and `paths::tests` read it — without this
        // they fail on a value this test swapped underneath them.
        let _env = crate::paths::tests::env_lock();

        // A home with no `.claude`, so the walk finds nothing to list.
        let home = tempfile::tempdir().expect("tempdir");
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let file = migrated();
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let now = super::super::gotime::now_go_text();
        seed_scanned_at_for(file.path(), &conn, &now);
        // An out-of-date scanner version, so the gate is open for a reason that
        // only a recorded marker could close.
        conn.execute(
            "UPDATE claude_cache_metadata SET scanner_version = ?1 WHERE id = 1",
            [CURRENT_SCANNER_VERSION - 1],
        )
        .expect("age the version");
        drop(conn);

        state().lock().expect("lock").no_dirs_at = None;
        run_scan(file.path()).expect("a scan with nothing to walk still succeeds");

        // (a) The marker did not advance: the drift is still recorded.
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let version: i64 = conn
            .query_row(
                "SELECT scanner_version FROM claude_cache_metadata WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .expect("scanner_version");
        assert_eq!(
            version,
            CURRENT_SCANNER_VERSION - 1,
            "a scan that listed nothing must not claim a re-read it never did"
        );
        drop(conn);

        // (b) …so the gate is still open and the work stays retryable.
        assert!(
            needs_scan(file.path()),
            "the drift must survive, or an unplugged drive drops a pending re-read for good"
        );

        // (c) But an automatic retry is refused, which is the point.
        state().lock().expect("lock").files_done = 42;
        ensure_scan(file.path().to_path_buf());
        assert_eq!(
            state().lock().expect("lock").files_done,
            42,
            "a gated request inside the cooldown must not admit another scan"
        );

        // (d) …while the user asking explicitly still gets through.
        force_scan(file.path().to_path_buf());
        for _ in 0..200 {
            if state().lock().expect("lock").files_done != 42 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_ne!(
            state().lock().expect("lock").files_done,
            42,
            "/refresh must bypass the cooldown, or the button would do nothing"
        );

        // Leave the process as we found it for the tests sharing this state.
        for _ in 0..200 {
            if !state().lock().expect("lock").in_progress {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        {
            let mut s = state().lock().expect("lock");
            s.no_dirs_at = None;
            s.files_done = 0;
        }
        match previous_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// `EnsureScan` admits one scan, so a double-click cannot start a second
    /// full re-read on top of the first.
    #[test]
    fn a_second_ensure_scan_while_one_runs_is_a_no_op() {
        let _serialised = scan_state_lock();
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

//! The `api_tokens` rows, and the revocation set the guard consults (#405).
//!
//! A user token is a JWT like any other, so **nothing here stores the
//! credential** — not the token, not a hash of it. The signature and the claims
//! are the whole check; what a row adds is the two things a signature cannot
//! carry: a name the user recognises, and a `jti` that can be struck off before
//! the token expires.
//!
//! # The revocation set is in memory, and that is a correctness argument
//!
//! `guards::reject` runs on the request path and is **synchronous** — it is
//! called from `proxy::dispatch`, which has no place to await a query. A
//! `SELECT` per request would also put SQLite in front of every read the UI
//! polls for.
//!
//! So revocation lives in a process-wide `RwLock<HashSet<String>>`, loaded once
//! at startup and updated by the writes that change it. That is authoritative
//! rather than a cache, and for one specific reason: **this process is the only
//! writer of `api_tokens`.** `main`'s Go server has never heard of the table
//! (migration 31 is this branch's own), and a second desktop instance cannot
//! exist — `tauri-plugin-single-instance` focuses the first window instead. If
//! either of those ever stops being true, this set becomes a cache and needs an
//! invalidation story; there is no such story today because there is no such
//! writer.
//!
//! # Absence is not revocation
//!
//! A `jti` with **no row** is served. That is what lets the app's own webview
//! session be a self-signed JWT with nothing in the database — and it is why
//! `DELETE /api/security/tokens/{id}` sets `revoked_at` rather than deleting the
//! row. Deleting it would put the `jti` back in the "no row" bucket and
//! *un-revoke* the credential.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock, RwLock};

use rusqlite::Connection;

use super::token::Scope;
use crate::native::gotime;

/// Every revoked `jti`. See the module header for why this is authoritative.
static REVOKED: RwLock<Option<HashSet<String>>> = RwLock::new(None);

/// Whether this `jti` has been revoked.
///
/// **Fails closed on a poisoned lock**, which is the only way this can fail: a
/// panic while the set was being written leaves it unreadable, and answering
/// "not revoked" there would serve every revoked token in the install. Answering
/// "revoked" costs a user a 401 and a restart.
pub fn is_revoked(jti: &str) -> bool {
    match REVOKED.read() {
        Ok(guard) => guard.as_ref().is_some_and(|set| set.contains(jti)),
        Err(_) => true,
    }
}

/// Load the revoked set from the database, replacing whatever is there.
///
/// Called at startup and after a regenerate (where the set is emptied, because
/// every token is dead by signature and keeping stale entries would only grow).
pub fn load_revoked(conn: &Connection) -> Result<(), String> {
    let mut stmt = conn
        .prepare("SELECT jti FROM api_tokens WHERE revoked_at IS NOT NULL")
        .map_err(|e| format!("preparing revoked token query: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("reading revoked tokens: {e}"))?;
    let mut set = HashSet::new();
    for row in rows {
        set.insert(row.map_err(|e| format!("reading revoked tokens: {e}"))?);
    }
    if let Ok(mut guard) = REVOKED.write() {
        *guard = Some(set);
    }
    Ok(())
}

/// Add one `jti` to the set, so a revoke takes effect on the very next request
/// rather than at the next launch.
pub fn mark_revoked(jti: &str) {
    if let Ok(mut guard) = REVOKED.write() {
        guard
            .get_or_insert_with(HashSet::new)
            .insert(jti.to_string());
    }
}

/// Empty the set. Only [`super::keys::regenerate`]'s caller does this: every
/// token is already dead by signature, so the entries are pure growth.
pub fn clear_revoked() {
    if let Ok(mut guard) = REVOKED.write() {
        *guard = Some(HashSet::new());
    }
}

/// One `api_tokens` row, as the API renders it.
///
/// **No token field, and there never can be one** — see the module header. The
/// only moment a token string exists is the response to its own creation.
///
/// # The timestamps are stored one way and served another
///
/// A DATETIME column here holds `time.Time.String()` text —
/// `2026-08-23 11:10:09.162077311 +0000 UTC` — because that is what every other
/// DATETIME in this schema holds, and `ORDER BY created_at DESC` sorts these
/// columns **as text**. A second writer stamping some other spelling into the
/// same table would sort into a different place than the rows around it, which
/// is a silently wrong list rather than an error.
///
/// That text is **not** something `new Date()` can parse, and these routes are
/// desktop-only with no Go counterpart to be byte-compatible with, so they are
/// converted to RFC 3339 on the way out by [`wire_time`]. Serving the stored
/// text was the first version of this, and every column in the Security tab's
/// table rendered as an em dash — the failure a type cannot catch, because both
/// spellings are a `String`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenRow {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub created_at: String,
    /// `null` when the token never expires, which nothing mints today but the
    /// column allows.
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// A stored DATETIME as RFC 3339, for the wire.
///
/// An unparsable value passes through unchanged rather than becoming `null`: it
/// is a column a human may have edited, and showing the raw text is more useful
/// than erasing it. Nothing downstream parses it beyond formatting for display.
pub fn wire_time(stored: &str) -> String {
    match gotime::GoTime::parse_any(stored) {
        Ok(t) => t.to_rfc3339_nano(),
        Err(_) => stored.to_string(),
    }
}

impl TokenRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let optional = |raw: Option<String>| raw.as_deref().map(wire_time);
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            scope: row.get(2)?,
            created_at: wire_time(&row.get::<_, String>(3)?),
            expires_at: optional(row.get(4)?),
            last_used_at: optional(row.get(5)?),
            revoked_at: optional(row.get(6)?),
        })
    }
}

const COLUMNS: &str =
    "id, name, scope, created_at, expires_at, last_used_at, revoked_at FROM api_tokens";

/// Every token, newest first. Revoked ones are included deliberately: the list
/// is a record of what was issued, and a revoked row vanishing would make a
/// revoke look like a deletion the user did not perform.
pub fn list(conn: &Connection) -> Result<Vec<TokenRow>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLUMNS} ORDER BY created_at DESC, id DESC"
        ))
        .map_err(|e| format!("preparing token list: {e}"))?;
    let rows = stmt
        .query_map([], TokenRow::from_row)
        .map_err(|e| format!("listing tokens: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("listing tokens: {e}"))
}

/// One token by id, or `None`.
pub fn get(conn: &Connection, id: &str) -> Result<Option<TokenRow>, String> {
    let mut stmt = conn
        .prepare(&format!("SELECT {COLUMNS} WHERE id = ?1"))
        .map_err(|e| format!("preparing token read: {e}"))?;
    let mut rows = stmt
        .query_map([id], TokenRow::from_row)
        .map_err(|e| format!("reading token: {e}"))?;
    match rows.next() {
        Some(row) => Ok(Some(row.map_err(|e| format!("reading token: {e}"))?)),
        None => Ok(None),
    }
}

/// Record a newly minted token. The caller has the token string; this never
/// sees it.
#[allow(clippy::too_many_arguments)]
pub fn insert(
    conn: &Connection,
    id: &str,
    name: &str,
    scope: Scope,
    jti: &str,
    created_at: &str,
    expires_at: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO api_tokens (id, name, scope, jti, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![id, name, scope.as_str(), jti, created_at, expires_at],
    )
    .map_err(|e| format!("inserting token: {e}"))?;
    Ok(())
}

/// Revoke a token. Returns the revoked `jti`, or `None` when there is no such
/// row — which is the 404, not a silent success.
///
/// Revoking an already-revoked token keeps the original `revoked_at`: the
/// timestamp records when the credential stopped working, and a second call
/// must not move it.
pub fn revoke(conn: &Connection, id: &str) -> Result<Option<String>, String> {
    let jti: Option<String> = conn
        .query_row("SELECT jti FROM api_tokens WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(format!("reading token: {other}")),
        })?;
    let Some(jti) = jti else { return Ok(None) };

    conn.execute(
        "UPDATE api_tokens SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        rusqlite::params![id, gotime::now_go_text()],
    )
    .map_err(|e| format!("revoking token: {e}"))?;
    mark_revoked(&jti);
    Ok(Some(jti))
}

/// How often one token's `last_used_at` is written, in seconds.
///
/// The guard sees every request, and the sessions list polls
/// `GET /api/claude-sessions/status` on a timer for the whole length of a scan —
/// so an unthrottled update would be a write per poll, on the request path, for
/// a column whose entire purpose is answering "has this token been used lately".
/// A minute's resolution answers that just as well.
const LAST_USED_INTERVAL: i64 = 60;

/// When each `jti` was last written, so the interval above can be enforced
/// without asking the database.
fn last_written() -> &'static Mutex<HashMap<String, i64>> {
    static SEEN: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    SEEN.get_or_init(Mutex::default)
}

/// Whether `last_used_at` for this `jti` is due to be written.
fn due(jti: &str, now: i64) -> bool {
    let Ok(mut seen) = last_written().lock() else {
        return false;
    };
    match seen.get(jti) {
        Some(&at) if now - at < LAST_USED_INTERVAL => false,
        _ => {
            seen.insert(jti.to_string(), now);
            true
        }
    }
}

/// Record that a token was just used, off the request path.
///
/// **Nothing awaits this**, and nothing may: it is called from the guard, which
/// is what stands between a request and its answer. The write goes through
/// [`crate::native::db::blocking`] because `open_read_write` sets a five-second
/// busy timeout, so a call meeting a lock held by the session scanner's batch
/// writer parks its thread for up to that long — the rule `CLAUDE.md` records
/// for the scheduler and the trigger dispatcher, and this is a third caller
/// reached from neither a request handler nor a timer but from the guard itself.
///
/// A failure is logged at debug and dropped. The column is a signal, not a
/// record: losing one update costs a stale timestamp, and failing the request
/// over it would make an unwritable database an outage.
pub fn touch(db_path: &Path, jti: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    if !due(jti, now) {
        return;
    }
    // The guard is sync and is also called from unit tests, which have no
    // runtime — so this asks rather than assuming, instead of panicking inside
    // `tokio::spawn`.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let db_path = db_path.to_path_buf();
    let jti = jti.to_string();
    handle.spawn(async move {
        crate::native::db::blocking("api token last_used_at", move || {
            let conn = match crate::native::db::open_read_write(&db_path) {
                Ok(conn) => conn,
                Err(e) => {
                    log::debug!("api token last_used_at: {e}");
                    return;
                }
            };
            if let Err(e) = conn.execute(
                "UPDATE api_tokens SET last_used_at = ?2 WHERE jti = ?1",
                rusqlite::params![jti, gotime::now_go_text()],
            ) {
                log::debug!("api token last_used_at: {e}");
            }
        })
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn
    }

    /// Insert a token with a `jti` **unique across the whole test binary**.
    ///
    /// Each test gets its own in-memory database, so the rows cannot collide —
    /// but [`REVOKED`] is process-wide and `cargo test` runs these in parallel,
    /// so a `jti` derived from the row id alone put two tests on one entry. That
    /// is exactly how it failed: a test that revokes `jti-t1` made a *different*
    /// test's `assert!(!is_revoked(&jti))` fail, intermittently and nowhere near
    /// the cause.
    ///
    /// Deriving it from a counter rather than the id keeps the shared static
    /// shared — which is what these tests are partly about — while making the
    /// keys disjoint.
    fn add(conn: &Connection, id: &str, name: &str, scope: Scope) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let jti = format!("jti-{id}-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        insert(
            conn,
            id,
            name,
            scope,
            &jti,
            &gotime::now_go_text(),
            &gotime::now_go_text(),
        )
        .expect("insert");
        jti
    }

    #[test]
    fn migration_31_creates_the_table() {
        let conn = schema();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='api_tokens'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(count, 1);
    }

    #[test]
    fn a_token_is_stored_listed_and_read_back() {
        let conn = schema();
        add(&conn, "t1", "CI runner", Scope::Read);

        let all = list(&conn).expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "t1");
        assert_eq!(all[0].name, "CI runner");
        assert_eq!(all[0].scope, "read");
        assert!(all[0].revoked_at.is_none());

        assert!(get(&conn, "t1").expect("get").is_some());
        assert!(get(&conn, "nope").expect("get").is_none());
    }

    /// **Every scope stores and lists, including `llm`** (#423).
    ///
    /// The column is free-text and was already wide enough, which is why the
    /// gateway scope needed no migration — but "wide enough" is a claim about
    /// the schema, and this is what checks it against the code that writes and
    /// reads the column. A scope that mints and then lists as something else
    /// would show the wrong badge in the Security tab and, worse, revoke the
    /// wrong thing.
    #[test]
    fn every_scope_round_trips_through_the_column() {
        let conn = schema();
        for (id, scope) in [
            ("t-read", Scope::Read),
            ("t-write", Scope::Write),
            ("t-llm", Scope::Llm),
        ] {
            add(&conn, id, "tool config", scope);
            let row = get(&conn, id).expect("get").expect("row");
            assert_eq!(
                row.scope,
                scope.as_str(),
                "{scope:?} must read back as its own wire spelling"
            );
            assert_eq!(
                Scope::parse(&row.scope),
                Some(scope),
                "and must parse back to the same variant"
            );
        }
        assert_eq!(list(&conn).expect("list").len(), 3);
    }

    /// **Every timestamp on the wire is RFC 3339**, not the `time.Time.String()`
    /// text the column holds.
    ///
    /// A type cannot catch this — both spellings are a `String` — and the
    /// failure is silent in the only place it shows: `new Date()` returns an
    /// invalid date for the stored form, and `relativeTime` renders that as an
    /// em dash. The first version of this served the column verbatim and every
    /// date in the Security tab's table was a dash.
    #[test]
    fn the_wire_timestamps_are_rfc_3339_rather_than_the_stored_text() {
        let conn = schema();
        add(&conn, "t1", "n", Scope::Read);
        revoke(&conn, "t1").expect("revoke");
        conn.execute(
            "UPDATE api_tokens SET last_used_at = ?1 WHERE id = 't1'",
            [gotime::now_go_text()],
        )
        .expect("touch");

        let row = &list(&conn).expect("list")[0];
        for (what, value) in [
            ("created_at", Some(row.created_at.clone())),
            ("expires_at", row.expires_at.clone()),
            ("last_used_at", row.last_used_at.clone()),
            ("revoked_at", row.revoked_at.clone()),
        ] {
            let value = value.unwrap_or_else(|| panic!("{what} should be set"));
            assert!(
                !value.contains(" UTC"),
                "{what} is still the stored Go text: {value:?}"
            );
            assert!(
                chrono::DateTime::parse_from_rfc3339(&value).is_ok(),
                "{what} must be RFC 3339 for the frontend to render it: {value:?}"
            );
        }

        // ...and the column itself is untouched, because `ORDER BY created_at`
        // sorts these as text alongside every other DATETIME in the schema.
        let stored: String = conn
            .query_row(
                "SELECT created_at FROM api_tokens WHERE id = 't1'",
                [],
                |r| r.get(0),
            )
            .expect("stored");
        assert!(stored.ends_with(" UTC"), "stored: {stored:?}");
    }

    /// A value neither parser understands is passed through rather than erased:
    /// it is a column a human may have edited, and the raw text says more than a
    /// `null` does.
    #[test]
    fn an_unparsable_timestamp_survives_the_conversion() {
        assert_eq!(wire_time("not a timestamp"), "not a timestamp");
        assert_eq!(wire_time(""), "");
    }

    /// The whole row shape exists to prove one negative: the credential is not
    /// in the database, in any column, in any form.
    #[test]
    fn no_column_holds_the_token() {
        let conn = schema();
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('api_tokens')")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("columns");
        for forbidden in ["token", "token_hash", "secret", "credential", "jwt"] {
            assert!(
                !columns.iter().any(|c| c == forbidden),
                "api_tokens must not have a {forbidden:?} column"
            );
        }
        assert!(columns.iter().any(|c| c == "jti"));
    }

    /// A revoke is an `UPDATE`, never a `DELETE`. Deleting the row would put the
    /// `jti` back in the "no row, therefore fine" bucket and **un-revoke the
    /// credential** — the one mistake this table's shape exists to prevent.
    #[test]
    fn revoking_keeps_the_row_and_marks_the_jti() {
        let conn = schema();
        let jti = add(&conn, "t1", "CI runner", Scope::Write);
        assert!(!is_revoked(&jti));

        let revoked = revoke(&conn, "t1").expect("revoke");
        assert_eq!(revoked.as_deref(), Some(jti.as_str()));
        assert!(is_revoked(&jti));

        let all = list(&conn).expect("list");
        assert_eq!(all.len(), 1, "the row survives the revoke");
        assert!(all[0].revoked_at.is_some());
    }

    #[test]
    fn revoking_an_unknown_token_is_not_a_silent_success() {
        let conn = schema();
        assert_eq!(revoke(&conn, "nope").expect("revoke"), None);
    }

    /// `revoked_at` records when the credential stopped working, so a second
    /// revoke must not move it.
    #[test]
    fn revoking_twice_keeps_the_first_timestamp() {
        let conn = schema();
        add(&conn, "t1", "n", Scope::Read);
        revoke(&conn, "t1").expect("first");
        let first = get(&conn, "t1").expect("get").expect("row").revoked_at;
        revoke(&conn, "t1").expect("second");
        let second = get(&conn, "t1").expect("get").expect("row").revoked_at;
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    /// The set is rebuilt from the rows at startup, so a revoke made in a
    /// previous launch is still in force in this one.
    #[test]
    fn the_revoked_set_is_rebuilt_from_the_rows() {
        let conn = schema();
        conn.execute(
            "INSERT INTO api_tokens (id, name, scope, jti, created_at, revoked_at) \
             VALUES ('t9', 'n', 'read', 'jti-from-a-previous-launch', ?1, ?1)",
            [gotime::now_go_text()],
        )
        .expect("insert");

        clear_revoked();
        assert!(!is_revoked("jti-from-a-previous-launch"));
        load_revoked(&conn).expect("load");
        assert!(is_revoked("jti-from-a-previous-launch"));
    }

    /// A `jti` with no row is served — which is what lets the app's own session
    /// be a self-signed JWT with nothing in the database.
    #[test]
    fn a_jti_with_no_row_is_not_revoked() {
        let conn = schema();
        load_revoked(&conn).expect("load");
        assert!(!is_revoked("some-jti-nobody-recorded"));
    }

    /// The throttle is what keeps a per-request column off the request path.
    /// Asserted as a pure function of the two clocks, because the write itself
    /// is spawned and a test of it would race.
    #[test]
    fn last_used_is_written_at_most_once_a_minute() {
        let jti = "throttle-probe";
        let now = 1_000_000;
        assert!(due(jti, now), "the first use is always due");
        assert!(!due(jti, now + 1));
        assert!(!due(jti, now + LAST_USED_INTERVAL - 1));
        assert!(due(jti, now + LAST_USED_INTERVAL));
    }

    /// `touch` is called from the guard, which is sync and runs in tests with no
    /// tokio runtime. It must return rather than panic inside `spawn`.
    #[test]
    fn touch_outside_a_runtime_is_a_no_op() {
        touch(Path::new("/nonexistent/agento.db"), "no-runtime-probe");
    }
}

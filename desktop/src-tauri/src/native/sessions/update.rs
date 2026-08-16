//! `PATCH /api/claude-sessions/{id}`, ported from `handleUpdateClaudeSession`
//! (`internal/api/claude_sessions.go`).
//!
//! The two mutable columns on a cached Claude session — everything else is
//! derived from the transcript and is read-only.
//!
//! # Why this moved, when #293 deferred it
//!
//! #293's rule is that a route moves only when Rust can reproduce **every**
//! effect it has, and it deferred this one on the grounds that it "goes through
//! `claudeSessionCache`". Reading what it goes through is what settles it:
//! `Cache.UpdateCustomTitle` and `Cache.UpdateFavorite` are each a single
//! `UPDATE` against the SQLite file and nothing else. The `Cache` struct holds
//! no session corpus in memory — a `*sql.DB`, the scan flag, the progress
//! counters and the analytics memo, which no title or favourite appears in — so
//! there is no Go-side state for a native write to leave stale. The mutex those
//! two take guards Go's own short statements against each other, which a second
//! process handles with `busy_timeout` instead.
//!
//! The other half of the argument is that the **scanner cannot fight it**:
//! `custom_title` and `is_favorite` are in neither of the scanner's write lists
//! — they are the only columns there the user typed — so the shell's own scan
//! (#289) never overwrites what this route stores.
//!
//! # Four behaviours that are Go's, not obvious, and all reproduced
//!
//! - **An unknown session id is `204`, not `404`.** The `UPDATE` matches no row
//!   and that is not an error, and the handler never checks first.
//! - **The two fields are pointers**, so `null` and *absent* are the same thing
//!   and both mean "not updating this field". A body of `{"custom_title":null}`
//!   is therefore `no fields to update` — a 400 — where `{"custom_title":""}`
//!   sets an empty title.
//! - **The title is `TrimSpace`d** before it is stored, so `"  hi  "` becomes
//!   `"hi"` and `"   "` becomes `""`, which resolves the display title back to
//!   the next fallback.
//! - **`204` carries no body and no `Content-Type`**: the handler calls
//!   `w.WriteHeader` directly rather than going through `writeJSON`.

use serde::Deserialize;

use crate::native::writes::{decode_body, WriteError};
use crate::native::{db, Answer};

/// The anonymous struct `handleUpdateClaudeSession` decodes into.
///
/// `Option` for both, because Go uses `*string`/`*bool` and the nil check is
/// the whole of the "no fields to update" rule.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UpdateRequest {
    custom_title: Option<String>,
    is_favorite: Option<bool>,
}

pub fn update(
    db_path: &std::path::Path,
    session_id: &str,
    body: &[u8],
) -> Result<Answer, WriteError> {
    let req = decode_body::<UpdateRequest>(body)?;
    if req.custom_title.is_none() && req.is_favorite.is_none() {
        return Err(WriteError::BadRequest("no fields to update".to_string()));
    }

    let mut conn = db::open_read_write(db_path)
        .map_err(|e| WriteError::Fallback(format!("opening database: {e}")))?;
    crate::native::migrate::verify(&conn).map_err(WriteError::Fallback)?;

    // Go runs the two statements separately, so a failed second one leaves the
    // first applied. One transaction is the safe divergence: `Err` forwards, and
    // a half-applied update followed by Go re-applying both reaches the same
    // rows either way — but only "both or neither" keeps the rule that a native
    // write fails *before* it mutates.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin session update: {e}")))?;

    if let Some(title) = &req.custom_title {
        // `strings.TrimSpace`. Go trims Unicode whitespace, which is what
        // `str::trim` trims.
        tx.execute(
            "UPDATE claude_session_cache SET custom_title = ?1 WHERE session_id = ?2",
            rusqlite::params![title.trim(), session_id],
        )
        .map_err(|e| WriteError::Fallback(format!("updating custom title: {e}")))?;
    }
    if let Some(is_favorite) = req.is_favorite {
        tx.execute(
            "UPDATE claude_session_cache SET is_favorite = ?1 WHERE session_id = ?2",
            rusqlite::params![is_favorite, session_id],
        )
        .map_err(|e| WriteError::Fallback(format!("updating favourite: {e}")))?;
    }

    // Nothing below this line may return `Fallback`. `no_content` allocates
    // nothing and cannot fail.
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit session update: {e}")))?;

    Ok(Answer::no_content())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    fn migrated_with_session(id: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity,
                  custom_title, is_favorite)
             VALUES (?1, '/p', '/p/f.jsonl', 0, 0, 0, '', 0)",
            [id],
        )
        .expect("seed session");
        file
    }

    fn row(file: &tempfile::NamedTempFile, id: &str) -> (String, bool) {
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.query_row(
            "SELECT custom_title, is_favorite FROM claude_session_cache WHERE session_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row")
    }

    #[test]
    fn each_field_updates_on_its_own_and_answers_204_with_no_body() {
        let file = migrated_with_session("s1");

        let answer = update(file.path(), "s1", br#"{"custom_title":"Renamed"}"#).expect("title");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        // `w.WriteHeader(204)` directly, so no body and no `Content-Type`.
        assert!(answer.body.is_none());
        assert_eq!(row(&file, "s1"), ("Renamed".to_string(), false));

        update(file.path(), "s1", br#"{"is_favorite":true}"#).expect("favourite");
        assert_eq!(row(&file, "s1"), ("Renamed".to_string(), true));

        // Both at once, and the title is trimmed.
        update(
            file.path(),
            "s1",
            br#"{"custom_title":"  spaced  ","is_favorite":false}"#,
        )
        .expect("both");
        assert_eq!(row(&file, "s1"), ("spaced".to_string(), false));
    }

    /// An all-whitespace title trims to `""`, which is how a user clears a
    /// rename — `custom_title` is the first term of `ResolveDisplayTitle`, so an
    /// empty one falls through to the next.
    #[test]
    fn a_whitespace_only_title_clears_the_rename() {
        let file = migrated_with_session("s1");
        update(file.path(), "s1", br#"{"custom_title":"kept"}"#).expect("set");
        update(file.path(), "s1", br#"{"custom_title":"   "}"#).expect("clear");
        assert_eq!(row(&file, "s1").0, "");
    }

    /// `null` and *absent* are the same thing to a Go pointer, so neither counts
    /// as a field to update — and a body carrying only nulls is the 400, not a
    /// no-op 204.
    #[test]
    fn a_null_field_is_absent_rather_than_a_value() {
        let file = migrated_with_session("s1");
        for body in [
            &br#"{}"#[..],
            &br#"{"custom_title":null}"#[..],
            &br#"{"is_favorite":null}"#[..],
            &br#"{"custom_title":null,"is_favorite":null}"#[..],
            // A `null` body is Go's zero value, so it reaches the same check
            // rather than the decoder's 400.
            &b"null"[..],
        ] {
            let err = update(file.path(), "s1", body).unwrap_err();
            assert_eq!(err.message(), "no fields to update", "{body:?}");
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        }
        assert_eq!(row(&file, "s1"), (String::new(), false));

        // An explicit empty title *is* a value, and sets one.
        update(file.path(), "s1", br#"{"custom_title":""}"#).expect("empty title");
    }

    #[test]
    fn a_malformed_body_is_the_decoders_400() {
        let file = migrated_with_session("s1");
        for body in [&b""[..], &b"{"[..], &b"[]"[..], &br#"["x"]"#[..]] {
            assert_eq!(
                update(file.path(), "s1", body).unwrap_err(),
                WriteError::InvalidBody,
                "{body:?}"
            );
        }
    }

    /// The one that looks like a bug and is not: Go never checks the session
    /// exists, and an `UPDATE` matching no row is not an error — so an unknown
    /// id is a **204**, not a 404.
    #[test]
    fn an_unknown_session_is_204_rather_than_404() {
        let file = migrated_with_session("s1");
        let answer = update(file.path(), "nope", br#"{"is_favorite":true}"#).expect("unknown id");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        // And it touched nothing.
        assert_eq!(row(&file, "s1"), (String::new(), false));
    }
}

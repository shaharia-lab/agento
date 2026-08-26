//! `POST /api/claude-sessions/{id}/continue`, ported from
//! `handleContinueClaudeSession` (`internal/api/claude_sessions.go`).
//!
//! Opens a new Agento chat that **resumes** an existing Claude Code
//! conversation: it inherits the transcript's working directory and model, and
//! carries the Claude session id in `sdk_session_id` so the SDK picks the
//! history up on the first message.
//!
//! # Two writes, one transaction
//!
//! Go creates the chat and then updates it, in that order and separately —
//! `CreateSession` has no `sdkSession` parameter. If the second write fails it
//! leaves an orphan chat behind, which is a real behaviour rather than a bug to
//! reproduce faithfully: the user gets a 500 and a stray "New Chat".
//!
//! This runs both statements inside one transaction, which is a deliberate
//! divergence. A failure *between* the two writes would otherwise leave the
//! orphan chat behind and report a 500 — so the user sees an error and gains a
//! stray "New Chat". Rolling back gives "both or neither"; the response is a
//! single `chat_id` either way, so nothing observable changes except that the
//! failure path stops leaking a row.
//!
//! # The statuses
//!
//! A missing session is `404`; a lookup *failure* is `500`. `detail::get`
//! collapses both into `None`/`Err`, so the split here is the same one it
//! makes.

use axum::http::StatusCode;
use serde::Serialize;

use crate::native::writes::WriteError;
use crate::native::{chats, gojson, Answer};

use super::detail;

/// `writeJSON(w, 201, map[string]string{"chat_id": …})`. One key, so there is
/// nothing for `encoding/json`'s map sorting to reorder — but it is a map, so
/// check the shape before adding a second.
#[derive(Serialize)]
struct ContinueResponse {
    chat_id: String,
}

pub fn continue_session(db_path: &std::path::Path, session_id: &str) -> Result<Answer, WriteError> {
    // `claudesessions.GetSessionDetail`. Reading the transcript is the whole
    // cost of this route, and it happens before anything is written.
    let detail = detail::get(db_path, session_id)
        .map_err(|e| WriteError::Fallback(format!("looking up claude session: {e}")))?;
    let Some(detail) = detail else {
        return Err(WriteError::NotFoundMessage("session not found".to_string()));
    };

    let mut conn = chats::open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin continue session: {e}")))?;

    // No agent slug: a continued conversation belongs to whoever ran it, and
    // `CreateSession` accepts an empty slug without looking one up.
    let session = chats::insert_session(
        &tx,
        chats::NewSessionParams {
            agent_slug: "",
            working_directory: &detail.summary.cwd,
            model: &detail.summary.model,
            settings_profile_id: "",
            permission_mode: "",
        },
    )?;

    // The chat is titled from the session it continues, so it is identifiable
    // in the Chats list instead of joining the pile of untitled "New Chat" rows
    // (#485). `display_title` falls all the way through to the transcript's
    // first-message preview, which is unbounded, so it goes through the *same*
    // 60-rune cut a chat's own derived title takes — one spelling of "how long
    // a chat title may be", not two. An empty one keeps `insert_session`'s
    // default rather than storing a blank.
    //
    // This is a deliberate divergence from the inherited behaviour, which left
    // every continued chat titled "New Chat".
    let from_source =
        crate::native::chat::persist::truncate_title(&detail.summary.display_title, 60);
    let title = if from_source.is_empty() {
        session.title.as_str()
    } else {
        from_source.as_str()
    };

    // `chatService.UpdateSession`, which stamps its own `updated_at` — so this
    // is a second statement with a second clock reading rather than a value
    // folded into the insert above. It writes every column, but only these
    // three can differ from what was just inserted.
    tx.execute(
        "UPDATE chat_sessions SET title = ?1, sdk_session_id = ?2, updated_at = ?3 WHERE id = ?4",
        rusqlite::params![
            title,
            session_id,
            crate::native::gotime::now_go_text(),
            session.id
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("linking claude session: {e}")))?;

    // Encoded before the commit: after it, an `Err` would report failure for a
    // chat that exists, and a retry would create a second one.
    let body = gojson::to_vec(&ContinueResponse {
        chat_id: session.id.clone(),
    })
    .map_err(|e| WriteError::Fallback(format!("encoding continue response: {e}")))?;

    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit continue session: {e}")))?;
    Ok(Answer::json_status(StatusCode::CREATED, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database with the real migrations and a Claude transcript on disk, so
    /// the route runs end to end: transcript → detail → chat row.
    ///
    /// The config dir is taken from the settings row the fixture writes, not
    /// from the environment, so this is a real end-to-end run rather than one
    /// that degrades to a no-op when another test got to the snapshot first.
    #[test]
    fn continuing_a_session_creates_one_chat_carrying_its_cwd_model_and_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let claude = dir.path().join(".claude");
        let project = claude.join("projects").join("-home-u-proj");
        std::fs::create_dir_all(&project).expect("project dir");

        let session_id = "11111111-2222-3333-4444-555555555555";
        let transcript = format!(
            "{}\n{}\n",
            r#"{"type":"user","uuid":"u1","parentUuid":null,"timestamp":"2026-08-01T10:00:00Z","cwd":"/home/u/proj","message":{"role":"user","content":"do the thing"}}"#,
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-08-01T10:00:05Z","cwd":"/home/u/proj","message":{"role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#
        );
        std::fs::write(project.join(format!("{session_id}.jsonl")), transcript).expect("write");

        let db = dir.path().join("agento.db");
        {
            let mut conn = rusqlite::Connection::open(&db).expect("open");
            crate::native::migrate::apply(&mut conn).expect("migrate");
            conn.execute(
                "INSERT INTO user_settings (id, claude_config_dir) VALUES (1, ?1)",
                [claude.to_string_lossy()],
            )
            .expect("settings row");
        }

        // The config dir comes from the settings row rather than the
        // environment, so this does not depend on process-wide state and has no
        // reason to be conditional.
        let answer = continue_session(&db, session_id).expect("continue");
        assert_eq!(answer.status, StatusCode::CREATED);

        let body = String::from_utf8(answer.body.expect("body")).expect("utf-8");
        let chat_id = body
            .trim_end()
            .strip_prefix(r#"{"chat_id":""#)
            .and_then(|s| s.strip_suffix(r#""}"#))
            .expect("one-key response")
            .to_string();

        let conn = rusqlite::Connection::open(&db).expect("open");
        let (count, sdk, cwd, model, slug, title): (i64, String, String, String, String, String) =
            conn.query_row(
                "SELECT (SELECT COUNT(*) FROM chat_sessions), sdk_session_id,
                        working_directory, model, agent_slug, title
                 FROM chat_sessions WHERE id = ?1",
                [&chat_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("the chat row");

        assert_eq!(count, 1, "one chat, not two");
        assert_eq!(sdk, session_id, "the link is what makes it a continuation");
        assert_eq!(cwd, "/home/u/proj");
        assert_eq!(model, "claude-opus-5");
        assert_eq!(slug, "", "a continued conversation has no agent");
        assert_eq!(
            title, "do the thing",
            "the chat is titled from the session it continues, not left \"New Chat\""
        );
    }

    /// The title is the source session's, cut the way a chat's own derived
    /// title is cut — `display_title` falls through to an unbounded transcript
    /// preview, so a long first message must not become a 4 KB title.
    #[test]
    fn a_long_source_title_is_cut_at_sixty_runes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let claude = dir.path().join(".claude");
        let project = claude.join("projects").join("-home-u-proj");
        std::fs::create_dir_all(&project).expect("project dir");

        let session_id = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
        let long = "x".repeat(200);
        let transcript = format!(
            "{{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\
             \"timestamp\":\"2026-08-01T10:00:00Z\",\"cwd\":\"/home/u/proj\",\
             \"message\":{{\"role\":\"user\",\"content\":\"{long}\"}}}}\n"
        );
        std::fs::write(project.join(format!("{session_id}.jsonl")), transcript).expect("write");

        let db = dir.path().join("agento.db");
        {
            let mut conn = rusqlite::Connection::open(&db).expect("open");
            crate::native::migrate::apply(&mut conn).expect("migrate");
            conn.execute(
                "INSERT INTO user_settings (id, claude_config_dir) VALUES (1, ?1)",
                [claude.to_string_lossy()],
            )
            .expect("settings row");
        }

        continue_session(&db, session_id).expect("continue");

        let conn = rusqlite::Connection::open(&db).expect("open");
        let title: String = conn
            .query_row("SELECT title FROM chat_sessions", [], |r| r.get(0))
            .expect("the chat row");
        assert_eq!(title, format!("{}...", "x".repeat(60)));
    }

    /// A source session with nothing to take a title from keeps the store's own
    /// default, rather than being written as an empty string — a blank row in
    /// the Chats list is worse than "New Chat".
    #[test]
    fn a_source_session_with_no_title_keeps_the_default() {
        let dir = tempfile::tempdir().expect("temp dir");
        let claude = dir.path().join(".claude");
        let project = claude.join("projects").join("-home-u-proj");
        std::fs::create_dir_all(&project).expect("project dir");

        let session_id = "cccccccc-dddd-eeee-ffff-000000000000";
        // Assistant-only: nothing this session can derive a display title from.
        let transcript = concat!(
            r#"{"type":"assistant","uuid":"a1","parentUuid":null,"timestamp":"2026-08-01T10:00:05Z","#,
            r#""cwd":"/home/u/proj","message":{"role":"assistant","model":"claude-opus-5",""#,
            r#"content":[{"type":"text","text":"done"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            "\n"
        );
        std::fs::write(project.join(format!("{session_id}.jsonl")), transcript).expect("write");

        let db = dir.path().join("agento.db");
        {
            let mut conn = rusqlite::Connection::open(&db).expect("open");
            crate::native::migrate::apply(&mut conn).expect("migrate");
            conn.execute(
                "INSERT INTO user_settings (id, claude_config_dir) VALUES (1, ?1)",
                [claude.to_string_lossy()],
            )
            .expect("settings row");
        }

        continue_session(&db, session_id).expect("continue");

        let conn = rusqlite::Connection::open(&db).expect("open");
        let title: String = conn
            .query_row("SELECT title FROM chat_sessions", [], |r| r.get(0))
            .expect("the chat row");
        assert_eq!(title, "New Chat");
    }

    /// A session that exists nowhere is Go's 404 with its own fixed wording,
    /// not the service error's — and it must write nothing.
    #[test]
    fn a_missing_session_is_404_and_creates_no_chat() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        drop(conn);

        let err = continue_session(&db, "no-such-session-id").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), "session not found");

        let conn = rusqlite::Connection::open(&db).expect("open");
        let chats: i64 = conn
            .query_row("SELECT COUNT(*) FROM chat_sessions", [], |r| r.get(0))
            .expect("count");
        assert_eq!(chats, 0);
    }
}

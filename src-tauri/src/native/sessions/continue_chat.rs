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
//! # Continue is idempotent per source session (#490)
//!
//! One Claude session, one chat. Continuing an already-continued session
//! reopens *that* chat and writes nothing — the insert was unconditional until
//! now, so a second click minted a second row pointing at the same
//! conversation. (#485 recommended the opposite, allowing several chats to
//! branch one session, and flagged the call; #490 overrules it.)
//!
//! The lookup matches on the **pair** `(session_id, project_path)`, because that
//! is how the corpus keys a session (the #362 family) — a session id can exist
//! under two projects, and the id alone would collapse them. It deliberately
//! does **not** look at `sdk_session_id`: `chat/persist.rs` rewrites that column
//! from whatever the stream reports, so it is a live pointer rather than an
//! identity.
//!
//! It also matches a chat whose **own id** is the session id, which is not a
//! coincidence: a turn pins the CLI session to the chat id, so a chat Agento
//! created itself is indexed in the corpus under that id. "Continue" invoked on
//! one of those is a request to reopen the chat it already is, not to clone it —
//! and that chat's history is in `chat_messages` already, so it records no
//! source and inherits nothing.
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

    // Already continued? Reopen that chat and write nothing.
    //
    // Inside the transaction, and an `Immediate` one, so two clicks racing each
    // other cannot both miss the lookup and both insert.
    //
    // `ORDER BY created_at DESC` is meaningful rather than decorative: a
    // database written before this landed can hold several chats for one source
    // session, and the newest is the one the user was last working in.
    if let Some(existing) = existing_chat_for(&tx, session_id, &detail.summary.project_path)? {
        let body = gojson::to_vec(&ContinueResponse { chat_id: existing })
            .map_err(|e| WriteError::Fallback(format!("encoding continue response: {e}")))?;
        return Ok(Answer::json_status(StatusCode::CREATED, body));
    }

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
    //
    // The three `continued_from_*` columns ride along (#490). They are the same
    // write because they are the same fact: this chat *is* that session resumed,
    // and a row carrying `sdk_session_id` without them is the state that opened
    // an empty transcript.
    //
    // The count is `detail.messages.len()` — the **unfiltered** normalized list,
    // which is what the view slices before it applies its own display filters, so
    // the boundary survives a change to those filters. It is read here, once,
    // and never advanced: the CLI appends resumed turns to the same transcript
    // file, so the source grows past this point on the first turn and the prefix
    // is what stops the newest turn rendering twice.
    tx.execute(
        "UPDATE chat_sessions
            SET title = ?1, sdk_session_id = ?2, updated_at = ?3,
                continued_from_session_id = ?5, continued_from_project_path = ?6,
                continued_from_message_count = ?7
          WHERE id = ?4",
        rusqlite::params![
            title,
            session_id,
            crate::native::gotime::now_go_text(),
            session.id,
            session_id,
            detail.summary.project_path,
            i64::try_from(detail.messages.len()).unwrap_or(i64::MAX),
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

/// The chat this session has already been continued into, if there is one.
///
/// Two ways a chat can already *be* this conversation, and both are matched:
///
/// - it records the source pair, i.e. an earlier `continue` created it; or
/// - its own **id** is the session id, i.e. Agento created the chat and the turn
///   pinned the CLI session to it (`custom_session_id`), so the transcript is
///   indexed under the chat's own id. Nothing else in the schema can collide with
///   that — a chat id is a v4 UUID and the primary key.
fn existing_chat_for(
    tx: &rusqlite::Transaction,
    session_id: &str,
    project_path: &str,
) -> Result<Option<String>, WriteError> {
    use rusqlite::OptionalExtension;

    tx.query_row(
        "SELECT id FROM chat_sessions
          WHERE id = ?1
             OR (continued_from_session_id = ?1 AND continued_from_project_path = ?2)
          ORDER BY created_at DESC
          LIMIT 1",
        rusqlite::params![session_id, project_path],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| WriteError::Fallback(format!("looking up an existing continuation: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A migrated database plus a two-event transcript on disk, which is what
    /// every #490 test needs and none of them varies. The Claude config dir comes
    /// from the settings row rather than the environment, so these run end to end
    /// without depending on process-wide state.
    struct Fixture {
        _dir: tempfile::TempDir,
        db: std::path::PathBuf,
        project: std::path::PathBuf,
        session_id: String,
    }

    impl Fixture {
        fn new(session_id: &str) -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let claude = dir.path().join(".claude");
            let project = claude.join("projects").join("-home-u-proj");
            std::fs::create_dir_all(&project).expect("project dir");

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

            Self {
                _dir: dir,
                db,
                project,
                session_id: session_id.to_string(),
            }
        }

        fn conn(&self) -> rusqlite::Connection {
            rusqlite::Connection::open(&self.db).expect("open")
        }

        /// What the CLI does to a resumed session: append to the **same** file.
        fn append_turn(&self) {
            use std::io::Write;

            let path = self.project.join(format!("{}.jsonl", self.session_id));
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .expect("append");
            const TURN: &str = concat!(
                r#"{"type":"user","uuid":"u2","parentUuid":"a1","timestamp":"2026-08-01T11:00:00Z","cwd":"/home/u/proj","message":{"role":"user","content":"and again"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a2","parentUuid":"u2","timestamp":"2026-08-01T11:00:05Z","cwd":"/home/u/proj","message":{"role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"done twice"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
                "\n",
            );
            f.write_all(TURN.as_bytes()).expect("write");
        }
    }

    fn chat_id_of(answer: Answer) -> String {
        let body = String::from_utf8(answer.body.expect("body")).expect("utf-8");
        body.trim_end()
            .strip_prefix(r#"{"chat_id":""#)
            .and_then(|s| s.strip_suffix(r#""}"#))
            .expect("one-key response")
            .to_string()
    }

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

    /// The three `continued_from_*` columns are what turn "this chat points at a
    /// session" into "this chat *is* that session resumed", which is the whole of
    /// #490's read path: without the pair the view cannot find the transcript,
    /// and without the count it cannot tell the inherited turns from its own.
    ///
    /// The count is asserted against `detail::get`'s own list rather than a
    /// literal, so it stays the boundary if normalization ever emits a different
    /// number of messages for this fixture.
    #[test]
    fn the_source_pair_and_the_message_boundary_are_recorded() {
        let f = Fixture::new("22222222-3333-4444-5555-666666666666");

        continue_session(&f.db, &f.session_id).expect("continue");

        let want = detail::get(&f.db, &f.session_id)
            .expect("detail")
            .expect("the session");

        let (from_id, from_path, count): (String, String, i64) = f
            .conn()
            .query_row(
                "SELECT continued_from_session_id, continued_from_project_path,
                        continued_from_message_count
                 FROM chat_sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("the chat row");

        assert_eq!(from_id, f.session_id);
        assert_eq!(
            from_path, want.summary.project_path,
            "the corpus keys a session on the pair, so the pair is what is stored"
        );
        assert_eq!(
            count,
            want.messages.len() as i64,
            "the boundary is the normalized list's length at continue time"
        );
        assert!(count > 0, "the fixture transcript has turns to inherit");
    }

    /// Continue means *resume*: one Claude session, one chat. A second call
    /// reopens the first chat and writes nothing.
    ///
    /// Reverting the lookup makes this fail on both assertions at once — a
    /// different id, and two rows.
    #[test]
    fn continuing_the_same_session_twice_returns_the_one_chat() {
        let f = Fixture::new("33333333-4444-5555-6666-777777777777");

        let first = chat_id_of(continue_session(&f.db, &f.session_id).expect("first"));
        let second = chat_id_of(continue_session(&f.db, &f.session_id).expect("second"));

        assert_eq!(first, second, "the same session reopens the same chat");
        let count: i64 = f
            .conn()
            .query_row("SELECT COUNT(*) FROM chat_sessions", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "one Claude session, one chat");
    }

    /// The recorded count is a **boundary, not a running total**.
    ///
    /// The CLI appends a resumed turn to the *same* transcript file, so the
    /// source session grows after the chat was created. Reopening it must not
    /// move the boundary — if it did, the turns Agento itself contributed would
    /// be rendered from the transcript *and* from `chat_messages`.
    #[test]
    fn a_transcript_that_grew_afterwards_does_not_move_the_boundary() {
        let f = Fixture::new("44444444-5555-6666-7777-888888888888");

        continue_session(&f.db, &f.session_id).expect("continue");
        let before: i64 = f
            .conn()
            .query_row(
                "SELECT continued_from_message_count FROM chat_sessions",
                [],
                |r| r.get(0),
            )
            .expect("the boundary");

        f.append_turn();
        assert!(
            detail::get(&f.db, &f.session_id)
                .expect("detail")
                .expect("the session")
                .messages
                .len() as i64
                > before,
            "the fixture must actually have grown, or this asserts nothing"
        );

        continue_session(&f.db, &f.session_id).expect("continue again");
        let after: i64 = f
            .conn()
            .query_row(
                "SELECT continued_from_message_count FROM chat_sessions",
                [],
                |r| r.get(0),
            )
            .expect("the boundary");

        assert_eq!(after, before, "the boundary is fixed at continue time");
    }

    /// A chat Agento created itself is indexed in the corpus under its **own
    /// id**, because the turn pins the CLI session to it. "Continue" on one of
    /// those reopens the chat rather than cloning it — and records no source,
    /// since its history is already in `chat_messages`.
    #[test]
    fn continuing_a_chat_agento_created_reopens_it_rather_than_cloning() {
        let f = Fixture::new("55555555-6666-7777-8888-999999999999");

        // The shape `chat/turn.rs` leaves behind: the chat's id *is* the CLI
        // session id, and `sdk_session_id` echoes it.
        f.conn()
            .execute(
                "INSERT INTO chat_sessions
                    (id, title, agent_slug, sdk_session_id, working_directory, model,
                     settings_profile_id, permission_mode, total_input_tokens,
                     total_output_tokens, total_cache_creation_tokens,
                     total_cache_read_tokens, created_at, updated_at)
                 VALUES (?1, 'Its own chat', '', ?1, '/home/u/proj', 'claude-opus-5',
                         '', '', 0, 0, 0, 0, ?2, ?2)",
                rusqlite::params![&f.session_id, crate::native::gotime::now_go_text()],
            )
            .expect("the pre-existing chat");

        let chat_id = chat_id_of(continue_session(&f.db, &f.session_id).expect("continue"));
        assert_eq!(chat_id, f.session_id, "it reopens the chat it already is");

        let (count, from_id): (i64, String) = f
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM chat_sessions), continued_from_session_id
                 FROM chat_sessions WHERE id = ?1",
                [&f.session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("the chat row");
        assert_eq!(count, 1, "no clone");
        assert_eq!(
            from_id, "",
            "its own history is in chat_messages, so it inherits nothing"
        );
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

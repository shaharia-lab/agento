//! `GET /api/chats` and `GET /api/chats/{id}`.
//!
//! Mirrors `SQLiteChatStore.ListSessions`/`GetSessionWithMessages`
//! (`internal/storage/sqlite_chat_store.go`), `chatService`'s two read methods —
//! which only wrap the store in a span — and `handleListChats`/`handleGetChat`
//! in `internal/api/chats.go`.
//!
//! Plus, since #274, the CRUD writes — create, patch, the two deletes — while
//! the streaming turn lives in `chat/`. (This header predates #274's storage
//! move; the "reads only" era ended there.)
//!
//! Three Go-isms decide the bytes here, and none of them is visible in the Go
//! structs:
//!
//! 1. **The detail response is a `map[string]any`, so its keys are sorted.**
//!    `handleGetChat` writes `map[string]any{"session": …, "messages": …}` and
//!    `encoding/json` sorts map keys — so the wire order is `messages` first.
//!    `desktop/CLAUDE.md` and issue #264 both describe it as `{session,
//!    messages}`, which is the shape but not the order; verified against a Go
//!    server built from this checkout.
//! 2. **`omitempty` drops zero values**, so a chat with no tokens sends neither
//!    counter and one that is not a favourite sends no `is_favorite`.
//! 3. **A `json.RawMessage` is re-emitted through Go's `compact`**, which strips
//!    insignificant whitespace and HTML-escapes but preserves key order and
//!    number spelling. See [`compact_raw_json`] — decoding a tool_use `input`
//!    into a `serde_json::Value` and re-encoding it would silently sort its keys
//!    and respell its numbers.

use std::path::Path;

use axum::http::{Method, StatusCode};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::db;
use super::gotime::GoTime;
use super::writes::{decode_body, finish, WriteError};

/// One chat session's metadata. Mirrors `storage.ChatSession`.
///
/// Field order is the Go struct's declaration order, which is **not** the order
/// the `SELECT` reads them in: the two timestamps are declared before the token
/// counters and selected after them.
#[derive(Debug, Clone, Serialize)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub agent_slug: String,
    pub sdk_session_id: String,
    pub working_directory: String,
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub settings_profile_id: String,
    /// This conversation's own permission mode, empty when none was chosen.
    /// `omitempty` in Go, so an empty one is off the wire — which is what keeps
    /// every row written before migration 30 byte-identical to what it was.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub permission_mode: String,
    pub created_at: GoTime,
    pub updated_at: GoTime,
    #[serde(skip_serializing_if = "is_zero")]
    pub total_input_tokens: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub total_output_tokens: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub total_cache_creation_tokens: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub total_cache_read_tokens: i64,
    #[serde(skip_serializing_if = "is_false")]
    pub is_favorite: bool,
    /// The Claude session this chat resumes, as the corpus keys one: the **pair**
    /// `(session_id, project_path)`, never the id alone (#490, the #362 family).
    ///
    /// Appended after `is_favorite` rather than woven into the order above, and
    /// all three are `omitempty`-shaped, so a chat that is not a continuation
    /// puts exactly the bytes on the wire it always did.
    ///
    /// Deliberately **not** `sdk_session_id`, which `chat/persist.rs` rewrites
    /// from whatever the stream reports: that column is a live pointer, this is a
    /// record of what the conversation was opened from.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub continued_from_session_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub continued_from_project_path: String,
    /// How many of the source transcript's normalized messages this chat
    /// inherited — a **boundary, not a total**.
    ///
    /// The CLI appends a resumed turn to the *same* transcript file, so the
    /// source grows past this point as soon as Agento takes a turn. The view
    /// renders exactly this prefix and its own `chat_messages` after it; anything
    /// that advanced this number would double-render the newest turn.
    #[serde(skip_serializing_if = "is_zero")]
    pub continued_from_message_count: i64,
}

/// One message in a chat. Mirrors `storage.ChatMessage`.
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: GoTime,
    /// `omitempty` on a slice drops nil *and* empty alike, so a `Vec` with
    /// `is_empty` reproduces both of Go's cases — unlike the agent capabilities,
    /// where the nil-versus-empty distinction does reach the wire.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<MessageBlock>,
}

/// One ordered content block of an assistant message. Mirrors
/// `storage.MessageBlock`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBlock {
    /// "thinking", "text" or "tool_use".
    #[serde(rename = "type", default)]
    pub block_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The tool call's arguments, kept as raw JSON.
    ///
    /// `Option` distinguishes an absent key from a present `null`, which Go also
    /// distinguishes: `omitempty` on a `json.RawMessage` tests the byte length,
    /// and the four bytes of `null` are not empty. Plain `Option` would collapse
    /// them, so [`captured_raw`] takes the value however it arrives.
    ///
    /// **Emitted through Go's compact, stated on the field** (#298). This struct
    /// has *two* sinks — the `blocks` column via `persist::append_message`, and
    /// the wire via `GET /api/chats/{id}` — and it had two independent
    /// compaction points, one of which was simply missing until #298. A third
    /// construction path would have been silently wrong the same way, so the
    /// rule lives in the type. The existing call-site `compact_raw`s are
    /// belt-and-braces: compaction is idempotent.
    #[serde(
        default,
        deserialize_with = "captured_raw",
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::gojson::serialize_compacted_option"
    )]
    pub input: Option<Box<RawValue>>,
}

/// The `GET /api/chats/{id}` body.
///
/// **`messages` before `session` is load-bearing.** Go builds this as a
/// `map[string]any` and `encoding/json` sorts map keys, so the handler's
/// source order is not the wire order. A struct is used rather than a map so
/// the order is stated once, here, instead of depending on a container's
/// iteration.
#[derive(Debug, Clone, Serialize)]
pub struct ChatDetail {
    pub messages: Vec<ChatMessage>,
    pub session: ChatSession,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

const SESSION_COLUMNS: &str =
    "SELECT id, title, agent_slug, sdk_session_id, working_directory, model,
       settings_profile_id, total_input_tokens, total_output_tokens,
       total_cache_creation_tokens, total_cache_read_tokens,
       created_at, updated_at, is_favorite, permission_mode,
       continued_from_session_id, continued_from_project_path,
       continued_from_message_count
FROM chat_sessions";

/// Every chat session, most recently updated first, as the store orders them.
///
/// `ORDER BY updated_at DESC` has no tiebreak in Go either; the column is text
/// (`time.Time.String()`), so the comparison is lexical and matches
/// chronological order only because every stored value is UTC.
pub fn list(db_path: &Path) -> Result<Vec<ChatSession>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{SESSION_COLUMNS}\nORDER BY updated_at DESC");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("listing chats: {e}"))?;
    let rows = stmt
        .query_map([], scan_session)
        .map_err(|e| format!("listing chats: {e}"))?;

    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row.map_err(|e| format!("listing chats: {e}"))?);
    }
    Ok(sessions)
}

/// One chat and its full message history, or `None` when there is no such
/// session — which the caller turns into the 404 Go returns.
pub fn get(db_path: &Path, id: &str) -> Result<Option<ChatDetail>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{SESSION_COLUMNS} WHERE id = ?");
    let session = conn
        .query_row(&sql, [id], scan_session)
        .optional()
        .map_err(|e| format!("getting chat {id:?}: {e}"))?;

    let Some(session) = session else {
        return Ok(None);
    };

    let mut stmt = conn
        .prepare(
            "SELECT role, content, blocks, timestamp
             FROM chat_messages
             WHERE session_id = ?
             ORDER BY id ASC",
        )
        .map_err(|e| format!("listing messages for chat {id:?}: {e}"))?;
    let rows = stmt
        .query_map([id], scan_message)
        .map_err(|e| format!("listing messages for chat {id:?}: {e}"))?;

    let mut messages = Vec::new();
    for row in rows {
        messages.push(row.map_err(|e| format!("listing messages for chat {id:?}: {e}"))?);
    }
    Ok(Some(ChatDetail { messages, session }))
}

fn scan_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatSession> {
    Ok(ChatSession {
        id: row.get(0)?,
        title: row.get(1)?,
        agent_slug: row.get(2)?,
        sdk_session_id: row.get(3)?,
        working_directory: row.get(4)?,
        model: row.get(5)?,
        settings_profile_id: row.get(6)?,
        permission_mode: row.get(14)?,
        created_at: timestamp(row, 11)?,
        updated_at: timestamp(row, 12)?,
        total_input_tokens: row.get(7)?,
        total_output_tokens: row.get(8)?,
        total_cache_creation_tokens: row.get(9)?,
        total_cache_read_tokens: row.get(10)?,
        is_favorite: row.get(13)?,
        continued_from_session_id: row.get(15)?,
        continued_from_project_path: row.get(16)?,
        continued_from_message_count: row.get(17)?,
    })
}

fn scan_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    let blocks: String = row.get(2)?;
    Ok(ChatMessage {
        role: row.get(0)?,
        content: row.get(1)?,
        timestamp: timestamp(row, 3)?,
        blocks: decode_blocks(&blocks),
    })
}

/// Read a DATETIME column as the `time.Time` the Go driver round-trips.
///
/// Unparsable text fails the whole request rather than decoding to a zero time,
/// which is what `rows.Scan` does — a zero timestamp would be indistinguishable
/// from a real one.
fn timestamp(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<GoTime> {
    let text: String = row.get(index)?;
    GoTime::parse_any(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e)),
        )
    })
}

/// Decode the `blocks` column exactly as `GetSessionWithMessages` does.
///
/// Every failure is **non-fatal and yields no blocks**, which is deliberate on
/// the Go side and worth keeping: `""` and `"[]"` skip the decode outright, a
/// decode error sets `msg.Blocks = nil` without surfacing anything, and a stored
/// `null` decodes to nil the same way. All four reach the wire identically,
/// because `omitempty` drops an empty slice — so a corrupt column costs the
/// blocks of one message rather than the whole conversation.
///
/// This is the opposite of `agents.rs`, where unparsable stored JSON fails the
/// read. The difference is Go's, not a choice made here.
fn decode_blocks(stored: &str) -> Vec<MessageBlock> {
    if stored.is_empty() || stored == "[]" {
        return Vec::new();
    }
    let mut blocks: Vec<MessageBlock> = serde_json::from_str(stored).unwrap_or_default();
    for block in &mut blocks {
        if let Some(raw) = block.input.take() {
            block.input = Some(compact_raw_json(raw));
        }
    }
    blocks
}

// The compaction and raw-capture helpers live in `super::gojson` — see
// `compact_raw` there for what Go's `compact` does and why it is a byte pass —
// because the session-detail port needs the same pair for `tool_use` inputs.
use super::gojson::{captured_raw, compact_raw as compact_raw_json};

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`. Covers both reads, because the
/// list and the per-chat read share this file and a registry entry is per area,
/// not per path.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "chats",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    match *method {
        Method::GET => path == "/api/chats" || id_of(path).is_some(),
        // Creating a chat, and the bulk delete — both on the collection.
        Method::POST | Method::DELETE if path == "/api/chats" => true,
        // Renaming/favouriting, and deleting one chat.
        Method::PATCH | Method::DELETE => id_of(path).is_some(),
        _ => false,
    }
}

/// The id in `/api/chats/{id}`, or `None` for anything else.
///
/// One segment only: `/api/chats/{id}/messages`, `/input`, `/permission` and
/// `/stop` are separate routes — all writes, and `messages` is the SSE turn —
/// and a prefix match would swallow them. An empty id is not a match either,
/// because chi routes `/api/chats/` to nothing.
fn id_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/chats/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(rest)
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    match *req.method {
        Method::GET => serve_read(ctx, req),
        Method::POST => finish(create(&ctx.db_path, req.body)),
        Method::PATCH => match id_of(req.path) {
            Some(id) => finish(patch(&ctx.db_path, id, req.body)),
            None => Err("PATCH /api/chats has no id".to_string()),
        },
        Method::DELETE => match id_of(req.path) {
            Some(id) => finish(delete_one(&ctx.db_path, id)),
            None => finish(bulk_delete(&ctx.db_path, req.body)),
        },
        _ => Err(format!("{} /api/chats is not ported", req.method)),
    }
}

fn serve_read(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let body = match id_of(req.path) {
        None => super::gojson::to_vec(&list(&ctx.db_path)?)
            .map_err(|e| format!("encoding chats: {e}"))?,
        Some(id) => match get(&ctx.db_path, id)? {
            Some(detail) => {
                super::gojson::to_vec(&detail).map_err(|e| format!("encoding chat: {e}"))?
            }
            // `handleGetChat`'s own 404, answered here since #278.
            None => {
                return super::Answer::error(axum::http::StatusCode::NOT_FOUND, "chat not found")
            }
        },
    };
    Ok(super::Answer::json(body))
}

// ─── Writes ───────────────────────────────────────────────────────────────────

/// `createChatRequest` (`internal/api/chats.go`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CreateChatRequest {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    agent_slug: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    working_directory: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    model: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    settings_profile_id: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    permission_mode: String,
}

/// The PATCH body. Both fields are genuinely optional — `null` and absent mean
/// the same thing (leave it alone), and "neither present" is its own 400. So
/// these are `Option`, not zero-value fields: a `title` of `""` is a *different*
/// request from no title at all, and Go rejects the first and ignores the
/// second.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PatchChatRequest {
    title: Option<String>,
    is_favorite: Option<bool>,
}

/// `BulkDeleteRequest` (`internal/api/types.go`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct BulkDeleteRequest {
    /// A `null` element is `""` to Go, not an error (#295) — and an empty id
    /// simply matches no row, exactly as Go's does.
    ids: Option<super::gojson::GoList<String>>,
}

/// Go's `maxQueryLimit`, reused as the bulk-delete cap.
const MAX_BULK_IDS: usize = 500;

/// `chatService.CreateSession`.
///
/// The agent slug is validated only when non-empty — a chat with no agent is
/// legal — and the row is created with a fixed title and zeroed counters.
fn create(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<CreateChatRequest>(body)?;

    // `isValidChatPermissionMode`, and it runs before the agent lookup for the
    // same reason Go's does: a body that is wrong about two things reports the
    // mode, not the missing agent.
    if !is_valid_permission_mode(&req.permission_mode) {
        return Err(WriteError::validation(
            "permission_mode",
            r#"must be one of "bypass", "default", "plan", "dontAsk", or empty"#,
        ));
    }

    let mut conn = open_for_write(db_path)?;

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin chat create: {e}")))?;

    if !req.agent_slug.is_empty() {
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM agents WHERE slug = ?1",
                [&req.agent_slug],
                |_| Ok(true),
            )
            .optional()
            .map_err(|e| WriteError::Fallback(format!("looking up agent: {e}")))?
            .unwrap_or(false);
        if !exists {
            return Err(WriteError::NotFound {
                resource: "agent".to_string(),
                id: req.agent_slug.clone(),
            });
        }
    }

    let session = insert_session(
        &tx,
        NewSessionParams {
            agent_slug: &req.agent_slug,
            working_directory: &req.working_directory,
            model: &req.model,
            settings_profile_id: &req.settings_profile_id,
            permission_mode: &req.permission_mode,
        },
    )?;

    // Everything fallible happens before the commit. After it, an `Err` answers
    // 500 for a chat that was actually created — so the caller retries and ends
    // up with a *second* chat under a fresh UUID. A failing `commit` is the one
    // safe exception: it rolls back, so the 500 is honest.
    let body = super::gojson::to_vec(&session)
        .map_err(|e| WriteError::Fallback(format!("encoding chat: {e}")))?;

    // Nothing below this line may return `Fallback`.
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit chat create: {e}")))?;
    // `chatService.CreateSession`'s own line, with its three keys in Go's order
    // — see `writes::service_log_convention`.
    log::info!(
        "chat session created session_id={:?} agent_slug={:?} settings_profile_id={:?} permission_mode={:?}",
        session.id,
        req.agent_slug,
        req.settings_profile_id,
        req.permission_mode
    );
    Ok(super::Answer::json_status(StatusCode::CREATED, body))
}

/// `chatService.CreateSession`'s row, without the handler around it.
///
/// Shared with `POST /api/claude-sessions/{id}/continue` (#308), which creates
/// exactly this row and then links it to a Claude session. Two copies of the
/// INSERT would be two places for the literals below to drift — and one of them
/// would be the copy nobody looks at.
///
/// The handler answers with the session the store built rather than a re-read,
/// so the timestamps are the ones just written, parsed back from the same text
/// rather than re-taken from the clock.
/// `storage.NewSessionParams`, borrowed.
///
/// A struct rather than five positional `&str`s for the reason the Go side gives:
/// there is no call site where transposing two of them fails to compile.
pub(super) struct NewSessionParams<'a> {
    pub agent_slug: &'a str,
    pub working_directory: &'a str,
    pub model: &'a str,
    pub settings_profile_id: &'a str,
    pub permission_mode: &'a str,
}

/// `isValidChatPermissionMode` (`internal/service/chat_service.go`). All four of
/// Claude Code's modes plus empty; the *agent* validator is narrower on purpose.
pub(super) fn is_valid_permission_mode(mode: &str) -> bool {
    matches!(mode, "" | "bypass" | "default" | "plan" | "dontAsk")
}

pub(super) fn insert_session(
    tx: &rusqlite::Transaction,
    p: NewSessionParams<'_>,
) -> Result<ChatSession, WriteError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = super::gotime::now_go_text();
    // `sdk_session_id` and the four token totals are literals in the Go INSERT
    // rather than bound parameters, so they are literals here too.
    tx.execute(
        "INSERT INTO chat_sessions
            (id, title, agent_slug, sdk_session_id, working_directory, model,
             settings_profile_id, permission_mode, total_input_tokens, total_output_tokens,
             total_cache_creation_tokens, total_cache_read_tokens, created_at, updated_at)
         VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, 0, 0, 0, 0, ?8, ?9)",
        rusqlite::params![
            id,
            "New Chat",
            p.agent_slug,
            p.working_directory,
            p.model,
            p.settings_profile_id,
            p.permission_mode,
            now,
            now,
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("creating session: {e}")))?;

    let stamp = super::gotime::from_sql_text(&now, 0)
        .map_err(|e| WriteError::Fallback(format!("re-reading the write timestamp: {e}")))?;
    Ok(ChatSession {
        id,
        title: "New Chat".to_string(),
        agent_slug: p.agent_slug.to_string(),
        sdk_session_id: String::new(),
        working_directory: p.working_directory.to_string(),
        model: p.model.to_string(),
        settings_profile_id: p.settings_profile_id.to_string(),
        permission_mode: p.permission_mode.to_string(),
        created_at: stamp,
        updated_at: stamp,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_creation_tokens: 0,
        total_cache_read_tokens: 0,
        is_favorite: false,
        // A freshly inserted row is not a continuation. `continue_chat.rs`
        // records the source in the same transaction, immediately after this.
        continued_from_session_id: String::new(),
        continued_from_project_path: String::new(),
        continued_from_message_count: 0,
    })
}

/// `handleUpdateChat`: rename and/or favourite.
///
/// Two statuses, and the split is not the usual one. A missing chat is a
/// **404** with the fixed string `chat not found` — not the service's
/// `NotFoundError` wording, and not the 400 the other checks give. Everything
/// else here is a **400** rather than a 422, because these checks live in the
/// handler and the chats handlers never call `httpErr`.
fn patch(db_path: &Path, id: &str, body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<PatchChatRequest>(body)?;
    if req.title.is_none() && req.is_favorite.is_none() {
        return Err(WriteError::BadRequest("no fields to update".to_string()));
    }
    let title = match req.title {
        Some(raw) => {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() {
                return Err(WriteError::BadRequest("title cannot be empty".to_string()));
            }
            Some(trimmed)
        }
        None => None,
    };

    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin chat patch: {e}")))?;

    // The handler reads the session, mutates the struct and writes the whole
    // row back, so a concurrent change to any other column would be clobbered.
    // Doing it in one transaction is what stops that being observable here.
    let mut session = match get_session_tx(&tx, id)? {
        Some(session) => session,
        // `err != nil || session == nil` collapses both to the same 404 with a
        // fixed message — a **404**, not a 400: this check is in the handler but
        // its status is not the handler's usual one.
        None => return Err(WriteError::NotFoundMessage("chat not found".to_string())),
    };

    if let Some(title) = title {
        session.title = title;
    }
    if let Some(favorite) = req.is_favorite {
        session.is_favorite = favorite;
    }
    let now = super::gotime::now_go_text();

    tx.execute(
        "UPDATE chat_sessions SET
            title = ?1, agent_slug = ?2, sdk_session_id = ?3, working_directory = ?4,
            model = ?5, settings_profile_id = ?6, permission_mode = ?7,
            total_input_tokens = ?8, total_output_tokens = ?9,
            total_cache_creation_tokens = ?10, total_cache_read_tokens = ?11,
            is_favorite = ?12,
            updated_at = ?13
         WHERE id = ?14",
        rusqlite::params![
            session.title,
            session.agent_slug,
            session.sdk_session_id,
            session.working_directory,
            session.model,
            session.settings_profile_id,
            session.permission_mode,
            session.total_input_tokens,
            session.total_output_tokens,
            session.total_cache_creation_tokens,
            session.total_cache_read_tokens,
            session.is_favorite,
            now,
            id,
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("updating session {id:?}: {e}")))?;

    // The response carries the **new** timestamp. `chatService.UpdateSession`
    // sets `session.UpdatedAt = time.Now().UTC()` before handing the struct to
    // the store, and the handler then serializes that same struct — so Go's 200
    // body shows the write's time, not the row's previous one. Returning the
    // stale value would put the wrong `updated_at` in the frontend's cache for
    // a list that sorts on exactly that column.
    session.updated_at = super::gotime::from_sql_text(&now, 0)
        .map_err(|e| WriteError::Fallback(format!("re-reading the write timestamp: {e}")))?;
    let body = super::gojson::to_vec(&session)
        .map_err(|e| WriteError::Fallback(format!("encoding chat: {e}")))?;

    // Nothing below this line may return `Fallback` — see `create`.
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit chat patch: {e}")))?;
    Ok(super::Answer::json(body))
}

/// `handleDeleteChat`. A missing chat is a 500 in Go — the store returns a
/// plain error, so the `NotFoundError` branch never fires and the handler
/// writes its own fixed 500 body, reproduced here since #278.
fn delete_one(db_path: &Path, id: &str) -> Result<super::Answer, WriteError> {
    let conn = open_for_write(db_path)?;
    let affected = conn
        .execute("DELETE FROM chat_sessions WHERE id = ?1", [id])
        .map_err(|e| {
            log::warn!("deleting session {id:?}: {e}");
            WriteError::Internal("failed to delete chat".to_string())
        })?;
    if affected == 0 {
        log::warn!("deleting session {id:?}: not found");
        return Err(WriteError::Internal("failed to delete chat".to_string()));
    }
    log::info!("chat session deleted session_id={id:?}");
    Ok(super::Answer::no_content())
}

/// `handleBulkDeleteChats`. Unlike the single delete, ids that do not exist are
/// not an error — the statement simply matches nothing.
fn bulk_delete(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<BulkDeleteRequest>(body)?;
    let ids = req.ids.unwrap_or_default();
    if ids.is_empty() {
        return Err(WriteError::BadRequest("ids must not be empty".to_string()));
    }
    if ids.len() > MAX_BULK_IDS {
        return Err(WriteError::BadRequest("too many ids (max 500)".to_string()));
    }

    let conn = open_for_write(db_path)?;
    // `vec!` rather than `iter::repeat_n`, which needs Rust 1.82 and this
    // crate's MSRV is 1.77.
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("DELETE FROM chat_sessions WHERE id IN ({placeholders})");
    conn.execute(&sql, rusqlite::params_from_iter(ids.iter()))
        .map_err(|e| WriteError::Fallback(format!("bulk deleting sessions: {e}")))?;

    // Go's `count` is `len(ids)` — what was asked for, not what matched. An id
    // that exists nowhere is not an error on this route, so the two numbers
    // differ routinely and the line reports the request rather than the effect.
    log::info!("chat sessions bulk deleted count={}", ids.len());
    Ok(super::Answer::no_content())
}

/// The session row, read inside the caller's transaction.
fn get_session_tx(tx: &rusqlite::Transaction, id: &str) -> Result<Option<ChatSession>, WriteError> {
    tx.query_row(
        "SELECT id, title, agent_slug, sdk_session_id, working_directory, model,
                settings_profile_id, total_input_tokens, total_output_tokens,
                total_cache_creation_tokens, total_cache_read_tokens, is_favorite,
                created_at, updated_at, permission_mode,
                continued_from_session_id, continued_from_project_path,
                continued_from_message_count
         FROM chat_sessions WHERE id = ?1",
        [id],
        |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                title: row.get(1)?,
                agent_slug: row.get(2)?,
                sdk_session_id: row.get(3)?,
                working_directory: row.get(4)?,
                model: row.get(5)?,
                settings_profile_id: row.get(6)?,
                permission_mode: row.get(14)?,
                total_input_tokens: row.get(7)?,
                total_output_tokens: row.get(8)?,
                total_cache_creation_tokens: row.get(9)?,
                total_cache_read_tokens: row.get(10)?,
                is_favorite: row.get(11)?,
                created_at: super::gotime::from_sql_text(&row.get::<_, String>(12)?, 12)?,
                updated_at: super::gotime::from_sql_text(&row.get::<_, String>(13)?, 13)?,
                continued_from_session_id: row.get(15)?,
                continued_from_project_path: row.get(16)?,
                continued_from_message_count: row.get(17)?,
            })
        },
    )
    .optional()
    .map_err(|e| WriteError::Fallback(format!("looking up session {id:?}: {e}")))
}

pub(super) fn open_for_write(db_path: &Path) -> Result<rusqlite::Connection, WriteError> {
    let conn = db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::gojson;

    const SCHEMA: &str = "
        CREATE TABLE chat_sessions (
            id                          TEXT PRIMARY KEY,
            title                       TEXT NOT NULL DEFAULT '',
            agent_slug                  TEXT NOT NULL,
            sdk_session_id              TEXT NOT NULL DEFAULT '',
            working_directory           TEXT NOT NULL DEFAULT '',
            model                       TEXT NOT NULL DEFAULT '',
            settings_profile_id         TEXT NOT NULL DEFAULT '',
            total_input_tokens          INTEGER NOT NULL DEFAULT 0,
            total_output_tokens         INTEGER NOT NULL DEFAULT 0,
            total_cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            total_cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
            created_at                  DATETIME NOT NULL,
            updated_at                  DATETIME NOT NULL,
            is_favorite                 INTEGER NOT NULL DEFAULT 0,
            permission_mode             TEXT NOT NULL DEFAULT '',
            continued_from_session_id   TEXT NOT NULL DEFAULT '',
            continued_from_project_path TEXT NOT NULL DEFAULT '',
            continued_from_message_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE chat_messages (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
            role       TEXT NOT NULL,
            content    TEXT NOT NULL DEFAULT '',
            blocks     TEXT NOT NULL DEFAULT '[]',
            timestamp  DATETIME NOT NULL
        );";

    /// Two sessions and a handful of messages, in the exact column shapes the
    /// live database holds — timestamps included, since a DATETIME column
    /// carries `time.Time.String()` rather than RFC 3339.
    fn fixture() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute_batch(
            r#"
            INSERT INTO chat_sessions
                (id, title, agent_slug, sdk_session_id, working_directory, model,
                 settings_profile_id, total_input_tokens, total_output_tokens,
                 total_cache_creation_tokens, total_cache_read_tokens,
                 created_at, updated_at, is_favorite)
            VALUES
                ('older', 'New Chat', 'writer', '', '/w/one', 'claude-sonnet-4-6',
                 '', 0, 0, 0, 0,
                 '2026-01-02 03:04:05 +0000 UTC', '2026-01-02 03:04:05 +0000 UTC', 0),
                ('newer', 'Renamed <b>chat</b> & co', 'writer', 'sdk-1', '/w/two', 'claude-opus-4-1',
                 'work-profile', 1200, 340, 90, 7700,
                 '2026-03-04 05:06:07.5 +0000 UTC', '2026-03-04 05:06:08.123456789 +0000 UTC', 1);

            INSERT INTO chat_messages (session_id, role, content, blocks, timestamp) VALUES
                ('older', 'user', 'Hello <world> & "friends"', '[]',
                 '2026-01-02 03:04:05 +0000 UTC'),
                ('older', 'assistant', '',
                 '[{"type":"thinking","text":"hm"},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"/a"}}]',
                 '2026-01-02 03:04:06.5 +0000 UTC');
            "#,
        )
        .expect("seed");
        file
    }

    fn encoded(value: &impl Serialize) -> String {
        String::from_utf8(gojson::to_vec(value).expect("encode")).expect("utf-8")
    }

    #[test]
    fn sessions_are_ordered_by_most_recently_updated() {
        let file = fixture();
        let sessions = list(file.path()).expect("list");
        assert_eq!(
            sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["newer", "older"]
        );
    }

    #[test]
    fn a_missing_chat_is_none_not_an_error() {
        let file = fixture();
        assert!(get(file.path(), "nope").expect("get").is_none());
        assert!(get(file.path(), "older").expect("get").is_some());
    }

    /// The declaration order of `storage.ChatSession`, which puts the two
    /// timestamps *before* the token counters even though the `SELECT` reads
    /// them after.
    #[test]
    fn the_session_field_order_is_gos_declaration_order() {
        let file = fixture();
        let sessions = list(file.path()).expect("list");
        assert_eq!(
            encoded(&sessions[0]).trim_end(),
            r#"{"id":"newer","title":"Renamed \u003cb\u003echat\u003c/b\u003e \u0026 co","agent_slug":"writer","sdk_session_id":"sdk-1","working_directory":"/w/two","model":"claude-opus-4-1","settings_profile_id":"work-profile","created_at":"2026-03-04T05:06:07.5Z","updated_at":"2026-03-04T05:06:08.123456789Z","total_input_tokens":1200,"total_output_tokens":340,"total_cache_creation_tokens":90,"total_cache_read_tokens":7700,"is_favorite":true}"#
        );
    }

    /// Go's `omitempty` drops the empty profile id, all four zero counters and a
    /// false `is_favorite` — so a fresh chat sends eight fewer keys than a used
    /// one, and the frontend defaults them.
    #[test]
    fn omitempty_drops_the_zero_valued_session_fields() {
        let file = fixture();
        let sessions = list(file.path()).expect("list");
        assert_eq!(
            encoded(&sessions[1]).trim_end(),
            r#"{"id":"older","title":"New Chat","agent_slug":"writer","sdk_session_id":"","working_directory":"/w/one","model":"claude-sonnet-4-6","created_at":"2026-01-02T03:04:05Z","updated_at":"2026-01-02T03:04:05Z"}"#
        );
    }

    /// `handleGetChat` writes a `map[string]any`, and `encoding/json` sorts map
    /// keys — so `messages` comes first however the handler spells it.
    #[test]
    fn the_detail_envelope_puts_messages_before_session() {
        let file = fixture();
        let detail = get(file.path(), "older").expect("get").expect("chat");
        assert!(
            encoded(&detail).starts_with(r#"{"messages":[{"role":"user""#),
            "{}",
            encoded(&detail)
        );
    }

    /// An empty history is `[]`, never `null`: the store builds it with
    /// `make([]ChatMessage, 0)` before the first row.
    #[test]
    fn a_chat_with_no_messages_sends_an_empty_array() {
        let file = fixture();
        let detail = get(file.path(), "newer").expect("get").expect("chat");
        assert!(encoded(&detail).starts_with(r#"{"messages":[],"session":"#));
    }

    #[test]
    fn messages_are_ordered_by_insertion() {
        let file = fixture();
        let detail = get(file.path(), "older").expect("get").expect("chat");
        assert_eq!(
            detail
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["user", "assistant"]
        );
    }

    /// Every way the `blocks` column can fail to produce blocks — and they all
    /// have to reach the wire as an absent key, not as `[]` or an error.
    #[test]
    fn every_blockless_column_shape_omits_the_key() {
        for stored in [
            "",
            "[]",
            "null",
            "not json at all",
            "{\"not\":\"an array\"}",
        ] {
            assert!(
                decode_blocks(stored).is_empty(),
                "{stored:?} should decode to no blocks"
            );
        }

        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute_batch(
            "INSERT INTO chat_sessions (id, agent_slug, created_at, updated_at)
             VALUES ('s', 'a', '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC');
             INSERT INTO chat_messages (session_id, role, content, blocks, timestamp)
             VALUES ('s', 'assistant', 'corrupt', 'not json at all', '2026-01-01 00:00:00 +0000 UTC');",
        )
        .expect("seed");

        let detail = get(file.path(), "s").expect("get").expect("chat");
        let json = encoded(&detail);
        assert!(!json.contains("\"blocks\""), "{json}");
    }

    /// A block carrying only a type sends only a type — `text`, `id`, `name` and
    /// `input` are all `omitempty`.
    #[test]
    fn an_empty_block_field_is_omitted() {
        let blocks = decode_blocks(r#"[{"type":"text"}]"#);
        assert_eq!(encoded(&blocks).trim_end(), r#"[{"type":"text"}]"#);
    }

    /// Go's `compact` preserves the stored key order and number spelling, strips
    /// whitespace outside strings, and HTML-escapes. Decoding into a
    /// `serde_json::Value` and re-encoding would produce
    /// `{"a":[1,2],"s":"a<b>&c","z":1.5}` — sorted and respelled.
    #[test]
    fn a_tool_use_input_keeps_its_key_order_and_number_spelling() {
        let blocks = decode_blocks(
            r#"[ { "type" : "tool_use" , "id" : "t" , "name" : "Bash" ,
                   "input" : { "z" : 1.50 , "a" : [ 1 , 2 ] , "s" : "a<b>&c" } } ]"#,
        );
        assert_eq!(
            encoded(&blocks).trim_end(),
            r#"[{"type":"tool_use","id":"t","name":"Bash","input":{"z":1.50,"a":[1,2],"s":"a\u003cb\u003e\u0026c"}}]"#
        );
    }

    /// `omitempty` on a `json.RawMessage` tests the byte length, and `null` is
    /// four bytes — so a stored explicit null is emitted, while an absent key is
    /// dropped.
    #[test]
    fn an_explicit_null_input_survives_but_an_absent_one_does_not() {
        let blocks = decode_blocks(r#"[{"type":"tool_use","id":"t","name":"X","input":null}]"#);
        assert_eq!(
            encoded(&blocks).trim_end(),
            r#"[{"type":"tool_use","id":"t","name":"X","input":null}]"#
        );

        let blocks = decode_blocks(r#"[{"type":"tool_use","id":"t","name":"X"}]"#);
        assert_eq!(
            encoded(&blocks).trim_end(),
            r#"[{"type":"tool_use","id":"t","name":"X"}]"#
        );
    }

    /// Vectors taken **verbatim from a running Go server** built from this
    /// checkout: each input was written into a scratch instance's `blocks`
    /// column and each expectation is the bytes `GET /api/chats/{id}` came back
    /// with.
    ///
    /// `compact` is a hand-written reimplementation of `encoding/json`'s, so it
    /// is the riskiest code in this module and the one place a silent
    /// divergence would not surface as a failure anywhere else. Pinning the
    /// live evidence here keeps it reproducible after the scratch database is
    /// gone.
    #[test]
    fn a_raw_input_matches_the_bytes_go_actually_produced() {
        // Numbers keep the digits they were stored with — exponent form in
        // either case, negative zero, an integer past f64's exact range,
        // trailing zeros — and so do `\/`, `\t` and `\n`. A
        // `serde_json::Value` round trip respells every one of them.
        let already_compact = r#"[{"type":"tool_use","id":"t4","name":"N","input":{"n":[1e10,-0.0,1E-7,123456789012345678,0.1000],"s":"quote \" back \\\\ slash \/ tab \t nl \n","deep":{"a":{"b":[{"c":null}]}},"empty_obj":{},"empty_arr":[],"t":true,"f":false}}]"#;
        assert_eq!(
            encoded(&decode_blocks(already_compact)).trim_end(),
            already_compact
        );

        // HTML escaping applies to keys as well as values, at every depth, and
        // the whitespace between them goes.
        assert_eq!(
            encoded(&decode_blocks(
                r#"[{"type":"tool_use","id":"t5","name":"N","input":{ "a<k>&" : "v" , "nested" : { "x&y" : [ "<" , ">" , "&" ] } }}]"#
            ))
            .trim_end(),
            r#"[{"type":"tool_use","id":"t5","name":"N","input":{"a\u003ck\u003e\u0026":"v","nested":{"x\u0026y":["\u003c","\u003e","\u0026"]}}}]"#
        );

        // An `input` need not be an object: a number, a string and an array all
        // round-trip, and only the string is escaped.
        assert_eq!(
            encoded(&decode_blocks(
                r#"[{"type":"tool_use","id":"t6","name":"N","input":42},{"type":"tool_use","id":"t7","name":"N","input":"a <string> & more"},{"type":"tool_use","id":"t8","name":"N","input":[ 1 , 2 ]}]"#
            ))
            .trim_end(),
            r#"[{"type":"tool_use","id":"t6","name":"N","input":42},{"type":"tool_use","id":"t7","name":"N","input":"a \u003cstring\u003e \u0026 more"},{"type":"tool_use","id":"t8","name":"N","input":[1,2]}]"#
        );

        // U+2028/U+2029 on both paths at once: `text` goes through gojson's
        // string escaping, `input` through this module's byte pass. The two are
        // written with placeholders so the separators cannot be mistaken for
        // spaces in an editor — which is exactly how they read.
        let separators = r#"[{"type":"text","text":"para1@para2"},{"type":"tool_use","id":"t9","name":"N","input":{"sep":"a@b#c","emoji":"😀 ünïcödé"}}]"#
            .replace('@', "\u{2028}")
            .replace('#', "\u{2029}");
        assert_eq!(
            encoded(&decode_blocks(&separators)).trim_end(),
            r#"[{"type":"text","text":"para1\u2028para2"},{"type":"tool_use","id":"t9","name":"N","input":{"sep":"a\u2028b\u2029c","emoji":"😀 ünïcödé"}}]"#
        );
    }

    // ─── Writes ───────────────────────────────────────────────────────────────

    /// Built by the real migrations, both because the write path checks the
    /// recorded schema version and because `ON DELETE CASCADE` on
    /// `chat_messages` only exists in the real schema.
    fn migrated() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        file
    }

    fn created_id(answer: &super::super::Answer) -> String {
        let body: serde_json::Value =
            serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
        body["id"].as_str().expect("id").to_string()
    }

    #[test]
    fn creating_a_chat_answers_201_with_the_new_session() {
        let file = migrated();
        let answer =
            create(file.path(), br#"{"working_directory":"/tmp","model":"m"}"#).expect("create");

        assert_eq!(answer.status, StatusCode::CREATED);
        let body: serde_json::Value =
            serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
        assert_eq!(body["title"], "New Chat");
        assert_eq!(body["working_directory"], "/tmp");
        assert_eq!(body["model"], "m");
        // omitempty: zero counters and a false favourite are absent, and so is
        // the empty settings profile.
        assert!(body.get("total_input_tokens").is_none());
        assert!(body.get("is_favorite").is_none());
        assert!(body.get("settings_profile_id").is_none());
        // The id is a v4 UUID, as `newSQLiteUUID` produces.
        assert!(uuid::Uuid::parse_str(body["id"].as_str().unwrap()).is_ok());

        assert_eq!(list(file.path()).expect("list").len(), 1);
    }

    /// Migration 30's column, round-tripped. `omitempty` is what keeps every
    /// pre-existing row byte-identical, so the absence in the test above is as
    /// load-bearing as the presence here.
    #[test]
    fn a_chats_permission_mode_is_stored_and_returned() {
        let file = migrated();
        let answer = create(
            file.path(),
            br#"{"working_directory":"/tmp","permission_mode":"plan"}"#,
        )
        .expect("create");

        let body: serde_json::Value =
            serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
        assert_eq!(body["permission_mode"], "plan");

        let stored = list(file.path()).expect("list");
        assert_eq!(stored[0].permission_mode, "plan");
    }

    /// `isValidChatPermissionMode`. All four of Claude Code's modes are legal
    /// here — unlike the *agent* validator, which takes only bypass/default —
    /// and anything else is the service layer's 422 rather than a handler 400.
    /// The check runs before the row is touched, so a rejected request leaves
    /// nothing behind.
    #[test]
    fn an_unknown_permission_mode_is_422_and_creates_nothing() {
        let file = migrated();
        for mode in ["bypass", "default", "plan", "dontAsk", ""] {
            let body = format!(r#"{{"permission_mode":"{mode}"}}"#);
            create(file.path(), body.as_bytes())
                .unwrap_or_else(|e| panic!("{mode:?} should be accepted: {}", e.message()));
        }
        let accepted = list(file.path()).expect("list").len();

        let err = create(file.path(), br#"{"permission_mode":"acceptEverything"}"#).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            err.message(),
            "validation error for \"permission_mode\": must be one of \"bypass\", \"default\", \"plan\", \"dontAsk\", or empty"
        );
        assert_eq!(
            list(file.path()).expect("list").len(),
            accepted,
            "a rejected mode must not have written a row"
        );
    }

    /// A chat with no agent is legal, so the slug is only checked when present.
    #[test]
    fn an_empty_agent_slug_is_not_validated() {
        let file = migrated();
        create(file.path(), br#"{"agent_slug":""}"#).expect("create");
        assert_eq!(list(file.path()).expect("list").len(), 1);
    }

    #[test]
    fn an_unknown_agent_slug_is_404_and_creates_nothing() {
        let file = migrated();
        let err = create(file.path(), br#"{"agent_slug":"ghost"}"#).unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), "agent \"ghost\" not found");
        assert_eq!(list(file.path()).expect("list").len(), 0);
    }

    #[test]
    fn patching_renames_and_favourites() {
        let file = migrated();
        let id = created_id(&create(file.path(), b"{}").expect("create"));

        let answer = patch(file.path(), &id, br#"{"title":"  Renamed  "}"#).expect("patch");
        assert_eq!(answer.status, StatusCode::OK);
        // The title is stored trimmed.
        assert_eq!(
            get(file.path(), &id).expect("get").unwrap().session.title,
            "Renamed"
        );

        patch(file.path(), &id, br#"{"is_favorite":true}"#).expect("patch");
        let session = get(file.path(), &id).expect("get").unwrap().session;
        assert!(session.is_favorite);
        // The rename survived the second patch — the handler writes the whole
        // row back, so a lost read would silently revert it.
        assert_eq!(session.title, "Renamed");
    }

    /// Every one of these is a 400, not a 422: the checks live in the handler,
    /// and the chats handlers never reach `httpErr`.
    #[test]
    fn the_patch_rejections_are_400() {
        let file = migrated();
        let id = created_id(&create(file.path(), b"{}").expect("create"));

        let err = patch(file.path(), &id, b"{}").unwrap_err();
        assert_eq!(err, WriteError::BadRequest("no fields to update".into()));
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);

        let err = patch(file.path(), &id, br#"{"title":"   "}"#).unwrap_err();
        assert_eq!(err, WriteError::BadRequest("title cannot be empty".into()));

        // A null title is "leave it alone", not "set it to empty" — so with no
        // other field it is the no-fields case rather than the empty-title one.
        let err = patch(file.path(), &id, br#"{"title":null}"#).unwrap_err();
        assert_eq!(err, WriteError::BadRequest("no fields to update".into()));
    }

    /// A **404**, not a 400. The status was the whole finding here: the message
    /// alone was asserted and passed while the status was wrong.
    #[test]
    fn patching_a_missing_chat_is_404_with_the_fixed_message() {
        let file = migrated();
        let err = patch(file.path(), "ghost", br#"{"title":"x"}"#).unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), "chat not found");
    }

    /// `chatService.UpdateSession` stamps `UpdatedAt` before writing, and the
    /// handler serializes that same struct — so the 200 body carries the
    /// write's timestamp, not the row's previous one. Returning the stale value
    /// puts the wrong `updated_at` into a list that sorts on it.
    #[test]
    fn the_patch_response_carries_the_new_updated_at() {
        let file = migrated();
        let id = created_id(&create(file.path(), b"{}").expect("create"));
        let before = get(file.path(), &id).expect("get").unwrap().session;

        let answer = patch(file.path(), &id, br#"{"title":"New"}"#).expect("patch");
        let body: serde_json::Value =
            serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
        let responded = body["updated_at"].as_str().expect("updated_at");

        let stored = get(file.path(), &id).expect("get").unwrap().session;
        let stored_text = serde_json::to_value(&stored).expect("value")["updated_at"]
            .as_str()
            .expect("str")
            .to_string();
        let before_text = serde_json::to_value(&before).expect("value")["updated_at"]
            .as_str()
            .expect("str")
            .to_string();

        assert_ne!(
            responded, before_text,
            "the response must not be the old timestamp"
        );
        assert_eq!(
            responded, stored_text,
            "the response must match what was written"
        );
    }

    #[test]
    fn deleting_a_chat_removes_its_messages_too() {
        let file = migrated();
        let id = created_id(&create(file.path(), b"{}").expect("create"));
        {
            let conn = rusqlite::Connection::open(file.path()).expect("open");
            conn.execute(
                "INSERT INTO chat_messages (session_id, role, content, blocks, timestamp)
                 VALUES (?1, 'user', 'hi', '[]', '2026-01-01 00:00:00 +0000 UTC')",
                [&id],
            )
            .expect("message");
        }

        let answer = delete_one(file.path(), &id).expect("delete");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert!(answer.body.is_none());

        // The cascade only fires because the write handle sets foreign_keys=ON.
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let orphans: i64 = conn
            .query_row("SELECT COUNT(*) FROM chat_messages", [], |r| r.get(0))
            .expect("count");
        assert_eq!(orphans, 0, "messages must cascade with their session");
    }

    #[test]
    fn deleting_a_missing_chat_answers_gos_own_500() {
        let file = migrated();
        let err = delete_one(file.path(), "ghost").unwrap_err();
        // Go's store returns a plain error for a missing row, so the handler
        // writes its fixed 500 body — reproduced here since #278.
        assert!(
            matches!(err, WriteError::Internal(ref m) if m == "failed to delete chat"),
            "{err:?}"
        );
    }

    #[test]
    fn bulk_delete_removes_the_named_chats_only() {
        let file = migrated();
        let a = created_id(&create(file.path(), b"{}").expect("a"));
        let b = created_id(&create(file.path(), b"{}").expect("b"));
        let keep = created_id(&create(file.path(), b"{}").expect("keep"));

        let payload = format!(r#"{{"ids":["{a}","{b}","never-existed"]}}"#);
        let answer = bulk_delete(file.path(), payload.as_bytes()).expect("bulk");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);

        // An id that does not exist is not an error here, unlike the single
        // delete — the statement simply matches nothing.
        let remaining = list(file.path()).expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, keep);
    }

    /// A `null` element is `""` to Go — no error — and an empty id simply
    /// matches no row, so the other two are still deleted. Reverting the
    /// deserializer makes this a 400 for a request Go applies (#295).
    #[test]
    fn a_null_id_is_an_empty_string_rather_than_a_400() {
        let file = migrated();
        let a = created_id(&create(file.path(), b"{}").expect("a"));
        let keep = created_id(&create(file.path(), b"{}").expect("keep"));

        let payload = format!(r#"{{"ids":["{a}",null]}}"#);
        let answer = bulk_delete(file.path(), payload.as_bytes()).expect("bulk");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);

        let remaining = list(file.path()).expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, keep);
    }

    #[test]
    fn bulk_delete_bounds_are_400() {
        let file = migrated();
        for (body, want) in [
            (r#"{}"#.to_string(), "ids must not be empty"),
            (r#"{"ids":[]}"#.to_string(), "ids must not be empty"),
            (r#"{"ids":null}"#.to_string(), "ids must not be empty"),
            (
                format!(r#"{{"ids":[{}]}}"#, vec!["\"x\""; 501].join(",")),
                "too many ids (max 500)",
            ),
        ] {
            let err = bulk_delete(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(err.message(), want);
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        }
        // Exactly 500 is allowed — the check is `>`, not `>=`.
        let at_limit = format!(r#"{{"ids":[{}]}}"#, vec!["\"x\""; 500].join(","));
        assert!(bulk_delete(file.path(), at_limit.as_bytes()).is_ok());
    }

    #[test]
    fn the_chat_write_routes_are_claimed_and_the_streaming_ones_are_not() {
        assert!(claims(&Method::POST, "/api/chats"));
        assert!(claims(&Method::DELETE, "/api/chats"));
        assert!(claims(&Method::PATCH, "/api/chats/abc"));
        assert!(claims(&Method::DELETE, "/api/chats/abc"));

        // #276 owns these: a subprocess, in-memory channels and an SSE body.
        assert!(!claims(&Method::POST, "/api/chats/abc/messages"));
        assert!(!claims(&Method::POST, "/api/chats/abc/input"));
        assert!(!claims(&Method::POST, "/api/chats/abc/permission"));
        assert!(!claims(&Method::POST, "/api/chats/abc/stop"));
        assert!(!claims(&Method::PATCH, "/api/chats"));
    }

    /// #335: the chat CRUD's own lines. `chat sessions bulk deleted count=` is
    /// the clearest case for why the access line is not enough — `DELETE
    /// /api/chats 204` cannot say how many.
    #[test]
    fn the_chat_writes_log_their_entity_and_outcome() {
        crate::native::writes::testlog::install();
        let file = migrated();

        let id = created_id(&create(file.path(), b"{}").expect("create"));
        crate::native::writes::testlog::assert_info_once(&format!(
            r#"chat session created session_id="{id}" agent_slug="" settings_profile_id="""#
        ));

        delete_one(file.path(), &id).expect("delete");
        crate::native::writes::testlog::assert_info_once(&format!(
            r#"chat session deleted session_id="{id}""#
        ));

        let a = created_id(&create(file.path(), b"{}").expect("a"));
        let b = created_id(&create(file.path(), b"{}").expect("b"));
        // Three ids, two of which exist: Go's `count` is `len(ids)`, so the line
        // reports what was asked for rather than what matched.
        let payload = format!(r#"{{"ids":["{a}","{b}","never-existed"]}}"#);
        bulk_delete(file.path(), payload.as_bytes()).expect("bulk");
        crate::native::writes::testlog::assert_info_present("chat sessions bulk deleted count=3");
    }
}

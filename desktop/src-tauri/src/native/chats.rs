//! `GET /api/chats` and `GET /api/chats/{id}`.
//!
//! Mirrors `SQLiteChatStore.ListSessions`/`GetSessionWithMessages`
//! (`internal/storage/sqlite_chat_store.go`), `chatService`'s two read methods —
//! which only wrap the store in a span — and `handleListChats`/`handleGetChat`
//! in `internal/api/chats.go`.
//!
//! Reads only. Create, update, delete and the streaming turn stay with Go until
//! the storage layer moves: this is the same database file, and a second writer
//! would race the migrations and seeding the Go server performs on startup.
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

use axum::http::Method;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use super::db;
use super::gotime::GoTime;

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
    #[serde(
        default,
        deserialize_with = "captured_raw",
        skip_serializing_if = "Option::is_none"
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

/// Deserialize the value as-is, including an explicit `null`.
///
/// `Option<Box<RawValue>>`'s own impl turns `null` into `None`, which would drop
/// a key Go emits.
fn captured_raw<'de, D>(deserializer: D) -> Result<Option<Box<RawValue>>, D::Error>
where
    D: Deserializer<'de>,
{
    Box::<RawValue>::deserialize(deserializer).map(Some)
}

const SESSION_COLUMNS: &str =
    "SELECT id, title, agent_slug, sdk_session_id, working_directory, model,
       settings_profile_id, total_input_tokens, total_output_tokens,
       total_cache_creation_tokens, total_cache_read_tokens,
       created_at, updated_at, is_favorite
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
        created_at: timestamp(row, 11)?,
        updated_at: timestamp(row, 12)?,
        total_input_tokens: row.get(7)?,
        total_output_tokens: row.get(8)?,
        total_cache_creation_tokens: row.get(9)?,
        total_cache_read_tokens: row.get(10)?,
        is_favorite: row.get(13)?,
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
/// Unparsable text is an error rather than a zero time: Go's `rows.Scan` fails
/// the whole request there, and the proxy's fallback then lets Go answer.
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

/// Re-encode a captured raw JSON value the way Go's `compact` does.
///
/// Marshaling a `json.RawMessage` runs `encoding/json`'s `compact` with HTML
/// escaping on, which:
///
/// - drops whitespace *outside* strings,
/// - escapes `<`, `>`, `&` and U+2028/U+2029 wherever they appear,
/// - and **changes nothing else** — object keys keep the order they were stored
///   in and numbers keep the digits they were stored with.
///
/// Those last two are why this is a byte pass rather than a decode/re-encode:
/// a stored `{"z":1.50,"a":[1,2]}` stays `{"z":1.50,"a":[1,2]}` on the wire,
/// while a `serde_json::Value` round trip would emit `{"a":[1,2],"z":1.5}` —
/// reordered and respelled, with nothing to signal it.
///
/// In practice the column is already compact and escaped, because Go wrote it
/// with `json.Marshal`; this makes a hand-edited or older row match too.
fn compact_raw_json(raw: Box<RawValue>) -> Box<RawValue> {
    let compacted = compact(raw.get());
    if compacted == raw.get() {
        return raw;
    }
    // Compacting valid JSON leaves valid JSON, so the fallback is unreachable —
    // and keeping the original is the harmless direction if it ever is not.
    RawValue::from_string(compacted).unwrap_or(raw)
}

fn compact(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;

    while i < bytes.len() {
        let byte = bytes[i];

        // U+2028 (E2 80 A8) and U+2029 (E2 80 A9): valid JSON, invalid
        // JavaScript, so Go escapes both.
        if byte == 0xE2
            && i + 2 < bytes.len()
            && bytes[i + 1] == 0x80
            && (bytes[i + 2] & !1) == 0xA8
        {
            out.extend_from_slice(if bytes[i + 2] == 0xA8 {
                b"\\u2028"
            } else {
                b"\\u2029"
            });
            i += 3;
            continue;
        }

        match byte {
            b'<' => out.extend_from_slice(b"\\u003c"),
            b'>' => out.extend_from_slice(b"\\u003e"),
            b'&' => out.extend_from_slice(b"\\u0026"),
            b' ' | b'\t' | b'\n' | b'\r' if !in_string => {}
            _ => out.push(byte),
        }

        // Track string context so whitespace *inside* a string survives. The
        // three escaped bytes above can never change it.
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        }

        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

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
    method == Method::GET && (path == "/api/chats" || id_of(path).is_some())
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
    let body = match id_of(req.path) {
        None => super::gojson::to_vec(&list(&ctx.db_path)?)
            .map_err(|e| format!("encoding chats: {e}"))?,
        Some(id) => match get(&ctx.db_path, id)? {
            Some(detail) => {
                super::gojson::to_vec(&detail).map_err(|e| format!("encoding chat: {e}"))?
            }
            // Falling back lets Go answer the 404, rather than this having to
            // reproduce its body and status.
            None => return Err(format!("chat {id:?} not found")),
        },
    };
    Ok(super::Answer { body, probe: None })
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
            is_favorite                 INTEGER NOT NULL DEFAULT 0
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

    #[test]
    fn compact_leaves_already_compact_json_untouched() {
        let compact_json = r#"{"a":1,"b":[true,null],"s":"x y"}"#;
        assert_eq!(compact(compact_json), compact_json);
    }

    /// Whitespace *inside* a string is content, not formatting.
    #[test]
    fn compact_keeps_whitespace_inside_strings() {
        assert_eq!(
            compact("{ \"k\" : \"a b\\tc\\n d\" }"),
            "{\"k\":\"a b\\tc\\n d\"}"
        );
    }

    /// A quote closes a string unless it is itself escaped — get that wrong and
    /// every space after the first `\"` is stripped out of the payload.
    #[test]
    fn compact_tracks_escaped_quotes() {
        assert_eq!(compact(r#"{"k":"a \" b"}"#), r#"{"k":"a \" b"}"#);
        assert_eq!(compact(r#"{ "k" : "a \\" }"#), r#"{"k":"a \\"}"#);
    }

    #[test]
    fn compact_escapes_the_characters_go_escapes() {
        assert_eq!(compact(r#"{"k":"<&>"}"#), r#"{"k":"\u003c\u0026\u003e"}"#);
        assert_eq!(
            compact("{\"k\":\"a\u{2028}b\u{2029}c\"}"),
            r#"{"k":"a\u2028b\u2029c"}"#
        );
    }

    /// Multi-byte UTF-8 has to survive a byte-wise pass intact.
    #[test]
    fn compact_preserves_multibyte_content() {
        assert_eq!(
            compact(r#"{ "k" : "ünïcödé 😀" }"#),
            r#"{"k":"ünïcödé 😀"}"#
        );
    }
}

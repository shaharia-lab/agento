//! What a finished turn writes. Mirrors `chatService.CommitMessage`
//! (`internal/service/chat_service.go`).
//!
//! # "A turn that produced no final text persists nothing" — but only messages
//!
//! The rule is real and load-bearing: an interrupted stream must not leave an
//! orphaned user message, because the Claude CLI's own transcript does not have
//! one and the two would diverge on the next resume. Go guards **both**
//! `AppendMessage` calls on `assistantText != ""`.
//!
//! What the phrase does *not* cover, and a port that stops reading there gets
//! wrong: the `UPDATE chat_sessions` runs **unconditionally**. An aborted,
//! errored or text-less turn still writes `updated_at`, the accumulated token
//! totals, and — on a first message — a title derived from a user message that
//! was never stored.
//!
//! # No transaction, deliberately faithful
//!
//! Go issues two inserts and an update with no transaction around them, so a
//! failed assistant insert leaves the user row behind. Wrapping them here would
//! be an improvement that makes the two implementations differ on a failure
//! path, so the writes are issued the same way.

use rusqlite::Connection;

use super::runner::ChatRow;
use super::turn::TurnState;
use crate::native::chats::MessageBlock;

/// Persist a finished turn.
pub fn commit(
    db_path: &std::path::Path,
    row: &ChatRow,
    user_content: &str,
    state: &TurnState,
    is_first_message: bool,
) -> Result<(), String> {
    let conn = crate::native::db::open_read_write(db_path)?;
    crate::native::migrate::verify(&conn)?;

    if !state.assistant_text.is_empty() {
        if !user_content.is_empty() {
            append_message(&conn, &row.id, "user", user_content, &[])?;
        }
        append_message(
            &conn,
            &row.id,
            "assistant",
            &state.assistant_text,
            &state.blocks,
        )?;
    }

    // Always, whatever the turn produced.
    let title = if is_first_message {
        truncate_title(user_content, 60)
    } else {
        row.title.clone()
    };
    // Only overwrite the CLI session id when the turn returned one: an
    // interrupted turn reports none, and blanking it would make the next
    // message start a new CLI session instead of resuming this one.
    let sdk_session_id = if state.sdk_session_id.is_empty() {
        row.sdk_session_id.clone()
    } else {
        state.sdk_session_id.clone()
    };

    let now = crate::native::gotime::now_go_text();
    conn.execute(
        "UPDATE chat_sessions SET
            title = ?1, sdk_session_id = ?2,
            total_input_tokens = total_input_tokens + ?3,
            total_output_tokens = total_output_tokens + ?4,
            total_cache_creation_tokens = total_cache_creation_tokens + ?5,
            total_cache_read_tokens = total_cache_read_tokens + ?6,
            updated_at = ?7
         WHERE id = ?8",
        rusqlite::params![
            title,
            sdk_session_id,
            state.input_tokens,
            state.output_tokens,
            state.cache_creation_tokens,
            state.cache_read_tokens,
            now,
            row.id,
        ],
    )
    .map_err(|e| format!("updating session {:?}: {e}", row.id))?;

    Ok(())
}

fn append_message(
    conn: &Connection,
    session_id: &str,
    role: &str,
    content: &str,
    blocks: &[MessageBlock],
) -> Result<(), String> {
    // The column defaults to the literal `[]` rather than to NULL, and the read
    // path distinguishes them.
    let encoded = if blocks.is_empty() {
        "[]".to_string()
    } else {
        let bytes = crate::native::gojson::to_vec_marshal(&blocks)
            .map_err(|e| format!("encoding blocks: {e}"))?;
        String::from_utf8(bytes).map_err(|e| format!("blocks are not UTF-8: {e}"))?
    };
    conn.execute(
        "INSERT INTO chat_messages (session_id, role, content, blocks, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            session_id,
            role,
            content,
            encoded,
            crate::native::gotime::now_go_text()
        ],
    )
    .map_err(|e| format!("storing {role} message: {e}"))?;
    Ok(())
}

/// `truncateTitle`: cut at 60 **runes** and append an ellipsis.
///
/// Runes, not bytes — a title of accented text would otherwise be cut mid
/// character and stored as invalid UTF-8.
fn truncate_title(content: &str, max: usize) -> String {
    let count = content.chars().count();
    if count <= max {
        return content.to_string();
    }
    let cut: String = content.chars().take(max).collect();
    format!("{cut}...")
}

/// `appendAssistantBlocks`: keep non-empty `thinking` and `text`, and **every**
/// `tool_use`, in arrival order. An unparseable event leaves the list untouched
/// rather than failing the turn.
///
/// # The `input` must never round-trip through a `Value`
///
/// A `tool_use` input of `{"z":1.50,"a":1}` is stored by Go through `compact`,
/// which preserves key order and number spelling. Decoding it into a
/// `serde_json::Value` and re-encoding sorts the keys and drops the trailing
/// zero — `{"a":1,"z":1.5}` — with nothing to signal it. This is not
/// hypothetical: the first version of this function did exactly that, and
/// `tests/chat_turn.rs` caught it. `RawValue` is what keeps the bytes.
///
/// # …and it is compacted **on store**, not on emit (#298)
///
/// This comment used to say Go stored the bytes verbatim and compacted them on
/// the way out. It is the other way round: `chatService` marshals a struct
/// holding a `json.RawMessage`, and `encoding/json` runs
/// `compact(…, escapeHTML=true)` over a nested raw value as it marshals — so
/// what reaches the column is already whitespace-stripped and has `<`, `>` and
/// `&` escaped. Writing the SDK's bytes as-is left the two implementations'
/// databases different for the same input. It was masked on read, since
/// `chats::decode_blocks` compacts what it loads, which is exactly why nothing
/// noticed.
pub fn append_assistant_blocks(blocks: &mut Vec<MessageBlock>, raw: &[u8]) {
    #[derive(serde::Deserialize)]
    struct Envelope<'a> {
        #[serde(borrow)]
        message: Option<Message<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct Message<'a> {
        #[serde(borrow, default)]
        content: Vec<ContentBlock<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct ContentBlock<'a> {
        #[serde(rename = "type", default)]
        block_type: &'a str,
        #[serde(default)]
        thinking: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        /// Borrowed raw, so the bytes are the CLI's own.
        #[serde(borrow, default)]
        input: Option<&'a serde_json::value::RawValue>,
    }

    let Ok(envelope) = serde_json::from_slice::<Envelope>(raw) else {
        return;
    };
    let Some(message) = envelope.message else {
        return;
    };

    for block in message.content {
        match block.block_type {
            "thinking" => {
                let text = block.thinking.unwrap_or_default();
                if !text.is_empty() {
                    blocks.push(MessageBlock {
                        block_type: "thinking".into(),
                        text,
                        id: String::new(),
                        name: String::new(),
                        input: None,
                    });
                }
            }
            "text" => {
                let text = block.text.unwrap_or_default();
                if !text.is_empty() {
                    blocks.push(MessageBlock {
                        block_type: "text".into(),
                        text,
                        id: String::new(),
                        name: String::new(),
                        input: None,
                    });
                }
            }
            "tool_use" => {
                blocks.push(MessageBlock {
                    block_type: "tool_use".into(),
                    text: String::new(),
                    id: block.id.unwrap_or_default(),
                    name: block.name.unwrap_or_default(),
                    // `to_owned` on the borrowed raw copies the bytes, not a
                    // parsed representation of them — then `compact_raw` puts
                    // them in the form Go's marshal would have stored, which is
                    // where the column's bytes are decided.
                    input: block
                        .input
                        .map(|raw| serde_json::value::RawValue::from_string(raw.get().to_string()))
                        .transpose()
                        .ok()
                        .flatten()
                        .map(crate::native::gojson::compact_raw),
                });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO chat_sessions (id, title, agent_slug, created_at, updated_at)
             VALUES ('c1', 'New Chat', '', '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed");
        file
    }

    fn row() -> ChatRow {
        ChatRow {
            id: "c1".into(),
            title: "New Chat".into(),
            agent_slug: String::new(),
            sdk_session_id: String::new(),
            working_dir: String::new(),
            model: String::new(),
            settings_profile_id: String::new(),
            permission_mode: String::new(),
        }
    }

    fn message_count(file: &tempfile::NamedTempFile) -> i64 {
        let conn = Connection::open(file.path()).expect("open");
        conn.query_row("SELECT COUNT(*) FROM chat_messages", [], |r| r.get(0))
            .expect("count")
    }

    #[test]
    fn a_turn_with_text_stores_both_messages() {
        let file = migrated();
        let state = TurnState {
            assistant_text: "hello".into(),
            sdk_session_id: "sdk-1".into(),
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        commit(file.path(), &row(), "hi", &state, true).expect("commit");

        assert_eq!(message_count(&file), 2);
        let conn = Connection::open(file.path()).expect("open");
        let (title, sdk, input): (String, String, i64) = conn
            .query_row(
                "SELECT title, sdk_session_id, total_input_tokens FROM chat_sessions WHERE id='c1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("row");
        assert_eq!(title, "hi");
        assert_eq!(sdk, "sdk-1");
        assert_eq!(input, 10);
    }

    /// The rule, and the half of it that is easy to over-apply: no messages,
    /// but the session row is still written.
    #[test]
    fn a_turn_with_no_text_stores_no_messages_but_still_updates_the_session() {
        let file = migrated();
        let state = TurnState {
            assistant_text: String::new(),
            input_tokens: 7,
            ..Default::default()
        };
        commit(file.path(), &row(), "hi", &state, true).expect("commit");

        assert_eq!(message_count(&file), 0, "not even the user message");

        let conn = Connection::open(file.path()).expect("open");
        let (title, input): (String, i64) = conn
            .query_row(
                "SELECT title, total_input_tokens FROM chat_sessions WHERE id='c1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row");
        // The title comes from a user message that was never stored.
        assert_eq!(title, "hi");
        assert_eq!(input, 7, "token totals accumulate regardless");
    }

    /// Blanking the CLI session id would make the next message start a new
    /// session instead of resuming this one.
    #[test]
    fn an_interrupted_turn_keeps_the_previous_sdk_session_id() {
        let file = migrated();
        let mut existing = row();
        existing.sdk_session_id = "sdk-original".into();
        {
            let conn = Connection::open(file.path()).expect("open");
            conn.execute(
                "UPDATE chat_sessions SET sdk_session_id='sdk-original' WHERE id='c1'",
                [],
            )
            .expect("seed");
        }

        let state = TurnState::default();
        commit(file.path(), &existing, "hi", &state, false).expect("commit");

        let conn = Connection::open(file.path()).expect("open");
        let sdk: String = conn
            .query_row(
                "SELECT sdk_session_id FROM chat_sessions WHERE id='c1'",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(sdk, "sdk-original");
    }

    #[test]
    fn token_totals_accumulate_rather_than_replace() {
        let file = migrated();
        let state = TurnState {
            assistant_text: "a".into(),
            input_tokens: 10,
            output_tokens: 1,
            ..Default::default()
        };
        commit(file.path(), &row(), "hi", &state, false).expect("first");
        commit(file.path(), &row(), "hi", &state, false).expect("second");

        let conn = Connection::open(file.path()).expect("open");
        let input: i64 = conn
            .query_row(
                "SELECT total_input_tokens FROM chat_sessions WHERE id='c1'",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(input, 20);
    }

    #[test]
    fn the_title_is_cut_at_sixty_runes() {
        assert_eq!(truncate_title("short", 60), "short");
        let long = "a".repeat(61);
        assert_eq!(truncate_title(&long, 60), format!("{}...", "a".repeat(60)));
        // Runes, not bytes: 61 accented characters are 122 bytes, and a byte
        // cut would split one in half.
        let accented = "é".repeat(61);
        let cut = truncate_title(&accented, 60);
        assert_eq!(cut.chars().count(), 63); // 60 + "..."
        assert!(cut.starts_with(&"é".repeat(60)));
    }

    #[test]
    fn blocks_keep_thinking_text_and_every_tool_use_in_order() {
        let mut blocks = Vec::new();
        append_assistant_blocks(
            &mut blocks,
            br#"{"message":{"content":[
                {"type":"thinking","thinking":"hmm"},
                {"type":"text","text":""},
                {"type":"text","text":"hello"},
                {"type":"tool_use","id":"t1","name":"Bash","input":{"z":1.50,"a":1}}
            ]}}"#,
        );
        assert_eq!(blocks.len(), 3, "the empty text block is dropped");
        assert_eq!(blocks[0].block_type, "thinking");
        assert_eq!(blocks[1].text, "hello");
        assert_eq!(blocks[2].name, "Bash");
    }

    /// The stored bytes are what Go's marshal would have written, not the SDK's
    /// own (#298): whitespace stripped and `<`, `>`, `&` escaped, with the key
    /// order and number spelling intact.
    ///
    /// Both halves matter and they pull in opposite directions — `compact` is
    /// the one pass that does the first without doing what a `Value` round trip
    /// would do to the second.
    #[test]
    fn a_stored_tool_use_input_is_compacted_the_way_gos_marshal_stores_one() {
        let mut blocks = Vec::new();
        append_assistant_blocks(
            &mut blocks,
            br#"{"message":{"content":[
                {"type":"tool_use","id":"t1","name":"Bash","input":{ "z" : 1.50 , "cmd" : "ls & cat <f>" }}
            ]}}"#,
        );

        // Exactly what Go writes for the same input — measured, not assumed:
        // marshalling a struct holding this `json.RawMessage` yields
        // `{"z":1.50,"cmd":"ls \u0026 cat \u003cf\u003e"}`.
        let input = blocks[0].input.as_ref().expect("an input").get();
        assert_eq!(input, r#"{"z":1.50,"cmd":"ls \u0026 cat \u003cf\u003e"}"#);
    }

    /// …and that is what lands in the column, since `append_message` marshals
    /// the blocks. Asserted end to end because the read path compacts too, so a
    /// verbatim write is invisible through the API — which is exactly why this
    /// went unnoticed.
    #[test]
    fn the_blocks_column_holds_gos_bytes() {
        let file = migrated();
        let mut blocks = Vec::new();
        append_assistant_blocks(
            &mut blocks,
            br#"{"message":{"content":[
                {"type":"tool_use","id":"t1","name":"Bash","input":{ "z" : 1.50 , "cmd" : "a & b" }}
            ]}}"#,
        );
        let state = TurnState {
            assistant_text: "done".into(),
            blocks,
            ..Default::default()
        };
        commit(file.path(), &row(), "q", &state, true).expect("commit");

        let conn = Connection::open(file.path()).expect("open");
        let stored: String = conn
            .query_row(
                "SELECT blocks FROM chat_messages WHERE role = 'assistant'",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert!(
            stored.contains(r#""input":{"z":1.50,"cmd":"a \u0026 b"}"#),
            "{stored}"
        );
    }

    #[test]
    fn an_unparseable_assistant_event_leaves_the_blocks_untouched() {
        let mut blocks = Vec::new();
        append_assistant_blocks(&mut blocks, b"not json");
        append_assistant_blocks(&mut blocks, b"{}");
        assert!(blocks.is_empty());
    }
}

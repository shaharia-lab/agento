//! The `messaging` service's eleven tools, ported from
//! `internal/integrations/telegram/tools.go`.
//!
//! One service group registered in three batches (`registerSendingTools`,
//! `registerReadingTools`, `registerManagementTools`). Conventions are
//! `native/integrations/github/repos.rs`'s.
//!
//! # Two shapes no earlier integration had, both documented as unreached until now
//!
//! `claude/schema_vectors.rs` lists three reflector divergences left standing
//! because "nothing in the six integrations" reached them. Telegram reaches two:
//!
//! 1. **`create_poll` takes `Options []string`** — the first slice parameter
//!    anywhere in the six. `jsonschema-go` renders *every* slice as
//!    `["null","array"]`, because a Go nil slice marshals as `null` and must be
//!    accepted back; `Vec<String>` renders a bare `array`. The map's guidance is
//!    explicit — "a port that needs one must add the null itself" — so
//!    [`go_string_slice`] does, and `null_is_zero_value` makes the null decode to
//!    an empty `Vec` the way it decodes to a nil slice. That matters beyond the
//!    schema: `len(nil)` is 0, so a null reaches Go's own "2-10 options" refusal
//!    rather than a decode error.
//! 2. **`send_location` takes `float64`** — the first float parameters. The
//!    schema side is free (`normalize_go_schema` drops the `format` `schemars`
//!    adds), but the *encoding* is not: `encoding/json` spells a float its own
//!    way, and `gojson`'s `go_float` already reproduces it — `1e+21` and `1e-7`
//!    among the shapes it gets right and `serde_json` does not.
//!
//! # Other things that read as mistakes and are Go's
//!
//! - **`read_messages` sends `offset` only when it is non-zero, and the test is
//!   `!= 0`** — so a *negative* offset is sent, which is a documented Telegram
//!   idiom for "the last N updates". `> 0` would drop it.
//! - **`read_messages`' limit falls back to its own maximum.** `<= 0 || > 100`
//!   becomes 100, not a smaller default like every other integration's clamp.
//! - **`timeout: 0` is always sent**, with the comment "never long-poll inside a
//!   tool call" — a key no argument controls.
//! - **`delete_message` and `pin_message` discard the response** and answer a
//!   fixed sentence; `create_poll` validates before it makes any request at all.

use schemars::JsonSchema;
use serde_json::{json, Value};

use crate::claude::{new_tool, CancellationToken, ToolDef};

use super::client::{clamp_limit, Client};
use super::text_result;

/// `json.Marshal(payload)`, which `callTelegram` does inside itself in Go.
fn marshal(payload: &Value) -> Result<Vec<u8>, String> {
    crate::native::gojson::to_vec_marshal(payload).map_err(|e| format!("marshaling request: {e}"))
}

/// `jsonschema-go`'s rendering of a `[]string`, which is not `schemars`'.
///
/// Go admits a JSON `null` for every slice — a nil slice marshals as `null`, so
/// the reflector accepts one back — and renders `"type": ["null","array"]`. This
/// is the "a port that needs one must add the null itself" case from
/// `claude/schema_vectors.rs`, and `create_poll` is the first tool in the six to
/// need it.
fn go_string_slice(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["null", "array"],
        "items": {"type": "string"},
    })
}

/// `send_message`.
#[allow(dead_code)] // read through serde, never constructed in Rust
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendMessageInput {
    /// required,Target chat ID or username
    chat_id: String,
    /// required,Text of the message to send
    text: String,
    /// Optional parse mode: Markdown or HTML
    parse_mode: String,
}

pub fn send_message(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "send_message",
        "Sends a text message to a Telegram chat.",
        move |input: SendMessageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut payload = json!({"chat_id": input.chat_id, "text": input.text});
                if !input.parse_mode.is_empty() {
                    payload["parse_mode"] = Value::String(input.parse_mode.clone());
                }
                let response = client.call(&ct, "sendMessage", marshal(&payload)?).await?;
                Ok(text_result(format!(
                    "Message sent successfully. Response: {}",
                    response.result()
                )))
            }
        },
    )
}

/// `send_photo`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendPhotoInput {
    /// required,Target chat ID or username
    chat_id: String,
    /// required,Photo URL to send
    photo: String,
    /// Optional caption for the photo
    caption: String,
}

pub fn send_photo(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "send_photo",
        "Sends a photo to a Telegram chat by URL.",
        move |input: SendPhotoInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut payload = json!({"chat_id": input.chat_id, "photo": input.photo});
                if !input.caption.is_empty() {
                    payload["caption"] = Value::String(input.caption.clone());
                }
                let response = client.call(&ct, "sendPhoto", marshal(&payload)?).await?;
                Ok(text_result(format!(
                    "Photo sent successfully. Response: {}",
                    response.result()
                )))
            }
        },
    )
}

/// `send_location`.
///
/// The first tool in the six with float parameters — see the module header on
/// why the encoding matters and the schema does not.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendLocationInput {
    /// required,Target chat ID or username
    chat_id: String,
    /// required,Latitude of the location
    latitude: f64,
    /// required,Longitude of the location
    longitude: f64,
}

pub fn send_location(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "send_location",
        "Sends a geographic location to a chat.",
        move |input: SendLocationInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({
                    "chat_id": input.chat_id,
                    "latitude": input.latitude,
                    "longitude": input.longitude,
                });
                let response = client.call(&ct, "sendLocation", marshal(&payload)?).await?;
                Ok(text_result(format!(
                    "Location sent successfully. Response: {}",
                    response.result()
                )))
            }
        },
    )
}

/// `create_poll`.
///
/// The first tool in the six with a slice parameter — see the module header and
/// [`go_string_slice`].
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreatePollInput {
    /// required,Target chat ID or username
    chat_id: String,
    /// required,Poll question (1-300 characters)
    question: String,
    /// required,Answer options (2-10 strings)
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    #[schemars(schema_with = "go_string_slice")]
    options: Vec<String>,
    /// True if poll should be anonymous
    is_anonymous: bool,
    /// Poll type: regular or quiz (default regular)
    #[serde(rename = "type")]
    poll_type: String,
}

pub fn create_poll(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "create_poll",
        "Creates a poll in a Telegram chat.",
        move |input: CreatePollInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // Go refuses **before** it builds a request, so a bad option
                // count never reaches Telegram — and a `null` arrives here as an
                // empty vec, exactly as a nil slice does, so it is a `got 0`
                // rather than a decode failure.
                if input.options.len() < 2 || input.options.len() > 10 {
                    return Err(format!(
                        "poll requires 2-10 options, got {}",
                        input.options.len()
                    ));
                }

                let mut payload = json!({
                    "chat_id": input.chat_id,
                    "question": input.question,
                    "options": input.options,
                });
                if input.is_anonymous {
                    payload["is_anonymous"] = Value::Bool(true);
                }
                if !input.poll_type.is_empty() {
                    payload["type"] = Value::String(input.poll_type.clone());
                }
                let response = client.call(&ct, "sendPoll", marshal(&payload)?).await?;
                Ok(text_result(format!(
                    "Poll created successfully. Response: {}",
                    response.result()
                )))
            }
        },
    )
}

/// `read_messages`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadMessagesInput {
    /// Identifier of the first update to be returned
    offset: i64,
    /// Max number of updates to retrieve (1-100)
    limit: i64,
}

pub fn read_messages(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "read_messages",
        "Reads recent messages (updates) received by the bot via getUpdates.",
        move |input: ReadMessagesInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // `timeout: 0` always — "never long-poll inside a tool call".
                let mut payload = json!({"timeout": 0});
                // `!= 0`, not `> 0`: a **negative** offset is Telegram's idiom
                // for "the last N updates" and is deliberately sent.
                if input.offset != 0 {
                    payload["offset"] = json!(input.offset);
                }
                // The fallback is the *maximum*, unlike every other clamp here.
                payload["limit"] = json!(clamp_limit(input.limit));
                let response = client.call(&ct, "getUpdates", marshal(&payload)?).await?;
                Ok(text_result(format!(
                    "Updates received: {}",
                    response.result()
                )))
            }
        },
    )
}

/// `get_chat_info`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetChatInfoInput {
    /// required,Target chat ID or username
    chat_id: String,
}

pub fn get_chat_info(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_chat_info",
        "Gets detailed information about a chat.",
        move |input: GetChatInfoInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({"chat_id": input.chat_id});
                let response = client.call(&ct, "getChat", marshal(&payload)?).await?;
                Ok(text_result(format!("Chat info: {}", response.result())))
            }
        },
    )
}

/// `get_chat_members`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetChatMembersInput {
    /// required,Unique identifier for the target chat
    chat_id: String,
}

pub fn get_chat_members(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_chat_members",
        "Gets the number of members in a chat.",
        move |input: GetChatMembersInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({"chat_id": input.chat_id});
                // Telegram's own spelling, deprecated on their side and kept
                // here because changing it would change the request.
                let response = client
                    .call(&ct, "getChatMembersCount", marshal(&payload)?)
                    .await?;
                Ok(text_result(format!(
                    "Chat member count: {}",
                    response.result()
                )))
            }
        },
    )
}

/// `forward_message`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForwardMessageInput {
    /// required,Target chat to forward the message to
    chat_id: String,
    /// required,Source chat ID
    from_chat_id: String,
    /// required,Message identifier in from_chat_id
    message_id: i64,
}

pub fn forward_message(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "forward_message",
        "Forwards a message from one chat to another.",
        move |input: ForwardMessageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({
                    "chat_id": input.chat_id,
                    "from_chat_id": input.from_chat_id,
                    "message_id": input.message_id,
                });
                let response = client
                    .call(&ct, "forwardMessage", marshal(&payload)?)
                    .await?;
                Ok(text_result(format!(
                    "Message forwarded successfully. Response: {}",
                    response.result()
                )))
            }
        },
    )
}

/// `edit_message`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditMessageInput {
    /// required,The chat containing the message to edit
    chat_id: String,
    /// required,Identifier of the message to edit
    message_id: i64,
    /// required,New text of the message
    text: String,
    /// Optional parse mode: Markdown or HTML
    parse_mode: String,
}

pub fn edit_message(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "edit_message",
        "Edits the text of a previously sent message.",
        move |input: EditMessageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut payload = json!({
                    "chat_id": input.chat_id,
                    "message_id": input.message_id,
                    "text": input.text,
                });
                if !input.parse_mode.is_empty() {
                    payload["parse_mode"] = Value::String(input.parse_mode.clone());
                }
                let response = client
                    .call(&ct, "editMessageText", marshal(&payload)?)
                    .await?;
                Ok(text_result(format!(
                    "Message edited successfully. Response: {}",
                    response.result()
                )))
            }
        },
    )
}

/// `delete_message`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteMessageInput {
    /// required,The chat containing the message to delete
    chat_id: String,
    /// required,Identifier of the message to delete
    message_id: i64,
}

pub fn delete_message(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "delete_message",
        "Deletes a message from a chat.",
        move |input: DeleteMessageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({
                    "chat_id": input.chat_id,
                    "message_id": input.message_id,
                });
                // The response is **discarded**: a fixed sentence, whatever
                // Telegram answered.
                client
                    .call(&ct, "deleteMessage", marshal(&payload)?)
                    .await?;
                Ok(text_result("Message deleted successfully.".to_string()))
            }
        },
    )
}

/// `pin_message`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PinMessageInput {
    /// required,The chat containing the message to pin
    chat_id: String,
    /// required,Identifier of the message to pin
    message_id: i64,
    /// Pin silently without notifying members
    disable_notification: bool,
}

pub fn pin_message(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "pin_message",
        "Pins a message in a chat.",
        move |input: PinMessageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut payload = json!({
                    "chat_id": input.chat_id,
                    "message_id": input.message_id,
                });
                // `if params.DisableNotification` — the literal `true`, so a
                // false flag leaves no key at all.
                if input.disable_notification {
                    payload["disable_notification"] = Value::Bool(true);
                }
                client
                    .call(&ct, "pinChatMessage", marshal(&payload)?)
                    .await?;
                Ok(text_result("Message pinned successfully.".to_string()))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(payload: &Value) -> String {
        String::from_utf8(marshal(payload).expect("encode")).expect("utf-8")
    }

    /// Go's float spelling, which `serde_json` does not share. Measured against
    /// `json.Marshal` before it was written.
    #[test]
    fn coordinates_are_spelled_the_way_encoding_json_spells_them() {
        for (value, want) in [
            (51.5074_f64, "51.5074"),
            (-0.1278, "-0.1278"),
            (0.0, "0"),
            (90.0, "90"),
            (-180.0, "-180"),
            (1e21, "1e+21"),
            (1e-7, "1e-7"),
        ] {
            assert_eq!(
                encoded(&json!({"latitude": value})),
                format!(r#"{{"latitude":{want}}}"#),
                "{value}"
            );
        }
    }

    /// A body is sorted at every level and HTML-escaped, so `send_message`'s is
    /// not in the handler's order.
    #[test]
    fn a_body_is_sorted_and_html_escaped() {
        assert_eq!(
            encoded(&json!({"text": "a <b> & c", "chat_id": "@x"})),
            r#"{"chat_id":"@x","text":"a \u003cb\u003e \u0026 c"}"#
        );
    }

    /// The clamp whose fallback is its own maximum.
    #[test]
    fn the_limit_falls_back_to_its_own_maximum() {
        for (input, want) in [(0_i64, 100_i64), (-5, 100), (101, 100), (100, 100), (1, 1)] {
            assert_eq!(clamp_limit(input), want, "{input}");
        }
    }
}

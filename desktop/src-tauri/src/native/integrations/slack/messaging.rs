//! The `messaging` service's seven tools, ported from
//! `internal/integrations/slack/tools.go`.
//!
//! One service group registered in three batches (`registerChannelTools`,
//! `registerMessageTools`, `registerWorkspaceTools`). Conventions are
//! `native/integrations/github/repos.rs`'s: Go `json:` tags as field names, Go
//! `jsonschema:` tags as doc comments verbatim, no `Option`, and
//! `deny_unknown_fields`.
//!
//! # What is different here
//!
//! - **Five tools return Slack's body untouched** and two prefix it (`Message
//!   sent successfully. Response: …`). The five that pass it through do not even
//!   wrap it in a label, unlike every other integration.
//! - **Two encodings.** `list_channels`, `get_channel_info`, `read_messages`,
//!   `list_users` and `search_messages` send `url.Values.Encode()`; `send_message`
//!   and `send_reply` send `json.Marshal` of a `map[string]any`. Both sort their
//!   keys, and both are pinned as request bodies in the vectors.
//! - **Every clamp is different.** `<= 0 || > max` with a per-tool `max` and a
//!   per-tool fallback: 1000/100 for the two listers, 100/20 for `read_messages`
//!   and for `search_messages`' count, and `page <= 0 → 1` with no ceiling at
//!   all. Read one by one from `tools.go` rather than generalised from the first.
//! - **`search_messages`' description says it needs a user token** and will error
//!   on a bot token. That is advertised text, so it is copied verbatim; nothing
//!   in the code enforces it, because Slack does.

use schemars::JsonSchema;
use serde_json::{json, Value};

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::Values;

use super::client::{clamp, Client};
use super::text_result;

/// `json.Marshal(payload)` for the two JSON-body tools.
fn marshal(payload: &Value) -> Result<Vec<u8>, String> {
    crate::native::gojson::to_vec_marshal(payload).map_err(|e| format!("marshaling request: {e}"))
}

/// `list_channels`.
#[allow(dead_code)] // read through serde, never constructed in Rust
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListChannelsInput {
    /// Maximum number of channels to return (default 100, max 1000)
    limit: i64,
    /// Pagination cursor for next page of results
    cursor: String,
}

pub fn list_channels(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_channels",
        "Lists Slack channels (public and private) the bot has access to.",
        move |input: ListChannelsInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut values = Values::new();
                values.set("types", "public_channel,private_channel");
                values.set("limit", clamp(input.limit, 1000, 100).to_string());
                // A cursor is written only when non-empty, so the first page
                // carries no `cursor` key at all.
                if !input.cursor.is_empty() {
                    values.set("cursor", &input.cursor);
                }
                let body = client
                    .call_form(&ct, "conversations.list", values.encode())
                    .await?;
                // The body, verbatim and unlabelled.
                Ok(text_result(body))
            }
        },
    )
}

/// `get_channel_info`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetChannelInfoInput {
    /// required,Channel ID to get info for
    channel: String,
}

pub fn get_channel_info(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "get_channel_info",
        "Gets detailed information about a Slack channel.",
        move |input: GetChannelInfoInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut values = Values::new();
                values.set("channel", &input.channel);
                let body = client
                    .call_form(&ct, "conversations.info", values.encode())
                    .await?;
                Ok(text_result(body))
            }
        },
    )
}

/// `read_messages`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadMessagesInput {
    /// required,Channel ID to read messages from
    channel: String,
    /// Maximum number of messages to return (default 20, max 100)
    limit: i64,
    /// Pagination cursor for next page of results
    cursor: String,
}

pub fn read_messages(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "read_messages",
        "Reads recent messages from a Slack channel.",
        move |input: ReadMessagesInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut values = Values::new();
                values.set("channel", &input.channel);
                // 100/20 here, not 1000/100 — the two listers' ceiling is ten
                // times this one's.
                values.set("limit", clamp(input.limit, 100, 20).to_string());
                if !input.cursor.is_empty() {
                    values.set("cursor", &input.cursor);
                }
                let body = client
                    .call_form(&ct, "conversations.history", values.encode())
                    .await?;
                Ok(text_result(body))
            }
        },
    )
}

/// `send_message`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendMessageInput {
    /// required,Channel ID to send the message to
    channel: String,
    /// required,Text of the message to send
    text: String,
}

pub fn send_message(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "send_message",
        "Sends a message to a Slack channel.",
        move |input: SendMessageInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({"channel": input.channel, "text": input.text});
                let body = client
                    .call_json(&ct, "chat.postMessage", marshal(&payload)?)
                    .await?;
                Ok(text_result(format!(
                    "Message sent successfully. Response: {body}"
                )))
            }
        },
    )
}

/// `send_reply`.
///
/// The same Slack method as [`send_message`] with one more key — so the
/// *request* is nearly identical and the result sentence is not.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendReplyInput {
    /// required,Channel ID containing the thread
    channel: String,
    /// required,Timestamp of the parent message to reply to
    thread_ts: String,
    /// required,Text of the reply
    text: String,
}

pub fn send_reply(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "send_reply",
        "Sends a threaded reply to a message in a Slack channel.",
        move |input: SendReplyInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let payload = json!({
                    "channel": input.channel,
                    "thread_ts": input.thread_ts,
                    "text": input.text,
                });
                let body = client
                    .call_json(&ct, "chat.postMessage", marshal(&payload)?)
                    .await?;
                Ok(text_result(format!(
                    "Reply sent successfully. Response: {body}"
                )))
            }
        },
    )
}

/// `list_users`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListUsersInput {
    /// Maximum number of users to return (default 100, max 1000)
    limit: i64,
    /// Pagination cursor for next page of results
    cursor: String,
}

pub fn list_users(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_users",
        "Lists users in the Slack workspace.",
        move |input: ListUsersInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut values = Values::new();
                values.set("limit", clamp(input.limit, 1000, 100).to_string());
                if !input.cursor.is_empty() {
                    values.set("cursor", &input.cursor);
                }
                let body = client.call_form(&ct, "users.list", values.encode()).await?;
                Ok(text_result(body))
            }
        },
    )
}

/// `search_messages`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchMessagesInput {
    /// required,Search query string
    query: String,
    /// Number of results to return per page (default 20, max 100)
    count: i64,
    /// Page number of results to return (default 1)
    page: i64,
}

pub fn search_messages(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "search_messages",
        "Searches messages across the Slack workspace. \
         Note: requires OAuth authentication (user token). \
         This tool will return an error when used with a bot token.",
        move |input: SearchMessagesInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut values = Values::new();
                // Unconditional, unlike every cursor above: an empty search
                // still sends `query=`.
                values.set("query", &input.query);
                values.set("count", clamp(input.count, 100, 20).to_string());
                // `page` has **no ceiling** — only a floor — so it is not a
                // `clamp` call.
                let page = if input.page <= 0 { 1 } else { input.page };
                values.set("page", page.to_string());
                let body = client
                    .call_form(&ct, "search.messages", values.encode())
                    .await?;
                Ok(text_result(body))
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

    /// `url.Values.Encode()` sorts its keys and writes a space as `+`, so the
    /// form body is not the order the handler set them in.
    #[test]
    fn a_form_body_is_sorted_and_plus_encoded() {
        let mut values = Values::new();
        values.set("types", "public_channel,private_channel");
        values.set("limit", "100");
        values.set("cursor", "a b&c");
        assert_eq!(
            values.encode(),
            "cursor=a+b%26c&limit=100&types=public_channel%2Cprivate_channel"
        );
    }

    /// …and `json.Marshal` over a map sorts too, so `send_reply`'s body is
    /// channel, text, thread_ts — not the source's order.
    #[test]
    fn a_json_body_is_sorted_and_html_escaped() {
        assert_eq!(
            encoded(&json!({"channel": "C1", "thread_ts": "1.2", "text": "a <b> & c"})),
            r#"{"channel":"C1","text":"a \u003cb\u003e \u0026 c","thread_ts":"1.2"}"#
        );
    }

    /// `page` has a floor and no ceiling, unlike every other numeric input here.
    #[test]
    fn the_page_floor_has_no_ceiling() {
        for (input, want) in [(0_i64, 1_i64), (-3, 1), (1, 1), (5, 5), (100_000, 100_000)] {
            let page = if input <= 0 { 1 } else { input };
            assert_eq!(page, want, "{input}");
        }
    }
}

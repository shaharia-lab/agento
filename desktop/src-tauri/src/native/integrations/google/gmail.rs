//! The `gmail` service's three tools, ported from
//! `internal/integrations/google/gmail.go`.
//!
//! Three things here are unlike anything in the other five integrations:
//!
//! 1. **`search_email` makes N+1 requests.** It lists, then fetches each message
//!    for its headers, and a failed fetch is **skipped** (`continue`) rather than
//!    surfaced. The count in the result sentence is `len(list.Messages)` — the
//!    *listed* total — not the number of entries actually rendered, so a partial
//!    failure produces a sentence whose number does not match its body. Go's, and
//!    pinned.
//! 2. **Repeated query parameters.** `MetadataHeaders("Subject","From","Date")`
//!    encodes as three `metadataHeaders=` pairs in insertion order under one
//!    sorted key. That is why `gourl::Values` stopped being single-valued.
//! 3. **base64url with padding, both ways.** `base64.URLEncoding` is the padded
//!    alphabet; `RawURLEncoding` is not, and Gmail's own `body.data` is usually
//!    unpadded — which `DecodeString` then *rejects*, so `extractBody` falls
//!    through to the next part. That is Go's behaviour and it is reproduced,
//!    padding requirement included.
//!
//! # One deliberate divergence, and it is a panic
//!
//! `read_email` does `msg.Payload.Headers` with no nil check, and
//! `search_email` does the same on each fetched message. A Gmail response
//! carrying no `payload` therefore **panics** the Go handler. This port treats a
//! missing payload as no headers and an empty body. A panic is not a behaviour
//! worth reproducing, it cannot be recorded in a vector, and the resulting text
//! is what Go produces for the *adjacent* case of a payload with no matching
//! headers — so the divergence is narrow and is documented rather than hidden.

use schemars::JsonSchema;
use serde_json::json;

use crate::claude::{new_tool, CancellationToken, ToolDef};

use super::calendar::base_query;
use super::client::{Api, Client};
use super::text_result;

/// `send_email`.
#[allow(dead_code)] // read through serde, never constructed in Rust
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendEmailInput {
    /// required,Recipient email address(es), comma-separated
    to: String,
    /// required,Email subject line
    subject: String,
    /// required,Plain text body of the email
    body: String,
}

pub fn send_email(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "send_email",
        "Sends an email via Gmail.",
        move |input: SendEmailInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // The RFC822 message, with `\r\n` line endings and a blank line
                // before the body — built by the handler, not by a library.
                let raw = format!(
                    "To: {}\r\nSubject: {}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{}",
                    input.to, input.subject, input.body
                );
                // `base64.URLEncoding` — the **padded** URL-safe alphabet.
                let encoded = super::base64_url_encode(raw.as_bytes());

                let body = super::marshal(&json!({"raw": encoded}))?;
                let sent: Message = client
                    .post_json(
                        &ct,
                        Api::Gmail,
                        "gmail/v1/users/me/messages/send",
                        &base_query(),
                        body,
                    )
                    .await
                    .and_then(|raw| super::decode(&raw))
                    .map_err(|e| format!("sending email: {e}"))?;
                Ok(text_result(format!(
                    "Email sent successfully. Message ID: {}",
                    sent.id
                )))
            }
        },
    )
}

/// `read_email`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadEmailInput {
    /// required,The Gmail message ID to read
    message_id: String,
}

pub fn read_email(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "read_email",
        "Reads the full content of a Gmail message by its ID.",
        move |input: ReadEmailInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mut query = base_query();
                query.set("format", "full");
                // The message id is a **path** parameter, escaped per segment by
                // the generated client — measured: `a/b c?d` becomes
                // `a%2Fb%20c%3Fd`.
                let path = format!(
                    "gmail/v1/users/me/messages/{}",
                    super::client::expand_path_segment(&input.message_id)
                );

                let message: Message = client
                    .get(&ct, Api::Gmail, &path, &query)
                    .await
                    .and_then(|raw| super::decode(&raw))
                    // Go quotes the id with `%q` here.
                    .map_err(|e| format!("reading email {:?}: {e}", input.message_id))?;
                let headers = message.headers();
                Ok(text_result(format!(
                    "Subject: {}\nFrom: {}\nDate: {}\n\n{}",
                    headers.subject,
                    headers.from,
                    headers.date,
                    message
                        .payload
                        .as_ref()
                        .map_or_else(String::new, |payload| { extract_body(&payload.0) })
                )))
            }
        },
    )
}

/// `search_email`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchEmailInput {
    /// required,Gmail search query (e.g. 'from:alice@example.com is:unread')
    query: String,
    /// Maximum number of messages to return (default 10, max 50)
    max_results: i64,
}

pub fn search_email(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "search_email",
        "Searches Gmail messages using Gmail query syntax (e.g. 'from:alice@example.com is:unread').",
        move |input: SearchEmailInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // 50 here, not 100 — every clamp in this integration differs.
                let max_results = if input.max_results <= 0 || input.max_results > 50 {
                    10
                } else {
                    input.max_results
                };

                let mut query = base_query();
                query.set("maxResults", max_results.to_string());
                // Unconditional, unlike Drive's: an empty search still sends
                // `q=`.
                query.set("q", &input.query);

                let listed: MessageList = client
                    .get(&ct, Api::Gmail, "gmail/v1/users/me/messages", &query)
                    .await
                    .and_then(|raw| super::decode(&raw))
                    .map_err(|e| format!("searching emails: {e}"))?;
                if listed.messages.is_empty() {
                    return Ok(text_result(
                        "No messages found matching the query.".to_string(),
                    ));
                }

                let mut rendered = Vec::with_capacity(listed.messages.len());
                for message in listed.messages() {
                    let mut query = base_query();
                    query.set("format", "metadata");
                    // Three values under one key, in this order — the reason
                    // `gourl::Values` is a multimap.
                    query.add("metadataHeaders", "Subject");
                    query.add("metadataHeaders", "From");
                    query.add("metadataHeaders", "Date");
                    let path = format!(
                        "gmail/v1/users/me/messages/{}",
                        super::client::expand_path_segment(&message.id)
                    );

                    // A failed fetch is **skipped**, not surfaced — so a search
                    // can answer with fewer entries than its own count. A failed
                    // *decode* is skipped for the same reason: both are one
                    // `Do()` to Go.
                    let Ok(fetched) = client
                        .get(&ct, Api::Gmail, &path, &query)
                        .await
                        .and_then(|raw| super::decode::<Message>(&raw))
                    else {
                        continue;
                    };
                    let headers = fetched.headers();
                    rendered.push(format!(
                        "ID: {}\nSubject: {}\nFrom: {}\nDate: {}",
                        message.id, headers.subject, headers.from, headers.date
                    ));
                }

                // `len(list.Messages)`, the **listed** count — not
                // `len(results)`. A partial failure leaves the two disagreeing,
                // which is Go's and is pinned.
                Ok(text_result(format!(
                    "Found {} message(s):\n\n{}",
                    listed.messages.len(),
                    rendered.join("\n\n---\n\n")
                )))
            }
        },
    )
}

/// The three headers both readers pull out, in Go's `switch` order.
#[derive(Default)]
struct Headers {
    subject: String,
    from: String,
    date: String,
}

/// `gmail.Message`, reduced to the fields the handlers read.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct Message {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    id: String,
    payload: Option<crate::native::gojson::GoStruct<MessagePart>>,
}

impl Message {
    /// Go's `for _, h := range msg.Payload.Headers { switch h.Name … }`.
    ///
    /// A **later** header of the same name wins, because the switch assigns on
    /// every iteration. A missing payload is no headers here where Go panics;
    /// see the module docs.
    fn headers(&self) -> Headers {
        let mut out = Headers::default();
        let Some(payload) = self.payload.as_ref() else {
            return out;
        };
        for header in payload.0.headers() {
            match header.name.as_str() {
                "Subject" => out.subject = header.value.clone(),
                "From" => out.from = header.value.clone(),
                "Date" => out.date = header.value.clone(),
                _ => {}
            }
        }
        out
    }
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct MessagePart {
    #[serde(rename = "mimeType")]
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    mime_type: String,
    body: Option<crate::native::gojson::GoStruct<MessagePartBody>>,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    headers: Vec<crate::native::gojson::GoStruct<Header>>,
    /// `Parts []*MessagePart` is a **pointer** slice and `extractBody` has an
    /// explicit nil check for it (`gmail.go:117`) — the one place in this
    /// integration where Go handles a null element gracefully instead of
    /// panicking. So a `null` here is a skipped part, not a decode failure.
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    parts: Vec<Option<crate::native::gojson::GoStruct<MessagePart>>>,
}

impl MessagePart {
    fn headers(&self) -> impl Iterator<Item = &Header> {
        self.headers.iter().map(|wrapped| &wrapped.0)
    }
    fn parts(&self) -> impl Iterator<Item = &MessagePart> {
        self.parts.iter().flatten().map(|wrapped| &wrapped.0)
    }
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct MessagePartBody {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    data: String,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct Header {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    name: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    value: String,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct MessageList {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    messages: Vec<crate::native::gojson::GoStruct<Message>>,
}

impl MessageList {
    fn messages(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter().map(|wrapped| &wrapped.0)
    }
}

/// `extractBody`: the first `text/plain` part whose data decodes, depth-first.
///
/// Three details that are Go's and are easy to lose:
///
/// - The part must be `text/plain` **and** have non-empty data before a decode is
///   even attempted.
/// - A decode **failure** is not an error: it falls through to the children, so a
///   part whose base64 is unpadded is skipped rather than surfaced.
/// - The recursion takes the first child that yields a **non-empty** string, so a
///   `text/plain` part decoding to `""` does not stop the search.
fn extract_body(payload: &MessagePart) -> String {
    if payload.mime_type == "text/plain" {
        if let Some(body) = payload.body.as_ref().filter(|body| !body.0.data.is_empty()) {
            if let Some(decoded) = super::base64_url_decode(&body.0.data) {
                return String::from_utf8_lossy(&decoded).into_owned();
            }
        }
    }
    for part in payload.parts() {
        let text = extract_body(part);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(json: &str) -> Message {
        super::super::decode(json).expect("decodes")
    }

    /// The header scan: a later header of the same name wins, an unknown one is
    /// ignored, and a missing payload is empty rather than a panic.
    #[test]
    fn headers_are_read_the_way_gos_switch_reads_them() {
        let headers = message(
            r#"{"payload":{"headers":[
                {"name":"Subject","value":"first"},
                {"name":"X-Other","value":"ignored"},
                {"name":"From","value":"a@b"},
                {"name":"Subject","value":"second"}
            ]}}"#,
        )
        .headers();
        assert_eq!(headers.subject, "second", "a later header wins");
        assert_eq!(headers.from, "a@b");
        assert_eq!(headers.date, "");

        let empty = message(r#"{"id":"m1"}"#).headers();
        assert_eq!(empty.subject, "");
    }

    /// `extractBody`'s three rules.
    #[test]
    fn the_body_is_the_first_decodable_text_plain_part() {
        let body = |json: &str| {
            let msg = message(json);
            msg.payload
                .as_ref()
                .map_or_else(String::new, |p| extract_body(&p.0))
        };

        // `aGk=` is "hi" in padded base64url.
        assert_eq!(
            body(r#"{"payload":{"mimeType":"text/plain","body":{"data":"aGk="}}}"#),
            "hi"
        );
        // A non-text/plain parent recurses into its parts.
        assert_eq!(
            body(
                r#"{"payload":{"mimeType":"multipart/alternative","parts":[
                    {"mimeType":"text/html","body":{"data":"PGI-"}},
                    {"mimeType":"text/plain","body":{"data":"aGk="}}
                ]}}"#
            ),
            "hi",
            "only text/plain is decoded"
        );
        // **Unpadded** data fails `URLEncoding` and falls through to the next
        // part rather than erroring — Go's behaviour.
        assert_eq!(
            body(
                r#"{"payload":{"mimeType":"multipart/mixed","parts":[
                    {"mimeType":"text/plain","body":{"data":"aGk"}},
                    {"mimeType":"text/plain","body":{"data":"Ynll"}}
                ]}}"#
            ),
            "bye",
            "an unpadded part is skipped, not reported"
        );
        // No body at all.
        assert_eq!(body(r#"{"payload":{"mimeType":"text/plain"}}"#), "");
    }
}

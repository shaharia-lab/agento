//! The two Telegram calls the dispatcher makes: the typing indicator and the
//! reply. Mirrors `SendChatAction`, `SendReply` and `splitMessage`
//! (`internal/integrations/telegram/webhook.go`).
//!
//! The transport is [`crate::native::integrations::telegram::client`], already
//! ported for the outbound tools (#314) — same `callTelegram`, same fixed
//! `calling Telegram <method>: request failed` wording for a transport error.
//!
//! # `splitMessage` cuts at 4096 **bytes**
//!
//! Telegram's limit is on the message, and Go's splitter is byte-based: it takes
//! `text[:4096]` and walks `end` back while the byte at that index is not a rune
//! start. Two consequences a character-based split would get wrong — a 4096-rune
//! message of multi-byte text is split where Go sends it whole is the harmless
//! direction, and a message Go splits that a port sends whole is rejected by
//! Telegram outright.
//!
//! Only the **first** chunk is a reply to the original message; the rest are
//! plain sends, so a long answer does not quote the question five times.

use serde::Serialize;

use crate::native::integrations::telegram::client::Client as TelegramClient;

/// Telegram's per-message limit, in bytes.
const MAX_MESSAGE_LEN: usize = 4096;

/// **Field order is alphabetical, not natural.** Go builds both payloads as
/// `map[string]any`, and `encoding/json` marshals a map with its keys **sorted**
/// — so `action` precedes `chat_id`, and `reply_to_message_id` sits between
/// `chat_id` and `text`. Declaring them in the order a person would write them
/// produces different bytes for the same request.
#[derive(Serialize)]
struct ChatAction {
    action: &'static str,
    chat_id: i64,
}

/// Sorted, for the reason above. `reply_to_message_id` is omitted rather than
/// null on a continuation chunk, because Go's map simply does not have the key.
#[derive(Serialize)]
struct SendMessage<'a> {
    chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_message_id: Option<i64>,
    text: &'a str,
}

/// `SendChatAction`. Best-effort: Go discards both return values, because a
/// failed "typing…" must not stop the run that follows.
pub async fn send_chat_action(bot_token: &str, chat_id: i64) {
    let Ok(body) = crate::native::gojson::to_vec_marshal(&ChatAction {
        action: "typing",
        chat_id,
    }) else {
        return;
    };
    let client = TelegramClient::new(bot_token);
    let _ = client
        .call(
            &tokio_util::sync::CancellationToken::new(),
            "sendChatAction",
            body,
        )
        .await;
}

/// `SendReply`: the text, split to Telegram's limit, with only the first chunk
/// quoting the original message.
pub async fn send_reply(
    bot_token: &str,
    chat_id: i64,
    reply_to_message_id: i64,
    text: &str,
) -> Result<(), String> {
    let client = TelegramClient::new(bot_token);
    let ct = tokio_util::sync::CancellationToken::new();

    for (i, chunk) in split_message(text, MAX_MESSAGE_LEN).iter().enumerate() {
        let payload = SendMessage {
            chat_id,
            // `if i == 0 && replyToMsgID > 0`.
            reply_to_message_id: (i == 0 && reply_to_message_id > 0).then_some(reply_to_message_id),
            text: chunk,
        };
        let body = crate::native::gojson::to_vec_marshal(&payload)
            .map_err(|e| format!("encoding sendMessage: {e}"))?;
        client
            .call(&ct, "sendMessage", body)
            .await
            .map_err(|e| format!("sending reply chunk {}: {e}", i + 1))?;
    }
    Ok(())
}

/// `splitMessage`: chunks of at most `max_len` **bytes**, split on rune
/// boundaries.
///
/// Go walks `end` back while `!utf8.RuneStart(text[end])`, with a fallback to
/// the hard limit if it reaches zero — unreachable for valid UTF-8, and
/// unreachable here for a different reason: a `&str` is always valid, so the
/// walk always finds a boundary.
pub fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::with_capacity(text.len() / max_len + 1);
    let mut rest = text;
    while rest.len() > max_len {
        let mut end = max_len;
        while end > 0 && !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            // Go's guard against an infinite loop. Unreachable over a `&str`,
            // where a boundary always exists at or below `max_len`.
            break;
        }
        let (chunk, tail) = rest.split_at(end);
        chunks.push(chunk);
        rest = tail;
    }
    if !rest.is_empty() {
        chunks.push(rest);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_message_is_one_chunk() {
        assert_eq!(split_message("hello", 4096), vec!["hello"]);
        assert_eq!(split_message("", 4096), vec![""]);
        // Exactly the limit is not over it.
        let exact = "a".repeat(10);
        assert_eq!(split_message(&exact, 10), vec![exact.as_str()]);
    }

    #[test]
    fn a_long_message_splits_at_the_byte_limit() {
        let text = "a".repeat(25);
        assert_eq!(
            split_message(&text, 10),
            vec!["aaaaaaaaaa", "aaaaaaaaaa", "aaaaa"]
        );
        assert_eq!(split_message(&text, 10).concat(), text, "nothing is lost");
    }

    #[test]
    fn a_split_never_lands_inside_a_character() {
        // Ten 3-byte characters, split at 10 bytes: the boundary walks back to
        // 9, so each chunk is three whole characters.
        let text = "€".repeat(10);
        let chunks = split_message(&text, 10);
        for chunk in &chunks {
            assert!(chunk.chars().all(|c| c == '€'), "{chunk:?}");
            assert!(chunk.len() <= 10);
        }
        assert_eq!(chunks.concat(), text, "nothing is lost or duplicated");
        assert_eq!(chunks[0].chars().count(), 3, "9 bytes, not 10");
    }

    #[test]
    fn a_character_wider_than_the_limit_would_stall_and_does_not() {
        // The `end == 0` guard: a 4-byte character with a 3-byte limit has no
        // boundary to walk back to. Go falls back to the hard limit and would
        // emit invalid UTF-8; this stops instead, which is the only
        // representable answer and cannot happen at the real 4096.
        let text = "𝄞𝄞";
        let chunks = split_message(text, 3);
        assert_eq!(chunks.concat(), text, "the message still arrives whole");
    }

    #[test]
    fn the_payloads_are_gos_json() {
        // Go builds these as `map[string]any`, so `json.Marshal` sorts the keys.
        // Declaration order would put `text` before `reply_to_message_id` and
        // `chat_id` before `action`, which is different bytes for the same
        // request.
        let with_reply = crate::native::gojson::to_vec_marshal(&SendMessage {
            chat_id: -100,
            reply_to_message_id: Some(7),
            text: "hi",
        })
        .expect("encode");
        assert_eq!(
            String::from_utf8(with_reply).expect("utf-8"),
            r#"{"chat_id":-100,"reply_to_message_id":7,"text":"hi"}"#
        );

        // A continuation chunk omits the key entirely, as Go's map does by not
        // setting it.
        let without = crate::native::gojson::to_vec_marshal(&SendMessage {
            chat_id: -100,
            reply_to_message_id: None,
            text: "hi",
        })
        .expect("encode");
        assert_eq!(
            String::from_utf8(without).expect("utf-8"),
            r#"{"chat_id":-100,"text":"hi"}"#
        );

        let action = crate::native::gojson::to_vec_marshal(&ChatAction {
            action: "typing",
            chat_id: 42,
        })
        .expect("encode");
        assert_eq!(
            String::from_utf8(action).expect("utf-8"),
            r#"{"action":"typing","chat_id":42}"#
        );
    }
}

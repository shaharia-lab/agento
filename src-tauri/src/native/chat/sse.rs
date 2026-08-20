//! The SSE frames, byte for byte.
//!
//! Go has **two** writers and the difference is load-bearing:
//!
//! - `sendSSERaw` writes the CLI's JSON with three bare `w.Write` calls, so the
//!   bytes are never decoded and re-encoded. A `tool_use` input of
//!   `{"z":1.50,"a":1}` ships exactly that; going through a `serde_json::Value`
//!   would ship `{"a":1,"z":1.5}` — reordered and respelled, with nothing to
//!   signal it.
//! - `sendSSEEvent` marshals a value and formats
//!   `"event: %s\ndata: %s\n\n"`. Only the two synthetic events use it.
//!
//! Frames are **LF-only**, one blank line, no `id:`, no `retry:`, and there is
//! **no heartbeat** — a quiet turn sends nothing at all.

use serde::Serialize;

/// One frame carrying a payload that is already JSON.
///
/// The event name is the CLI's own `type` field, so unknown and future types
/// pass through unchanged; the frontend ignores names it does not know.
pub fn raw_frame(event: &str, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(event.len() + data.len() + 16);
    out.extend_from_slice(b"event: ");
    out.extend_from_slice(event.as_bytes());
    out.extend_from_slice(b"\ndata: ");
    out.extend_from_slice(data);
    out.extend_from_slice(b"\n\n");
    out
}

/// One frame for a value this side constructs — the two synthetic events and
/// the "streaming not supported" error.
///
/// Encoded through `gojson` rather than plain `serde_json` so the escaping
/// matches: Go's encoder HTML-escapes `<`, `>` and `&`, and an
/// `AskUserQuestion` payload routinely contains them.
pub fn json_frame<T: Serialize>(event: &str, data: &T) -> Result<Vec<u8>, String> {
    let encoded = crate::native::gojson::to_vec_marshal(data)
        .map_err(|e| format!("encoding {event} payload: {e}"))?;
    Ok(raw_frame(event, &encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes `internal/api/chats_test.go` asserts.
    #[test]
    fn a_raw_frame_is_gos_bytes() {
        assert_eq!(
            raw_frame("assistant", br#"{"message":"hello"}"#),
            b"event: assistant\ndata: {\"message\":\"hello\"}\n\n".to_vec()
        );
    }

    /// LF only. A CRLF frame parses in most clients and is still a divergence,
    /// and it is exactly what a naive `writeln!` would produce on some targets.
    #[test]
    fn frames_use_lf_and_end_with_one_blank_line() {
        let frame = raw_frame("result", b"{}");
        let text = String::from_utf8(frame).unwrap();
        assert!(!text.contains('\r'), "no CR: {text:?}");
        assert!(text.ends_with("\n\n"));
        assert_eq!(text.matches("\n\n").count(), 1);
        // No id:/retry: fields.
        assert!(!text.contains("id:"));
        assert!(!text.contains("retry:"));
    }

    /// The whole reason `raw_frame` takes bytes: a round trip through a JSON
    /// value would sort these keys and respell the number.
    #[test]
    fn a_raw_payload_keeps_its_key_order_and_number_spelling() {
        let payload = br#"{"z":1.50,"a":[1,2]}"#;
        let frame = raw_frame("assistant", payload);
        let text = String::from_utf8(frame).unwrap();
        assert!(text.contains(r#"{"z":1.50,"a":[1,2]}"#), "{text}");
    }

    /// Go's encoder HTML-escapes, and an AskUserQuestion payload routinely
    /// carries `<` and `&`.
    #[test]
    fn a_json_frame_escapes_the_way_gos_encoder_does() {
        #[derive(serde::Serialize)]
        struct Payload<'a> {
            error: &'a str,
        }
        let frame = json_frame("error", &Payload { error: "a < b & c" }).expect("encode");
        let text = String::from_utf8(frame).unwrap();
        // Escaped, not literal: `encoding/json` turns `<`, `>` and `&` into
        // their \u form, and an AskUserQuestion payload routinely carries them.
        assert!(text.contains(r"a \u003c b \u0026 c"), "{text}");
        assert!(!text.contains("a < b"), "{text}");
        assert!(text.ends_with("\n\n"));
    }

    /// `to_vec_marshal` rather than `to_vec`: the latter appends the trailing
    /// newline `json.Encoder` adds, which inside a `data:` line would end the
    /// frame early and split one event into two.
    #[test]
    fn a_json_frame_has_no_stray_newline_in_its_data_line() {
        #[derive(serde::Serialize)]
        struct Payload {
            ok: bool,
        }
        let frame = json_frame("x", &Payload { ok: true }).expect("encode");
        assert_eq!(
            String::from_utf8(frame).unwrap(),
            "event: x\ndata: {\"ok\":true}\n\n"
        );
    }
}

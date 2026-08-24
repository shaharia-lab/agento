//! The two streaming surfaces' framing, and the `anthropic-beta` merge (#424).
//!
//! # Why the frames are bytes rather than `axum::response::sse::Event`
//!
//! The issue's plan was `impl From<SseFrame> for axum::response::sse::Event`.
//! That cannot be written here: both types are foreign to this crate, so the
//! orphan rule refuses it, and a newtype around one of them would exist purely
//! to satisfy that rule.
//!
//! Emitting bytes is the better answer anyway, because the acceptance criteria
//! are *byte* properties — "exactly one `data: [DONE]\n\n`", and the Anthropic
//! event order — and `Sse`'s serializer is a layer between the assertion and
//! what a client reads. It also settles the one thing `Event` decides for us:
//! `axum` writes `event:name\n` with no space, while every fixture in
//! `ferrox/docs/user/api-reference.md`, and Anthropic's own documentation,
//! shows `event: name`. The SSE grammar strips a single leading space either
//! way, so both parse; this emits the documented spelling.
//!
//! # What a mid-stream failure does, on both surfaces
//!
//! Nothing fallible may run after the response head commits, and by the first
//! frame it has. So an upstream failure *during* a stream cannot become a
//! status code — it becomes a final frame in the surface's own dialect and the
//! stream ends. Deliberately **without** a trailing `[DONE]` on the OpenAI
//! surface: `[DONE]` means the completion finished, and a client that saw one
//! after an error would record a truncated answer as a whole one. ferrox drops
//! the connection instead, which tells the client even less.

use std::convert::Infallible;

use axum::body::Bytes;
use ferrox_providers::error::{anthropic_error_body, openai_error_body, ProxyError};
use ferrox_providers::providers::ProviderStream;
use ferrox_providers::sse::SseFrame;
use ferrox_providers::types::ChatCompletionChunk;
use serde_json::Value;
use tokio_stream::{Stream, StreamExt};

use super::usage;

/// `data: {payload}\n\n` — one unnamed SSE frame, the OpenAI surface's only shape.
pub fn data_frame(payload: &str) -> Bytes {
    Bytes::from(format!("data: {payload}\n\n"))
}

/// `event: {name}\ndata: {payload}\n\n` — the Anthropic surface's shape.
pub fn named_frame(frame: &SseFrame) -> Bytes {
    Bytes::from(format!("event: {}\ndata: {}\n\n", frame.event, frame.data))
}

/// The terminator the OpenAI surface owes every completed stream, exactly once.
pub const DONE: &str = "[DONE]";

/// The item type both surfaces produce.
///
/// `Infallible` on the error side is a statement rather than a convenience: by
/// the first frame the response head is committed, so there is no error left to
/// hand axum that a client could act on. Anything that goes wrong from here is
/// a frame, not a status.
pub type FrameStream = tokio_stream::wrappers::ReceiverStream<Result<Bytes, Infallible>>;

/// An OpenAI chunk stream as SSE bytes.
///
/// Each chunk is serialized and framed; a clean end appends `data: [DONE]`. A
/// mid-stream error emits the OpenAI error body as its own frame and stops,
/// with no `[DONE]` — see the module header for why that asymmetry is
/// deliberate.
///
/// `accounting` (#425) decides the **status** here and nothing else — the token
/// counts arrive through `usage::meter`, wrapped around the provider stream one
/// layer out, which is the only way the Anthropic surface can get them and so
/// is the one way both surfaces do.
///
/// It is finished on **every** exit — all three protocol endings plus the
/// failed-send path — with the status that ending means. Each is a separate
/// `return` in the original shape, which is exactly how an arm gets missed, so
/// they are funnelled through one `status` variable and a single `finish` at
/// the bottom.
pub fn openai_sse(stream: ProviderStream, accounting: usage::Accounting) -> FrameStream {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        let mut stream = stream;
        // `Interrupted` until something says otherwise: the arm most easily
        // missed is the one where the client vanished, so it is the default
        // rather than the special case.
        let mut status = usage::Status::Interrupted;
        let mut terminate = false;
        loop {
            let item = match next_or_disconnect(&tx, &mut stream).await {
                Next::Item(item) => item,
                // The upstream said everything it had — this is the only path
                // that earns a terminator.
                Next::Ended => {
                    status = usage::Status::Ok;
                    terminate = true;
                    break;
                }
                // The client left. Breaking drops `stream`, which cancels the
                // upstream request; sending `[DONE]` into a channel nobody
                // holds would be pointless, and *reaching* the terminator on
                // this path is precisely the bug the two variants prevent.
                Next::Disconnected => break,
            };
            match item {
                Ok(chunk) => {
                    if tx
                        .send(Ok(data_frame(&serialize_chunk(&chunk))))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    log::warn!("gateway openai stream failed mid-flight: {e}");
                    let (_, body) = openai_error_body(&e);
                    let _ = tx.send(Ok(data_frame(&body.to_string()))).await;
                    status = usage::Status::UpstreamError;
                    break;
                }
            }
        }
        if terminate {
            let _ = tx.send(Ok(data_frame(DONE))).await;
        }
        accounting.finish(status);
    });
    tokio_stream::wrappers::ReceiverStream::new(rx)
}

/// Why a frame loop stopped waiting.
///
/// [`Ended`](Next::Ended) and [`Disconnected`](Next::Disconnected) are both
/// "there is no next item", and collapsing them into one `None` is a real bug
/// rather than a tidiness question: on the OpenAI surface the first owes the
/// client a `data: [DONE]` and the second must not send one, because `[DONE]`
/// means *the completion finished* and a client that reconnected and saw it
/// would record an abandoned answer as a whole one.
enum Next<T> {
    Item(T),
    Ended,
    Disconnected,
}

/// The next item, or which of the two ways there will not be one.
///
/// # Why this is not `stream.next().await`
///
/// A failed `send` catches a client that left while a frame was being written,
/// and that is the case a first implementation covers: it is what happens when
/// tokens are flowing. It is *not* what happens when the client leaves while
/// the upstream is thinking — and models spend most of a request thinking. In
/// that state the loop is parked on `next()`, nothing is being sent, and the
/// disconnect is invisible to every code path that would otherwise notice; the
/// task parks forever holding the upstream connection open, and one such leak
/// happens per abandoned request.
///
/// This is `native/chat/turn.rs`'s rule one level down, and the shape is the
/// same: **race every unbounded wait against the client's departure.** There
/// the disconnect signal is the body channel's closure and so it is here —
/// `Sender::closed` resolves when the receiver `Body::from_stream` holds is
/// dropped, which is exactly what a disconnect does. Returning drops the
/// stream, and dropping the stream is what cancels the upstream request.
///
/// `biased` so the disconnect is checked first: with both ready, finishing the
/// frame would be harmless but the extra chunk is pure waste, and a biased
/// select makes the test's timing deterministic rather than lucky.
async fn next_or_disconnect<S>(
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, Infallible>>,
    stream: &mut S,
) -> Next<S::Item>
where
    S: Stream + Unpin,
{
    tokio::select! {
        biased;
        () = tx.closed() => Next::Disconnected,
        item = stream.next() => match item {
            Some(item) => Next::Item(item),
            None => Next::Ended,
        },
    }
}

/// An Anthropic frame stream as SSE bytes.
///
/// The frames come from `ferrox_providers::anthropic_types`, which owns the
/// `message_start → content_block_start → content_block_delta* →
/// content_block_stop → message_delta → message_stop` state machine; this only
/// writes them. A mid-stream error becomes an `event: error` frame carrying the
/// Anthropic error body, which is the shape Anthropic's own API uses — there is
/// no `[DONE]` analogue here, so the sequence simply ends.
///
/// The token counts (#425) do **not** come from this loop: the emitter consumes
/// the provider stream to produce these frames, so per-chunk `usage` is gone by
/// the time a frame is visible. `usage::meter` wraps the provider stream one
/// layer down, before the translation, and this loop only decides the status
/// and fires `finish`.
pub fn anthropic_sse<S>(frames: S, accounting: usage::Accounting) -> FrameStream
where
    S: Stream<Item = Result<SseFrame, ProxyError>> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        let mut frames = Box::pin(frames);
        // As on the OpenAI surface: `Interrupted` unless something says
        // otherwise, so the arm most easily missed is the default.
        let mut status = usage::Status::Interrupted;
        // Raced against the client's departure for the reason
        // `next_or_disconnect` documents — and it matters more here, not less:
        // the Anthropic emitter buffers upstream chunks into protocol frames,
        // so this loop can be parked with nothing to send even while the
        // upstream is talking.
        //
        // Both terminal arms do the same thing here, unlike the OpenAI surface
        // above: there is no `[DONE]` analogue in the Anthropic protocol —
        // `message_stop` is a frame the emitter produces — so an ended stream
        // and a departed client both simply stop.
        loop {
            let item = match next_or_disconnect(&tx, &mut frames).await {
                Next::Item(item) => item,
                Next::Ended => {
                    status = usage::Status::Ok;
                    break;
                }
                Next::Disconnected => break,
            };
            let bytes = match item {
                Ok(frame) => named_frame(&frame),
                Err(e) => {
                    log::warn!("gateway anthropic stream failed mid-flight: {e}");
                    let (_, body) = anthropic_error_body(&e);
                    let _ = tx
                        .send(Ok(named_frame(&SseFrame::new("error", body.to_string()))))
                        .await;
                    status = usage::Status::UpstreamError;
                    break;
                }
            };
            if tx.send(Ok(bytes)).await.is_err() {
                // Client gone; breaking drops `frames`, which cancels upstream.
                break;
            }
        }
        accounting.finish(status);
    });
    tokio_stream::wrappers::ReceiverStream::new(rx)
}

/// A chunk as the JSON a `data:` line carries.
///
/// A serialization failure here is not reachable — every field of
/// `ChatCompletionChunk` is `Serialize` over owned data — but it is not worth a
/// panic on the streaming path either, so it degrades to an empty object rather
/// than to `unwrap_or_default`'s empty *string*, which is not a JSON document
/// and would make a client's parser fail rather than skip.
fn serialize_chunk(chunk: &ChatCompletionChunk) -> String {
    serde_json::to_string(chunk).unwrap_or_else(|e| {
        log::warn!("gateway could not serialize a chunk: {e}");
        "{}".to_string()
    })
}

/// The `anthropic-beta` value to forward upstream, or `None` to send no header.
///
/// Copied from `ferrox/src/handlers/anthropic_messages.rs`. The Anthropic SDK
/// sends beta flags either as the `anthropic-beta` header or as a `betas` array
/// in the body, and Claude Code has used both — so the two are merged into one
/// comma-separated header value, header first. Losing this silently disables
/// whatever beta the client asked for, with a working request as the symptom.
pub fn merge_betas(header: Option<&str>, raw: &Value) -> Option<String> {
    let mut betas: Vec<String> = Vec::new();
    if let Some(v) = header {
        betas.push(v.to_string());
    }
    if let Some(arr) = raw.get("betas").and_then(Value::as_array) {
        for b in arr {
            if let Some(s) = b.as_str() {
                betas.push(s.to_string());
            }
        }
    }
    if betas.is_empty() {
        None
    } else {
        Some(betas.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_openai_frame_is_data_space_payload_and_two_newlines() {
        assert_eq!(&data_frame("{\"a\":1}")[..], b"data: {\"a\":1}\n\n");
        assert_eq!(&data_frame(DONE)[..], b"data: [DONE]\n\n");
    }

    /// The `event:` line carries a space, which `axum::response::sse::Event`
    /// does not write — see the module header. Anthropic's own documented
    /// stream and every fixture in ferrox's API reference use this spelling.
    #[test]
    fn an_anthropic_frame_names_its_event_and_matches_the_documented_spelling() {
        let frame = SseFrame::new("message_stop", r#"{"type":"message_stop"}"#);
        assert_eq!(
            &named_frame(&frame)[..],
            b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
    }

    #[test]
    fn the_betas_merge_takes_the_header_then_the_body_in_order() {
        assert_eq!(
            merge_betas(Some("oauth-2025-04-20"), &json!({})),
            Some("oauth-2025-04-20".to_string())
        );
        assert_eq!(
            merge_betas(None, &json!({"betas": ["a", "b"]})),
            Some("a,b".to_string())
        );
        assert_eq!(
            merge_betas(Some("h"), &json!({"betas": ["a", "b"]})),
            Some("h,a,b".to_string()),
            "the header comes first, and neither half replaces the other"
        );
    }

    /// The absent case is `None`, not `Some("")` — an empty `anthropic-beta`
    /// header is not the same request as no header, and some upstreams reject it.
    #[test]
    fn no_betas_anywhere_sends_no_header() {
        assert_eq!(merge_betas(None, &json!({})), None);
        assert_eq!(merge_betas(None, &json!({"betas": []})), None);
        assert_eq!(
            merge_betas(None, &json!({"betas": "not-an-array"})),
            None,
            "a `betas` that is not an array is ignored rather than stringified"
        );
        assert_eq!(
            merge_betas(None, &json!({"betas": [1, 2]})),
            None,
            "non-string members are skipped, as ferrox's `as_str` filter does"
        );
    }
}

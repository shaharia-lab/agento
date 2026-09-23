//! A scheduled task's output, sent to a Telegram chat (#639, epic #626).
//!
//! The `telegram` arm of `schedule::delivery` calls [`deliver_chat`] once per
//! configured chat, with the token `trigger::receiver::telegram_delivery_token`
//! resolved. Rules, each enforced here:
//!
//! - **The dispatcher's send path, unchanged.** Every message goes through
//!   `trigger::telegram_api::send_reply(token, chat, 0, text)` — the reply the
//!   webhook dispatcher sends, with no message to quote — so the split at 4096
//!   **bytes** and the payload bytes are that module's, not a second copy.
//! - **Header, then output.** One message naming the task, its status and its
//!   duration; then the output, in as many chunks as `send_reply` cuts it into.
//!   A failed header attempts no output. Plain text: no `parse_mode`, as the
//!   dispatcher's replies have none.
//! - **Errors are sentences, and name the chat.** The client reports `ok:
//!   false` as `telegram API error: <description>`; [`readable`] maps the three
//!   descriptions a misconfigured destination produces and passes anything else
//!   through. The HTTP status is never consulted — the envelope decides.
//! - **No token in any result.** The token is in the request *path*, so a
//!   transport error must never carry the URL; the client's failure sentence
//!   names only the method, and results are built from that and Telegram's
//!   description alone.

use crate::native::integrations::slack::delivery::format_duration;
use crate::native::trigger::dispatcher::NO_RESPONSE_REPLY;
use crate::native::trigger::telegram_api::send_reply;

/// What one run says to Telegram. `body` is the answer on a successful run, the
/// failure message on a failed one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub task_name: String,
    /// `true` for a successful run.
    pub succeeded: bool,
    pub duration_ms: i64,
    pub body: String,
}

/// The header, then the output, to one chat. `Err` is the sentence the
/// delivery row records.
pub async fn deliver_chat(token: &str, chat_id: i64, run: &RunSummary) -> Result<(), String> {
    send_reply(token, chat_id, 0, &header(run))
        .await
        .map_err(|e| readable(&e, chat_id))?;
    send_reply(token, chat_id, 0, &body(run))
        .await
        .map_err(|e| {
            format!(
                "sent the header; the output failed: {}",
                readable(&e, chat_id)
            )
        })
}

/// `Daily brief — Completed in 3m 12s`.
fn header(run: &RunSummary) -> String {
    let name = if run.task_name.trim().is_empty() {
        "Scheduled task"
    } else {
        run.task_name.as_str()
    };
    let status = if run.succeeded { "Completed" } else { "Failed" };
    format!("{name} — {status} in {}", format_duration(run.duration_ms))
}

/// The answer, the failure, or — so no delivery is a bare header with nothing
/// after it — the inbound path's no-response sentence.
fn body(run: &RunSummary) -> String {
    if !run.body.trim().is_empty() {
        run.body.clone()
    } else if run.succeeded {
        NO_RESPONSE_REPLY.to_string()
    } else {
        "The run failed.".to_string()
    }
}

/// A send error as a sentence the Jobs view can show.
///
/// `send_reply` wraps the client's error as `sending reply chunk N: <e>`, and
/// the client reports `ok: false` as `telegram API error: <description>`. The
/// three descriptions a misconfigured destination produces become advice
/// naming the chat, with Telegram's own words kept after it; anything else —
/// a rate limit, a failed request — passes through as written.
fn readable(e: &str, chat_id: i64) -> String {
    let Some(description) = e
        .split_once("telegram API error: ")
        .map(|(_, description)| description)
    else {
        return e.to_string();
    };
    let sentence = if description.starts_with("Bad Request: chat not found") {
        format!(
            "chat {chat_id} not found — the bot can only message a chat after someone there has started it or added it"
        )
    } else if description.starts_with("Forbidden") {
        format!("the bot was blocked or is not a member of chat {chat_id}")
    } else if description.starts_with("Unauthorized") {
        "the bot token was rejected — re-validate the integration".to_string()
    } else {
        return e.to_string();
    };
    format!("{sentence} ({description})")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::http::{StatusCode, Uri};

    use super::super::client::{api_base_lock, set_api_base};
    use super::*;

    const TOKEN: &str = "123456:AAF-DELIVERY-SUPER-SECRET";

    /// One `sendMessage` the fake saw: `(path, chat_id, text)`.
    type Sent = (String, i64, String);

    /// The fake's `(status, body)` for the `n`th call.
    type Reply = fn(usize) -> (u16, &'static str);

    /// Serves every request with `(status, body)` from `reply(n)` for the `n`th
    /// call, recording what was sent.
    async fn fake(reply: Reply) -> Arc<Mutex<Vec<Sent>>> {
        let sent: Arc<Mutex<Vec<Sent>>> = Arc::default();
        let log = sent.clone();
        let app = axum::Router::new().fallback(move |uri: Uri, body: String| {
            let log = log.clone();
            async move {
                let payload: serde_json::Value = serde_json::from_str(&body).expect("JSON");
                let n = {
                    let mut log = log.lock().expect("lock");
                    log.push((
                        uri.path().to_string(),
                        payload["chat_id"].as_i64().expect("a numeric chat_id"),
                        payload["text"].as_str().unwrap_or_default().to_string(),
                    ));
                    log.len() - 1
                };
                let (status, body) = reply(n);
                (StatusCode::from_u16(status).expect("status"), body)
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake telegram");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));
        sent
    }

    fn ok(_: usize) -> (u16, &'static str) {
        (200, r#"{"ok":true,"result":{}}"#)
    }

    fn run(body: &str) -> RunSummary {
        RunSummary {
            task_name: "Daily brief".into(),
            succeeded: true,
            duration_ms: 192_000,
            body: body.into(),
        }
    }

    #[tokio::test]
    async fn the_header_then_the_output_split_at_4096_bytes() {
        let _guard = api_base_lock().await;
        let sent = fake(ok).await;
        // Multi-byte text, so a character split would differ from a byte one.
        let long = "é".repeat(5000);
        let result = deliver_chat(TOKEN, -1_001_234_567_890, &run(&long)).await;
        set_api_base(None);

        assert_eq!(result, Ok(()));
        let sent = sent.lock().expect("lock").clone();
        assert_eq!(sent.len(), 4, "the header and three chunks: {}", sent.len());
        assert_eq!(sent[0].2, "Daily brief — Completed in 3m 12s");
        for (path, chat, text) in &sent {
            assert_eq!(path, &format!("/bot{TOKEN}/sendMessage"));
            assert_eq!(*chat, -1_001_234_567_890);
            assert!(text.len() <= 4096, "{}", text.len());
        }
        let output: String = sent[1..].iter().map(|s| s.2.as_str()).collect();
        assert_eq!(output, long, "nothing is lost or reordered");
        assert!(!sent[1..].iter().any(|s| s.2.contains("Daily brief")));
    }

    #[tokio::test]
    async fn a_failed_run_sends_its_header_and_error() {
        let _guard = api_base_lock().await;
        let sent = fake(ok).await;
        let failed = RunSummary {
            succeeded: false,
            duration_ms: 850,
            body: "the CLI exited 1".into(),
            ..run("")
        };
        assert_eq!(deliver_chat(TOKEN, 42, &failed).await, Ok(()));
        let empty = RunSummary {
            body: String::new(),
            ..failed.clone()
        };
        assert_eq!(deliver_chat(TOKEN, 42, &empty).await, Ok(()));
        set_api_base(None);

        let texts: Vec<String> = sent
            .lock()
            .expect("lock")
            .iter()
            .map(|s| s.2.clone())
            .collect();
        assert_eq!(
            texts,
            vec![
                "Daily brief — Failed in 850ms",
                "the CLI exited 1",
                "Daily brief — Failed in 850ms",
                "The run failed.",
            ]
        );
    }

    /// The envelope decides, whatever the status: a 500 carrying `ok: true` is
    /// a success.
    #[tokio::test]
    async fn a_500_carrying_ok_true_is_a_success() {
        let _guard = api_base_lock().await;
        let _sent = fake(|_| (500, r#"{"ok":true}"#)).await;
        let result = deliver_chat(TOKEN, 42, &run("hi")).await;
        set_api_base(None);
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn each_known_refusal_is_a_sentence_naming_the_chat() {
        let _guard = api_base_lock().await;
        let cases: [(Reply, &str); 4] = [
            (
                |_| (400, r#"{"ok":false,"description":"Bad Request: chat not found"}"#),
                "chat 42 not found — the bot can only message a chat after someone there has started it or added it (Bad Request: chat not found)",
            ),
            (
                |_| (403, r#"{"ok":false,"description":"Forbidden: bot was blocked by the user"}"#),
                "the bot was blocked or is not a member of chat 42 (Forbidden: bot was blocked by the user)",
            ),
            (
                |_| (401, r#"{"ok":false,"description":"Unauthorized"}"#),
                "the bot token was rejected — re-validate the integration (Unauthorized)",
            ),
            (
                |_| (429, r#"{"ok":false,"description":"Too Many Requests: retry after 5"}"#),
                "sending reply chunk 1: telegram API error: Too Many Requests: retry after 5",
            ),
        ];
        for (reply, want) in cases {
            let sent = fake(reply).await;
            let got = deliver_chat(TOKEN, 42, &run("hi")).await;
            assert_eq!(got.as_ref().map_err(String::as_str), Err(want));
            assert!(!want.contains(TOKEN));
            assert_eq!(
                sent.lock().expect("lock").len(),
                1,
                "no output after a failed header"
            );
        }
        set_api_base(None);
    }

    #[tokio::test]
    async fn a_failure_after_the_header_says_so() {
        let _guard = api_base_lock().await;
        let _sent = fake(|n| {
            if n == 0 {
                (200, r#"{"ok":true}"#)
            } else {
                (
                    403,
                    r#"{"ok":false,"description":"Forbidden: bot was kicked from the group chat"}"#,
                )
            }
        })
        .await;
        let got = deliver_chat(TOKEN, -100, &run("hi")).await;
        set_api_base(None);
        assert_eq!(
            got.as_ref().map_err(String::as_str),
            Err("sent the header; the output failed: the bot was blocked or is not a member of chat -100 (Forbidden: bot was kicked from the group chat)")
        );
    }

    /// The token is in the URL path; a transport failure must not carry it.
    #[tokio::test]
    async fn a_transport_failure_never_names_the_token() {
        let _guard = api_base_lock().await;
        set_api_base(Some("http://127.0.0.1:1".to_string()));
        let got = deliver_chat(TOKEN, 42, &run("hi")).await;
        set_api_base(None);
        let e = got.expect_err("nothing listens on port 1");
        assert_eq!(
            e,
            "sending reply chunk 1: calling Telegram sendMessage: request failed"
        );
        assert!(!e.contains(TOKEN));
    }
}

//! The shared HTTP client, ported from `internal/integrations/telegram/tools.go`
//! (`apiURL`, `callTelegram`) and `validate.go` (`telegramHTTPClient`,
//! `telegramResponse`).
//!
//! # The bot token is in the URL path
//!
//! `apiURL` is `fmt.Sprintf("%s/bot%s/%s", apiBaseURL, token, method)`, so every
//! request carries the credential in its **path** rather than in a header. That
//! is Telegram's API design, not a choice available to the port, and it changes
//! what the error strings have to avoid: a `reqwest::Error`'s `Display` carries
//! the URL it was building, so interpolating a transport cause here would put the
//! bot token into text the model reads and a `tool_result` stores. Go's wording
//! already avoids it — `calling Telegram %s: request failed` names the *method*
//! and nothing else — and [`a_failed_call_names_neither_the_token_nor_the_cause`]
//! asserts that rather than trusting it.
//!
//! It also means the token reaches [`crate::native::gourl`]-free URL
//! construction: `Sprintf` does not escape it, so neither does this. See
//! [`Client::endpoint`] for the one guard that adds.
//!
//! # The envelope decides, and it is Slack's shape with different names
//!
//! `callTelegram` reads `{"ok":…,"description":…,"result":…}` and never looks at
//! the HTTP status at all — not even a 429, which Slack's does. `ok: false` is
//! the failure, whatever the status; a 500 carrying `ok: true` is a success. The
//! three `encoding/json` rules that decode needs are spelled out on
//! [`TelegramResponse`], because #315 shipped two of them wrong in the file next
//! door before review caught it.
//!
//! Cap 10 MiB and timeout 60 seconds, the largest of the six on both counts.

use std::sync::OnceLock;
#[cfg(test)]
use std::sync::RwLock;
use std::time::Duration;

use serde_json::value::RawValue;
use tokio_stream::StreamExt;

use crate::claude::CancellationToken;

/// `apiBaseURL`'s default.
pub const DEFAULT_API_BASE: &str = "https://api.telegram.org";

/// Go's `apiBaseURL`, "a variable so tests can point it at a local httptest
/// server".
///
/// **Test-only, where Go's is not** — `github::client::API_BASE`'s reasoning, and
/// sharper here: the seam points every request, *token in the path*, at an
/// arbitrary host. Go had to export `SetAPIBase` (`telegram/parity.go`) because
/// `desktop/parity` is a different package; both callers here are in-crate.
#[cfg(test)]
static API_BASE: RwLock<Option<String>> = RwLock::new(None);

#[cfg(test)]
fn api_base() -> String {
    API_BASE
        .read()
        .expect("the telegram API base lock is poisoned")
        .clone()
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
}

#[cfg(not(test))]
fn api_base() -> String {
    DEFAULT_API_BASE.to_string()
}

/// Points every subsequent request at `base`; `None` restores the default.
#[cfg(test)]
pub(super) fn set_api_base(base: Option<String>) {
    *API_BASE
        .write()
        .expect("the telegram API base lock is poisoned") = base;
}

/// Serializes the tests that redirect [`API_BASE`].
#[cfg(test)]
pub(super) async fn api_base_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// `io.LimitReader(resp.Body, 10*1024*1024)`.
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// `telegramHTTPClient` — 60 seconds.
fn http_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .ok()
        })
        .as_ref()
}

/// `telegramResponse`, and the three `encoding/json` rules a derived
/// `Deserialize` gets wrong.
///
/// All three are written down in `desktop/CLAUDE.md` and all three were missed
/// in #315's equivalent struct before review caught them, so they are stated
/// here rather than assumed:
///
/// 1. **A JSON `null` is a zero value, not a type error.** Telegram sends
///    `"description": null` beside a failure and `"result": null` beside a
///    success; a plain derive answers `invalid type: null`. Hence
///    `null_is_zero_value` on the two scalars.
/// 2. **A JSON array is not a struct.** serde builds a struct from a sequence
///    positionally when every field has a default, so `[true]` would decode to
///    `ok: true` and turn Go's `cannot unmarshal array` into a *success*. Hence
///    [`crate::native::gojson::GoStruct`].
/// 3. **A bare `null` body is a no-op**, not a parse failure —
///    `json.Unmarshal([]byte("null"), &v)` leaves the struct zeroed and returns
///    `nil`, so Go falls through to the `!ok` branch. Hence the `Option<…>`
///    around the wrapper in [`read_response`].
///
/// And one that is Telegram's own: **`result` is a `json.RawMessage`**, so an
/// *absent* field is `nil` (which `string(nil)` renders as `""`) while an
/// explicit `null` is the four bytes `null`. `Option<Box<RawValue>>` cannot tell
/// those apart on its own — serde maps a JSON `null` to `None` — so
/// [`raw_or_absent`] captures the null as a `RawValue` and lets `#[serde(default)]`
/// supply the absent case. Both spellings reach the model verbatim in a result
/// sentence, so the difference is observable.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
pub struct TelegramResponse {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    pub ok: bool,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    pub description: String,
    #[serde(deserialize_with = "raw_or_absent")]
    result: Option<Box<RawValue>>,
}

impl TelegramResponse {
    /// `string(tgResp.Result)`: the raw bytes, `""` when the field was absent and
    /// `null` when it was an explicit null.
    pub fn result(&self) -> &str {
        self.result.as_ref().map_or("", |raw| raw.get())
    }
}

/// Captures an explicit `null` as a `RawValue` rather than as `None`.
///
/// `Option<Box<RawValue>>`'s own impl folds a JSON `null` into `None`, which
/// would make `{"result":null}` and an absent `result` the same value — and they
/// are not: Go renders the first as `null` and the second as the empty string,
/// both of which land in a result sentence the model reads.
fn raw_or_absent<'de, D>(deserializer: D) -> Result<Option<Box<RawValue>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// Go's implicit client: a bot token and the requests made with it.
#[derive(Clone)]
pub struct Client {
    token: String,
}

impl Client {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    /// `callTelegram`: a JSON POST whose envelope is checked before the caller
    /// sees it.
    ///
    /// `body` is already-encoded bytes, because the encoding has to be
    /// `json.Marshal`'s — sorted keys, HTML escaping, and Go's own float
    /// spelling for `send_location`'s coordinates.
    pub async fn call(
        &self,
        ct: &CancellationToken,
        method: &str,
        body: Vec<u8>,
    ) -> Result<TelegramResponse, String> {
        let failed = format!("calling Telegram {method}: request failed");

        let request = http_client()
            .ok_or_else(|| failed.clone())?
            .post(self.endpoint(method).ok_or_else(|| failed.clone())?)
            .header("Content-Type", "application/json")
            .body(body);

        let response = tokio::select! {
            () = ct.cancelled() => return Err(failed),
            result = request.send() => result.map_err(|_| failed)?,
        };

        read_response(ct, response).await
    }

    /// `apiURL(token, method)` — `{base}/bot{token}/{method}`, unescaped, plus
    /// the guard that costs.
    ///
    /// Go hands `Sprintf`'s output to `http.NewRequestWithContext` and `net/http`
    /// writes it verbatim; `reqwest` builds every request through
    /// `url::Url::parse`, which normalizes. The interpolated value here is the
    /// **bot token**, so a token holding a `.`/`..` segment or a byte `url`
    /// re-encodes would send this request somewhere Go does not — with the token
    /// itself as the credential. A token is not model-supplied, which makes this
    /// far less reachable than #312's and #317's equivalents, but the failure is
    /// the same shape and the check is three lines.
    ///
    /// `None` rather than a sentence, because the caller already has Go's
    /// wording for a request it could not make.
    fn endpoint(&self, method: &str) -> Option<reqwest::Url> {
        let path = format!("/bot{}/{method}", self.token);
        let url = reqwest::Url::parse(&format!("{}{path}", api_base())).ok()?;
        (url.path() == path && url.query().is_none()).then_some(url)
    }
}

/// The tail of `callTelegram`: read the body, decode the envelope, and turn
/// `ok: false` into Go's sentence.
///
/// **The HTTP status is never looked at** — not even 429, which Slack's
/// equivalent does check. A 500 carrying `{"ok":true}` is a success here.
async fn read_response(
    ct: &CancellationToken,
    response: reqwest::Response,
) -> Result<TelegramResponse, String> {
    let body = read_capped(ct, response).await?;

    let envelope =
        serde_json::from_str::<Option<crate::native::gojson::GoStruct<TelegramResponse>>>(&body)
            .map(|wrapped| wrapped.map_or_else(TelegramResponse::default, |wrapped| wrapped.0))
            // `fmt.Errorf("parsing response: %w", err)`. `encoding/json`'s wording is not
            // reproducible, so this carries serde's — pinned as a divergence. It cannot
            // leak the token: what is being parsed is Telegram's response.
            .map_err(|e| format!("parsing response: {e}"))?;

    if !envelope.ok {
        return Err(format!("telegram API error: {}", envelope.description));
    }
    Ok(envelope)
}

/// `io.ReadAll(io.LimitReader(resp.Body, 10 MiB))`.
async fn read_capped(
    ct: &CancellationToken,
    response: reqwest::Response,
) -> Result<String, String> {
    let mut body = Vec::new();
    let mut stream = Box::pin(response.bytes_stream());
    loop {
        let chunk = tokio::select! {
            () = ct.cancelled() => return Err("reading response: context canceled".to_string()),
            chunk = stream.next() => chunk,
        };
        match chunk {
            None => break,
            Some(Ok(chunk)) => {
                body.extend_from_slice(&chunk);
                if body.len() >= MAX_RESPONSE_BYTES {
                    body.truncate(MAX_RESPONSE_BYTES);
                    break;
                }
            }
            Some(Err(e)) => return Err(format!("reading response: {e}")),
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// `read_messages`' clamp — the only one in this integration, and the only one
/// in the six whose fallback is its own **maximum**.
///
/// `limit <= 0 || limit > 100` becomes 100, where every sibling falls back to
/// something smaller than its ceiling. Read from `tools.go` rather than
/// generalised from the pattern.
pub fn clamp_limit(limit: i64) -> i64 {
    if limit <= 0 || limit > 100 {
        100
    } else {
        limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// A distinctive token, so a leak is unmistakable — and this one would be in
    /// the *URL*, which is what makes it worth asserting.
    const SECRET: &str = "123456:AAF-SUPER-SECRET-BOT-TOKEN";

    /// One scripted reply through the real client, reduced to what a tool would
    /// actually use: `result()` on success, the sentence on failure.
    ///
    /// Reduced rather than returned whole because [`TelegramResponse`]
    /// deliberately derives no `Debug` — it holds Telegram's `result`, which is
    /// chat content, and an assertion helper is exactly the kind of place a
    /// `{:?}` gets added later and then copied into a log line.
    async fn reply(status: u16, body: &str) -> Result<String, String> {
        let body = body.to_string();
        let app = axum::Router::new().fallback(move || {
            let body = body.clone();
            async move { (StatusCode::from_u16(status).expect("status"), body) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));
        Client::new(SECRET)
            .call(&CancellationToken::new(), "getChat", b"{}".to_vec())
            .await
            .map(|response| response.result().to_string())
    }

    /// The three `encoding/json` rules, each at the shape that distinguishes it.
    ///
    /// Every expectation here was measured against a real `json.Unmarshal` into
    /// `telegramResponse` before it was written.
    #[tokio::test]
    async fn the_envelope_decodes_the_way_encoding_json_decodes_it() {
        let _guard = api_base_lock().await;

        // 1. A null is a zero value, not a type error.
        assert_eq!(
            reply(200, r#"{"ok":false,"description":null}"#)
                .await
                .expect_err("ok:false"),
            "telegram API error: "
        );
        assert_eq!(
            reply(200, r#"{"ok":null}"#).await.expect_err("ok:false"),
            "telegram API error: "
        );

        // 2. A bare null body is a no-op, so Go reaches the !ok branch.
        assert_eq!(
            reply(200, "null").await.expect_err("ok:false"),
            "telegram API error: "
        );

        // 3. An array is not a struct — without `GoStruct` this decodes
        //    positionally to `ok: true` and becomes a success.
        assert!(reply(200, "[true]")
            .await
            .expect_err("an array is a parse failure")
            .starts_with("parsing response: "));

        set_api_base(None);
    }

    /// `result` distinguishes **absent** from an explicit `null`, and both reach
    /// the model in a result sentence.
    #[tokio::test]
    async fn an_absent_result_and_a_null_result_are_different_strings() {
        let _guard = api_base_lock().await;

        assert_eq!(
            reply(200, r#"{"ok":true}"#).await.expect("ok"),
            "",
            "an absent RawMessage is nil, and string(nil) is empty"
        );
        assert_eq!(
            reply(200, r#"{"ok":true,"result":null}"#)
                .await
                .expect("ok"),
            "null",
            "an explicit null is the four bytes"
        );
        // …and a real result is passed through verbatim, key order and all.
        assert_eq!(
            reply(200, r#"{"ok":true,"result":{"z":1,"a":2}}"#)
                .await
                .expect("ok"),
            r#"{"z":1,"a":2}"#
        );

        set_api_base(None);
    }

    /// The status is never looked at — not even a 429, which Slack's client does
    /// check.
    #[tokio::test]
    async fn the_status_never_decides() {
        let _guard = api_base_lock().await;
        assert_eq!(
            reply(500, r#"{"ok":true,"result":1}"#)
                .await
                .expect("a 500 with ok:true is a success"),
            "1"
        );
        assert_eq!(
            reply(429, r#"{"ok":false,"description":"Too Many Requests"}"#)
                .await
                .expect_err("ok:false"),
            "telegram API error: Too Many Requests"
        );
        set_api_base(None);
    }

    /// The token is in the URL, so the failure sentence must name the method and
    /// nothing else — not the URL, not the cause, not the token.
    #[tokio::test]
    async fn a_failed_call_names_neither_the_token_nor_the_cause() {
        let _guard = api_base_lock().await;
        set_api_base(Some("http://127.0.0.1:1".to_string()));

        let message = Client::new(SECRET)
            .call(&CancellationToken::new(), "sendMessage", b"{}".to_vec())
            .await
            .map(|response| response.result().to_string())
            .expect_err("nothing is listening");
        assert_eq!(message, "calling Telegram sendMessage: request failed");
        assert!(!message.contains(SECRET), "the bot token reached the model");
        assert!(!message.contains("127.0.0.1"));

        set_api_base(None);
    }

    #[tokio::test]
    async fn a_cancelled_call_answers_what_a_cancelled_go_call_answers() {
        let _guard = api_base_lock().await;
        set_api_base(Some("http://127.0.0.1:1".to_string()));
        let ct = CancellationToken::new();
        ct.cancel();
        assert_eq!(
            Client::new(SECRET)
                .call(&ct, "getChat", b"{}".to_vec())
                .await
                .map(|response| response.result().to_string()),
            Err("calling Telegram getChat: request failed".to_string())
        );
        set_api_base(None);
    }

    /// The endpoint guard: a token `url` would resolve differently is refused
    /// rather than sent somewhere Go does not send it.
    ///
    /// The accepted half is the more interesting one. A token cannot begin a dot
    /// segment, because `apiURL` glues it to the literal `bot` — `../evil`
    /// becomes the segment `bot..`, which neither parser touches — so the guard
    /// admits it, and it must, since Go sends exactly that. Only a separator
    /// *inside* the token can produce one.
    #[test]
    fn a_token_url_would_resolve_differently_is_refused() {
        for (token, path) in [
            ("123456:AAF-token", "/bot123456:AAF-token/sendMessage"),
            // The `bot` prefix defuses a leading `..`: the segment is `bot..`.
            ("../evil", "/bot../evil/sendMessage"),
            // Reserved bytes a path keeps are kept by both.
            ("a$&+:=@b", "/bota$&+:=@b/sendMessage"),
        ] {
            let url = Client::new(token)
                .endpoint("sendMessage")
                .unwrap_or_else(|| panic!("{token} is a token Go sends"));
            assert_eq!(url.path(), path, "{token}");
        }

        for token in [
            // A separator *inside* the token can build a dot segment, and `url`
            // resolves it away.
            "a/../b", "a/./b", "a/..",
            // …and these end the path early, so the rest becomes a query or a
            // fragment where Go sends it as path bytes.
            "x?y", "x#y", // A byte `url` percent-encodes and Go does not.
            "a b", "a\\b",
        ] {
            assert!(
                Client::new(token).endpoint("sendMessage").is_none(),
                "{token} must be refused"
            );
        }
    }
}

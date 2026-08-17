//! The shared HTTP client, ported from `internal/integrations/slack/tools.go`
//! (`callSlack`, `callSlackJSON`, `readSlackResponse`) and `validate.go`
//! (`slackHTTPClient`, `slackAPIBase`).
//!
//! # Slack is not shaped like the other four
//!
//! Every integration ported so far builds a URL out of model input. Slack does
//! not: the base is a constant and the method is a literal (`conversations.list`,
//! `chat.postMessage`), so **nothing model-supplied reaches the path**. That
//! removes the whole class of problem #312 and #317 spent their reviews on —
//! there is no dot-segment guard here and no [`base_url::Base`], because there is
//! nothing to guard. Model input goes in a form body or a JSON body instead.
//!
//! Four other things differ from every sibling:
//!
//! - **Two call shapes, not one.** `callSlack` sends
//!   `application/x-www-form-urlencoded` built by `url.Values.Encode()`;
//!   `callSlackJSON` sends `application/json; charset=utf-8` — note the charset,
//!   which no other integration sends. Five tools use the first and two the
//!   second.
//! - **Failure is in the envelope, not the status.** `readSlackResponse` checks
//!   HTTP 429 and then stops looking at the status: it decodes `{"ok":…}` and
//!   treats `ok: false` as the error. So a **500 carrying `{"ok":true}` is a
//!   success** and a 200 carrying `{"ok":false,"error":"…"}` is a failure. That
//!   is the opposite of the 2xx-range gate every other integration uses, and it
//!   is reproduced exactly.
//! - **Rate limiting is a distinct sentence** carrying the `Retry-After` header
//!   verbatim — absent header included, which renders as an empty string in the
//!   middle of the sentence.
//! - **The cap is 5 MiB and the timeout 60 seconds**, both larger than any
//!   sibling's. Set on the client, which is what bounds a graceful shutdown.
//!
//! # Cancellation
//!
//! Go threads `ctx` into `http.NewRequestWithContext`, so a cancelled turn aborts
//! the call and `client.Do` fails — which becomes `calling Slack %s: request
//! failed`. That is what a cancelled call answers here too.

use std::sync::OnceLock;
#[cfg(test)]
use std::sync::RwLock;
use std::time::Duration;

use tokio_stream::StreamExt;

use crate::claude::CancellationToken;

/// `slackAPIBase`'s default.
pub const DEFAULT_API_BASE: &str = "https://slack.com/api";

/// Go's `slackAPIBase`, "a variable so tests can point it at a local httptest
/// server".
///
/// **Test-only, where Go's is not**, exactly as `github::client::API_BASE` is:
/// the seam points every Slack request — each bearing the workspace token — at
/// an arbitrary host, so it should not exist in a shipped binary. Go had to
/// export `SetAPIBase` (`slack/parity.go`) because `desktop/parity` is a
/// different package; both callers here are in-crate.
///
/// Confluence and Jira needed no such thing, because their base is per row and a
/// test can simply pass one. Slack's is a package-level constant, so the seam is
/// back.
#[cfg(test)]
static API_BASE: RwLock<Option<String>> = RwLock::new(None);

#[cfg(test)]
fn api_base() -> String {
    API_BASE
        .read()
        .expect("the slack API base lock is poisoned")
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
        .expect("the slack API base lock is poisoned") = base;
}

/// Serializes the tests that redirect [`API_BASE`].
///
/// The base is process-wide, as Go's package variable is, so two tests pointing
/// it at different fakes would race — and `cargo test` runs tests in parallel
/// where `go test` runs a package's in sequence.
#[cfg(test)]
pub(super) async fn api_base_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// `io.LimitReader(resp.Body, 5*1024*1024)`.
const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// `slackHTTPClient` — 60 seconds, the longest of the six.
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

/// Go's implicit client: a token and the requests made with it.
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

    /// `callSlack`: a form-encoded POST.
    ///
    /// `body` is `url.Values.Encode()`'s output — sorted keys, `+` for space —
    /// which [`crate::native::gourl::Values`] reproduces. Passed already encoded
    /// rather than as a map, so the encoding is visible at the call site the way
    /// it is in Go.
    pub async fn call_form(
        &self,
        ct: &CancellationToken,
        method: &str,
        body: String,
    ) -> Result<String, String> {
        self.send(
            ct,
            method,
            "application/x-www-form-urlencoded",
            body.into_bytes(),
        )
        .await
    }

    /// `callSlackJSON`: a JSON POST.
    ///
    /// Note the content type carries `; charset=utf-8`, which no other
    /// integration sends and which the vectors pin.
    pub async fn call_json(
        &self,
        ct: &CancellationToken,
        method: &str,
        body: Vec<u8>,
    ) -> Result<String, String> {
        self.send(ct, method, "application/json; charset=utf-8", body)
            .await
    }

    async fn send(
        &self,
        ct: &CancellationToken,
        method: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<String, String> {
        // `slackAPIBase + "/" + method`. Nothing escapes the method because
        // every one is a literal in the source — there is no model input in this
        // URL at all, which is why this module has no dot-segment guard.
        let failed = format!("calling Slack {method}: request failed");
        let url = format!("{}/{method}", api_base());

        let request = http_client()
            .ok_or_else(|| failed.clone())?
            .post(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", content_type)
            .body(body);

        let response = tokio::select! {
            () = ct.cancelled() => return Err(failed),
            result = request.send() => result.map_err(|_| failed)?,
        };

        read_slack_response(ct, method, response).await
    }
}

/// `readSlackResponse`: the 429 check, the cap, and the envelope.
///
/// **The HTTP status is otherwise never looked at.** Go checks 429 and then
/// decodes the body; `ok: false` is the failure and `ok: true` is the success,
/// whatever the status was. A port that added a 2xx gate — as every other
/// integration here has — would turn a `500` carrying `{"ok":true}` into an
/// error and change what the model reads.
async fn read_slack_response(
    ct: &CancellationToken,
    method: &str,
    response: reqwest::Response,
) -> Result<String, String> {
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        // `Header.Get` on an absent header is `""` to Go, which lands in the
        // middle of the sentence — `retry after  seconds`, two spaces. Kept.
        //
        // `from_utf8_lossy` over the raw bytes rather than `to_str()`, which
        // refuses non-ASCII: Go hands back whatever bytes arrived.
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
            .unwrap_or_default();
        return Err(format!(
            "slack rate limited ({method}), retry after {retry_after} seconds"
        ));
    }

    let body = read_capped(ct, response).await?;

    // Only `ok` and `error` are decoded, and everything else in the envelope is
    // ignored — so the body is returned to the caller **verbatim**, never
    // re-encoded from this value.
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(default)]
        ok: bool,
        #[serde(default)]
        error: String,
    }
    let envelope: Envelope = serde_json::from_str(&body).map_err(|e| {
        // `fmt.Errorf("parsing response: %w", err)`. `encoding/json`'s wording is
        // not reproducible (see `github::body::parse_string_map`), so this
        // carries serde's — a divergence the vectors pin. It cannot leak the
        // token: the text being parsed is Slack's response, not the request.
        format!("parsing response: {e}")
    })?;
    if !envelope.ok {
        return Err(format!("slack API error ({method}): {}", envelope.error));
    }

    Ok(body)
}

/// `io.ReadAll(io.LimitReader(resp.Body, 5 MiB))`.
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

/// The `<= 0 || > max` clamp all five paging tools apply, with a per-tool
/// fallback and a per-tool ceiling — both of which differ between them, which is
/// why neither is a constant here.
///
/// `list_channels` and `list_users` are `(1000, 100)`, `read_messages` is
/// `(100, 20)`, `search_messages`' count is `(100, 20)`. Verified one by one
/// rather than assumed from the first.
pub fn clamp(value: i64, max: i64, fallback: i64) -> i64 {
    if value <= 0 || value > max {
        fallback
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "xoxb-SUPER-SECRET-SLACK-TOKEN";

    #[test]
    fn the_clamps_are_gos() {
        // list_channels / list_users
        assert_eq!(clamp(0, 1000, 100), 100);
        assert_eq!(clamp(1001, 1000, 100), 100);
        assert_eq!(clamp(1000, 1000, 100), 1000);
        assert_eq!(clamp(-5, 1000, 100), 100);
        // read_messages / search_messages count
        assert_eq!(clamp(0, 100, 20), 20);
        assert_eq!(clamp(101, 100, 20), 20);
        assert_eq!(clamp(100, 100, 20), 100);
        assert_eq!(clamp(1, 100, 20), 1);
    }

    #[tokio::test]
    async fn the_api_base_is_redirectable_and_restorable() {
        let _guard = api_base_lock().await;
        assert_eq!(api_base(), DEFAULT_API_BASE);
        set_api_base(Some("http://127.0.0.1:1".to_string()));
        assert_eq!(api_base(), "http://127.0.0.1:1");
        set_api_base(None);
        assert_eq!(api_base(), DEFAULT_API_BASE);
    }

    /// The failure sentence names the **method** and nothing else — not the URL,
    /// not the cause, not the token.
    #[tokio::test]
    async fn a_failed_call_names_the_method_and_nothing_secret() {
        let _guard = api_base_lock().await;
        set_api_base(Some("http://127.0.0.1:1".to_string()));

        let message = Client::new(SECRET)
            .call_form(
                &CancellationToken::new(),
                "conversations.list",
                String::new(),
            )
            .await
            .expect_err("nothing is listening");
        assert_eq!(message, "calling Slack conversations.list: request failed");
        assert!(!message.contains(SECRET));
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
                .call_json(&ct, "chat.postMessage", b"{}".to_vec())
                .await,
            Err("calling Slack chat.postMessage: request failed".to_string())
        );
        set_api_base(None);
    }

    /// The envelope decides, not the status — which is Slack's own convention and
    /// the opposite of every other integration in this tree.
    #[tokio::test]
    async fn the_envelope_decides_and_the_status_does_not() {
        let _guard = api_base_lock().await;
        let cases: &[(u16, &str, Result<&str, &str>)] = &[
            // A 500 carrying `ok: true` is a **success**.
            (
                500,
                r#"{"ok":true,"note":"served"}"#,
                Ok(r#"{"ok":true,"note":"served"}"#),
            ),
            // …and a 200 carrying `ok: false` is a failure.
            (
                200,
                r#"{"ok":false,"error":"channel_not_found"}"#,
                Err("slack API error (conversations.info): channel_not_found"),
            ),
            // An absent `error` is an empty one, which Go interpolates as such.
            (
                200,
                r#"{"ok":false}"#,
                Err("slack API error (conversations.info): "),
            ),
        ];

        for (status, body, want) in cases {
            let (status, body) = (*status, body.to_string());
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

            let got = Client::new(SECRET)
                .call_form(
                    &CancellationToken::new(),
                    "conversations.info",
                    String::new(),
                )
                .await;
            assert_eq!(
                got.as_deref().map_err(String::as_str),
                *want,
                "status {status}"
            );
        }
        set_api_base(None);
    }

    use axum::http::StatusCode;
}

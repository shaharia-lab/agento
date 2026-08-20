//! The shared HTTP client, ported from `internal/integrations/jira/tools.go`'s
//! `client` and `validate.go`'s `jiraHTTPClient`.
//!
//! Everything the nine tools have in common: how a request is authenticated and
//! shaped, how much of a response is read, and the four sentences a failure can
//! produce. None of it shows in a tool's success text, which is why
//! `desktop/parity/jira_vectors.json` records the *request* the fake Jira
//! received as well as the answer.
//!
//! # What differs from `confluence::client`, which is otherwise its twin
//!
//! Both are Atlassian, both use basic auth over a per-row site URL, both cap at
//! 2 MiB. Three things are not shared, and none is cosmetic:
//!
//! - **The timeout is 15 seconds**, not Confluence's 30. Set on the client,
//!   which is what bounds a graceful shutdown — see `desktop/CLAUDE.md`.
//! - **The failure sentence names the method and the path**: `calling Jira %s
//!   %s: request failed`, where Confluence's names neither. The *path* and not
//!   the URL, so the site URL never appears in text the model reads.
//! - **A bad base is answered per call, not by refusing to host.** This is the
//!   one that matters, and it is Go's doing: `jira.Start` validates the site URL
//!   **not at all** — it reads `creds.SiteURL` and hands it straight to
//!   `buildMCPServer` — where `confluence.Start` runs `ValidateSiteURL` and
//!   fails. So Go hosts a Jira server and advertises all nine tools whatever the
//!   base says, and a port that refused to host would differ on the *advertised
//!   tool set*, which is the surface every agent's stored `capabilities.mcp`
//!   allowlist depends on. [`Client`] therefore holds `Option<Base>` and answers
//!   Go's own transport sentence when it is `None`: same tools, and no request
//!   built that this port cannot build faithfully. #277 pinned that Jira's and
//!   Confluence's site-URL rules are deliberately different; this is the same
//!   difference reaching the port.
//!
//! # Cancellation
//!
//! Go threads `ctx` into `http.NewRequestWithContext`, so a cancelled turn aborts
//! the outbound call and `client.Do` returns an error — which becomes `calling
//! Jira %s %s: request failed`. That is what a cancelled call answers here too,
//! so there is no divergence to invent. `crate::claude::tool` explains why the
//! token has to be watched at all: `rmcp` spawns handlers detached, so a handler
//! that ignores it keeps its socket open after the caller is gone.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Method;
use tokio_stream::StreamExt;

use crate::claude::CancellationToken;
use crate::native::integrations::base_url::Base;

/// `io.LimitReader(resp.Body, 2*1024*1024)`.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// `jiraHTTPClient` — 15 seconds, redirects followed.
///
/// `Option`, because building one can fail here where Go's cannot:
/// `&http.Client{Timeout: …}` is a struct literal and the trust store is not
/// consulted until the handshake, while `reqwest` reads the platform roots inside
/// `build()` and reports an unusable store as a builder error. That has to reach
/// the model as Go's own `request failed` rather than as a panic inside a handler
/// `rmcp` spawned detached.
pub(super) fn http_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .ok()
        })
        .as_ref()
}

/// `fmt.Errorf("calling Jira %s %s: request failed", method, path)`.
///
/// The **path**, not the URL: the site URL is a stored credential-adjacent value
/// and this text is read by the model and persisted in a `tool_result`.
fn request_failed(method: &Method, path: &str) -> String {
    format!("calling Jira {method} {path}: request failed")
}

/// Go's `client` struct: the site URL and the credentials every request carries.
///
/// Cloned per call from the closure that captured it — the shape
/// `crate::claude::tool`'s module docs describe — so it is deliberately cheap to
/// clone and holds nothing else.
#[derive(Clone)]
pub struct Client {
    /// `None` when the stored site URL is one this build cannot send a request
    /// through. Every call then answers Go's transport sentence; see the module
    /// header for why that rather than refusing to host.
    base: Option<Base>,
    email: String,
    api_token: String,
}

impl Client {
    /// Builds the client, checking the base **once**.
    ///
    /// A rejected base is logged here and nowhere else, which is the one place
    /// this port is less visible than Confluence's: there a bad site URL fails
    /// `Start`, so the registry logs it and the integration is plainly absent.
    /// Here the server starts, all nine tools are advertised, and every call
    /// answers a transport sentence — so without this line the only symptom
    /// would be a Jira integration that never works and never says why. The
    /// *reason* is logged and the site URL is not: it is a stored value from the
    /// credentials blob and this is a shared log file.
    pub fn new(
        site_url: impl Into<String>,
        email: impl Into<String>,
        api_token: impl Into<String>,
    ) -> Self {
        let base = match Base::new(&site_url.into()) {
            Ok(base) => Some(base),
            Err(mismatch) => {
                log::warn!(
                    "jira site URL cannot be used: {mismatch:?} — the server will host \
                     its tools and every call will fail; see \
                     native/integrations/base_url.rs"
                );
                None
            }
        };
        Self {
            base,
            email: email.into(),
            api_token: api_token.into(),
        }
    }

    /// `(*client).call`: an authenticated request whose body is returned as the
    /// raw response bytes.
    ///
    /// `body` is already-encoded bytes rather than a value. Go passes
    /// `map[string]any` to `json.Marshal` *inside* `call`; the callers reproduce
    /// that with [`crate::native::gojson::to_vec_marshal`] before calling,
    /// because the encoding has to be Go's — sorted keys at every level, and
    /// `\u003c`/`\u003e`/`\u0026` — and doing it here would hide that. The one
    /// visible consequence: Go's `marshaling request body` error is produced at
    /// the tool rather than here. It is unreachable either way (every value is a
    /// string, an integer or a map of those) and the wording is kept.
    ///
    /// The `Content-Type` header is set **only when there is a body** — Go's
    /// `if body != nil` — so every GET sends `Accept` alone, and `update_issue`
    /// with nothing set still sends `{"fields":{}}` and therefore still sends the
    /// header.
    pub async fn call(
        &self,
        ct: &CancellationToken,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<String, String> {
        let failed = request_failed(&method, path);
        let has_body = body.is_some();

        // The base was checked once, when this client was built. A `None` is a
        // stored site URL `url` and `net/url` do not agree about, and `resolve`
        // is the per-call dot-segment guard — both in
        // `crate::native::integrations::base_url`, which explains why each is
        // needed and why the answer is a refusal rather than a correction.
        let url = self
            .base
            .as_ref()
            .and_then(|base| base.resolve(path))
            .ok_or_else(|| failed.clone())?;

        let mut request = http_client()
            .ok_or_else(|| failed.clone())?
            .request(method, url)
            // `req.SetBasicAuth(c.email, c.apiToken)`:
            // `Basic base64(email + ":" + apiToken)`.
            .basic_auth(&self.email, Some(&self.api_token))
            .header("Accept", "application/json");
        if has_body {
            request = request.header("Content-Type", "application/json");
        }
        if let Some(body) = body {
            request = request.body(body);
        }

        let response = send(ct, request, &failed).await?;
        let status = response.status();
        let text = read_capped(ct, response).await?;
        // Go's `if resp.StatusCode < 200 || resp.StatusCode >= 300` — a *range*,
        // not `!= 200`, so the `204` a transition answers is a success.
        if !(200..300).contains(&status.as_u16()) {
            // `fmt.Errorf("jira API error: status %d: %s", …)` — Jira's own body,
            // **verbatim**. Decoding and re-encoding it would reorder its keys
            // and respell its numbers, and that body is what tells the model
            // whether it hit a permission problem or a bad issue key.
            return Err(format!(
                "jira API error: status {}: {text}",
                status.as_u16()
            ));
        }
        Ok(text)
    }
}

/// Sends a request, racing the run's cancellation.
///
/// Cancellation and a transport failure produce the same sentence, and that is
/// not a shortcut: in Go a cancelled `ctx` *is* how `client.Do` fails.
async fn send(
    ct: &CancellationToken,
    request: reqwest::RequestBuilder,
    failed: &str,
) -> Result<reqwest::Response, String> {
    tokio::select! {
        () = ct.cancelled() => Err(failed.to_string()),
        result = request.send() => result.map_err(|_| failed.to_string()),
    }
}

/// `io.ReadAll(io.LimitReader(resp.Body, 2 MiB))`.
///
/// Streamed rather than buffered whole: the cap exists so a hostile or runaway
/// response cannot be pulled into memory, and `bytes()` would read it all before
/// there was anything to truncate. Lossy UTF-8 for `confluence::client`'s
/// reason — a Go string holds arbitrary bytes and a Rust `String` cannot, and
/// `rmcp` demands a `String` at the end of the pipe anyway.
pub(super) async fn read_capped(
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
            // `fmt.Errorf("reading response: %w", err)`. The cause *is*
            // interpolated here, unlike the send failure: this error describes a
            // body already in flight and cannot carry the request URL.
            Some(Err(e)) => return Err(format!("reading response: {e}")),
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// `search_issues`'s clamp: `maxResults <= 0 || > 100` falls back to 50.
///
/// The only clamp in this integration, and unlike Confluence's pair it has one
/// caller — so it lives here rather than taking a fallback parameter.
pub fn clamp_max_results(max_results: i64) -> i64 {
    if max_results <= 0 || max_results > 100 {
        50
    } else {
        max_results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distinctive token, so a leak is unmistakable in any assertion.
    const SECRET: &str = "jira-SUPER-SECRET-API-TOKEN";

    fn client(site_url: &str) -> Client {
        Client::new(site_url, "person@example.com", SECRET)
    }

    #[test]
    fn the_clamp_is_gos() {
        for (input, want) in [(0, 50), (-5, 50), (101, 50), (100, 100), (1, 1), (50, 50)] {
            assert_eq!(clamp_max_results(input), want, "{input}");
        }
    }

    /// The failure sentence carries the method and the path, and **not** the
    /// site URL, the email or the token.
    ///
    /// A `reqwest::Error`'s `Display` carries the URL it was building, and a
    /// future edit that "helpfully" interpolated the cause would put the request
    /// — and, one refactor later, a header — into text the model reads and the
    /// transcript stores. Go's wording has the same property.
    #[tokio::test]
    async fn a_failed_call_names_the_path_and_nothing_secret() {
        // Port 1 on loopback: nothing listens, so the connect fails fast.
        let message = client("http://127.0.0.1:1")
            .call(
                &CancellationToken::new(),
                Method::GET,
                "/rest/api/3/project",
                None,
            )
            .await
            .expect_err("nothing is listening");
        assert_eq!(
            message,
            "calling Jira GET /rest/api/3/project: request failed"
        );
        assert!(!message.contains(SECRET));
        assert!(!message.contains("person@example.com"));
        assert!(!message.contains("127.0.0.1"));
    }

    /// A cancelled call answers **Go's sentence**, because in Go a cancelled
    /// `ctx` is how `client.Do` fails.
    #[tokio::test]
    async fn a_cancelled_call_answers_what_a_cancelled_go_call_answers() {
        let ct = CancellationToken::new();
        ct.cancel();
        assert_eq!(
            client("http://127.0.0.1:1")
                .call(&ct, Method::GET, "/rest/api/3/project", None)
                .await,
            Err("calling Jira GET /rest/api/3/project: request failed".to_string())
        );
    }

    /// A site URL this build cannot agree with Go about makes **every** call
    /// answer the transport sentence — and leaves the tool set alone, which is
    /// the whole reason it is handled here rather than by refusing to host.
    ///
    /// `super::super::tests` asserts the other half: that all nine tools are
    /// still advertised.
    #[tokio::test]
    async fn a_base_this_build_cannot_send_through_refuses_every_call() {
        for site in [
            r"https://evil.com\@jira.atlassian.net",
            "https://jira.atlassian.net%2Eevil.com",
            "https://jira.atlassian.net/a/../b",
            "https://user:pw@jira.atlassian.net",
        ] {
            let answer = client(site)
                .call(
                    &CancellationToken::new(),
                    Method::GET,
                    "/rest/api/3/project",
                    None,
                )
                .await;
            assert_eq!(
                answer,
                Err("calling Jira GET /rest/api/3/project: request failed".to_string()),
                "{site}"
            );
        }
    }

    /// `io.LimitReader` truncates rather than failing, and it stops *reading* —
    /// which is why the port streams instead of buffering and then slicing.
    #[tokio::test]
    async fn a_response_is_truncated_at_the_cap_rather_than_refused() {
        let oversized = MAX_RESPONSE_BYTES + 4096;
        let app = axum::Router::new().fallback(move || async move { "x".repeat(oversized) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let body = client(&base)
            .call(&CancellationToken::new(), Method::GET, "/big", None)
            .await
            .expect("a 200 is a success however long the body is");
        assert_eq!(body.len(), MAX_RESPONSE_BYTES);
    }
}

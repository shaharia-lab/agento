//! The shared HTTP client, ported from `internal/integrations/confluence/tools.go`
//! (`callConfluence`) and `validate.go` (`confluenceHTTPClient`).
//!
//! Everything the six tools have in common: how a request is authenticated and
//! shaped, how much of a response is read, and the three sentences a failure can
//! produce. None of it shows in a tool's success text, which is why
//! `desktop/parity/confluence_vectors.json` records the *request* the fake
//! Confluence received as well as the answer.
//!
//! # What is different from `github::client`, and it is not cosmetic
//!
//! - **The base is per integration, not per process.** GitHub has one API root
//!   and a package variable pointing at it; a Confluence site URL comes out of
//!   the row's credentials, so it is a field on [`Client`] and there is no
//!   test-only seam to gate — a parity run simply constructs a client against a
//!   loopback fake. (The Go side still needs one, because `Start` insists on
//!   HTTPS before it ever builds the server; see
//!   `internal/integrations/confluence/parity.go`.)
//! - **Basic auth, not a bearer token.** `req.SetBasicAuth(email, apiToken)` is
//!   `Authorization: Basic base64(email + ":" + apiToken)`, which is exactly what
//!   `reqwest`'s `basic_auth` writes. The vectors pin the encoded header, so a
//!   divergence in either implementation fails rather than shipping.
//! - **The failure sentence names nothing.** `calling confluence API: request
//!   failed` carries neither the method nor the URL, where GitHub's carries
//!   both. That is Go's wording and the reason is the same there: the URL is
//!   built from a site URL and model-supplied ids.
//! - **The timeout is 30 seconds**, not GitHub's 15. Set on the client, which is
//!   what bounds a graceful shutdown — see `desktop/CLAUDE.md` on why a handler
//!   with no client timeout leaves a revoked credential answering `tools/call`.
//!
//! # Cancellation
//!
//! Go threads `ctx` into `http.NewRequestWithContext`, so a cancelled turn aborts
//! the outbound call and `client.Do` returns an error — which becomes `calling
//! confluence API: request failed`. That is what a cancelled call answers here
//! too, so there is no divergence to invent. `crate::claude::tool` explains why
//! the token has to be watched at all: `rmcp` spawns handlers detached, so a
//! handler that ignores it keeps its socket open after the caller is gone.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Method;
use tokio_stream::StreamExt;

use crate::claude::CancellationToken;

/// `maxResponseBytes`: 2 MiB, "keeping it small avoids flooding the LLM context".
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// `confluenceHTTPClient` — 30 seconds, redirects followed.
///
/// `Option`, because building one can fail here where Go's cannot.
/// `&http.Client{Timeout: …}` is a struct literal and the trust store is not
/// consulted until the handshake; `reqwest` reads the platform roots inside
/// `build()` (see `Cargo.toml` on `rustls-tls-native-roots`) and reports an
/// unusable store as a builder error. That has to reach the model as Go's own
/// `calling confluence API: request failed` — a handshake that cannot complete
/// is a transport failure there too — rather than as a panic inside a handler
/// `rmcp` spawned detached.
fn http_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .ok()
        })
        .as_ref()
}

/// `fmt.Errorf("calling confluence API: request failed")`, which is also what a
/// cancelled call and a refused URL answer. See the module header.
const REQUEST_FAILED: &str = "calling confluence API: request failed";

/// Go's `client`-equivalent: the site URL and the credentials every request
/// carries.
///
/// Cloned per call from the closure that captured it — the shape
/// `crate::claude::tool`'s module docs describe — so it is deliberately cheap to
/// clone and holds nothing else.
#[derive(Clone)]
pub struct Client {
    site_url: String,
    email: String,
    api_token: String,
}

impl Client {
    /// `site_url` is the value [`super::validate_site_url`] already cleaned:
    /// HTTPS, a host, no trailing slash. Nothing here re-checks it, exactly as
    /// nothing in `tools.go` does — `Start` is the one gate, and a parity run
    /// deliberately bypasses it to point a client at loopback.
    pub fn new(
        site_url: impl Into<String>,
        email: impl Into<String>,
        api_token: impl Into<String>,
    ) -> Self {
        Self {
            site_url: site_url.into(),
            email: email.into(),
            api_token: api_token.into(),
        }
    }

    /// `callConfluence`: an authenticated request whose body is returned as the
    /// raw response bytes.
    ///
    /// `path` is appended to the site URL, already escaped by its caller —
    /// `gourl::path_escape` per segment, `gourl::query_escape` per query value —
    /// because that is what `fmt.Sprintf` over `url.PathEscape` does on the Go
    /// side. `body` is already-encoded bytes rather than a value: the encoding
    /// has to be `json.Marshal`'s (sorted keys, HTML escaping) and doing it here
    /// would hide that. See [`super::content`].
    ///
    /// The `Content-Type` header is set **only when there is a body** — Go's
    /// `if body != nil` — so both GETs send `Accept` alone.
    pub async fn call(
        &self,
        ct: &CancellationToken,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<String, String> {
        let has_body = body.is_some();
        let mut request = http_client()
            .ok_or_else(|| REQUEST_FAILED.to_string())?
            .request(method, self.absolute(path)?)
            // `req.SetBasicAuth(email, apiToken)`:
            // `Basic base64(email + ":" + apiToken)`, which is what this writes.
            .basic_auth(&self.email, Some(&self.api_token))
            .header("Accept", "application/json");
        if has_body {
            request = request.header("Content-Type", "application/json");
        }
        if let Some(body) = body {
            request = request.body(body);
        }

        let response = send(ct, request).await?;
        let status = response.status();
        let text = read_capped(ct, response).await?;
        if !(200..300).contains(&status.as_u16()) {
            // `fmt.Errorf("confluence API error (status %d): %s", …)` — the
            // API's own body, **verbatim**. Decoding and re-encoding it would
            // reorder its keys and respell its numbers, and that body is what
            // tells the model whether it hit a permission problem or a typo.
            return Err(format!(
                "confluence API error (status {}): {text}",
                status.as_u16()
            ));
        }
        Ok(text)
    }

    /// The URL a request to `path` goes to — or [`REQUEST_FAILED`], if this port
    /// cannot build the request Go builds.
    ///
    /// # Why this exists, and why it refuses instead of correcting
    ///
    /// Go concatenates the site URL and `path`, hands the string to
    /// `http.NewRequestWithContext`, and `net/http` writes `URL.RequestURI()` on
    /// the wire **verbatim**. Nothing normalizes: `url.PathEscape` leaves `.` and
    /// `..` alone (both are unreserved) and `net/url` does not remove dot
    /// segments, so `get_page(page_id: "..")` asks Confluence for
    /// `/wiki/api/v2/pages/..` and gets a 404.
    ///
    /// `reqwest` builds every request through `url::Url::parse`, which applies
    /// WHATWG dot-segment removal for http(s). The same call would leave as
    /// `/wiki/api/v2/` — the space *listing* rather than one page, on a request
    /// already carrying the user's API token. `page_id`, `space_id` and
    /// `parent_id` are model-supplied and every tool result carries
    /// attacker-authored wiki content, so it is reachable under prompt
    /// injection, and it applies to `update_page`'s write too. Escaping the dots
    /// is not a fix — `%2E%2E` is collapsed as well.
    ///
    /// `reqwest` offers no way to send an unnormalized target, so the faithful
    /// option is to **refuse**: the model reads the sentence a transport failure
    /// produces rather than the answer to a question it did not ask. This is
    /// `github::client::absolute`'s reasoning verbatim, and #313–#316 each need
    /// it.
    ///
    /// # What is compared, and why it is not the raw string
    ///
    /// GitHub's base is a fixed, already-encoded string, so there the whole
    /// target could be compared against the `path` argument. A site URL is
    /// per row and **user-typed**, so it is not necessarily encoded:
    /// `https://intranet.example.com/my atlassian` is one Go accepts, and both
    /// `net/url`'s `EscapedPath` and `url` send it as `/my%20atlassian/…`. They
    /// agree — but the raw text does not, so comparing against the raw
    /// concatenation would refuse every call for a site URL that works.
    ///
    /// So the base is parsed **on its own** and its rendered path is the
    /// expected prefix; only the tool's own suffix is compared against the raw
    /// bytes it built. That is sound because the suffix *is* fully encoded, and
    /// not by luck: `encodePathSegment` escapes every byte in `url`'s path
    /// encode set and `encodeQueryComponent` every byte in its query one, so
    /// there is nothing left for `url` to percent-encode; existing `%XX` escapes
    /// are passed through with their case intact, so Go's uppercase hex
    /// survives; and a path with no `?` compares `""` against `query()`'s
    /// `None`. Host, scheme and port are not compared at all, so `url`'s
    /// default-port and case normalization cannot reach this.
    ///
    /// The comparison stays **exact** rather than a `..` scan, so anything else
    /// `url` normalizes in a tool's path — now or after an upgrade — is caught by
    /// construction.
    ///
    /// [`super::validate_site_url`] has already refused the three base shapes
    /// this cannot work behind: one `url` will not parse, one carrying its own
    /// `?` or `#`, and one holding a dot segment of its own. The re-checks below
    /// are cheap and keep the function total rather than trusting a caller.
    fn absolute(&self, path: &str) -> Result<reqwest::Url, String> {
        let base = reqwest::Url::parse(&self.site_url).map_err(|_| REQUEST_FAILED.to_string())?;
        if base.query().is_some() || base.fragment().is_some() {
            return Err(REQUEST_FAILED.to_string());
        }
        // `url` renders a base with no path of its own as `/`; the suffix
        // supplies that slash, so the prefix is empty.
        let prefix = match base.path() {
            "/" => "",
            other => other,
        };

        let url = reqwest::Url::parse(&format!("{}{path}", self.site_url))
            .map_err(|_| REQUEST_FAILED.to_string())?;
        let (want_path, want_query) = path.split_once('?').unwrap_or((path, ""));
        if url.path() != format!("{prefix}{want_path}") || url.query().unwrap_or("") != want_query {
            return Err(REQUEST_FAILED.to_string());
        }
        Ok(url)
    }
}

/// Sends a request, racing the run's cancellation.
///
/// Cancellation and a transport failure produce the same sentence, and that is
/// not a shortcut: in Go a cancelled `ctx` *is* how `client.Do` fails, so this is
/// the message that reaches the model there too.
async fn send(
    ct: &CancellationToken,
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, String> {
    tokio::select! {
        () = ct.cancelled() => Err(REQUEST_FAILED.to_string()),
        result = request.send() => result.map_err(|_| REQUEST_FAILED.to_string()),
    }
}

/// `io.ReadAll(io.LimitReader(resp.Body, maxResponseBytes))`.
///
/// Streamed rather than buffered whole: the cap exists so a hostile or runaway
/// response cannot be pulled into memory, and `bytes()` would read it all before
/// there was anything to truncate.
///
/// **Lossy UTF-8**, the one place this is not literally Go. Go does
/// `string(respBody)` on arbitrary bytes and a Go string holds them; a Rust
/// `String` cannot, so invalid sequences become U+FFFD. Every Confluence
/// response this reaches is JSON, and the alternative — carrying `Vec<u8>`
/// through to the `ContentBlock` — would only move the same conversion to the
/// end of the pipe, where `rmcp` demands a `String` anyway.
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
            // `fmt.Errorf("reading response: %w", err)`. The cause *is*
            // interpolated here, unlike the send failure: this error describes a
            // body already in flight and cannot carry the request URL.
            Some(Err(e)) => return Err(format!("reading response: {e}")),
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// The `1-250` clamp both paging tools apply, with a per-tool fallback.
///
/// `limit <= 0 || limit > 250` falls back, so a zero (the value an
/// omitted-but-required field cannot be, since every field *is* required) and a
/// 500 land in the same place. The two callers disagree on the fallback —
/// `list_spaces` uses 50 and `search_content` 25 — which is why it is a
/// parameter rather than a constant.
pub fn clamp_limit(limit: i64, fallback: i64) -> i64 {
    if limit <= 0 || limit > 250 {
        fallback
    } else {
        limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distinctive token, so a leak is unmistakable in any assertion.
    const SECRET: &str = "atlassian-SUPER-SECRET-API-TOKEN";

    fn client(site_url: &str) -> Client {
        Client::new(site_url, "person@example.com", SECRET)
    }

    #[test]
    fn the_limit_clamp_is_gos() {
        for (limit, fallback, want) in [
            (0, 50, 50),
            (-5, 50, 50),
            (251, 50, 50),
            (250, 50, 250),
            (1, 50, 1),
            (0, 25, 25),
            (251, 25, 25),
        ] {
            assert_eq!(clamp_limit(limit, fallback), want, "{limit}/{fallback}");
        }
    }

    /// The dot-segment guard, at the unit rather than at the vector: what it
    /// refuses, and — the half that would bite — everything it must not.
    ///
    /// `confluence_vectors.json` covers this end to end for every call; this
    /// covers the shapes a *future* tool could build that no vector has yet,
    /// which is where a guard like this normally goes wrong.
    #[test]
    fn dot_segments_are_refused_and_nothing_else_is() {
        let c = client("https://acme.atlassian.net");
        let ok = |path: &str| {
            c.absolute(path)
                .unwrap_or_else(|_| panic!("{path} is a legitimate path"))
        };

        // `url` resolves every one of these into a different endpoint than Go
        // calls, and escaping the dots does not help — `%2E%2E` is collapsed
        // too, so there is nothing to encode our way out of.
        for path in [
            "/wiki/api/v2/pages/..",
            "/wiki/api/v2/pages/.",
            "/wiki/api/v2/pages/%2E%2E",
            "/wiki/api/v2/spaces/../../..",
        ] {
            assert_eq!(c.absolute(path), Err(REQUEST_FAILED.to_string()), "{path}");
        }

        // Everything `gourl` can emit. `PathEscape` already escapes every byte
        // in `url`'s path encode set and `QueryEscape` every byte in its query
        // one, so there is nothing left for `url` to re-encode; percent-escapes
        // keep Go's uppercase hex; a space is `+` in a query and `%20` in a
        // segment; and a path with no `?` has no query on either side.
        assert_eq!(
            ok("/wiki/api/v2/spaces?limit=50").path(),
            "/wiki/api/v2/spaces"
        );
        assert_eq!(ok("/wiki/api/v2/spaces/123").query(), None);
        assert_eq!(
            ok("/wiki/api/v2/search?cql=space+%3D+DEV+%26+type+%3D+page&limit=25").query(),
            Some("cql=space+%3D+DEV+%26+type+%3D+page&limit=25")
        );
        assert_eq!(
            ok("/wiki/api/v2/pages/my%20page%2Fid").path(),
            "/wiki/api/v2/pages/my%20page%2Fid",
            "uppercase hex survives, and so does an escaped slash"
        );
        // A dot *inside* a segment is not a dot segment, and neither is one
        // inside a query value — only the path is resolved.
        assert_eq!(
            ok("/wiki/api/v2/pages/v1.2.3").path(),
            "/wiki/api/v2/pages/v1.2.3"
        );
        assert_eq!(
            ok("/wiki/api/v2/search?cql=..%2F..&limit=25").query(),
            Some("cql=..%2F..&limit=25")
        );
    }

    /// A site URL with its own base path is concatenated, not resolved — Go's
    /// `fmt.Sprintf` has no notion of a base — so the guard has to accept it.
    #[test]
    fn a_site_url_with_a_base_path_is_still_a_prefix() {
        let c = client("https://intranet.example.com/atlassian");
        let url = c
            .absolute("/wiki/api/v2/spaces?limit=50")
            .expect("a base path is legitimate");
        assert_eq!(url.path(), "/atlassian/wiki/api/v2/spaces");
        assert_eq!(url.query(), Some("limit=50"));
    }

    /// A base `url` re-encodes is **not** a refusal, and this is the case that
    /// a raw-string comparison gets wrong.
    ///
    /// A site URL is user-typed, so it need not be percent-encoded.
    /// `net/url`'s `EscapedPath` and `url` both send `/my atlassian` as
    /// `/my%20atlassian`, so Go and this port agree on the wire — but the raw
    /// text does not match either of them, and comparing against it would answer
    /// `request failed` for every call against a site URL that works.
    #[test]
    fn a_base_the_url_crate_re_encodes_is_still_served() {
        for (site, prefix) in [
            (
                "https://intranet.example.com/my atlassian",
                "/my%20atlassian",
            ),
            ("https://intranet.example.com/café", "/caf%C3%A9"),
            ("https://intranet.example.com/a%20b", "/a%20b"),
        ] {
            let url = client(site)
                .absolute("/wiki/api/v2/spaces?limit=50")
                .unwrap_or_else(|_| panic!("{site} is a site URL Go serves"));
            assert_eq!(url.path(), format!("{prefix}/wiki/api/v2/spaces"), "{site}");
            assert_eq!(url.query(), Some("limit=50"), "{site}");
        }
    }

    /// …and the dot-segment guard still fires behind such a base, because only
    /// the tool's own suffix is compared against the bytes it built.
    #[test]
    fn the_guard_still_fires_behind_a_re_encoded_base() {
        assert_eq!(
            client("https://intranet.example.com/my atlassian").absolute("/wiki/api/v2/pages/.."),
            Err(REQUEST_FAILED.to_string())
        );
    }

    /// A site URL whose authority is followed by a `?` or a `#` has no faithful
    /// target. `validate_site_url` refuses it, and so does this — the function
    /// stays total rather than trusting a caller.
    #[test]
    fn a_site_url_that_is_not_a_prefix_of_the_target_is_refused() {
        for site in [
            "https://acme.atlassian.net?a=b",
            "https://acme.atlassian.net#x",
        ] {
            assert_eq!(
                client(site).absolute("/wiki/api/v2/spaces?limit=50"),
                Err(REQUEST_FAILED.to_string()),
                "{site}"
            );
        }
    }

    /// The whole reason the failure message names nothing.
    ///
    /// A `reqwest::Error`'s `Display` carries the URL it was building, and a
    /// future edit that "helpfully" interpolated the cause would put the request
    /// — and, one refactor later, a header — into text the model reads and the
    /// transcript stores. Go's wording has the same property and this asserts it
    /// rather than trusting it.
    #[tokio::test]
    async fn a_failed_call_names_neither_the_credentials_nor_the_cause() {
        // Port 1 on loopback: nothing listens, so the connect fails fast.
        let message = client("http://127.0.0.1:1")
            .call(
                &CancellationToken::new(),
                Method::GET,
                "/wiki/api/v2/spaces?limit=50",
                None,
            )
            .await
            .expect_err("nothing is listening");
        assert_eq!(message, REQUEST_FAILED);
        assert!(!message.contains(SECRET), "a token reached the model");
        assert!(
            !message.contains("person@example.com"),
            "an email reached the model"
        );
    }

    /// A cancelled call answers **Go's sentence**, and that is a port rather
    /// than a choice: in Go a cancelled `ctx` is how `client.Do` fails.
    #[tokio::test]
    async fn a_cancelled_call_answers_what_a_cancelled_go_call_answers() {
        let ct = CancellationToken::new();
        ct.cancel();
        assert_eq!(
            client("http://127.0.0.1:1")
                .call(&ct, Method::GET, "/wiki/api/v2/spaces?limit=50", None)
                .await,
            Err(REQUEST_FAILED.to_string())
        );
    }

    /// `io.LimitReader` truncates rather than failing, and it stops *reading* —
    /// which is why the port streams instead of buffering the body and then
    /// slicing it.
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

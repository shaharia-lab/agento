//! The shared HTTP client, ported from `internal/integrations/github/tools.go`.
//!
//! Everything the twenty tools have in common: where the API is, how a request
//! is authenticated and shaped, how much of a response is read, and the four
//! sentences a failure can produce. None of it is visible in a tool's success
//! text, which is why `desktop/parity/github_vectors.json` records the *request*
//! the fake GitHub received as well as the answer.
//!
//! # The error strings are the interface
//!
//! `crate::claude::new_tool` packs an `Err(String)` into a `CallToolResult`
//! with `is_error`, so every message below is **text the model reads and
//! retries on** — the same contract `mcp.AddTool`'s `ToolHandlerFor` gives the
//! Go side. They are reproduced byte for byte, including the two that look like
//! oversights and are not:
//!
//! - `"calling GitHub %s %s: request failed"` deliberately drops the transport
//!   error. A `reqwest`/`net/http` error can carry the URL, and the URL is
//!   built from user input — so the wording is fixed and the cause is not
//!   interpolated.
//! - `github API error: status %d: %s` carries GitHub's own body **verbatim**.
//!   That body is what tells the model whether it hit a permission problem or a
//!   typo, and re-encoding it through a JSON value would reorder its keys.
//!
//! # Cancellation
//!
//! Go threads `ctx` into `http.NewRequestWithContext`, so a cancelled turn
//! aborts the outbound call and `client.Do` returns an error — which the caller
//! turns into `calling GitHub %s %s: request failed`. That is exactly what a
//! cancelled call answers here, so the port needs no divergence: the
//! `tokio::select!` arm produces the same sentence Go's cancelled `Do` does.
//! (`crate::claude::tool` explains why the token has to be watched at all:
//! `rmcp` spawns handlers detached, so a handler that ignores it keeps its
//! socket open after the caller is gone.)

use std::sync::OnceLock;
#[cfg(test)]
use std::sync::RwLock;
use std::time::Duration;

use reqwest::Method;
use tokio_stream::StreamExt;

use crate::claude::CancellationToken;

/// `githubAPIBase`'s default — the root every path below is appended to.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// Go's `githubAPIBase`, a package variable "exposed as a variable so tests can
/// redirect requests to a local server".
///
/// **Test-only, where Go's is not, and deliberately.** The seam is "point every
/// GitHub request — bearing the user's personal access token — at an arbitrary
/// host", so it should not exist in a shipped binary. Go had no choice:
/// `desktop/parity` is a different package, so `internal/integrations/github/`
/// had to export `SetAPIBase` to be redirectable at all. Both callers here are
/// in-crate, so `#[cfg(test)]` costs nothing and compiles the whole thing out —
/// which is what [`api_base_lock`] below already does, for the same reason.
/// #313 adds the last seam and should follow this, as #314 and #315 did —
/// Slack's and Telegram's bases are package variables too. #317's is a constructor argument
/// rather than a static, because a Confluence site URL is per row; #316 needs
/// none at all, because `jira.Start` does not validate the site URL and so a
/// parity run can simply store the fake's URL.
///
/// A `RwLock` rather than a `OnceLock` because a test points it at a loopback
/// fake and then puts it back.
#[cfg(test)]
static API_BASE: RwLock<Option<String>> = RwLock::new(None);

/// Where requests go — [`DEFAULT_API_BASE`] unless [`set_api_base`] said
/// otherwise.
///
/// A poisoned lock means a test panicked between taking the write guard and
/// dropping it, three lines later. There is no recovery that is not a lie about
/// which server the next request reaches, and the caller is a test either way,
/// so this panics rather than guessing.
#[cfg(test)]
pub(super) fn api_base() -> String {
    API_BASE
        .read()
        .expect("the github API base lock is poisoned")
        .clone()
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
}

/// Where requests go. There is one answer in a shipped binary, because nothing
/// outside a test can move it — see [`API_BASE`].
#[cfg(not(test))]
pub(super) fn api_base() -> String {
    DEFAULT_API_BASE.to_string()
}

/// Points every subsequent request at `base`; `None` restores the default.
///
/// Only tests call this, and only tests *can* — see [`API_BASE`]. It is not
/// per-integration configuration: GitHub Enterprise would need a per-row base,
/// which the Go side does not have either.
#[cfg(test)]
pub(crate) fn set_api_base(base: Option<String>) {
    *API_BASE
        .write()
        .expect("the github API base lock is poisoned") = base;
}

/// Serializes the tests that redirect [`API_BASE`].
///
/// The base is process-wide, as Go's package variable is, so two tests that
/// point it at different fakes would race — and `cargo test` runs them in
/// parallel where `go test` runs a package's tests in sequence. Every test that
/// touches it takes this first.
///
/// `tokio`'s mutex rather than `std`'s: every holder awaits an HTTP round trip
/// while holding it, which is exactly what `clippy::await_holding_lock` exists
/// to refuse.
#[cfg(test)]
pub(crate) async fn api_base_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// `ghHTTPClient` — 15 seconds, redirects followed.
///
/// `Option`, because building one can fail here where Go's cannot.
/// `&http.Client{Timeout: …}` is a struct literal and the trust store is not
/// consulted until the handshake, so a machine with an unusable one fails
/// *per request*; `reqwest` reads the platform roots inside `build()` (see
/// `Cargo.toml` on `rustls-tls-native-roots`) and reports an empty store as a
/// builder error. That has to reach the model as Go's own
/// `calling GitHub …: request failed` — a handshake that cannot complete is a
/// transport failure there too — rather than as a panic inside a handler
/// `rmcp` spawned detached.
pub(super) fn http_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            crate::native::http::client_builder()
                .timeout(Duration::from_secs(15))
                .build()
                .ok()
        })
        .as_ref()
}

/// `ghNoRedirectClient` — the same, with `CheckRedirect` returning
/// `http.ErrUseLastResponse` so a 302's `Location` is readable.
///
/// A second client rather than a per-request policy because `reqwest` has no
/// per-request one; Go's is a second client too.
fn no_redirect_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            crate::native::http::client_builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
}

/// `call`'s cap: 2 MiB.
const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;
/// `callRaw`'s cap: 10 MB, "to accommodate large PR diffs".
const MAX_RAW_BYTES: usize = 10 * 1024 * 1024;
/// `getRedirectURL`'s cap on the body it quotes back when the response was not
/// a redirect after all. A *third* number, and the smallest by four orders of
/// magnitude — so a 500 there is truncated mid-sentence where the same 500 from
/// `call` is not.
const MAX_REDIRECT_ERROR_BYTES: usize = 512;

/// Go's `client` struct: a token and the requests made with it.
///
/// Cloned per call from the closure that captured it — the shape
/// `crate::claude::tool`'s module docs describe — so it is deliberately cheap
/// to clone and holds nothing else.
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

    /// `call`: a JSON request whose body is returned as the raw response bytes.
    ///
    /// `body` is already-encoded bytes rather than a value, because Go passes
    /// `map[string]any` to `json.Marshal` and the callers reproduce that with
    /// [`super::body::Body`] — the encoding has to be Go's (sorted keys, HTML
    /// escaping) and doing it here would hide that.
    ///
    /// The `Content-Type` header is set **only when there is a body**, which is
    /// observable: `update_issue` with nothing to update still sends `{}` and
    /// therefore still sends the header, while every GET sends neither.
    pub async fn call(
        &self,
        ct: &CancellationToken,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<String, String> {
        let has_body = body.is_some();
        let failed = request_failed(&method, path);
        let mut request = http_client()
            .ok_or_else(|| failed.clone())?
            .request(method, absolute(path, &failed)?)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json");
        if has_body {
            request = request.header("Content-Type", "application/json");
        }
        if let Some(body) = body {
            request = request.body(body);
        }

        let response = send(ct, request, &failed).await?;
        let status = response.status();
        let text = read_capped(ct, response, MAX_JSON_BYTES).await?;
        api_error_or(status, text)
    }

    /// `callRaw`: the same request under a caller-chosen `Accept`, no body, and
    /// a 10 MB cap. One caller — `get_pull_diff`, whose response is a diff.
    pub async fn call_raw(
        &self,
        ct: &CancellationToken,
        method: Method,
        path: &str,
        accept: &str,
    ) -> Result<String, String> {
        let failed = request_failed(&method, path);
        let request = http_client()
            .ok_or_else(|| failed.clone())?
            .request(method, absolute(path, &failed)?)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", accept);

        let response = send(ct, request, &failed).await?;
        let status = response.status();
        let text = read_capped(ct, response, MAX_RAW_BYTES).await?;
        api_error_or(status, text)
    }

    /// `getRedirectURL`: a GET that is *expected* to redirect, answering with
    /// the `Location` header.
    ///
    /// Note the failure message shape, which is Go's and is not the one `call`
    /// uses: `calling GitHub GET %s: request failed` has no method placeholder,
    /// because the method is a literal there.
    pub async fn get_redirect_url(
        &self,
        ct: &CancellationToken,
        path: &str,
    ) -> Result<String, String> {
        let failed = format!("calling GitHub GET {path}: request failed");
        let request = no_redirect_client()
            .ok_or_else(|| failed.clone())?
            .get(absolute(path, &failed)?)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json");

        let response = send(ct, request, &failed).await?;

        let status = response.status();
        if status == reqwest::StatusCode::FOUND || status == reqwest::StatusCode::MOVED_PERMANENTLY
        {
            // `Header.Get` on an absent header is `""`, and a header whose
            // value is the empty string is the same thing to Go — so both
            // reach the same sentence.
            //
            // `from_utf8_lossy` over the raw bytes rather than `to_str()`,
            // which refuses anything non-ASCII: Go's `Header.Get` hands back
            // whatever bytes arrived, so a `Location` carrying a non-ASCII
            // byte — a redirect to a signed URL through a host with a
            // percent-decoding proxy in front of it, say — succeeds there and
            // would have become `redirect response missing Location header`
            // here, which is a sentence about a header that is present.
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
                .unwrap_or_default();
            if location.is_empty() {
                return Err("redirect response missing Location header".to_string());
            }
            return Ok(location);
        }

        // Go ignores the read error here (`body, _ := io.ReadAll(...)`), so a
        // truncated body still produces the status sentence rather than
        // replacing it with `reading response`.
        let text = read_capped(ct, response, MAX_REDIRECT_ERROR_BYTES)
            .await
            .unwrap_or_default();
        Err(github_api_error(status, &text))
    }
}

/// `fmt.Errorf("calling GitHub %s %s: request failed", method, path)`.
fn request_failed(method: &Method, path: &str) -> String {
    format!("calling GitHub {method} {path}: request failed")
}

/// The URL a request to `path` goes to — or `failed`, if this port cannot build
/// the request Go builds.
///
/// # Why this exists, and why it refuses instead of correcting
///
/// Go appends `path` to `githubAPIBase`, hands the string to
/// `http.NewRequestWithContext`, and `net/http` writes `URL.RequestURI()` on
/// the wire **verbatim**. Nothing normalizes: `url.PathEscape` leaves `.` and
/// `..` alone (both are unreserved), and `net/url` does not remove dot
/// segments. So `list_issues(owner: "..", repo: "..")` asks GitHub for
/// `/repos/../../issues` and gets a 404.
///
/// `reqwest` builds every request through `url::Url::parse`, which applies
/// WHATWG dot-segment removal for http(s). The same call would leave as
/// `/issues` — on a request already carrying the user's PAT, that is *the
/// authenticated user's issues across every repository* in place of one
/// repository's. `owner`, `repo` and `workflow_id` are model-supplied and every
/// tool result carries attacker-authored GitHub content, so it is reachable
/// under prompt injection, and it applies to the write tools too. Escaping the
/// dots is not a fix — `%2E%2E` is collapsed as well.
///
/// `reqwest` offers no way to send an unnormalized target, so the faithful
/// option is to **refuse**: the model reads the sentence a transport failure
/// produces rather than the answer to a question it did not ask. Pinned in
/// `desktop/parity/github_vectors.json` as `rust_no_request` plus `rust_text`,
/// alongside the 2^53 divergence — five cases, covering all three URL builders.
///
/// # Why the whole target is compared rather than `..` scanned for
///
/// An exact comparison catches anything else `url` normalizes, now or after an
/// upgrade, instead of the one shape known today. Nothing legitimate trips it,
/// and that is a property of `gourl`'s escaping rather than luck: Go's
/// `encodePathSegment` escapes every byte in `url`'s path encode set (space,
/// `"`, `#`, `<`, `>`, `?`, `` ` ``, `{`, `}`) and its `encodeQueryComponent`
/// every byte in the query one, so there is nothing left for `url` to
/// percent-encode; existing `%XX` escapes are passed through with their case
/// intact, so Go's uppercase hex survives; and a path with no `?` compares
/// `""` against `query()`'s `None`. Host, scheme and port are not compared at
/// all, so `url`'s default-port and case normalization cannot reach this.
/// Verified against all 50 call vectors, `+`-encoded spaces and multi-byte
/// UTF-8 among them.
fn absolute(path: &str, failed: &str) -> Result<reqwest::Url, String> {
    let url =
        reqwest::Url::parse(&format!("{}{path}", api_base())).map_err(|_| failed.to_string())?;
    let (want_path, want_query) = path.split_once('?').unwrap_or((path, ""));
    if url.path() != want_path || url.query().unwrap_or("") != want_query {
        return Err(failed.to_string());
    }
    Ok(url)
}

/// `fmt.Errorf("github API error: status %d: %s", status, body)`.
fn github_api_error(status: reqwest::StatusCode, body: &str) -> String {
    format!("github API error: status {}: {body}", status.as_u16())
}

/// Go's `if resp.StatusCode < 200 || resp.StatusCode >= 300` — note it is a
/// *range*, not `!= 200`, so the `204` a workflow dispatch answers is a success
/// and a `304` would not be.
fn api_error_or(status: reqwest::StatusCode, body: String) -> Result<String, String> {
    if !(200..300).contains(&status.as_u16()) {
        return Err(github_api_error(status, &body));
    }
    Ok(body)
}

/// Sends a request, racing the run's cancellation.
///
/// `failed` is passed in rather than built here because the two call shapes
/// word it differently — see [`Client::get_redirect_url`].
async fn send(
    ct: &CancellationToken,
    request: reqwest::RequestBuilder,
    failed: &str,
) -> Result<reqwest::Response, String> {
    tokio::select! {
        // Cancellation and a transport failure produce the same sentence, and
        // that is not a shortcut: in Go a cancelled `ctx` *is* how `client.Do`
        // fails, so this is the message that reaches the model there too.
        () = ct.cancelled() => Err(failed.to_string()),
        result = request.send() => result.map_err(|_| failed.to_string()),
    }
}

/// `io.ReadAll(io.LimitReader(resp.Body, cap))`.
///
/// Streamed rather than buffered whole: the cap exists so a hostile or
/// runaway response cannot be pulled into memory, and `bytes()` would read it
/// all before there was anything to truncate.
///
/// **Lossy UTF-8**, which is the one place this is not literally Go. Go does
/// `string(respBody)` on arbitrary bytes and a Go string holds them; a Rust
/// `String` cannot, so invalid sequences become U+FFFD. Every GitHub response
/// this reaches is JSON or a diff, both UTF-8 by definition, and the
/// alternative — carrying `Vec<u8>` through to the `ContentBlock` — would only
/// move the same conversion to the end of the pipe, where `rmcp` demands a
/// `String` anyway.
pub(super) async fn read_capped(
    ct: &CancellationToken,
    response: reqwest::Response,
    cap: usize,
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
                if body.len() >= cap {
                    body.truncate(cap);
                    break;
                }
            }
            // `fmt.Errorf("reading response: %w", err)`. The cause *is*
            // interpolated here, unlike the send failure: this error describes
            // a body already in flight and cannot carry the request URL.
            Some(Err(e)) => return Err(format!("reading response: {e}")),
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// `splitCSV`: comma-separated, trimmed, empties dropped.
///
/// Returns a `Vec` that may be **empty**, and the callers must keep that
/// distinct from a non-empty one: Go's nil slice marshals as `null`, so
/// `labels: " , , "` sends `"labels":null` rather than `"labels":[]`. See
/// [`super::body::Body::set_csv`].
pub fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// The per-page clamp and the page gate, which are **identical at all nine**
/// paging tools — verified one by one against `repos.go`, `issues.go`,
/// `pulls.go`, `actions.go` and `releases.go` rather than assumed from the
/// first one.
///
/// `perPage <= 0 || perPage > 100` both fall back to 30, so a zero (the value
/// an omitted-but-required field cannot be, since every field *is* required)
/// and a 500 land in the same place. `page` is written only when positive, so
/// the default page produces no `page` key at all rather than `page=1`.
pub fn set_paging(values: &mut crate::native::gourl::Values, per_page: i64, page: i64) {
    let per_page = if per_page <= 0 || per_page > 100 {
        30
    } else {
        per_page
    };
    values.set("per_page", per_page.to_string());
    if page > 0 {
        values.set("page", page.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_csv_matches_gos_trimming() {
        assert!(split_csv("").is_empty());
        assert_eq!(split_csv("a"), ["a"]);
        assert_eq!(split_csv("a,b,c"), ["a", "b", "c"]);
        assert_eq!(split_csv("a, b , c"), ["a", "b", "c"]);
        assert!(split_csv(",,,").is_empty());
        assert_eq!(split_csv("  , a ,  "), ["a"]);
    }

    #[test]
    fn the_clamp_is_the_same_at_every_paging_tool() {
        let encode = |per_page, page| {
            let mut values = crate::native::gourl::Values::new();
            set_paging(&mut values, per_page, page);
            values.encode()
        };
        assert_eq!(encode(0, 0), "per_page=30");
        assert_eq!(encode(-5, 0), "per_page=30");
        assert_eq!(encode(101, 0), "per_page=30");
        assert_eq!(encode(100, 0), "per_page=100");
        assert_eq!(encode(50, 2), "page=2&per_page=50");
        // A page of 1 *is* written — the gate is `> 0`, not `> 1`.
        assert_eq!(encode(0, 1), "page=1&per_page=30");
    }

    /// 2xx is a range, so the `204` a workflow dispatch answers is a success.
    #[test]
    fn success_is_the_2xx_range_rather_than_200() {
        for code in [200u16, 201, 204, 299] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            assert_eq!(api_error_or(status, "body".into()), Ok("body".to_string()));
        }
        for code in [199u16, 300, 301, 404, 500] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            assert_eq!(
                api_error_or(status, "b".into()),
                Err(format!("github API error: status {code}: b"))
            );
        }
    }

    /// The dot-segment guard, at the unit rather than at the vector: what it
    /// refuses, and — the half that would bite — everything it must not.
    ///
    /// `github_vectors.json` covers this end to end for all 50 calls; this
    /// covers the shapes a *future* tool could build that no vector has yet,
    /// which is where a guard like this normally goes wrong.
    #[test]
    fn dot_segments_are_refused_and_nothing_else_is() {
        let ok = |path: &str| {
            absolute(path, "no").unwrap_or_else(|_| panic!("{path} is a legitimate path"))
        };

        // `url` resolves every one of these into a different endpoint than Go
        // calls, and escaping the dots does not help — `%2E%2E` is collapsed
        // too, so there is nothing to encode our way out of.
        for path in [
            "/repos/../../issues",
            "/repos/./x/issues",
            "/repos/%2E%2E/%2E%2E/issues",
            "/repos/o/r/..",
        ] {
            assert_eq!(
                absolute(path, "refused"),
                Err("refused".to_string()),
                "{path}"
            );
        }

        // Everything `gourl` can emit. `PathEscape` already escapes every byte
        // in `url`'s path encode set and `QueryEscape` every byte in its query
        // one, so there is nothing left for `url` to re-encode; percent-escapes
        // keep Go's uppercase hex; a space is `+` in a query and `%20` in a
        // segment; and a path with no `?` has no query on either side.
        assert_eq!(ok("/user/repos?per_page=30").path(), "/user/repos");
        assert_eq!(ok("/repos/o/r").query(), None);
        assert_eq!(
            ok("/search/code?q=repo%3Ao%2Fr+func+foo%2Bbar+~x%2Fy%26z%3D1+%2A&per_page=30").query(),
            Some("q=repo%3Ao%2Fr+func+foo%2Bbar+~x%2Fy%26z%3D1+%2A&per_page=30")
        );
        assert_eq!(
            ok("/repos/my%20org/a%2Fb").path(),
            "/repos/my%20org/a%2Fb",
            "uppercase hex survives, and so does an escaped slash"
        );
        assert_eq!(
            ok("/repos/a$&+:=@b/caf%C3%A9%3B%E6%97%A5%E6%9C%AC%E8%AA%9E%2Cx%3Fy").path(),
            "/repos/a$&+:=@b/caf%C3%A9%3B%E6%97%A5%E6%9C%AC%E8%AA%9E%2Cx%3Fy",
            "the reserved bytes a segment keeps are not re-encoded"
        );
        // A dot *inside* a segment is not a dot segment, and neither is one
        // inside a query value — only the path is resolved.
        assert_eq!(
            ok("/repos/o/r/actions/workflows/ci.yml").path(),
            "/repos/o/r/actions/workflows/ci.yml"
        );
        assert_eq!(
            ok("/repos/o/r/issues?labels=..%2F..&per_page=30").query(),
            Some("labels=..%2F..&per_page=30")
        );
    }

    /// The base is a variable so the parity suite can redirect it, and putting
    /// it back has to work — otherwise one test would leak into the next.
    #[tokio::test]
    async fn the_api_base_is_redirectable_and_restorable() {
        let _guard = api_base_lock().await;
        assert_eq!(api_base(), DEFAULT_API_BASE);
        set_api_base(Some("http://127.0.0.1:1".to_string()));
        assert_eq!(api_base(), "http://127.0.0.1:1");
        set_api_base(None);
        assert_eq!(api_base(), DEFAULT_API_BASE);
    }

    /// A distinctive token, so a leak is unmistakable in any assertion.
    const SECRET: &str = "ghp-SUPER-SECRET-PERSONAL-ACCESS-TOKEN";

    /// The whole reason the failure message drops the transport error.
    ///
    /// A `reqwest::Error`'s `Display` carries the URL it was building, and a
    /// future edit that "helpfully" interpolated the cause would put the
    /// request — and, one refactor later, a header — into text the model reads
    /// and the transcript stores. Go's wording has the same property and this
    /// asserts it rather than trusting it.
    #[tokio::test]
    async fn a_failed_call_names_neither_the_token_nor_the_cause() {
        let _guard = api_base_lock().await;
        // Port 1 on loopback: nothing listens, so the connect fails fast.
        set_api_base(Some("http://127.0.0.1:1".to_string()));
        let client = Client::new(SECRET);
        let ct = CancellationToken::new();

        let call = client
            .call(&ct, Method::GET, "/repos/o/r", None)
            .await
            .expect_err("nothing is listening");
        assert_eq!(call, "calling GitHub GET /repos/o/r: request failed");

        let raw = client
            .call_raw(&ct, Method::GET, "/repos/o/r", "text/plain")
            .await
            .expect_err("nothing is listening");
        assert_eq!(raw, "calling GitHub GET /repos/o/r: request failed");

        // `getRedirectURL`'s sentence has no method placeholder — the method is
        // a literal there — so it is a different string, not the same one.
        let redirect = client
            .get_redirect_url(&ct, "/repos/o/r/logs")
            .await
            .expect_err("nothing is listening");
        assert_eq!(
            redirect,
            "calling GitHub GET /repos/o/r/logs: request failed"
        );

        for message in [call, raw, redirect] {
            assert!(
                !message.contains(SECRET),
                "a token reached the model: {message}"
            );
        }
        set_api_base(None);
    }

    /// A cancelled call answers **Go's sentence**, and that is a port rather
    /// than a choice: in Go a cancelled `ctx` is how `client.Do` fails, so
    /// `calling GitHub %s %s: request failed` is what the model reads there
    /// too.
    #[tokio::test]
    async fn a_cancelled_call_answers_what_a_cancelled_go_call_answers() {
        let _guard = api_base_lock().await;
        set_api_base(Some("http://127.0.0.1:1".to_string()));
        let ct = CancellationToken::new();
        ct.cancel();
        assert_eq!(
            Client::new(SECRET)
                .call(&ct, Method::GET, "/user/repos", None)
                .await,
            Err("calling GitHub GET /user/repos: request failed".to_string())
        );
        set_api_base(None);
    }

    /// `io.LimitReader` truncates rather than failing, and it stops *reading* —
    /// which is why the port streams instead of buffering the body and then
    /// slicing it.
    #[tokio::test]
    async fn a_response_is_truncated_at_the_cap_rather_than_refused() {
        let _guard = api_base_lock().await;
        let oversized = MAX_JSON_BYTES + 4096;
        let app = axum::Router::new().fallback(move || async move { "x".repeat(oversized) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));

        let body = Client::new(SECRET)
            .call(&CancellationToken::new(), Method::GET, "/big", None)
            .await
            .expect("a 200 is a success however long the body is");
        assert_eq!(body.len(), MAX_JSON_BYTES);
        set_api_base(None);
    }
}

/// The `User-Agent` this module's client puts on the wire (#514).
///
/// Read off a request a loopback server received, not off the builder: a
/// `ClientBuilder`'s default headers cannot be inspected, and asserting that
/// this file *calls* `client_builder` would only restate
/// `native::http`'s own source guard.
#[cfg(test)]
mod user_agent {
    #[tokio::test]
    async fn the_github_client_sends_the_agento_user_agent() {
        let client = super::http_client().expect("a usable HTTP client");
        assert_eq!(
            crate::native::http::testing::user_agent_seen_by_a_server(client).await,
            crate::native::http::USER_AGENT,
        );
    }

    /// The no-redirect client is a *second* client, so it omits the header
    /// independently — and it is the one `get_pull_diff` and `download_*` use.
    #[tokio::test]
    async fn the_no_redirect_client_sends_it_too() {
        let client = super::no_redirect_client().expect("a usable HTTP client");
        assert_eq!(
            crate::native::http::testing::user_agent_seen_by_a_server(client).await,
            crate::native::http::USER_AGENT,
        );
    }
}

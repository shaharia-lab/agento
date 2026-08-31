//! The OAuth2-authenticated HTTP client, ported from
//! `internal/integrations/google/server.go`'s `buildHTTPClient` plus the parts of
//! `golang.org/x/oauth2` and `google.golang.org/api` that are observable on the
//! wire.
//!
//! # Google is the only one of the six that does not hand-roll HTTP
//!
//! The other five build a request with `http.NewRequest` and `json.Marshal`, so
//! the port reproduces bytes it can read in `tools.go`. Google calls the
//! **generated** client libraries (`calendar/v3`, `gmail/v1`, `drive/v3`) over an
//! `oauth2` transport, so what goes on the wire is decided by code that is not in
//! this repository. Every value below was therefore *measured* — a recording
//! server behind `option.WithEndpoint` — rather than read off a source file.
//!
//! ## What is reproduced, and what cannot be
//!
//! Reproduced, and pinned by `desktop/parity/google_vectors.json`: the method,
//! the path, the **whole** query string (`alt=json&prettyPrint=false` on every
//! call, sorted keys, the field masks, `uploadType=multipart`), the JSON request
//! bodies, the `Authorization` header, and every result and error sentence.
//!
//! Not reproducible, and pinned as divergence rather than hidden:
//!
//! - **`X-Goog-Api-Client: gl-go/1.26.5 gdcl/0.291.0`** and
//!   **`User-Agent: google-api-go-client/0.5`**. The first embeds the *Go
//!   toolchain* version and the *client library* version; no Rust build can emit
//!   `gl-go/…`, and freezing the value would break on any `go.mod` bump. This
//!   port sends neither.
//! - **The multipart boundary** `create_file` generates is random per request, so
//!   the vectors pin the *parts* — the JSON metadata part and the media part with
//!   its content type — rather than the byte stream.
//! - `Accept-Encoding: gzip`, which both sides send but neither controls.
//!
//! ## Four small divergences that are known and left standing
//!
//! Each was found by review, checked against the Go source, and judged not worth
//! the machinery — but "we did not notice" and "we decided not to" are different
//! things, so they are written down rather than left implicit:
//!
//! - **Dot segments in a path parameter.** `.` is unreserved, so
//!   [`expand_path_segment`] passes `..` through, and `reqwest::Url` then
//!   normalizes it away — `download_file` with a `file_id` of `..` requests the
//!   Drive *collection* where Go requests `/files/..`. Go never normalizes,
//!   because it expands the template after resolving it. Fixing this means not
//!   using `Url` at all, since both `parse` and `join` normalize; the input is
//!   nonsense either way and both answers are errors from Google.
//! - **`status_line` uses `canonical_reason()`** where Go reads the reason phrase
//!   off the wire, so a non-standard status or a custom phrase renders
//!   differently inside `oauth2: cannot fetch token: …`.
//! - **The `Authorization` scheme is the literal `Bearer`**, where Go title-cases
//!   `Token.TokenType`. Google always says `Bearer`.
//! - **`expires_in` must be a JSON number here.** Go's `expirationTime` goes
//!   through `json.Number`, which also accepts `"3600"` as a string; Google sends
//!   a number.
//!
//! ## The upload shape is settled only under 16 MiB
//!
//! `googleapi.DefaultUploadChunkSize` is 16 MiB, above which `PrepareUpload`
//! switches to a **resumable** upload: a different endpoint, `uploadType=resumable`,
//! an `X-Upload-Content-Type` header and a two-phase POST/PUT. This port always
//! sends `uploadType=multipart`. `create_file`'s content arrives as a JSON string
//! in a `tools/call`, so reaching 16 MiB means a 16 MiB tool argument; if that
//! ever becomes reachable, this is the thing to port.
//!
//! # The token source
//!
//! `buildHTTPClient` wraps the stored token in `oauth2.Config.TokenSource` and
//! `oauth2.NewClient`, which refreshes transparently. Measured, and reproduced
//! exactly:
//!
//! - A token refreshes when `Valid()` is false — which is `AccessToken != ""` and
//!   not expired, where **a zero expiry never expires** and the comparison
//!   carries a **10-second** `expiryDelta`. A token due in 5s refreshes; one due
//!   in an hour does not.
//! - The refresh is `POST` to `https://oauth2.googleapis.com/token`,
//!   `application/x-www-form-urlencoded`, body
//!   `client_id=…&client_secret=…&grant_type=refresh_token&refresh_token=…` —
//!   `url.Values.Encode`'s sorted keys, and the client credentials in the
//!   **body**, not a `Basic` header, because `google.Endpoint` declares
//!   `AuthStyleInParams` (measured: `AuthStyle=1`).
//! - A response with no `refresh_token` keeps the old one.
//! - **Nothing is persisted.** Go refreshes in memory for the life of the client
//!   and never writes the new token back, so a restart re-reads the stored one.
//!
//! #318 owns the OAuth *flow* and says of this: "Token refresh is shared with the
//! Google MCP server. One implementation, not two." [`TokenSource`] is that
//! implementation, kept behind a narrow interface so #318 can adopt it rather
//! than write a second.

use std::sync::OnceLock;
#[cfg(test)]
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

use tokio::sync::Mutex;

use crate::claude::CancellationToken;
use crate::native::gourl::Values;

/// `google.Endpoint.TokenURL`, measured.
pub const DEFAULT_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// The three generated clients' `BasePath`s, measured off a real service.
///
/// Note they are not one host: Gmail moved to its own, and its relative paths
/// carry `gmail/v1/` where the other two carry it in the base.
pub const CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3/";
pub const DRIVE_BASE: &str = "https://www.googleapis.com/drive/v3/";
pub const GMAIL_BASE: &str = "https://gmail.googleapis.com/";

/// Where requests go, when a test has redirected them.
///
/// One override for all four bases: `option.WithEndpoint` replaces the base and
/// leaves each generated method's *relative* reference alone, so a fake sees
/// `/calendars/primary/events`, `/gmail/v1/users/me/messages/send` and `/files`
/// — which is exactly what this reproduces. Test-only for
/// `github::client::API_BASE`'s reason, and sharper here: the override would
/// point a refresh carrying the user's `client_secret` at an arbitrary host.
#[cfg(test)]
static API_BASE: RwLock<Option<String>> = RwLock::new(None);

#[cfg(test)]
pub(super) fn set_api_base(base: Option<String>) {
    *API_BASE
        .write()
        .expect("the google API base lock is poisoned") = base;
}

#[cfg(test)]
pub(super) async fn api_base_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::const_new(());
    LOCK.lock().await
}

#[cfg(test)]
fn override_base() -> Option<String> {
    API_BASE
        .read()
        .expect("the google API base lock is poisoned")
        .clone()
}

#[cfg(not(test))]
fn override_base() -> Option<String> {
    None
}

/// Which generated client a request belongs to — and therefore which base it
/// resolves against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Api {
    Calendar,
    Drive,
    Gmail,
}

impl Api {
    fn base(self) -> String {
        if let Some(base) = override_base() {
            return base;
        }
        match self {
            Self::Calendar => CALENDAR_BASE.to_string(),
            Self::Drive => DRIVE_BASE.to_string(),
            Self::Gmail => GMAIL_BASE.to_string(),
        }
    }
}

/// `googleapi.ResolveRelative(basePath, ref)`.
///
/// A reference beginning with `/` replaces the base's whole path — which is what
/// makes Drive's upload endpoint `https://www.googleapis.com/upload/drive/v3/files`
/// rather than something under `/drive/v3/`. Anything else is appended to the
/// base, which always ends in `/`.
fn resolve_relative(base: &str, reference: &str) -> Option<reqwest::Url> {
    let base = reqwest::Url::parse(base).ok()?;
    base.join(reference).ok()
}

/// `googleapi.Expand`'s path-parameter escaping — **not** `url.PathEscape`.
///
/// The generated clients build their paths from RFC 6570 URI templates
/// (`gmail/v1/users/{userId}/messages/{id}`) and expand them with
/// `googleapi.Expand`, whose simple-string expansion percent-encodes everything
/// outside RFC 3986's *unreserved* set. `url.PathEscape` leaves the sub-delims
/// alone, so the two disagree on characters a Gmail message id or a Drive file id
/// can legitimately contain: measured, `a/b c?d&e` expands to `a%2Fb%20c%3Fd%26e`
/// where `PathEscape` gives `a%2Fb%20c%3Fd&e` — an `&` that would start a query
/// parameter.
///
/// This is `googleapi`'s rule rather than `net/url`'s, which is why it lives here
/// and not in `gourl`.
pub(super) fn expand_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `oauth2.Token`, reduced to what the transport reads.
#[derive(Clone)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    /// `None` is Go's zero `time.Time`, which **never expires**.
    pub expiry: Option<SystemTime>,
}

impl Token {
    /// `(*Token).Valid()`: a non-empty access token that is not expired.
    ///
    /// `expired()` is `Expiry.Add(-expiryDelta).Before(now)` with a **10-second**
    /// delta, and a zero expiry short-circuits to "not expired". Both halves are
    /// measured: a token due in 5 seconds refreshes, one with no expiry never
    /// does.
    fn valid(&self, now: SystemTime) -> bool {
        if self.access_token.is_empty() {
            return false;
        }
        match self.expiry {
            None => true,
            Some(expiry) => expiry
                .checked_sub(EXPIRY_DELTA)
                .is_none_or(|deadline| deadline >= now),
        }
    }
}

/// `oauth2`'s `defaultExpiryDelta`.
const EXPIRY_DELTA: Duration = Duration::from_secs(10);

/// `oauth2.Config.TokenSource` + `oauth2.NewClient`, reduced to the one thing
/// they do that is observable: hand out a valid access token, refreshing it when
/// it is not.
///
/// A `Mutex` rather than a lock-free cache because two concurrent `tools/call`s
/// on an expired token must not both refresh — Go's `reuseTokenSource` holds a
/// mutex for exactly that.
pub struct TokenSource {
    client_id: String,
    client_secret: String,
    current: Mutex<Token>,
}

impl TokenSource {
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        token: Token,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            current: Mutex::new(token),
        }
    }

    /// The access token to put in `Authorization`, refreshing first if the stored
    /// one is not `Valid()`.
    ///
    /// `failed` is the caller's sentence, because a refresh failure surfaces as
    /// the *tool's* failure in Go — `oauth2`'s transport returns it from
    /// `RoundTrip`, so the generated client wraps it exactly as it wraps any
    /// other transport error.
    async fn access_token(&self, ct: &CancellationToken) -> Result<String, String> {
        let mut current = self.current.lock().await;
        if current.valid(SystemTime::now()) {
            return Ok(current.access_token.clone());
        }
        let refreshed = self.refresh(ct, &current).await?;
        *current = refreshed;
        Ok(current.access_token.clone())
    }

    /// The refresh request, measured against `golang.org/x/oauth2`.
    async fn refresh(&self, ct: &CancellationToken, current: &Token) -> Result<Token, String> {
        // `tokenRefresher.Token` refuses **before opening a socket** when there
        // is no refresh token (`oauth2.go:274`). Reproducing the refusal is not
        // only about the sentence: without it this port POSTs the user's
        // `client_secret` to Google on a request Go never sends. A re-consent
        // without `prompt=consent` is exactly how a row ends up in this state.
        if current.refresh_token.is_empty() {
            return Err(NO_REFRESH_TOKEN.to_string());
        }

        // `url.Values.Encode()` — sorted keys, and the client credentials in the
        // **body** because `google.Endpoint` declares `AuthStyleInParams`. A
        // `Basic` header here would be a different request.
        let mut form = Values::new();
        form.set("client_id", &self.client_id);
        form.set("client_secret", &self.client_secret);
        form.set("grant_type", "refresh_token");
        form.set("refresh_token", &current.refresh_token);

        let token_url = override_base().map_or_else(
            || DEFAULT_TOKEN_URL.to_string(),
            |base| format!("{}{}", base.trim_end_matches('/'), "/token"),
        );

        let request = http_client()
            .ok_or(REFRESH_FAILED)?
            .post(token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form.encode());

        let response = tokio::select! {
            () = ct.cancelled() => return Err(REFRESH_FAILED.to_string()),
            result = request.send() => result.map_err(|_| REFRESH_FAILED.to_string())?,
        };

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|_| REFRESH_FAILED.to_string())?;

        // `oauth2/internal.doTokenRoundTrip`, reproduced in its own order —
        // which is not the obvious one. The status is *not* checked first: the
        // body is parsed first, and an `error` code in a **200** is still a
        // failure, "because some unorthodox servers respond 200 in error case".
        let failure = !(200..=299).contains(&status.as_u16());

        // One deliberate omission. `doTokenRoundTrip` has a second branch for a
        // `application/x-www-form-urlencoded` or `text/plain` response body,
        // "because some endpoints return a query string". Google's does not — it
        // is `application/json` and this port only ever talks to Google's — so
        // reproducing `url.ParseQuery` here would be unreachable code with its own
        // parity surface. A form-encoded body reaches the JSON branch instead and
        // fails to decode, which lands on the same two sentences below.
        //
        // The three decode rules every response in this port carries; see
        // `native/integrations/telegram/client.rs` for why each is needed.
        let parsed = match serde_json::from_str::<
            Option<crate::native::gojson::GoStruct<TokenResponse>>,
        >(&body)
        {
            Ok(wrapped) => wrapped.map_or_else(TokenResponse::default, |wrapped| wrapped.0),
            Err(_) if failure => {
                return Err(retrieve_error(status, &body, &TokenResponse::default()))
            }
            Err(e) => return Err(format!("oauth2: cannot parse json: {e}")),
        };

        if failure || !parsed.error.is_empty() {
            return Err(retrieve_error(status, &body, &parsed));
        }
        if parsed.access_token.is_empty() {
            return Err("oauth2: server response missing access_token".to_string());
        }

        Ok(Token {
            access_token: parsed.access_token,
            // "Don't overwrite `RefreshToken` with an empty value if this was a
            // token refreshing request" — `retrieveToken`, which substitutes the
            // one the *request* carried.
            refresh_token: if parsed.refresh_token.is_empty() {
                current.refresh_token.clone()
            } else {
                parsed.refresh_token
            },
            // `tokenJSON.expiry()` tests `!= 0`, not `> 0` — a negative
            // `expires_in` produces an expiry in the past rather than none.
            expiry: (parsed.expires_in != 0).then(|| {
                let delta = Duration::from_secs(parsed.expires_in.unsigned_abs());
                if parsed.expires_in > 0 {
                    SystemTime::now() + delta
                } else {
                    SystemTime::now() - delta
                }
            }),
        })
    }
}

/// `oauth2/internal.tokenJSON`, reduced to the fields the transport reads.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct TokenResponse {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    access_token: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    refresh_token: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    expires_in: i64,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    error: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    error_description: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    error_uri: String,
}

/// `oauth2.RetrieveError.Error()`, which has **two** forms and picks between them
/// on whether the body named an error code — not on the status.
///
/// Measured: a Google refresh refusal reads
/// `oauth2: "invalid_grant" "Token has been expired or revoked."`, not the
/// `cannot fetch token: 400 Bad Request` form the port originally had. Both are
/// reachable and both are pinned by `desktop/parity/google_vectors.json`.
///
/// `{:?}` stands in for `%q`, as it does at every other `%q` site in this port.
fn retrieve_error(status: reqwest::StatusCode, body: &str, parsed: &TokenResponse) -> String {
    if parsed.error.is_empty() {
        return format!(
            "oauth2: cannot fetch token: {}\nResponse: {body}",
            status_line(status)
        );
    }
    let mut out = format!("oauth2: {:?}", parsed.error);
    for extra in [&parsed.error_description, &parsed.error_uri] {
        if !extra.is_empty() {
            out.push_str(&format!(" {extra:?}"));
        }
    }
    out
}

/// `net/http`'s status line, which is what `%v` renders for `oauth2`'s error.
fn status_line(status: reqwest::StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("{} {reason}", status.as_u16()),
        None => format!("{}", status.as_u16()),
    }
}

/// The sentence a refresh failure carries when it never reached a response.
///
/// It names nothing: the request holds the `client_secret` and the refresh
/// token, and a `reqwest::Error`'s `Display` can carry the URL.
const REFRESH_FAILED: &str = "oauth2: cannot fetch token: request failed";

/// `tokenRefresher.Token`'s refusal when the stored token has no refresh token.
const NO_REFRESH_TOKEN: &str = "oauth2: token expired and refresh token is not set";

fn http_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // The generated clients set no timeout of their own — Go's
            // `oauth2.NewClient` builds a plain `http.Client`. One is set here
            // for the reason `desktop/CLAUDE.md` gives: a handler with no client
            // timeout is what an unbounded graceful shutdown waits on. 60s
            // matches the longest any sibling uses.
            crate::native::http::client_builder()
                .timeout(Duration::from_secs(60))
                .build()
                .ok()
        })
        .as_ref()
}

/// The two parts of a `multipart/related` upload, bundled so `post_multipart`
/// takes a body rather than three loose pieces of one.
pub struct Multipart {
    /// Already carrying `super::marshal`'s trailing newline.
    pub metadata: Vec<u8>,
    /// **Sniffed from the content**, not the tool's `mime_type` argument.
    pub media_type: String,
    pub media: Vec<u8>,
}

/// One authenticated call to a generated Google API.
///
/// Cloned per tool from the closure that captured it, so the token source is
/// shared: a refresh by one tool is seen by the next, which is what
/// `oauth2.NewClient` gives Go.
#[derive(Clone)]
pub struct Client {
    tokens: std::sync::Arc<TokenSource>,
}

impl Client {
    pub fn new(tokens: std::sync::Arc<TokenSource>) -> Self {
        Self { tokens }
    }

    /// A `GET` whose response is decoded as JSON.
    pub async fn get(
        &self,
        ct: &CancellationToken,
        api: Api,
        reference: &str,
        query: &Values,
    ) -> Result<String, String> {
        self.send(ct, api, reqwest::Method::GET, reference, query, None)
            .await
    }

    /// A `POST` carrying a JSON body.
    pub async fn post_json(
        &self,
        ct: &CancellationToken,
        api: Api,
        reference: &str,
        query: &Values,
        body: Vec<u8>,
    ) -> Result<String, String> {
        self.send(
            ct,
            api,
            reqwest::Method::POST,
            reference,
            query,
            Some(("application/json".to_string(), body)),
        )
        .await
    }

    /// A `POST` carrying a `multipart/related` upload — Drive's `create_file`.
    ///
    /// The boundary is random in Go and random here; the vectors pin the parts.
    pub async fn post_multipart(
        &self,
        ct: &CancellationToken,
        api: Api,
        reference: &str,
        query: &Values,
        upload: Multipart,
    ) -> Result<String, String> {
        let Multipart {
            metadata,
            media_type,
            media,
        } = upload;
        let boundary = multipart_boundary();
        let mut body = Vec::new();
        // `googleapi`'s `multipart/related` writer: the metadata part is
        // `application/json`, the media part carries the file's own type, and
        // each part's headers are followed by a blank line.
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Type: application/json\r\n\r\n").as_bytes(),
        );
        // `metadata` already carries `json.NewEncoder(...).Encode`'s trailing
        // newline — see `super::marshal`, which is where both body shapes get it.
        body.extend_from_slice(&metadata);
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Type: {media_type}\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(&media);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        self.send(
            ct,
            api,
            reqwest::Method::POST,
            reference,
            query,
            Some((format!("multipart/related; boundary={boundary}"), body)),
        )
        .await
    }

    async fn send(
        &self,
        ct: &CancellationToken,
        api: Api,
        method: reqwest::Method,
        reference: &str,
        query: &Values,
        body: Option<(String, Vec<u8>)>,
    ) -> Result<String, String> {
        let mut url = resolve_relative(&api.base(), reference).ok_or(TRANSPORT_FAILED)?;
        url.set_query(Some(&query.encode()));

        let token = self.tokens.access_token(ct).await?;
        let mut request = http_client()
            .ok_or(TRANSPORT_FAILED)?
            .request(method, url)
            // `oauth2`'s `SetAuthHeader`: the token type title-cased, and an
            // empty type is `Bearer`.
            .header("Authorization", format!("Bearer {token}"));
        if let Some((content_type, body)) = body {
            request = request.header("Content-Type", content_type).body(body);
        }

        let response = tokio::select! {
            () = ct.cancelled() => return Err(TRANSPORT_FAILED.to_string()),
            result = request.send() => result.map_err(|_| TRANSPORT_FAILED.to_string())?,
        };

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| format!("reading response: {e}"))?;
        if !(200..300).contains(&status.as_u16()) {
            return Err(googleapi_error(status, &text));
        }
        Ok(text)
    }
}

/// What a transport failure reads as.
///
/// The generated clients surface a `*url.Error` here, whose text carries the
/// method and URL — and the URL is a Google endpoint rather than a credential,
/// so Go's is not a leak. It is not reproducible all the same (`Get "…": dial
/// tcp …: connect: connection refused` embeds the resolver's message), so this
/// is a pinned divergence rather than an attempt.
const TRANSPORT_FAILED: &str = "the request could not be sent";

/// `googleapi.CheckResponse` + `Error.Error()`, ported statement by statement
/// rather than case by case.
///
/// It was first written as a four-case `match` over an earlier reading, and
/// review found three of the four branch conditions wrong. The shapes below are
/// all pinned by `desktop/parity/google_vectors.json`, but the reason this is now
/// a transcription of the Go function rather than a table of its outputs is that
/// the table was the bug: every case it did not list, it got wrong silently.
///
/// The three that were wrong, each a real Google response:
///
/// - **The one-error form is conditional** on the sub-error's message equalling
///   the top-level one. When they differ — the normal shape for a validation
///   error, where the top level is generic and the item specific — Go takes the
///   `More details` branch.
/// - **`error.details` was dropped entirely.** It is the modern envelope
///   (`google.rpc.ErrorInfo`, `QuotaFailure`) and it is what a real quota error
///   carries, rendered as an indented JSON block.
/// - **The raw-form test is `errors.is_empty() && message.is_empty()`**, not
///   "no code and no message" — an `errors` array alone takes the structured
///   branch, with the code defaulting to the HTTP status.
///
/// This text reaches the model inside every tool's own wrapper, so it is a parity
/// surface rather than a log line.
fn googleapi_error(status: reqwest::StatusCode, body: &str) -> String {
    let raw = || {
        format!(
            "googleapi: got HTTP response code {} with body: {body}",
            status.as_u16()
        )
    };

    // `errorReplyFromBody`: a body beginning with `[` is unwrapped as an array of
    // replies and the first is taken.
    let trimmed = body.trim_start();
    let parsed = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<crate::native::gojson::GoStruct<Envelope>>>(body)
            .ok()
            .and_then(|replies| replies.into_iter().next())
            .and_then(|wrapped| wrapped.0.error)
    } else {
        // `CheckResponse` only produces an `*Error` when the document has an
        // `error` **object**; `{"error":"invalid_grant"}` is a string and falls
        // through to the raw form, which is why a token-endpoint style error
        // reads differently from an API one.
        serde_json::from_str::<Option<crate::native::gojson::GoStruct<Envelope>>>(body)
            .ok()
            .and_then(|wrapped| wrapped.map(|wrapped| wrapped.0))
            .and_then(|envelope| envelope.error)
    };
    let Some(inner) = parsed.map(|wrapped| wrapped.0) else {
        return raw();
    };

    // `Error()`'s own first line, and the only place the raw form is reachable
    // from a document that *did* parse.
    if inner.errors.is_empty() && inner.message.is_empty() {
        return raw();
    }

    // `CheckResponse` defaults a zero code to the HTTP status.
    let code = if inner.code == 0 {
        i64::from(status.as_u16())
    } else {
        inner.code
    };

    let mut out = format!("googleapi: Error {code}: ");
    out.push_str(&inner.message);

    if !inner.details.is_empty() {
        // `json.NewEncoder(&buf).SetIndent("", "  ").Encode(details)` — Go's
        // sorted keys and HTML escaping, two-space indent, and the encoder's
        // trailing newline, which the `TrimSpace` below removes when no errors
        // section follows it.
        if let Ok(compact) = crate::native::gojson::to_vec_marshal(&inner.details) {
            let indented = crate::native::gojson::indent_compact(&compact);
            out.push_str("\nDetails:\n");
            out.push_str(&String::from_utf8_lossy(&indented));
            out.push('\n');
        }
    }

    if inner.errors.is_empty() {
        // `strings.TrimSpace`, which is what removes the encoder's newline.
        return out.trim().to_string();
    }

    let details = inner.details();
    if details.len() == 1 && details[0].message == inner.message {
        out.push_str(&format!(", {}", details[0].reason));
        return out;
    }

    // `Fprintln` — hence the newline after the colon as well as before.
    out.push_str("\nMore details:\n");
    for detail in details {
        out.push_str(&format!(
            "Reason: {}, Message: {}\n",
            detail.reason, detail.message
        ));
    }
    out
}

/// One entry of `googleapi.Error.Errors`.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct ErrorItem {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    message: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    reason: String,
}

/// `googleapi.Error`, reduced to the fields `Error()` reads.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct GoogleError {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    code: i64,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    message: String,
    /// Kept as raw values because `Error()` re-encodes them; `[]interface{}` in
    /// Go, so any shape is legal here.
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    details: Vec<serde_json::Value>,
    /// `Errors []ErrorItem` is a **value** slice in Go, so a `null` element
    /// unmarshals to the zero `ErrorItem` rather than failing the document. The
    /// `Option` is what reproduces that; `GoStruct` still refuses an array, which
    /// is the rule the rest of this port follows.
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    errors: Vec<Option<crate::native::gojson::GoStruct<ErrorItem>>>,
}

impl GoogleError {
    /// The `errors` array with every `null` collapsed to its zero value.
    fn details(&self) -> Vec<ErrorItem> {
        self.errors
            .iter()
            .map(|entry| {
                entry
                    .as_ref()
                    .map_or_else(ErrorItem::default, |wrapped| ErrorItem {
                        message: wrapped.0.message.clone(),
                        reason: wrapped.0.reason.clone(),
                    })
            })
            .collect()
    }
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct Envelope {
    error: Option<crate::native::gojson::GoStruct<GoogleError>>,
}

/// A boundary of the shape `googleapi`'s writer produces — 60 lowercase hex
/// characters.
///
/// Random per request there and here, which is why the vectors pin the parts
/// rather than the byte stream.
///
/// **The randomness has to be unpredictable, not merely varying.** Neither Go nor
/// this port checks whether the boundary occurs inside the content being
/// uploaded — Go can skip that because `googleapi` draws its boundary from
/// `crypto/rand`, so the chance is negligible. A boundary derived from the clock
/// and the thread id would not be: `create_file`'s `content` is a tool argument,
/// so a model could be steered into supplying one that terminates the upload
/// early and lets the rest of the file be read as MIME headers. Two `Uuid::v4`s
/// are 64 CSPRNG-backed hex characters, which is the repo's existing way of
/// asking for exactly this.
fn multipart_boundary() -> String {
    let mut out = uuid::Uuid::new_v4().simple().to_string();
    out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    out.truncate(60);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "GOCSPX-SUPER-SECRET-CLIENT-SECRET";
    const REFRESH: &str = "1//SUPER-SECRET-REFRESH-TOKEN";

    fn token(expiry: Option<SystemTime>) -> Token {
        Token {
            access_token: "OLD-ACCESS".to_string(),
            refresh_token: REFRESH.to_string(),
            expiry,
        }
    }

    /// `Valid()`'s three rules, each measured against `oauth2` before it was
    /// written: a zero expiry never expires, and the comparison carries a
    /// ten-second delta.
    #[test]
    fn validity_matches_oauth2s_rules() {
        let now = SystemTime::now();
        assert!(token(None).valid(now), "a zero expiry never expires");
        assert!(
            token(Some(now + Duration::from_secs(3600))).valid(now),
            "an hour out is valid"
        );
        assert!(
            !token(Some(now + Duration::from_secs(5))).valid(now),
            "inside the 10s delta is not"
        );
        assert!(
            !token(Some(now - Duration::from_secs(1))).valid(now),
            "past is not"
        );
        let mut empty = token(None);
        empty.access_token.clear();
        assert!(!empty.valid(now), "an empty access token is never valid");
    }

    /// `googleapi.Error.Error()`, all four shapes, each copied from a measured
    /// run against the real library.
    #[test]
    fn the_googleapi_error_text_matches_the_library() {
        let forbidden = reqwest::StatusCode::FORBIDDEN;
        assert_eq!(
            googleapi_error(
                forbidden,
                r#"{"error":{"code":403,"message":"Insufficient Permission","errors":[{"message":"Insufficient Permission","domain":"global","reason":"insufficientPermissions"}]}}"#
            ),
            "googleapi: Error 403: Insufficient Permission, insufficientPermissions"
        );
        assert_eq!(
            googleapi_error(forbidden, r#"{"error":{"code":404,"message":"Not Found"}}"#),
            "googleapi: Error 404: Not Found"
        );
        assert_eq!(
            googleapi_error(
                forbidden,
                r#"{"error":{"code":400,"message":"Bad Request","errors":[{"message":"m1","reason":"r1"},{"message":"m2","reason":"r2"}]}}"#
            ),
            "googleapi: Error 400: Bad Request\nMore details:\nReason: r1, Message: m1\nReason: r2, Message: m2\n"
        );
        // An `error` that is a **string** is not a Google error document, so it
        // falls through to the raw form — which is what a token-endpoint style
        // failure looks like.
        let raw =
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        assert_eq!(
            googleapi_error(forbidden, raw),
            format!("googleapi: got HTTP response code 403 with body: {raw}")
        );
        assert_eq!(
            googleapi_error(forbidden, "not json at all"),
            "googleapi: got HTTP response code 403 with body: not json at all"
        );

        // The branch the earlier four-case version got wrong: the one-error form
        // requires the sub-error's message to *equal* the top-level one, so a
        // differing message takes `More details`. Pinned in the vectors too.
        assert_eq!(
            googleapi_error(
                forbidden,
                r#"{"error":{"code":403,"message":"Insufficient Permission","errors":[{"message":"a different message","reason":"insufficientPermissions"}]}}"#
            ),
            "googleapi: Error 403: Insufficient Permission\nMore details:\nReason: insufficientPermissions, Message: a different message\n"
        );
        // `errors` alone is enough to take the structured branch, and the code
        // defaults to the HTTP status.
        assert_eq!(
            googleapi_error(
                forbidden,
                r#"{"error":{"errors":[{"reason":"r","message":"m"}]}}"#
            ),
            "googleapi: Error 403: \nMore details:\nReason: r, Message: m\n"
        );
        // `Errors` is a value slice, so a null element is the zero item.
        assert_eq!(
            googleapi_error(
                forbidden,
                r#"{"error":{"code":403,"message":"Denied","errors":[null]}}"#
            ),
            "googleapi: Error 403: Denied\nMore details:\nReason: , Message: \n"
        );
        // …but an array is still not a struct, which is what keeps `GoStruct`
        // meaningful here: Go fails the document and falls to the raw form.
        let arrayed = r#"{"error":{"code":403,"message":"Denied","errors":[[]]}}"#;
        assert_eq!(
            googleapi_error(forbidden, arrayed),
            format!("googleapi: got HTTP response code 403 with body: {arrayed}")
        );
        // A body beginning `[` is unwrapped by `errorReplyFromBody`.
        assert_eq!(
            googleapi_error(
                forbidden,
                r#"[{"error":{"code":403,"message":"from an array"}}]"#
            ),
            "googleapi: Error 403: from an array"
        );
        // `details` renders as an indented block, and the encoder's trailing
        // newline is trimmed when nothing follows it.
        assert_eq!(
            googleapi_error(
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":{"code":429,"message":"Quota exceeded","details":[{"reason":"RATE_LIMIT"}]}}"#
            ),
            "googleapi: Error 429: Quota exceeded\nDetails:\n[\n  {\n    \"reason\": \"RATE_LIMIT\"\n  }\n]"
        );
    }

    /// `ResolveRelative`: an absolute reference replaces the base's path, which
    /// is what puts Drive's upload on `/upload/drive/v3/files`.
    #[test]
    fn resolve_relative_matches_googleapis_rule() {
        assert_eq!(
            resolve_relative(CALENDAR_BASE, "calendars/primary/events")
                .expect("resolves")
                .as_str(),
            "https://www.googleapis.com/calendar/v3/calendars/primary/events"
        );
        assert_eq!(
            resolve_relative(DRIVE_BASE, "files")
                .expect("resolves")
                .as_str(),
            "https://www.googleapis.com/drive/v3/files"
        );
        assert_eq!(
            resolve_relative(DRIVE_BASE, "/upload/drive/v3/files")
                .expect("resolves")
                .as_str(),
            "https://www.googleapis.com/upload/drive/v3/files",
            "an absolute reference replaces the base's whole path"
        );
        assert_eq!(
            resolve_relative(GMAIL_BASE, "gmail/v1/users/me/messages/send")
                .expect("resolves")
                .as_str(),
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/send"
        );
    }

    /// A refresh that never reaches a response names nothing: the request body
    /// holds both the `client_secret` and the refresh token.
    #[tokio::test]
    async fn a_failed_refresh_names_neither_secret() {
        let _guard = api_base_lock().await;
        set_api_base(Some("http://127.0.0.1:1".to_string()));

        let source = TokenSource::new("CID", SECRET, token(Some(SystemTime::now())));
        let err = source
            .access_token(&CancellationToken::new())
            .await
            .expect_err("nothing is listening");
        assert_eq!(err, REFRESH_FAILED);
        assert!(!err.contains(SECRET));
        assert!(!err.contains(REFRESH));

        set_api_base(None);
    }

    /// A valid token is handed back without a request being made at all — which
    /// is what stops every call refreshing.
    #[tokio::test]
    async fn a_valid_token_is_reused_without_a_refresh() {
        let _guard = api_base_lock().await;
        // Port 1: a refresh would fail, so reaching one is observable.
        set_api_base(Some("http://127.0.0.1:1".to_string()));

        let source = TokenSource::new(
            "CID",
            SECRET,
            token(Some(SystemTime::now() + Duration::from_secs(3600))),
        );
        assert_eq!(
            source
                .access_token(&CancellationToken::new())
                .await
                .expect("valid tokens need no refresh"),
            "OLD-ACCESS"
        );

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
    async fn the_google_client_sends_the_agento_user_agent() {
        let client = super::http_client().expect("a usable HTTP client");
        assert_eq!(
            crate::native::http::testing::user_agent_seen_by_a_server(client).await,
            crate::native::http::USER_AGENT,
        );
    }
}

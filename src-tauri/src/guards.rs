//! `internal/server/guards.go`, applied at the proxy (#329), **plus this
//! build's own bearer token** (#400).
//!
//! Three checks now, and only the first two are Go's. Go scopes both of those to
//! `r.Route("/api")`, and the third is scoped identically:
//!
//! - **`requireJSONContentType`.** A cross-origin `POST` carrying `text/plain`
//!   is a CORS *simple request*: the browser sends it with **no preflight**, and
//!   the side effect lands even though the attacker cannot read the response.
//!   The handlers decode JSON without checking the type, so requiring that
//!   content type is the only thing forcing a preflight — which same-origin CORS
//!   then refuses.
//! - **`validateHost`.** DNS rebinding makes an attacker's domain *same-origin*
//!   as far as the browser is concerned, at which point CORS stops applying at
//!   all. A loopback bind does not help, for the same reason: the browser is
//!   already inside.
//! - **The bearer token** (#400). Both of the above are *browser*-shaped, and
//!   neither is an obstacle to a caller that simply speaks HTTP:
//!   `curl -H 'Content-Type: application/json' http://127.0.0.1:<port>/api/agents`
//!   passes both trivially, and this API can create an agent with
//!   `permission_mode: bypass` and run it — arbitrary command execution. The
//!   ephemeral port is obscurity only; `/proc/net/tcp` says which one.
//!
//! # This build **does** authenticate, and that is a change
//!
//! This module used to open "Agento ships without authentication on purpose —
//! it is a single-user desktop app". #246 recorded the same decision and called
//! it sound. It was, **for the Go server as a web app**, where the threat model
//! was the browser and the LAN. It is not the whole story here: loopback
//! separates *hosts*, not *processes* or *users*, so on a multi-user machine any
//! other account reaches this port, as does any sandboxed or lower-privilege
//! process that can open a socket but cannot read `~/.agento`.
//!
//! It is defence in depth rather than a new boundary — a process already running
//! as this user can read `agento.db` and `~/.claude` directly — and its real
//! value is that the two halves of this binary now agree: every in-process MCP
//! server has required a token since #282 ([`crate::claude::mcp`]), for
//! precisely this reason and in precisely these words, while the far more
//! powerful API server did not.
//!
//! **A 401 is a status Go never answers**, so it is a deliberate divergence from
//! the ported surface rather than a reproduction of anything.
//!
//! # Why the proxy, and why this is not belt-and-braces
//!
//! `proxy.rs` checked `route_is_native` and, on a match, handed the request
//! straight to `native::serve` — no `Host` check, no `Content-Type` check. Only
//! requests that *forwarded* reached `guards.go`. So the guards' coverage shrank
//! with every endpoint the port claimed, which inverts the seam's rule that a
//! ported route can only be as broken as an unported one. That is what this
//! module fixes, and it is why the check runs **before** the `route_is_native`
//! branch rather than inside either half.
//!
//! For the content type the sidecar's own copy is then a second line. **For the
//! `Host` it is not**: [`crate::proxy::forward`] rewrites the header to the
//! upstream authority — which is what makes a proxied request indistinguishable
//! from a direct same-origin one, and is required for the sidecar to answer at
//! all — so `validateHost` upstream has never seen the browser's `Host`. This is
//! the only place it can be checked.
//!
//! # What is deliberately *not* guarded
//!
//! `POST /webhooks/telegram/{id}` is mounted at the root, arrives from
//! Telegram's servers with a foreign `Host`, and is authenticated by its own
//! secret token; a global guard would break inbound triggers. `/health` and
//! `/metrics` are likewise untouched, and the SPA is not an API path at all.
//! Same scoping as Go's.
//!
//! The SPA document and its embedded assets are the reason the token needs no
//! special case: a page cannot put a header on its own navigation request, and
//! those bytes hold no secrets. Everything the *page then does* is `/api`, which
//! its own `fetch` calls can and do authenticate.

use std::net::IpAddr;
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode, Uri};

/// `internal/server/uploadPath` — the one route that legitimately posts
/// multipart.
const UPLOAD_PATH: &str = "/api/uploads";

/// The prefix the guards cover, matching Go's `r.Route("/api", …)`.
const API_PREFIX: &str = "/api";

/// A refusal: the status and the exact body message Go writes.
///
/// A rejected request is an answer like any other, so these are byte-compared
/// against `writeGuardError`'s output rather than being paraphrased.
pub type Rejection = (StatusCode, &'static str);

/// `writeGuardError(w, StatusForbidden, …)`.
const HOST_REJECTION: Rejection = (
    StatusCode::FORBIDDEN,
    "request Host is not one this server is served under",
);

/// The two 415s, which carry **different** text in Go: an unparseable or absent
/// header is "required", a parseable but wrong one is "must be".
const CONTENT_TYPE_MISSING: Rejection = (
    StatusCode::UNSUPPORTED_MEDIA_TYPE,
    "a Content-Type of application/json is required",
);
const CONTENT_TYPE_WRONG: Rejection = (
    StatusCode::UNSUPPORTED_MEDIA_TYPE,
    "Content-Type must be application/json",
);

/// The 401 (#400). Go has no counterpart, so this wording is this build's own.
///
/// It names the scheme deliberately: the only clients are the app's own webview
/// and a developer holding the dev token file, and both benefit from the answer
/// saying what was missing.
const UNAUTHORIZED: Rejection = (
    StatusCode::UNAUTHORIZED,
    "a valid Authorization: Bearer token is required",
);

/// The one credential scheme accepted, spelled exactly as
/// [`crate::claude::mcp`] spells it.
const BEARER_PREFIX: &str = "Bearer ";

/// The token minted for this launch, set once during `lib.rs`'s `setup`.
///
/// A `OnceLock` rather than Tauri state because of who needs to read it:
/// [`reject`] is called from `proxy::dispatch`, a plain router function with no
/// state extractor, so there is no `tauri::State` to take. This is the shape
/// `native::scan::state`, `native::chat::live` and `native::integrations::registry`
/// already use for process-wide values.
static API_TOKEN: OnceLock<String> = OnceLock::new();

/// Install this launch's token, returning it.
///
/// Idempotent by construction: a second call returns the first token rather than
/// replacing it, which is what lets the tests seed one without racing each other
/// (a `cargo test` binary runs them in parallel against this one static).
pub fn set_api_token(token: String) -> &'static str {
    API_TOKEN.get_or_init(|| token).as_str()
}

/// This launch's token, or `None` before `setup` has installed one.
///
/// **An empty token reads as "none installed", and that is a safety property
/// rather than tidiness.** `token_rejection` strips the scheme and compares what
/// is left, so a request carrying *no* `Authorization` header at all presents
/// `""` — which against an empty expected token is an exact match, and every
/// unauthenticated request would be served. Nothing can reach that today (a v4
/// UUID is 32 hex characters), but the guard must not be one careless
/// `set_api_token(String::new())` away from being open, and the failure would be
/// silent in exactly the way this module exists to prevent.
pub fn api_token() -> Option<&'static str> {
    API_TOKEN
        .get()
        .map(String::as_str)
        .filter(|token| !token.is_empty())
}

/// Why a request must be refused, or `None` to let it through.
///
/// Order mirrors `server.go`'s `r.Use(s.validateHost)` then
/// `r.Use(requireJSONContentType)`: chi runs them outermost-first, so a request
/// that fails both is answered 403.
///
/// The token sits **between** them, and both halves of that placement are
/// deliberate. After `Host`, so a request failing both still reads 403 — the
/// rebinding answer outranks the credential one, and a browser that has been
/// pointed here by DNS should learn nothing further. Before `Content-Type`, so
/// an unauthenticated caller is not told the content-type rule; authentication
/// precedes request-shape validation.
pub fn reject(req: &Request<Body>) -> Option<Rejection> {
    // The **raw** request target, as `proxy::handle` uses for `is_api_path` and
    // for the same reason: reaching a divergence would need a percent-escape
    // inside the prefix itself (`/%61pi/agents`), which no client sends. It is
    // also why this runs before the `gourl::route_path` resolution — chi's path
    // is not what decides whether a request is inside `/api`, and a target with
    // no route path at all still must not slip past the `Host` check.
    let path = req.uri().path();
    if !is_guarded(path) {
        return None;
    }

    if !host_allowed(&request_host(req.headers(), req.uri())) {
        return Some(HOST_REJECTION);
    }
    if let Some(rejection) = token_rejection(req.headers(), api_token()) {
        return Some(rejection);
    }
    content_type_rejection(req.method(), path, req.headers())
}

/// The bearer check (#400).
///
/// **It applies to every method**, unlike `requireJSONContentType`, which is an
/// allowlist over the state-changing four. A `GET` is what reads chat
/// transcripts, agent system prompts and the integration list, so exempting
/// reads would leave most of what is worth stealing reachable.
///
/// `expected` is a parameter rather than a read of [`API_TOKEN`] so the
/// fail-closed branch is testable: a `OnceLock` cannot be un-set, so a test that
/// wanted to observe "no token installed" could not arrange it otherwise.
///
/// **No token installed refuses everything.** That is the safe direction: the
/// only way to reach it is a `setup` that failed before minting, and answering
/// an unauthenticated request in that state would be a listener with no
/// credential at all. The scheme match is exact, as `mcp.rs`'s is — the only
/// clients are this app's own webview and the Vite dev proxy, and both spell it
/// `Bearer `.
fn token_rejection(headers: &HeaderMap, expected: Option<&str>) -> Option<Rejection> {
    let Some(expected) = expected else {
        return Some(UNAUTHORIZED);
    };
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .strip_prefix(BEARER_PREFIX)
        .unwrap_or_default();

    // Shared with the MCP servers rather than reimplemented — one constant-time
    // comparison in the tree is one to get right. The dependency runs app → SDK,
    // which is the only direction available: `claude/` is a port of an external
    // SDK and imports nothing from the rest of this crate.
    if crate::claude::mcp::credentials_match(presented, expected) {
        None
    } else {
        Some(UNAUTHORIZED)
    }
}

/// Whether the guards apply to this path.
fn is_guarded(path: &str) -> bool {
    path == API_PREFIX || path.starts_with("/api/")
}

/// `r.Host`.
///
/// `net/http` fills it from the `Host` header on HTTP/1.1 and from `:authority`
/// on HTTP/2, and `axum::serve` speaks both, so the same fallback is needed
/// here — an h2 request whose authority lived only on the URI would otherwise
/// read as a body-less `Host` and be refused.
fn request_host(headers: &HeaderMap, uri: &Uri) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| uri.authority().map(|a| a.as_str().to_owned()))
        .unwrap_or_default()
}

/// `Server.hostAllowed`, minus the two branches the desktop proxy cannot have.
///
/// Go admits three things beyond loopback: `localhost`, the configured
/// `PublicURL`'s host, and — for a deliberately non-loopback `AGENTO_BIND` — a
/// bare IP literal. **The proxy has neither a configurable bind nor a public
/// URL**: `proxy::serve` binds `127.0.0.1` unconditionally and the window is
/// navigated to `http://127.0.0.1:<port>`, while the dev build reaches it
/// through Vite with `changeOrigin: false`, so the browser's `Host` is
/// `localhost:1420`. Reproducing either branch would widen the guard past
/// anything this app can be reached at, which is the one direction a guard must
/// not move in.
fn host_allowed(raw_host: &str) -> bool {
    if raw_host.is_empty() {
        // HTTP/1.0 with no Host. Nothing legitimate reaches the API this way.
        return false;
    }
    let host = host_of(raw_host);
    if host == "localhost" {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `net.SplitHostPort` then `strings.Trim(host, "[]")` and a lowercase.
///
/// Go keeps the whole value when `SplitHostPort` errors, which is what makes a
/// port-less `Host` work — and, less obviously, a bracket-less bare IPv6
/// literal: `::1` is "too many colons" to `SplitHostPort`, so it survives to
/// `ParseIP`. The arms below are that behaviour, not a simplification of it.
fn host_of(raw_host: &str) -> String {
    let raw = raw_host.trim();
    let host = if let Some(rest) = raw.strip_prefix('[') {
        // `[::1]:8991` and `[::1]`.
        rest.split(']').next().unwrap_or(rest)
    } else if raw.matches(':').count() == 1 {
        raw.split(':').next().unwrap_or(raw)
    } else {
        raw
    };
    host.trim_matches(['[', ']']).to_ascii_lowercase()
}

/// `requireJSONContentType`.
fn content_type_rejection(method: &Method, path: &str, headers: &HeaderMap) -> Option<Rejection> {
    if !is_state_changing(method) {
        return None;
    }

    let raw = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    // `mime.ParseMediaType` returning an error covers both the absent header
    // and a malformed one, and Go answers the same 415 to each.
    //
    // **A body-less request is not exempt**, however the root `CLAUDE.md` used
    // to read. Several state-changing endpoints take no body at all —
    // `/chats/{id}/stop`, `/webhook/regenerate-secret`, `/duplicate`,
    // `/claude-sessions/refresh` — and a cross-origin `POST` with no body and no
    // `Content-Type` is *itself* a simple request, so exempting them would leave
    // exactly the hole this exists to close. `guards_test.go` pins it
    // ("a body-less DELETE is refused without the header"), and the UI sends the
    // header on every request regardless of body.
    let Some(media_type) = parse_media_type(raw) else {
        return Some(CONTENT_TYPE_MISSING);
    };
    if media_type == "application/json" {
        return None;
    }
    // multipart/form-data is itself a CORS-simple type, so it is admitted only
    // for the one endpoint that legitimately posts it. Admitting it everywhere
    // would leave every other handler reachable cross-origin, relying on nothing
    // but a JSON decode failing to prevent the side effect.
    if media_type == "multipart/form-data" && path == UPLOAD_PATH {
        return None;
    }
    Some(CONTENT_TYPE_WRONG)
}

/// `isStateChanging` — an allowlist, so `GET`, `HEAD` and `OPTIONS` pass with no
/// header at all.
fn is_state_changing(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// `mime.ParseMediaType`, as far as accept-versus-error goes.
///
/// Only the media type is returned, lowercased. Go additionally lowercases the
/// parameter names, deduplicates them, unescapes quoted values and decodes
/// RFC 2231 continuations — none of which anything on this path reads. What is
/// reproduced is the *shape*, because a malformed parameter list is an **error**
/// to Go and an error here is a 415: admitting `application/json; charset` where
/// Go refuses it would be an over-accept on a native write route, which is the
/// one direction the port must not move in.
fn parse_media_type(raw: &str) -> Option<String> {
    let (media, params) = match raw.find(';') {
        Some(i) => (&raw[..i], &raw[i + 1..]),
        None => (raw, ""),
    };
    let media = media.trim().to_ascii_lowercase();
    let (kind, subtype) = media.split_once('/')?;
    if !is_token(kind) || !is_token(subtype) {
        return None;
    }
    parameters_are_well_formed(params).then_some(media)
}

/// `;`-separated `attribute=value`, where a value is a token or a quoted string.
/// A trailing `;` is tolerated, as it is by Go.
fn parameters_are_well_formed(params: &str) -> bool {
    let mut rest = params;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return true;
        }
        let Some((attribute, after)) = rest.split_once('=') else {
            return false;
        };
        if !is_token(attribute.trim()) {
            return false;
        }
        let after = after.trim_start();
        let tail = match after.strip_prefix('"') {
            Some(quoted) => match quoted_string_end(quoted) {
                Some(end) => &quoted[end + 1..],
                None => return false,
            },
            None => {
                let stop = after.find(';').unwrap_or(after.len());
                let (value, tail) = after.split_at(stop);
                if !is_token(value.trim()) {
                    return false;
                }
                tail
            }
        };
        let tail = tail.trim_start();
        match tail.strip_prefix(';') {
            Some(next) => rest = next,
            None => return tail.is_empty(),
        }
    }
}

/// The byte offset of the closing quote, honouring `\` escapes.
fn quoted_string_end(quoted: &str) -> Option<usize> {
    let mut chars = quoted.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next()?;
            }
            '"' => return Some(i),
            _ => {}
        }
    }
    None
}

/// A non-empty RFC 2045 token: ASCII printable, minus space, CTLs and
/// `tspecials`.
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_graphic()
                && !matches!(
                    b,
                    b'(' | b')'
                        | b'<'
                        | b'>'
                        | b'@'
                        | b','
                        | b';'
                        | b':'
                        | b'\\'
                        | b'"'
                        | b'/'
                        | b'['
                        | b']'
                        | b'?'
                        | b'='
                )
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The token every test in this binary authenticates with.
    ///
    /// [`set_api_token`] is `get_or_init`, so the first caller wins and every
    /// later one — in this module or in `proxy`'s tests, which share the
    /// process — sees the same value. That is what makes a seeded token safe
    /// under `cargo test`'s parallelism.
    pub(crate) fn seeded_token() -> &'static str {
        set_api_token("test-token".to_string())
    }

    fn request(method: Method, path: &str, host: &str, content_type: &str) -> Request<Body> {
        authed_request(method, path, host, content_type, Some(seeded_token()))
    }

    fn authed_request(
        method: Method,
        path: &str,
        host: &str,
        content_type: &str,
        token: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(path);
        if !host.is_empty() {
            builder = builder.header(header::HOST, host);
        }
        if !content_type.is_empty() {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).expect("request")
    }

    /// The case #329 exists for: before this, a claimed route was answered by
    /// `native::serve` without either guard, so `POST /api/claude-settings` with
    /// `Content-Type: text/plain` — a CORS simple request, sent with no
    /// preflight from any page the user has open — wrote `~/.claude/settings.json`
    /// and its `hooks` key.
    #[test]
    fn a_simple_request_shaped_post_is_refused() {
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/claude-settings/profiles",
                "localhost:8991",
                "text/plain"
            )),
            Some(CONTENT_TYPE_WRONG)
        );
    }

    #[test]
    fn the_state_changing_methods_are_guarded_and_the_rest_are_not() {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(
                reject(&request(method.clone(), "/api/agents", "localhost", "")),
                Some(CONTENT_TYPE_MISSING),
                "{method} should require the header"
            );
            assert_eq!(
                reject(&request(
                    method.clone(),
                    "/api/agents",
                    "localhost",
                    "application/json"
                )),
                None
            );
        }
        // `isStateChanging` is an allowlist, so these need no header at all —
        // the claim `desktop/CLAUDE.md` carried until #332.
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(
                reject(&request(method.clone(), "/api/agents", "localhost", "")),
                None,
                "{method} should be untouched"
            );
        }
    }

    /// Not an oversight: a cross-origin `POST` with neither body nor
    /// `Content-Type` is itself a simple request, and several state-changing
    /// endpoints take no body. Pinned because the root `CLAUDE.md` and #329's
    /// own text both said body-less requests pass, and `guards_test.go` says
    /// otherwise.
    #[test]
    fn a_body_less_request_is_not_exempt() {
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/claude-sessions/refresh",
                "localhost",
                ""
            )),
            Some(CONTENT_TYPE_MISSING)
        );
        assert_eq!(
            reject(&request(Method::DELETE, "/api/agents/a", "localhost", "")),
            Some(CONTENT_TYPE_MISSING)
        );
        // …and with the header it is admitted, which is what the UI sends.
        assert_eq!(
            reject(&request(
                Method::DELETE,
                "/api/agents/a",
                "localhost",
                "application/json"
            )),
            None
        );
    }

    #[test]
    fn multipart_is_admitted_only_on_the_upload_route() {
        assert_eq!(
            reject(&request(
                Method::POST,
                UPLOAD_PATH,
                "localhost",
                "multipart/form-data; boundary=x"
            )),
            None
        );
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/agents",
                "localhost",
                "multipart/form-data; boundary=x"
            )),
            Some(CONTENT_TYPE_WRONG)
        );
    }

    #[test]
    fn the_charset_parameter_is_tolerated_and_a_malformed_one_is_not() {
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/agents",
                "localhost",
                "application/json; charset=utf-8"
            )),
            None
        );
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/agents",
                "localhost",
                "Application/JSON"
            )),
            None,
            "the media type is compared case-insensitively"
        );
        // `mime.ParseMediaType` errors on a parameter with no value, and Go
        // answers the *missing* message for a parse error rather than the wrong
        // one.
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/agents",
                "localhost",
                "application/json; charset"
            )),
            Some(CONTENT_TYPE_MISSING)
        );
        assert_eq!(
            reject(&request(Method::POST, "/api/agents", "localhost", "json")),
            Some(CONTENT_TYPE_MISSING)
        );
    }

    /// DNS rebinding makes an attacker's domain same-origin, so CORS stops
    /// applying entirely — and unlike the content type, the sidecar's own copy
    /// of this check cannot help, because `proxy::forward` rewrites the `Host`
    /// to the upstream authority before it gets there.
    #[test]
    fn only_a_host_the_proxy_is_served_under_is_admitted() {
        for host in [
            "localhost",
            "localhost:8991",
            "LOCALHOST:8991",
            "127.0.0.1:1420",
            "127.0.0.1",
            "[::1]:8991",
            "::1",
            "127.5.5.5",
        ] {
            assert_eq!(
                reject(&request(Method::GET, "/api/agents", host, "")),
                None,
                "{host} should be admitted"
            );
        }
        for host in [
            "attacker.example.com",
            "attacker.example.com:8991",
            "agento.example.com",
            "192.168.1.10:8991",
            "",
        ] {
            assert_eq!(
                reject(&request(Method::GET, "/api/agents", host, "")),
                Some(HOST_REJECTION),
                "{host} should be refused"
            );
        }
    }

    /// `validateHost` is registered before `requireJSONContentType`, and chi
    /// runs middleware outermost-first, so a request failing both is a 403.
    #[test]
    fn the_host_check_runs_first() {
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/agents",
                "attacker.example.com",
                "text/plain"
            )),
            Some(HOST_REJECTION)
        );
    }

    /// All three guards are scoped to `/api` deliberately. The Telegram webhook
    /// is mounted at the root, arrives with a foreign `Host` and is
    /// authenticated by its own secret token; a global guard would break inbound
    /// triggers in production.
    ///
    /// Deliberately sends **no** token, which is what makes this the token's
    /// exemption test as well as the other two guards': the SPA document,
    /// `/health` and the webhook all reach the server without one, and a token
    /// check that had leaked outside `/api` would fail here.
    #[test]
    fn nothing_outside_api_is_guarded() {
        for path in [
            "/webhooks/telegram/1",
            "/health",
            "/metrics",
            "/",
            "/index.html",
            "/apiary",
        ] {
            assert_eq!(
                reject(&authed_request(
                    Method::POST,
                    path,
                    "api.telegram.org",
                    "application/x-www-form-urlencoded",
                    None
                )),
                None,
                "{path} should not be guarded"
            );
        }
        // The prefix itself is, and so is everything under it.
        assert_eq!(
            reject(&request(Method::POST, "/api", "attacker.example", "")),
            Some(HOST_REJECTION)
        );
    }

    /// The case #400 exists for, and the one every other guard passes: `curl`
    /// sends a loopback `Host` and sets its own `Content-Type`, so before the
    /// token the two Go guards admitted it — onto an API that can create a
    /// `bypass`-permission agent and run it.
    #[test]
    fn an_api_request_without_a_token_is_refused() {
        assert_eq!(
            reject(&authed_request(
                Method::POST,
                "/api/agents",
                "127.0.0.1:8991",
                "application/json",
                None
            )),
            Some(UNAUTHORIZED)
        );
    }

    #[test]
    fn a_wrong_token_is_refused_and_the_right_one_is_served() {
        let token = seeded_token();
        for wrong in [
            "",
            "not-the-token",
            // A prefix and an extension of the real token: the comparison is
            // length-checked before the byte fold, so neither is admitted.
            &token[..token.len() - 1],
            &format!("{token}x"),
        ] {
            assert_eq!(
                reject(&authed_request(
                    Method::GET,
                    "/api/agents",
                    "localhost",
                    "",
                    Some(wrong)
                )),
                Some(UNAUTHORIZED),
                "{wrong:?} should be refused"
            );
        }
        assert_eq!(
            reject(&authed_request(
                Method::GET,
                "/api/agents",
                "localhost",
                "",
                Some(token)
            )),
            None
        );
    }

    /// **Reads are guarded too**, unlike `requireJSONContentType`, which is an
    /// allowlist over the state-changing four. A `GET` is what returns chat
    /// transcripts, agent system prompts and the integration list.
    #[test]
    fn every_method_needs_the_token_not_only_the_state_changing_ones() {
        for method in [
            Method::GET,
            Method::HEAD,
            Method::OPTIONS,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ] {
            assert_eq!(
                reject(&authed_request(
                    method.clone(),
                    "/api/agents",
                    "localhost",
                    "application/json",
                    None
                )),
                Some(UNAUTHORIZED),
                "{method} should require the token"
            );
        }
    }

    /// A request failing the `Host` check **and** carrying no token reads 403,
    /// not 401: the rebinding answer outranks the credential one.
    #[test]
    fn the_host_check_still_runs_before_the_token_check() {
        assert_eq!(
            reject(&authed_request(
                Method::POST,
                "/api/agents",
                "attacker.example.com",
                "application/json",
                None
            )),
            Some(HOST_REJECTION)
        );
    }

    /// ...and the token check runs before the content-type one, so an
    /// unauthenticated caller is not told the content-type rule.
    #[test]
    fn the_token_check_runs_before_the_content_type_check() {
        assert_eq!(
            reject(&authed_request(
                Method::POST,
                "/api/agents",
                "localhost",
                "text/plain",
                None
            )),
            Some(UNAUTHORIZED)
        );
        // With a token, the same request is the 415 it was before #400.
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/agents",
                "localhost",
                "text/plain"
            )),
            Some(CONTENT_TYPE_WRONG)
        );
    }

    /// No token installed refuses everything.
    ///
    /// Only reachable through a `setup` that failed before minting, and the safe
    /// direction: the alternative is a listener with no credential at all.
    /// Tested through the parameter because a `OnceLock` cannot be un-set.
    #[test]
    fn no_installed_token_fails_closed() {
        let headers = HeaderMap::new();
        assert_eq!(token_rejection(&headers, None), Some(UNAUTHORIZED));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer anything".parse().expect("hv"),
        );
        assert_eq!(token_rejection(&headers, None), Some(UNAUTHORIZED));
    }

    /// An **empty** installed token must not authenticate the request that sends
    /// no credential at all.
    ///
    /// Both sides reduce to `""` — a header-less request presents `""` after the
    /// scheme strip — so a constant-time compare of the two is a match, and the
    /// guard would be wide open while looking installed. [`api_token`] maps
    /// empty to `None` for this reason; without that filter this test fails,
    /// which is the whole point of having it.
    #[test]
    fn an_empty_token_is_not_a_credential() {
        assert_eq!(
            token_rejection(&HeaderMap::new(), Some("")),
            None,
            "the raw comparison matches, which is exactly why api_token() must never return it"
        );

        // Seed **first**, and deliberately so. `set_api_token` is `get_or_init`
        // over a process-wide static, so calling it with an empty string before
        // anything else had installed a token would install one — poisoning
        // every other test in this binary, whichever order they happen to run
        // in. Seeding here makes the assertion about the accessor rather than
        // about which test won the race.
        let token = seeded_token();
        assert_eq!(
            set_api_token(String::new()),
            token,
            "a later set must not replace the installed token"
        );
        assert!(!api_token().unwrap_or_default().is_empty());
        assert_eq!(
            reject(&authed_request(
                Method::GET,
                "/api/agents",
                "localhost",
                "",
                None
            )),
            Some(UNAUTHORIZED)
        );
    }

    /// The scheme is matched exactly, as `mcp.rs` matches it. The only clients
    /// are this app's webview and the Vite dev proxy, and both send `Bearer `.
    #[test]
    fn the_credential_must_carry_the_bearer_scheme() {
        let token = seeded_token();
        for value in [
            token.to_string(),
            format!("bearer {token}"),
            format!("Basic {token}"),
            format!("Bearer  {token}"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value.parse().expect("hv"));
            assert_eq!(
                token_rejection(&headers, Some(token)),
                Some(UNAUTHORIZED),
                "{value:?} should be refused"
            );
        }
    }

    /// Two launches must not share a token — the `mcp.rs` property, applied to
    /// the API server. `set_api_token` is `get_or_init` within one process, so
    /// what is asserted is the generation: the value handed to it is fresh per
    /// call, which is what makes each launch's token independent.
    #[test]
    fn a_freshly_minted_token_is_never_the_previous_one() {
        let mint = || uuid::Uuid::new_v4().simple().to_string();
        let first = mint();
        let second = mint();
        assert_ne!(first, second);
        assert_eq!(first.len(), 32, "128 bits of hex, 122 of them random");
    }
}

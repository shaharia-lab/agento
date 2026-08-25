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
//! - **The signed bearer token** (#400, #405). Both of the above are
//!   *browser*-shaped, and neither is an obstacle to a caller that simply speaks
//!   HTTP:
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
//! the ported surface rather than a reproduction of anything. #405 adds a
//! second, on the same terms: a **403 for insufficient scope**.
//!
//! # What the credential is, since #405
//!
//! #400's credential was an opaque random string, minted per launch and
//! compared byte for byte. It authenticated exactly one client — the app's own
//! webview — and could express nothing else: no identity, no expiry, no scope,
//! and no revocation short of restarting the app.
//!
//! It is now an **EdDSA (Ed25519) JWT signed by a per-install keypair**
//! ([`crate::native::security`]). The *guard* is unchanged in every respect that
//! matters here — one check, `/api`-scoped, between the `Host` and
//! `Content-Type` ones, failing closed when it cannot verify. What changed is
//! that the check is now `verify(presented, required_scope)` rather than a
//! comparison, which is exactly the shape #400 left room for on purpose:
//! its own note said the check should be "does this request carry an accepted
//! credential — one `verify(&str) -> bool` — not a hardcoded compare against a
//! single string".
//!
//! Two consequences land in this file:
//!
//! - **The scope map is [`is_state_changing`], reused.** A `read` credential
//!   serves `GET`/`HEAD`/`OPTIONS` and is refused on the state-changing four, so
//!   there is one definition of read-versus-write in the tree rather than a
//!   second permission table that can drift from the first. The one exception —
//!   `/api/security/*`, which needs `write` whatever the method — lives with the
//!   routes it is about, in `security::required_scope`.
//! - **There is no token in this module any more.** `API_TOKEN` is gone; what
//!   process-wide state exists is the *keypair*, and it belongs to the module
//!   that generates and rotates it.
//!
//! # Why the proxy, and why this is not belt-and-braces
//!
//! These checks run in `dispatch`, **before** routing is decided, so a request
//! is refused identically whichever endpoint would have answered it. Putting
//! them inside a handler would mean every new endpoint had to remember them,
//! and the one that forgot would be the hole.
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

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode, Uri};

use crate::native::security;

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

/// The 401 (#400, #405). Go has no counterpart, so this wording is this build's
/// own.
///
/// It names the scheme deliberately: the clients are the app's own webview, a
/// developer holding the dev token file, and whatever local process the user has
/// issued a token to — and all three benefit from the answer saying what was
/// missing.
///
/// **One message for every way a credential can fail to be one**: absent,
/// malformed, signed by another key, expired, wrong `aud`, revoked. That is
/// deliberate. Distinguishing them would tell a caller holding a forged token
/// which part of the forgery to fix, and there is no legitimate client that
/// benefits — the app re-mints on any 401 without reading the reason, and the
/// *log* carries the detail for a developer who needs it.
const UNAUTHORIZED: Rejection = (
    StatusCode::UNAUTHORIZED,
    "a valid Authorization: Bearer token is required",
);

/// The 403 for a credential that verified but does not carry the scope this
/// request needs (#405).
///
/// A different status from [`UNAUTHORIZED`] on the ordinary HTTP terms: 401 is
/// "I do not know who you are", 403 is "I do, and you may not do this". Retrying
/// with the same token is pointless, and saying so is what stops a `read`-scoped
/// script from looping on a re-authentication that would never help.
///
/// It shares its status with [`HOST_REJECTION`] and not its body, which is what
/// the ordering tests distinguish them by.
const INSUFFICIENT_SCOPE: Rejection = (
    StatusCode::FORBIDDEN,
    "this token's scope does not permit this request",
);

/// The one credential scheme accepted, spelled exactly as
/// [`crate::claude::mcp`] spells it.
const BEARER_PREFIX: &str = "Bearer ";

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
    if let Some(rejection) = token_rejection(req.method(), path, req.headers()) {
        return Some(rejection);
    }
    content_type_rejection(req.method(), path, req.headers())
}

/// The credential check (#400, #405).
///
/// **It applies to every method**, unlike `requireJSONContentType`, which is an
/// allowlist over the state-changing four. A `GET` is what reads chat
/// transcripts, agent system prompts and the integration list, so exempting
/// reads would leave most of what is worth stealing reachable. What the method
/// decides is not *whether* a credential is needed but *which scope* it must
/// carry, and that decision lives in `security::required_scope`.
///
/// **No key installed refuses everything.** That is the safe direction and the
/// only one available: the sole way to reach it is a `setup` that failed before
/// loading the keypair, and a listener that cannot verify a signature cannot
/// tell a forged token from a real one, so it must accept neither.
///
/// The scheme match is exact, as `mcp.rs`'s is. Note what that costs and why it
/// is still right: a client sending `bearer ` lowercase is refused even though
/// RFC 7235 makes the scheme case-insensitive. Every client here is ours — the
/// app's webview, the Vite dev proxy, and whatever a user pastes a `curl` recipe
/// into — and a credential check is not the place to start accepting variants
/// nothing sends.
fn token_rejection(method: &Method, path: &str, headers: &HeaderMap) -> Option<Rejection> {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .strip_prefix(BEARER_PREFIX)
        .unwrap_or_default();

    match security::verify_request(presented, method, path) {
        Ok(_) => None,
        Err(security::token::Denied::Unauthenticated) => Some(UNAUTHORIZED),
        Err(security::token::Denied::InsufficientScope) => Some(INSUFFICIENT_SCOPE),
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
///
/// `pub(crate)` for a second caller since #424: the LLM gateway's listener has
/// the same property (`127.0.0.1` unconditionally, no public name) and so needs
/// the same allowlist. It is shared rather than copied deliberately — an
/// allowlist that exists twice is one that gets widened once.
pub(crate) fn host_allowed(raw_host: &str) -> bool {
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
///
/// **Two callers since #405**, and that is the point rather than a coincidence:
/// `security::required_scope` maps the same split onto `read` versus `write`, so
/// the tree has one definition of which methods change state. A second table
/// would be the beginning of the per-route permission model #405 explicitly
/// defers.
pub(crate) fn is_state_changing(method: &Method) -> bool {
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

    use crate::native::security::keys;
    use crate::native::security::token::{self, Scope};

    /// The keypair every test in this binary verifies against.
    ///
    /// Installed once and never replaced, which is what makes it safe under
    /// `cargo test`'s parallelism: the tests here share one process and one
    /// `security::keys::KEYPAIR`, so a test that *swapped* the key — the
    /// regenerate case — would break every test running beside it. That case is
    /// therefore tested where it belongs, in `security::token`, against two
    /// explicit keypairs and no global at all.
    fn seeded_keys() -> std::sync::Arc<keys::Keypair> {
        static SEEDED: std::sync::OnceLock<std::sync::Arc<keys::Keypair>> =
            std::sync::OnceLock::new();
        std::sync::Arc::clone(SEEDED.get_or_init(|| {
            // `install` is the whole seam: the tests below reach the guard
            // through `reject`, which reads the process-wide key, so there is no
            // parameter to pass one through.
            keys::install(keys::Keypair::generate().expect("generate"))
        }))
    }

    /// A `write` credential — what the app's own webview carries, and therefore
    /// what the pre-#405 tests below are asserting the *rest* of the guard with.
    pub(crate) fn seeded_token() -> String {
        mint(Scope::Write)
    }

    fn mint(scope: Scope) -> String {
        let keys = seeded_keys();
        token::mint(&keys, "test", scope, 3600).expect("mint").token
    }

    fn request(method: Method, path: &str, host: &str, content_type: &str) -> Request<Body> {
        let token = seeded_token();
        authed_request(method, path, host, content_type, Some(&token))
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
    /// applying entirely. This check is the only thing standing between a
    /// rebound name and the whole API.
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
    fn a_credential_this_key_did_not_sign_is_refused_and_a_real_one_is_served() {
        let token = seeded_token();
        // Every shape a forgery takes, against the one guard that sees them all.
        let tampered = {
            // Flip the last character of the signature. The header and claims
            // are untouched and still decode, so nothing but the verification
            // stands between this and being served.
            let mut chars: Vec<char> = token.chars().collect();
            let last = chars.len() - 1;
            chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
            chars.into_iter().collect::<String>()
        };
        let other_key = {
            let keys = keys::Keypair::generate().expect("generate");
            token::mint(&keys, "test", Scope::Write, 3600)
                .expect("mint")
                .token
        };
        for wrong in [
            String::new(),
            "not-a-jwt".to_string(),
            tampered,
            other_key,
            format!("{token}x"),
        ] {
            assert_eq!(
                reject(&authed_request(
                    Method::GET,
                    "/api/agents",
                    "localhost",
                    "",
                    Some(&wrong)
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
                Some(&token)
            )),
            None
        );
    }

    /// **Reads are guarded too**, unlike `requireJSONContentType`, which is an
    /// allowlist over the state-changing four. A `GET` is what returns chat
    /// transcripts, agent system prompts and the integration list.
    #[test]
    fn every_method_needs_a_credential_not_only_the_state_changing_ones() {
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
                "{method} should require a credential"
            );
        }
    }

    /// **The scope split, across all seven methods** (#405).
    ///
    /// A `read` token serves the safe three and is refused on the
    /// state-changing four — with a **403**, not a 401, because the caller has
    /// proved who it is and retrying with the same token would not help.
    #[test]
    fn a_read_token_serves_the_safe_methods_and_is_refused_on_the_rest() {
        let read = mint(Scope::Read);
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(
                reject(&authed_request(
                    method.clone(),
                    "/api/agents",
                    "localhost",
                    "",
                    Some(&read)
                )),
                None,
                "a read token should serve {method}"
            );
        }
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(
                reject(&authed_request(
                    method.clone(),
                    "/api/agents",
                    "localhost",
                    "application/json",
                    Some(&read)
                )),
                Some(INSUFFICIENT_SCOPE),
                "a read token must not serve {method}"
            );
        }
    }

    /// ...and a `write` token serves all seven, which is what the app's own
    /// session carries.
    #[test]
    fn a_write_token_serves_every_method() {
        let write = mint(Scope::Write);
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
                    Some(&write)
                )),
                None,
                "a write token should serve {method}"
            );
        }
    }

    /// **...and an `llm` token serves none of them** (#423).
    ///
    /// The gateway data plane's scope is disjoint from `read`/`write`, so a
    /// credential minted for a tool config reaches nothing on `/api` — not the
    /// safe methods, not the state-changing ones, and not the security routes.
    /// It is a **403**: the signature verified and the token is exactly what it
    /// claims to be, so retrying cannot help.
    ///
    /// This is the half of the disjointness that lives outside `Scope::covers`.
    /// That function says `llm` grants nothing; this says the guard actually
    /// asks it, on every method, rather than short-circuiting somewhere first.
    #[test]
    fn an_llm_token_serves_nothing_on_the_api() {
        let llm = mint(Scope::Llm);
        for method in [
            Method::GET,
            Method::HEAD,
            Method::OPTIONS,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ] {
            for path in ["/api/agents", "/api/security/tokens", "/api/chats"] {
                assert_eq!(
                    reject(&authed_request(
                        method.clone(),
                        path,
                        "localhost",
                        "application/json",
                        Some(&llm)
                    )),
                    Some(INSUFFICIENT_SCOPE),
                    "an llm token must not serve {method} {path}"
                );
            }
        }
    }

    /// The converse, and the reason the scope exists at all: the credentials
    /// that *do* serve `/api` must not reach the gateway.
    ///
    /// Asserted at the lattice rather than through `reject`, because no route
    /// here requires [`Scope::Llm`] — `required_scope` never returns it, which
    /// is deliberate and documented at that function. So this is the only place
    /// in the guard's own suite that can state the direction.
    #[test]
    fn no_api_credential_reaches_the_gateway() {
        assert!(!Scope::Write.covers(Scope::Llm));
        assert!(!Scope::Read.covers(Scope::Llm));
        for method in [Method::GET, Method::POST, Method::DELETE] {
            for path in ["/api/agents", "/api/security/tokens", "/api/chats/x"] {
                assert_ne!(
                    security::required_scope(&method, path),
                    Scope::Llm,
                    "no /api route may require the gateway's scope"
                );
            }
        }
    }

    /// The one route family where the scope is not a function of the method:
    /// a `read` token may not enumerate the machine's credentials.
    #[test]
    fn the_security_reads_refuse_a_read_token() {
        let read = mint(Scope::Read);
        let write = mint(Scope::Write);
        for path in ["/api/security/tokens", "/api/security/keys"] {
            assert_eq!(
                reject(&authed_request(
                    Method::GET,
                    path,
                    "localhost",
                    "",
                    Some(&read)
                )),
                Some(INSUFFICIENT_SCOPE),
                "{path} should refuse a read token"
            );
            assert_eq!(
                reject(&authed_request(
                    Method::GET,
                    path,
                    "localhost",
                    "",
                    Some(&write)
                )),
                None,
                "{path} should serve a write token"
            );
        }
    }

    /// A request failing the `Host` check **and** carrying no token reads 403
    /// with the *rebinding* message, not the credential one: the rebinding
    /// answer outranks it, and a browser pointed here by DNS should learn
    /// nothing further.
    #[test]
    fn the_host_check_still_runs_before_the_credential_check() {
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
        // The two 403s share a status and not a body, which is what makes this
        // assertion about ordering rather than about the number.
        assert_ne!(HOST_REJECTION.1, INSUFFICIENT_SCOPE.1);
    }

    /// ...and the credential check runs before the content-type one, so an
    /// unauthenticated caller is not told the content-type rule.
    #[test]
    fn the_credential_check_runs_before_the_content_type_check() {
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
        // With a credential, the same request is the 415 it was before #400.
        assert_eq!(
            reject(&request(
                Method::POST,
                "/api/agents",
                "localhost",
                "text/plain"
            )),
            Some(CONTENT_TYPE_WRONG)
        );
        // An insufficient *scope* is also decided before the content type, for
        // the same reason: the credential question comes first.
        assert_eq!(
            reject(&authed_request(
                Method::POST,
                "/api/agents",
                "localhost",
                "text/plain",
                Some(&mint(Scope::Read))
            )),
            Some(INSUFFICIENT_SCOPE)
        );
    }

    /// **Every way a credential can fail to be one is a 401 here**, including
    /// the ones a correct signature does not save: expired, wrong `aud`, wrong
    /// `iss`, and an unsigned `alg: none`.
    ///
    /// These are all signed by the *seeded* key, so the only thing refusing them
    /// is claim validation — which is what makes this an assertion about the
    /// guard rather than about the signature check the wrong-key case already
    /// covers. The individual rules are pinned in `security::token`; what is
    /// asserted here is that the guard reaches them at all and renders every one
    /// as the same answer.
    #[test]
    fn a_signed_credential_with_bad_claims_is_still_a_401() {
        use base64::Engine;

        let keys = seeded_keys();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;

        let signed = |claims: token::Claims| {
            jsonwebtoken::encode(
                &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA),
                &claims,
                keys.encoding(),
            )
            .expect("encode")
        };
        let base = |exp: i64| token::Claims {
            iss: token::ISSUER.to_string(),
            aud: token::AUDIENCE.to_string(),
            sub: "test".to_string(),
            scope: "write".to_string(),
            jti: "j".to_string(),
            iat: now - 60,
            exp,
        };

        let expired = signed(base(now - (token::LEEWAY as i64) - 60));
        let wrong_audience = signed(token::Claims {
            aud: "somebody-elses-api".to_string(),
            ..base(now + 600)
        });
        let wrong_issuer = signed(token::Claims {
            iss: "somebody-else".to_string(),
            ..base(now + 600)
        });
        // Unsigned, with claims that would otherwise pass. It needs no key at
        // all, so it is what someone who has read the public JWKS reaches for.
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let unsigned = format!(
            "{}.{}.",
            engine.encode(br#"{"alg":"none","typ":"JWT"}"#),
            engine.encode(serde_json::to_vec(&base(now + 600)).expect("claims")),
        );

        for (what, credential) in [
            ("expired", expired),
            ("wrong audience", wrong_audience),
            ("wrong issuer", wrong_issuer),
            ("alg: none", unsigned),
        ] {
            assert_eq!(
                reject(&authed_request(
                    Method::GET,
                    "/api/agents",
                    "localhost",
                    "",
                    Some(&credential)
                )),
                Some(UNAUTHORIZED),
                "{what} should be refused"
            );
        }
    }

    /// The scheme is matched exactly, as `mcp.rs` matches it.
    #[test]
    fn the_credential_must_carry_the_bearer_scheme() {
        let token = seeded_token();
        for value in [
            token.clone(),
            format!("bearer {token}"),
            format!("Basic {token}"),
            format!("Bearer  {token}"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value.parse().expect("hv"));
            assert_eq!(
                token_rejection(&Method::GET, "/api/agents", &headers),
                Some(UNAUTHORIZED),
                "{value:?} should be refused"
            );
        }
    }

    /// Two mints are two credentials — the `mcp.rs` property, applied to the API
    /// server. Since #405 they share a signing key, so what makes them
    /// independent is the `jti`, which is what revocation acts on.
    #[test]
    fn every_minted_credential_is_its_own() {
        let first = seeded_token();
        let second = seeded_token();
        assert_ne!(first, second);
    }
}

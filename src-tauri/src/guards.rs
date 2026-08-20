//! `internal/server/guards.go`, applied at the proxy (#329).
//!
//! Agento ships without authentication on purpose — it is a single-user desktop
//! app — and that only holds if the browser cannot be used as a way in. Two
//! middlewares are what makes it hold, and Go scopes both to `r.Route("/api")`:
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

use std::net::IpAddr;

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

/// Why a request must be refused, or `None` to let it through.
///
/// Order mirrors `server.go`'s `r.Use(s.validateHost)` then
/// `r.Use(requireJSONContentType)`: chi runs them outermost-first, so a request
/// that fails both is answered 403.
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
    content_type_rejection(req.method(), path, req.headers())
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
mod tests {
    use super::*;

    fn request(method: Method, path: &str, host: &str, content_type: &str) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(path);
        if !host.is_empty() {
            builder = builder.header(header::HOST, host);
        }
        if !content_type.is_empty() {
            builder = builder.header(header::CONTENT_TYPE, content_type);
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

    /// Both guards are scoped to `/api` deliberately. The Telegram webhook is
    /// mounted at the root, arrives with a foreign `Host` and is authenticated
    /// by its own secret token; a global guard would break inbound triggers in
    /// production.
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
                reject(&request(
                    Method::POST,
                    path,
                    "api.telegram.org",
                    "application/x-www-form-urlencoded"
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
}

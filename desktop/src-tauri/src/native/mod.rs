//! Endpoints answered by ported Rust code instead of the Go sidecar.
//!
//! This is the far side of the migration seam described in `proxy.rs`. A route
//! listed in [`claims`] is served from here; everything else forwards. Both
//! implementations stay runnable at once, which is what makes a port
//! *verifiable* rather than merely finished — see [`diff`].
//!
//! **Failure means fall back.** Every native handler returns a `Result`, and an
//! `Err` is not turned into an HTTP error: the proxy logs it and forwards the
//! request to Go. A ported route can therefore only ever be as broken as the
//! unported one, and a schema change that outruns the Rust reader degrades to
//! the behaviour the app had before the port instead of a 500.

pub mod agents;
pub mod analytics;
pub mod db;
pub mod diff;
pub mod gojson;
pub mod gotime;
pub mod insights;
pub mod pricing;
pub mod sessions;
pub mod settings;

use axum::body::Body;
use axum::http::{header, Method, Response, StatusCode};

use crate::paths;

/// How much of the seam is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Claimed routes are answered by Rust. The default.
    On,
    /// Nothing is claimed; every request forwards. The escape hatch if a port
    /// turns out to be wrong in the field.
    Off,
    /// Go answers, Rust computes alongside and the two are compared. The mode a
    /// port is validated in before it is trusted.
    Diff,
}

/// Read `AGENTO_DESKTOP_NATIVE` once. An unrecognized value is `On` rather than
/// an error: a typo in a developer's shell must not silently disable the code
/// paths the app now ships.
pub fn mode() -> Mode {
    use std::sync::OnceLock;
    static MODE: OnceLock<Mode> = OnceLock::new();

    *MODE.get_or_init(|| {
        match std::env::var("AGENTO_DESKTOP_NATIVE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "0" | "false" => Mode::Off,
            "diff" | "shadow" => Mode::Diff,
            _ => Mode::On,
        }
    })
}

/// A claimed request: the parts a native handler needs.
pub struct Request<'a> {
    pub method: &'a Method,
    pub path: &'a str,
    /// The raw query string without its leading `?`.
    pub query: &'a str,
}

/// What a native handler produced: the response body, plus any request the
/// proxy should fire at the sidecar afterwards to keep the corpus fresh.
pub struct Answer {
    pub body: Vec<u8>,
    pub probe: Option<&'static str>,
}

/// What every handler needs to reach the data the Go server owns.
///
/// Deliberately not an open connection: the modules differ in what they want —
/// one read-only handle, two, or a path passed through to a helper that opens
/// its own — and pre-opening one here would make the registry decide something
/// only the handler knows.
pub struct Ctx {
    pub db_path: std::path::PathBuf,
}

/// One ported area of the API: whether it claims a request, and how it answers.
///
/// The pair travels together so claiming a route and implementing it are the
/// same edit. Splitting them across two files is how a route ends up claimed by
/// a handler that does not exist, which fails as a fallback to Go — silently.
pub struct Endpoint {
    /// Shown when a claimed request has no handler. Not on the wire.
    pub name: &'static str,
    pub claims: fn(&Method, &str) -> bool,
    pub serve: fn(&Ctx, &Request) -> Result<Answer, String>,
}

/// Every ported area, in match order.
///
/// **Adding an endpoint is one appended line here plus its own module.** That
/// is the point: this file used to hold one `match` covering every route, so
/// two ports in flight at once always collided in the same hunk. Nothing here
/// knows what any module does, and no module knows about any other.
///
/// Order matters only where two entries could claim the same path, which is a
/// mistake rather than a feature — `an_endpoint_claims_at_most_one_path`
/// asserts none do.
const ENDPOINTS: &[Endpoint] = &[
    pricing::ENDPOINT,
    agents::ENDPOINT,
    sessions::ENDPOINT,
    analytics::ENDPOINT,
    insights::ENDPOINT,
];

/// Whether this request is answered by ported Rust code.
///
/// Each module matches on the exact path, so an unported sibling — or a
/// trailing slash, which chi treats as a different route — falls through to Go
/// and keeps whatever answer Go gives it.
pub fn claims(method: &Method, path: &str) -> bool {
    ENDPOINTS.iter().any(|e| (e.claims)(method, path))
}

/// Answer a claimed request. `Err` means "fall back to the Go sidecar".
pub fn serve(req: &Request) -> Result<Answer, String> {
    let ctx = Ctx {
        db_path: paths::database_path().ok_or("no home directory to resolve the data dir")?,
    };
    for endpoint in ENDPOINTS {
        if (endpoint.claims)(req.method, req.path) {
            return (endpoint.serve)(&ctx, req);
        }
    }
    Err(format!(
        "{} {} is claimed but has no handler",
        req.method, req.path
    ))
}

/// Wrap a native body in the response `writeJSON` would have produced.
///
/// The header set is Go's, exactly: `Content-Type: application/json` with no
/// charset. The frontend does not care, but a diff of the whole exchange would.
pub fn response(body: Vec<u8>) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ported_reads_are_claimed_and_their_siblings_are_not() {
        assert!(claims(&Method::GET, "/api/pricing/catalog"));
        assert!(claims(&Method::GET, "/api/claude-sessions"));
        assert!(claims(&Method::GET, "/api/claude-sessions/facets"));
        assert!(claims(&Method::GET, "/api/claude-analytics"));

        // Writes stay with Go until phase 3 moves the storage layer.
        assert!(!claims(&Method::POST, "/api/pricing/rates"));
        assert!(!claims(&Method::POST, "/api/claude-sessions/refresh"));
        // Scan lifecycle stays with Go while the scanner does.
        assert!(!claims(&Method::GET, "/api/claude-sessions/status"));
        // Not a prefix match: an unported endpoint under the same namespace
        // must not be swallowed. A session ID is a path segment, not a suffix.
        assert!(!claims(&Method::GET, "/api/claude-sessions/abc-123"));
        assert!(!claims(&Method::GET, "/api/claude-sessions/"));
        assert!(claims(
            &Method::GET,
            "/api/claude-sessions/insights/summary"
        ));
        // The per-session insight record is a different route and stays with Go.
        assert!(!claims(&Method::GET, "/api/claude-sessions/abc/insights"));

        // Agents: the two reads, and nothing that writes or nests.
        assert!(claims(&Method::GET, "/api/agents"));
        assert!(claims(&Method::GET, "/api/agents/my-agent"));
        assert!(!claims(&Method::POST, "/api/agents"));
        assert!(!claims(&Method::PUT, "/api/agents/my-agent"));
        assert!(!claims(&Method::DELETE, "/api/agents/my-agent"));
        assert!(!claims(&Method::GET, "/api/agents/my-agent/duplicate"));
        assert!(!claims(&Method::GET, "/api/agents/"));
    }

    #[test]
    fn an_unhandled_claim_is_an_error_so_the_proxy_falls_back() {
        assert!(serve(&Request {
            method: &Method::GET,
            path: "/api/nothing-here",
            query: "",
        })
        .is_err());
    }

    /// Two modules claiming one path is a merge accident, not a feature: the
    /// registry would silently hand the request to whichever was listed first,
    /// and the other module's tests would keep passing.
    #[test]
    fn no_two_endpoints_claim_the_same_request() {
        let paths = [
            "/api/pricing/catalog",
            "/api/claude-sessions",
            "/api/claude-sessions/facets",
            "/api/claude-analytics",
            "/api/claude-sessions/insights/summary",
            "/api/agents",
            "/api/agents/my-agent",
        ];
        for path in paths {
            let owners: Vec<&str> = ENDPOINTS
                .iter()
                .filter(|e| (e.claims)(&Method::GET, path))
                .map(|e| e.name)
                .collect();
            assert_eq!(owners.len(), 1, "{path} is claimed by {owners:?}");
        }
    }

    /// Every entry must be reachable. An endpoint appended to `ENDPOINTS` whose
    /// `claims` never fires is dead code that reads as a shipped port.
    #[test]
    fn every_registered_endpoint_claims_something() {
        let probes = [
            "/api/pricing/catalog",
            "/api/claude-sessions",
            "/api/claude-sessions/facets",
            "/api/claude-analytics",
            "/api/claude-sessions/insights/summary",
            "/api/agents",
            "/api/agents/my-agent",
        ];
        for endpoint in ENDPOINTS {
            assert!(
                probes.iter().any(|p| (endpoint.claims)(&Method::GET, p)),
                "{} claims none of the probe paths; add one when you add a route",
                endpoint.name
            );
        }
    }
}

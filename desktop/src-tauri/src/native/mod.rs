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

pub mod db;
pub mod diff;
pub mod gojson;
pub mod gotime;
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

/// Whether this request is answered by ported Rust code.
///
/// Matched on the exact path, so an unported sibling — or a trailing slash,
/// which chi treats as a different route — falls through to Go and keeps
/// whatever answer Go gives it.
pub fn claims(method: &Method, path: &str) -> bool {
    matches!(
        (method.as_str(), path),
        (
            "GET",
            "/api/pricing/catalog" | "/api/claude-sessions" | "/api/claude-sessions/facets"
        )
    )
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

/// Answer a claimed request. `Err` means "fall back to the Go sidecar".
pub fn serve(req: &Request) -> Result<Answer, String> {
    let db_path = paths::database_path().ok_or("no home directory to resolve the data dir")?;

    match (req.method.as_str(), req.path) {
        ("GET", "/api/pricing/catalog") => {
            let catalog = pricing::catalog(&db_path)?;
            Ok(Answer {
                body: gojson::to_vec(&catalog)
                    .map_err(|e| format!("encoding pricing catalog: {e}"))?,
                probe: None,
            })
        }
        ("GET", path @ ("/api/claude-sessions" | "/api/claude-sessions/facets")) => {
            let q = sessions::query::SessionQuery::parse(req.query)?;
            let conn = db::open_read_only(&db_path)?;
            let data_settings = settings::load(&conn);

            let body = if path == "/api/claude-sessions" {
                let page = sessions::page::list_page(&conn, &data_settings, &q)?;
                gojson::to_vec(&page).map_err(|e| format!("encoding session page: {e}"))?
            } else {
                let facets = sessions::page::facets(&conn, &data_settings, &q)?;
                gojson::to_vec(&facets).map_err(|e| format!("encoding session facets: {e}"))?
            };
            Ok(Answer {
                body,
                probe: sessions::freshness_probe(path, &q),
            })
        }
        _ => Err(format!(
            "{} {} is claimed but has no handler",
            req.method, req.path
        )),
    }
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

        // Writes stay with Go until phase 3 moves the storage layer.
        assert!(!claims(&Method::POST, "/api/pricing/rates"));
        assert!(!claims(&Method::POST, "/api/claude-sessions/refresh"));
        // Scan lifecycle stays with Go while the scanner does.
        assert!(!claims(&Method::GET, "/api/claude-sessions/status"));
        // Not a prefix match: an unported endpoint under the same namespace
        // must not be swallowed. A session ID is a path segment, not a suffix.
        assert!(!claims(&Method::GET, "/api/claude-sessions/abc-123"));
        assert!(!claims(&Method::GET, "/api/claude-sessions/"));
        assert!(!claims(&Method::GET, "/api/claude-analytics"));
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
}

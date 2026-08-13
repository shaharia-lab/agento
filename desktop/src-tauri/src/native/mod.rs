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
    matches!((method.as_str(), path), ("GET", "/api/pricing/catalog"))
}

/// Answer a claimed request. `Err` means "fall back to the Go sidecar".
pub fn serve(method: &Method, path: &str) -> Result<Vec<u8>, String> {
    match (method.as_str(), path) {
        ("GET", "/api/pricing/catalog") => {
            let db_path =
                paths::database_path().ok_or("no home directory to resolve the data dir")?;
            let catalog = pricing::catalog(&db_path)?;
            gojson::to_vec(&catalog).map_err(|e| format!("encoding pricing catalog: {e}"))
        }
        _ => Err(format!("{method} {path} is claimed but has no handler")),
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
    fn the_pricing_catalog_is_claimed_and_its_siblings_are_not() {
        assert!(claims(&Method::GET, "/api/pricing/catalog"));

        // Writes stay with Go until phase 3 moves the storage layer.
        assert!(!claims(&Method::POST, "/api/pricing/rates"));
        assert!(!claims(&Method::PUT, "/api/pricing/rates"));
        assert!(!claims(&Method::DELETE, "/api/pricing/rates"));
        // Not a prefix match: an unported endpoint under the same namespace
        // must not be swallowed.
        assert!(!claims(&Method::GET, "/api/pricing/catalog/extra"));
        assert!(!claims(&Method::GET, "/api/pricing/catalog/"));
        assert!(!claims(&Method::GET, "/api/claude-analytics"));
    }

    #[test]
    fn an_unhandled_claim_is_an_error_so_the_proxy_falls_back() {
        assert!(serve(&Method::GET, "/api/nothing-here").is_err());
    }
}

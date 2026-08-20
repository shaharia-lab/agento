//! `GET /api/version` and `GET /api/version/update-check`.
//!
//! Mirrors `handleVersion` and `handleUpdateCheck` (`internal/api/version.go`).
//!
//! Two things here are not obvious.
//!
//! **The build stamp defaults to Go's unstamped values.** Go's `internal/build`
//! variables were set by `-ldflags`, and only the Makefile did that —
//! `scripts/parity-instance.sh` built with no flags, so the server the parity
//! tests diffed against served the package defaults, and these were pinned to
//! them. [`VERSION`] and friends still read those defaults, with an
//! `option_env!` hook (`AGENTO_BUILD_VERSION` and friends) for the day the
//! desktop bundle starts stamping itself. Nothing constrains them any more —
//! #278 removed the Go binary that would have had to agree and #388 removed the
//! Go tree — so stamping is now a build-script change and nothing else.
//!
//! **The update check is not the updater.** Go's release-lookup branch asked
//! GitHub and compared, which is the self-updater — the one subsystem the
//! handover excludes from the port, because Tauri's own updater replaces it.
//! Every build therefore answers the short-circuit `update_available: false`;
//! offering an update from this route would duplicate the Tauri updater that
//! actually performs one. See [`update_check`].

use axum::http::Method;
use serde::Serialize;

/// `build.Version`. Unstamped is `"dev"`, exactly as the Go package declares it.
pub const VERSION: &str = match option_env!("AGENTO_BUILD_VERSION") {
    Some(v) => v,
    None => "dev",
};

/// `build.CommitSHA`.
pub const COMMIT_SHA: &str = match option_env!("AGENTO_BUILD_COMMIT") {
    Some(v) => v,
    None => "unknown",
};

/// `build.BuildDate`.
pub const BUILD_DATE: &str = match option_env!("AGENTO_BUILD_DATE") {
    Some(v) => v,
    None => "unknown",
};

/// `GET /api/version`.
///
/// Go builds this from a `map[string]string`, and **Go marshals map keys
/// sorted** — so the wire order is alphabetical rather than the order the
/// literal is written in. The fields below are declared in that order for that
/// reason; re-grouping them "logically" would change the response.
#[derive(Debug, Clone, Serialize)]
pub struct VersionResponse {
    pub build_date: &'static str,
    pub commit: &'static str,
    pub version: &'static str,
}

/// `GET /api/version/update-check`, for a build that names no published
/// release. Same story on the ordering: a `map[string]interface{}` in Go.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheckResponse {
    pub current_version: &'static str,
    pub latest_version: &'static str,
    pub release_url: &'static str,
    pub update_available: bool,
}

pub fn version() -> VersionResponse {
    VersionResponse {
        build_date: BUILD_DATE,
        commit: COMMIT_SHA,
        version: VERSION,
    }
}

/// The update check.
///
/// Go's other branch — a build that names a published release — asked GitHub
/// for the latest release and compared, which *is* the self-updater, the one
/// subsystem the port excludes because Tauri's updater replaces it. Until #278
/// that branch forwarded to the sidecar; with it gone, a stamped build answers
/// the same short-circuit a dev build always did: `update_available: false`,
/// because offering an update here would duplicate (and race) the Tauri
/// updater that actually performs one.
pub fn update_check() -> UpdateCheckResponse {
    UpdateCheckResponse {
        current_version: VERSION,
        latest_version: "",
        release_url: "",
        update_available: false,
    }
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "version",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && (path == "/api/version" || path == "/api/version/update-check")
}

fn serve(_ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let body = match req.path {
        "/api/version" => {
            super::gojson::to_vec(&version()).map_err(|e| format!("encoding version: {e}"))?
        }
        "/api/version/update-check" => super::gojson::to_vec(&update_check())
            .map_err(|e| format!("encoding update check: {e}"))?,
        other => return Err(format!("{other} is not a version read")),
    };
    Ok(super::Answer::json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key order is alphabetical because Go builds the body from a map. This is
    /// the whole reason the struct's fields are declared out of logical order.
    #[test]
    fn the_version_body_is_the_map_go_marshals() {
        let body = super::super::gojson::to_vec(&version()).expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            format!(
                "{{\"build_date\":\"{BUILD_DATE}\",\"commit\":\"{COMMIT_SHA}\",\
                 \"version\":\"{VERSION}\"}}\n"
            )
        );
    }

    /// The unstamped defaults are what both the bundled sidecar and the parity
    /// server serve, so parity depends on them matching `internal/build`.
    #[test]
    fn the_unstamped_defaults_match_the_go_package() {
        if option_env!("AGENTO_BUILD_VERSION").is_none() {
            assert_eq!(VERSION, "dev");
            assert_eq!(COMMIT_SHA, "unknown");
            assert_eq!(BUILD_DATE, "unknown");
        }
    }

    #[test]
    fn the_update_check_short_circuits_for_every_build() {
        let answer = update_check();
        assert!(!answer.update_available);
        assert_eq!(answer.latest_version, "");
        assert_eq!(answer.release_url, "");
        assert_eq!(answer.current_version, VERSION);

        let body = super::super::gojson::to_vec(&answer).expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            format!(
                "{{\"current_version\":\"{VERSION}\",\"latest_version\":\"\",\
                 \"release_url\":\"\",\"update_available\":false}}\n"
            )
        );
    }

    #[test]
    fn both_version_reads_are_claimed_and_nothing_else_is() {
        assert!(claims(&Method::GET, "/api/version"));
        assert!(claims(&Method::GET, "/api/version/update-check"));
        assert!(!claims(&Method::POST, "/api/version"));
        assert!(!claims(&Method::GET, "/api/version/"));
        assert!(!claims(&Method::GET, "/api/versions"));
    }
}

//! `GET /api/version` and `GET /api/version/update-check`.
//!
//! Mirrors `handleVersion` and `handleUpdateCheck` (`internal/api/version.go`).
//!
//! Two things here are not obvious.
//!
//! **The build stamp is not stamped.** Go's `internal/build` variables are set
//! by `-ldflags`, and only the Makefile does that. `desktop/scripts/build-sidecar.sh`
//! builds with `-ldflags "-s -w"` and `scripts/parity-instance.sh` with no flags
//! at all, so the sidecar the app ships — and the server the parity test diffs
//! against — both serve the package defaults. [`VERSION`] and friends therefore
//! read the same defaults, with an `option_env!` hook for the day the desktop
//! bundle starts stamping itself. That day is the cut-over, when the Go binary
//! that would have to agree is gone.
//!
//! **The update check is not the updater.** Go asks GitHub for the latest
//! release and compares it, which is the self-updater — the one subsystem the
//! handover explicitly excludes from the port, because Tauri's own updater
//! replaces it. What is ported is the branch that answers *without* a network
//! call: a build that names no published release short-circuits to
//! `update_available: false`, and that is every build the desktop app ships.
//! Anything else returns `Err` and falls back to the sidecar, which is the
//! seam's documented way to leave a case with Go. See [`update_check`].

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

/// The update check, or `None` when answering it would need the release lookup.
///
/// `None` is not a failure — it is the honest answer that this build's version
/// names a published release, and comparing against GitHub's is the updater's
/// job. The caller turns it into an `Err` so the request forwards to Go.
pub fn update_check() -> Option<UpdateCheckResponse> {
    if !is_dev_build(VERSION) {
        return None;
    }
    Some(UpdateCheckResponse {
        current_version: VERSION,
        latest_version: "",
        release_url: "",
        update_available: false,
    })
}

/// `build.IsDevBuild`: whether a version names a local working tree rather than
/// a published release.
///
/// The `git describe` case is why this is a shape check and not a semver parse.
/// `v0.8.0-21-gc325de6-dirty` *is* valid semver, and semver ranks a prerelease
/// **below** the release it precedes — so the published v0.8.0 would compare as
/// newer than a build 21 commits past it, and the banner would offer the
/// developer an update to code they already have.
///
/// A genuine prerelease tag such as `v1.0.0-rc.1` is deliberately not a dev
/// build: those are published, and their users should be offered updates.
pub fn is_dev_build(version: &str) -> bool {
    let v = version.trim().strip_prefix('v').unwrap_or(version.trim());
    if v.is_empty() || v == "dev" || v == "unknown" {
        return true;
    }
    if v.ends_with("-dirty") {
        return true;
    }
    has_git_describe_suffix(v)
}

/// Go's `-\d+-g[0-9a-f]{7,40}$`, hand-matched because nothing else in this
/// crate wants a regex engine.
///
/// The last `-g` is the only candidate: everything after it must be lowercase
/// hex, and `g` is not a hex digit, so there is no earlier match to prefer.
fn has_git_describe_suffix(v: &str) -> bool {
    let Some(marker) = v.rfind("-g") else {
        return false;
    };
    let sha = &v[marker + 2..];
    if !(7..=40).contains(&sha.len())
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return false;
    }
    let head = &v[..marker];
    let Some(dash) = head.rfind('-') else {
        return false;
    };
    let count = &head[dash + 1..];
    !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit())
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
        "/api/version/update-check" => match update_check() {
            Some(answer) => {
                super::gojson::to_vec(&answer).map_err(|e| format!("encoding update check: {e}"))?
            }
            // Deliberate: the release lookup is the self-updater, which the
            // desktop app replaces with Tauri's own. Falling back keeps Go's
            // answer — including its 502 when GitHub is unreachable — rather
            // than inventing one.
            None => {
                return Err(format!(
                    "update check for released build {VERSION:?} needs the release lookup"
                ))
            }
        },
        other => return Err(format!("{other} is not a version read")),
    };
    Ok(super::Answer { body, probe: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dev_build_is_recognised_in_every_shape_the_makefile_produces() {
        // The case the shape check exists for: valid semver that must not be
        // compared against a release.
        assert!(is_dev_build("v0.8.0-21-gc325de6-dirty"));
        assert!(is_dev_build("v0.8.0-dirty"));
        assert!(is_dev_build("v0.8.0-21-gc325de6"));
        assert!(is_dev_build(
            "v1.2.3-4-g0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(is_dev_build("0.8.0-21-gc325de6-dirty"));

        assert!(is_dev_build("dev"));
        assert!(is_dev_build("unknown"));
        assert!(is_dev_build(""));
        assert!(is_dev_build("  "));
    }

    #[test]
    fn a_published_release_is_not_a_dev_build() {
        assert!(!is_dev_build("v0.8.0"));
        assert!(!is_dev_build("0.8.0"));
        // Published prereleases still get update offers.
        assert!(!is_dev_build("v1.0.0-rc.1"));
        // Too short, too long, and not hex: none of these is a describe suffix.
        assert!(!is_dev_build("v1.0.0-4-gabc"));
        assert!(!is_dev_build("v1.0.0-4-gzzzzzzz"));
        assert!(!is_dev_build("v1.0.0-gc325de6"));
    }

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
    fn the_update_check_short_circuits_for_a_dev_build() {
        let answer = update_check().expect("an unstamped build short-circuits");
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

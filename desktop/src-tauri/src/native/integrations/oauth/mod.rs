//! The authorization URL half of the OAuth flow (#318).
//!
//! Mirrors `BuildAuthURL` in `internal/integrations/{google,slack}/oauth.go`,
//! and through it `oauth2.Config.AuthCodeURL`.
//!
//! # Why this is a vector rather than a live diff
//!
//! Every other ported route is verified by asking both implementations the same
//! question and comparing bytes. `POST …/auth/start` cannot be: the redirect
//! port comes from a fresh `FreePort()`, so two implementations answering the
//! same request produce two legitimately different URLs. Everything *else* about
//! the bytes still has to match, and all of it is rule rather than value — the
//! scope set and its order, the query encoding, the per-provider auth-code
//! options, the endpoint. `desktop/parity/oauth_vectors.json` records what Go's
//! own builder produced for a fixed port, and both languages assert against it.
//!
//! Generating rather than hand-writing was not ceremony: `oauth2.ApprovalForce`
//! emits **`prompt=consent`**, not the `approval_prompt=force` its name suggests
//! and that a transcription would have produced.
//!
//! # The three things that are easy to get wrong
//!
//! - **The scope union is ordered, not set-like.** `Scopes` walks calendar, then
//!   gmail, then drive, and deduplicates — so the string is deterministic even
//!   though `services` is a Go map with random iteration order. Sorting instead
//!   would pass most vectors and diverge on the rest.
//! - **`scope` is absent, not empty, when nothing is enabled.** `AuthCodeURL`
//!   guards with `if len(c.Scopes) > 0`, so a Google integration with no enabled
//!   service produces a URL with no `scope` key at all.
//! - **The encoding is `url.Values.Encode`**: keys sorted, values through
//!   [`crate::native::gourl::query_escape`], where a space is `+` rather than
//!   `%20`. The scope separator is a space, so this is on every URL with more
//!   than one scope.

pub mod exchange;
pub mod flow;

use std::collections::BTreeMap;

use crate::native::gourl::query_escape;
use crate::native::integrations::ServiceConfig;

/// `googleoauth.Endpoint.AuthURL`.
const GOOGLE_AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/auth";

/// Slack's **v2** authorize endpoint. The v1 path is a different grant.
const SLACK_AUTH_ENDPOINT: &str = "https://slack.com/oauth/v2/authorize";

/// `calendarScopes`, `gmailScopes`, `driveScopes` — in the order `Scopes` adds
/// them, which is the order they appear in the URL.
const CALENDAR_SCOPES: &[&str] = &["https://www.googleapis.com/auth/calendar"];
const GMAIL_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.send",
    "https://www.googleapis.com/auth/gmail.readonly",
];
const DRIVE_SCOPES: &[&str] = &["https://www.googleapis.com/auth/drive"];

/// `slack/oauth.go`'s `oauthScopes`, in its declared order.
const SLACK_SCOPES: &[&str] = &[
    "channels:read",
    "channels:history",
    "chat:write",
    "users:read",
    "search:read",
    "groups:read",
    "groups:history",
];

/// `google.Scopes`: the union of the enabled services' scopes, deduplicated,
/// in calendar → gmail → drive order.
///
/// The three service names are checked individually rather than by iterating
/// the map, which is what makes the result independent of map order — Go reads
/// `services["calendar"]`, `services["gmail"]`, `services["drive"]` in that
/// sequence, and a service present but `enabled: false` contributes nothing.
pub fn google_scopes(services: &BTreeMap<String, ServiceConfig>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let add = |scopes: &[&str], out: &mut Vec<String>| {
        for scope in scopes {
            if !out.iter().any(|seen| seen == scope) {
                out.push((*scope).to_string());
            }
        }
    };
    for (name, scopes) in [
        ("calendar", CALENDAR_SCOPES),
        ("gmail", GMAIL_SCOPES),
        ("drive", DRIVE_SCOPES),
    ] {
        if services.get(name).is_some_and(|svc| svc.enabled) {
            add(scopes, &mut out);
        }
    }
    out
}

/// The `services` column, for the scope union. Read separately from the hosting
/// row because that projection carries secrets and this does not.
pub fn services_of(
    db_path: &std::path::Path,
    id: &str,
) -> Result<BTreeMap<String, ServiceConfig>, String> {
    Ok(crate::native::integrations::get(db_path, id)?
        .and_then(|i| i.services)
        .unwrap_or_default())
}

/// `fmt.Sprintf("http://localhost:%d/callback", redirectPort)`.
///
/// `localhost`, not `127.0.0.1`: it is what the provider has registered as the
/// redirect URI, so the spelling is part of the contract rather than a choice.
pub fn redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}/callback")
}

/// `oauth2.Config.AuthCodeURL(state, opts…)`.
///
/// `url.Values.Encode()` sorts by key, so the parameters are collected into a
/// `BTreeMap` and rendered in that order rather than in the order they were
/// added.
fn auth_code_url(
    endpoint: &str,
    client_id: &str,
    port: u16,
    scopes: &[String],
    extra: &[(&str, &str)],
) -> String {
    let mut params: BTreeMap<&str, String> = BTreeMap::new();
    params.insert("response_type", "code".to_string());
    params.insert("client_id", client_id.to_string());
    params.insert("redirect_uri", redirect_uri(port));
    // `if len(c.Scopes) > 0` — absent, not empty, when there are none.
    if !scopes.is_empty() {
        params.insert("scope", scopes.join(" "));
    }
    // `AuthCodeURL` always sets state, and both callers pass the literal
    // "state" — neither verifies it on the way back, which is Go's behaviour
    // and not something a port may quietly improve.
    params.insert("state", "state".to_string());
    for (key, value) in extra {
        params.insert(key, (*value).to_string());
    }

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", query_escape(k), query_escape(v)))
        .collect::<Vec<_>>()
        .join("&");
    // Both endpoints have no query of their own, so `AuthCodeURL`'s `?` vs `&`
    // branch always takes the `?` arm.
    format!("{endpoint}?{query}")
}

/// `google.BuildAuthURL`.
///
/// `AccessTypeOffline` and `ApprovalForce` are what make Google re-issue a
/// refresh token on every consent — without them a second authorization returns
/// only an access token and the stored credential silently stops refreshing.
pub fn google_auth_url(
    client_id: &str,
    port: u16,
    services: &BTreeMap<String, ServiceConfig>,
) -> String {
    auth_code_url(
        GOOGLE_AUTH_ENDPOINT,
        client_id,
        port,
        &google_scopes(services),
        // `oauth2.ApprovalForce` is `prompt=consent`, despite the name.
        &[("access_type", "offline"), ("prompt", "consent")],
    )
}

/// `slack.BuildAuthURL`.
///
/// No auth-code options at all, and the scopes are fixed — `services` is not
/// read. Slack v2 tokens do not expire and carry no refresh token unless the
/// app enables rotation, which Go's own comment says this flow does not handle.
pub fn slack_auth_url(client_id: &str, port: u16) -> String {
    let scopes: Vec<String> = SLACK_SCOPES.iter().map(|s| (*s).to_string()).collect();
    auth_code_url(SLACK_AUTH_ENDPOINT, client_id, port, &scopes, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> ServiceConfig {
        ServiceConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn disabled() -> ServiceConfig {
        ServiceConfig::default()
    }

    fn services(pairs: &[(&str, ServiceConfig)]) -> BTreeMap<String, ServiceConfig> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn the_scope_union_is_ordered_and_deduplicated() {
        let all = services(&[
            ("drive", enabled()),
            ("gmail", enabled()),
            ("calendar", enabled()),
        ]);
        assert_eq!(
            google_scopes(&all),
            vec![
                "https://www.googleapis.com/auth/calendar",
                "https://www.googleapis.com/auth/gmail.send",
                "https://www.googleapis.com/auth/gmail.readonly",
                "https://www.googleapis.com/auth/drive",
            ],
            "calendar, then gmail, then drive — not the map's order and not sorted"
        );
    }

    #[test]
    fn a_disabled_or_absent_service_contributes_no_scope() {
        assert!(google_scopes(&services(&[])).is_empty());
        assert!(google_scopes(&services(&[("gmail", disabled())])).is_empty());
        assert_eq!(
            google_scopes(&services(&[("gmail", enabled()), ("drive", disabled())])).len(),
            2,
            "gmail's two scopes and nothing from drive"
        );
    }

    #[test]
    fn an_unknown_service_is_not_a_scope() {
        // `Scopes` reads three fixed keys; anything else in the column is
        // ignored rather than mapped.
        assert!(google_scopes(&services(&[("mail", enabled())])).is_empty());
    }
}

#[cfg(test)]
mod tests_vectors {
    use super::*;

    /// `desktop/parity/oauth_vectors.json`, byte for byte.
    ///
    /// The URLs are what Go's own `BuildAuthURL` produced; a divergence here is
    /// a user sent to a consent screen asking for the wrong scopes, or a
    /// provider rejecting the request outright.
    #[derive(serde::Deserialize)]
    struct Vectors {
        cases: Vec<Case>,
    }

    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        #[allow(dead_code)]
        note: String,
        #[serde(rename = "type")]
        integration_type: String,
        /// The raw `credentials` column, so this exercises the real decoder
        /// rather than a struct the test built.
        credentials: serde_json::Value,
        services: Option<std::collections::BTreeMap<String, ServiceConfig>>,
        port: u16,
        auth_url: String,
    }

    #[test]
    fn every_auth_url_matches_what_go_built() {
        let raw = include_str!("../../../../../parity/oauth_vectors.json");
        let vectors: Vectors = serde_json::from_str(raw).expect("vectors decode");
        assert!(!vectors.cases.is_empty(), "the vector file is empty");

        for case in &vectors.cases {
            let client_id = case
                .credentials
                .get("client_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let services = case.services.clone().unwrap_or_default();

            let got = match case.integration_type.as_str() {
                "google" => google_auth_url(client_id, case.port, &services),
                "slack" => slack_auth_url(client_id, case.port),
                other => panic!("case {:?} has no OAuth flow: {other}", case.name),
            };
            assert_eq!(got, case.auth_url, "case {:?}", case.name);
        }
    }
}

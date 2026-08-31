//! `ValidateCredentials` — `internal/integrations/jira/validate.go`.
//!
//! The plainest of the five: `GET {siteURL}/rest/api/3/myself` with basic auth,
//! any non-200 reported with its status and body, and the display name read off
//! a 200.
//!
//! Two things it does **not** do, both worth stating because its Confluence
//! sibling does one of them and every reader assumes the other:
//!
//! - it does not call `ValidateSiteURL`. The URL is concatenated as given. The
//!   trailing slash was already stripped by `validateJiraCredentials`, which is
//!   the *service* layer, so a caller reaching this function directly with a
//!   trailing slash gets a double slash — and Jira accepts it.
//! - it does not single out 401/403. An invalid token reports
//!   `jira API error: status 401: {…}` with Atlassian's body attached.
//!
//! The 15-second client and the 2 MiB read cap are `tools.go`'s as well, so both
//! come from `client`.

use crate::claude::CancellationToken;
use crate::native::integrations::check::CheckFailure;

use super::client::{http_client, read_capped};

/// `myselfResponse`. `emailAddress` is declared and unread in Go too.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
#[allow(dead_code)]
struct Myself {
    #[serde(
        rename = "displayName",
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    display_name: String,
    #[serde(
        rename = "emailAddress",
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    email_address: String,
}

/// `ValidateCredentials(ctx, siteURL, email, apiToken)` — the display name on
/// success.
///
/// It does not single out 401/403 in its *message* (see above) and it does have
/// to single them out in its [`CheckFailure`]: a 401 is Atlassian refusing the
/// token, while a 404 or a 5xx says nothing about it. Same sentence either way.
pub async fn validate_credentials(
    ct: &CancellationToken,
    site_url: &str,
    email: &str,
    api_token: &str,
) -> Result<String, CheckFailure> {
    let failed = "calling Jira /myself: request failed".to_string();
    let url = reqwest::Url::parse(&format!("{site_url}/rest/api/3/myself"))
        // `creating Jira myself request: %w` — `net/http`'s wording, so this
        // carries reqwest's.
        .map_err(|e| CheckFailure::unreachable(format!("creating Jira myself request: {e}")))?;

    let request = http_client()
        .ok_or_else(|| CheckFailure::unreachable(failed.clone()))?
        .get(url)
        .basic_auth(email, Some(api_token))
        .header("Accept", "application/json");

    // Go discards `client.Do`'s error: the URL is the customer's site and the
    // header is a credential.
    let response = tokio::select! {
        () = ct.cancelled() => return Err(CheckFailure::unreachable(failed.clone())),
        result = request.send() => result.map_err(|_| CheckFailure::unreachable(failed))?,
    };

    let status = response.status().as_u16();
    let body = read_capped(ct, response)
        .await
        .map_err(|e| CheckFailure::unreachable(format!("reading Jira response: {e}")))?;

    if status != 200 {
        return Err(CheckFailure::from_status(
            status,
            format!("jira API error: status {status}: {body}"),
        ));
    }

    let myself: Myself =
        serde_json::from_str::<Option<crate::native::gojson::GoStruct<Myself>>>(&body)
            .map(|wrapped| wrapped.map_or_else(Myself::default, |wrapped| wrapped.0))
            .map_err(|e| CheckFailure::unreachable(format!("parsing Jira myself response: {e}")))?;
    Ok(myself.display_name)
}

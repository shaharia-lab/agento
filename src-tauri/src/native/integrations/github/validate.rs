//! `ValidatePAT` — `internal/integrations/github/auth.go`.
//!
//! The one validator that lives in `auth.go` rather than a `validate.go`, and
//! the one that shares its `http.Client` with the tool path (`ghHTTPClient`, 15
//! seconds) instead of declaring its own. Both facts are why this is the
//! shortest of the five.
//!
//! Like Jira's and unlike Confluence's, it does not single out 401: a revoked
//! token reports `github API error: status 401: {…}` with GitHub's body
//! attached.

use crate::claude::CancellationToken;
use crate::native::integrations::check::CheckFailure;

use super::client::{api_base, http_client, read_capped};

/// `io.LimitReader(resp.Body, 2*1024*1024)`.
const MAX_VALIDATE_BYTES: usize = 2 * 1024 * 1024;

/// `userResponse` — GitHub sends a large object and Go reads one field.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct User {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    login: String,
}

/// `ValidatePAT(ctx, token)` — the GitHub login on success.
///
/// Like Jira's, the 401/403 pair does not change the *sentence* and does decide
/// the [`CheckFailure`]: only those two are GitHub answering about the token.
pub async fn validate_pat(ct: &CancellationToken, token: &str) -> Result<String, CheckFailure> {
    let failed = "calling GitHub /user: request failed".to_string();
    let url = reqwest::Url::parse(&format!("{}/user", api_base()))
        .map_err(|e| CheckFailure::unreachable(format!("creating GitHub user request: {e}")))?;

    let request = http_client()
        .ok_or_else(|| CheckFailure::unreachable(failed.clone()))?
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github.v3+json");

    // Go discards `client.Do`'s error rather than wrapping it — the request
    // carries the personal access token in a header.
    let response = tokio::select! {
        () = ct.cancelled() => return Err(CheckFailure::unreachable(failed.clone())),
        result = request.send() => result.map_err(|_| CheckFailure::unreachable(failed))?,
    };

    let status = response.status().as_u16();
    // `io.LimitReader(resp.Body, 2*1024*1024)` — auth.go's own cap, which
    // happens to equal `tools.go`'s; GitHub's `read_capped` takes it as a
    // parameter because the diff path uses a different one.
    let body = read_capped(ct, response, MAX_VALIDATE_BYTES)
        .await
        .map_err(|e| CheckFailure::unreachable(format!("reading GitHub response: {e}")))?;

    if status != 200 {
        return Err(CheckFailure::from_status(
            status,
            format!("github API error: status {status}: {body}"),
        ));
    }

    let user: User = serde_json::from_str::<Option<crate::native::gojson::GoStruct<User>>>(&body)
        .map(|wrapped| wrapped.map_or_else(User::default, |wrapped| wrapped.0))
        .map_err(|e| CheckFailure::unreachable(format!("parsing GitHub user response: {e}")))?;
    Ok(user.login)
}

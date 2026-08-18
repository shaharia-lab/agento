//! `ValidateToken` — `internal/integrations/slack/validate.go`.
//!
//! The one validator that checks a status **before** reading the body, and the
//! only one that reads a response *header*: a 429 answers `slack rate limited,
//! retry after {Retry-After} seconds` and never touches the body. An absent
//! `Retry-After` interpolates the empty string, so the sentence reads
//! `retry after  seconds` — two spaces. That is Go's output and it is
//! reproduced rather than tidied.
//!
//! Everything else is Slack's usual envelope: the HTTP status is ignored and
//! `ok` decides, exactly as in `callSlack`.
//!
//! # The quirk this port must carry, not fix
//!
//! `validateSlackTokenAuth` accepts an integration whose `auth_mode` is
//! `"oauth"` — `validateSlackCredentials` only requires `client_id` and
//! `client_secret` there — and then calls this function with
//! `creds.BotToken`, which in that mode is **empty**. So an OAuth-mode Slack
//! integration sends `Authorization: Bearer ` and Slack answers `not_authed`.
//! The user sees `slack API error: not_authed`. Reproducing that is the point:
//! the alternative is a native handler that succeeds where Go fails.

use crate::claude::CancellationToken;

use super::client::{api_base, http_client, read_capped};

/// `authTestResponse`. Go declares eight fields and reads two; the rest are
/// here so a wrongly-typed response fails the decode where Go's fails.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
#[allow(dead_code)]
struct AuthTest {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    ok: bool,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    error: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    url: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    team: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    user: String,
    #[serde(
        rename = "team_id",
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    team_id: String,
    #[serde(
        rename = "user_id",
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    user_id: String,
    #[serde(
        rename = "bot_id",
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    bot_id: String,
}

/// `ValidateToken(ctx, token)` — the team name on success.
pub async fn validate_token(ct: &CancellationToken, token: &str) -> Result<String, String> {
    let failed = "calling Slack auth.test: request failed".to_string();
    let url = reqwest::Url::parse(&format!("{}/auth.test", api_base()))
        .map_err(|e| format!("creating Slack auth.test request: {e}"))?;

    // A POST with **no body** and `Content-Type: application/json` — Go builds
    // it with a nil body and sets the header anyway.
    let request = http_client()
        .ok_or_else(|| failed.clone())?
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");

    // Go discards `client.Do`'s error: the request carries the bot token.
    let response = tokio::select! {
        () = ct.cancelled() => return Err(failed.clone()),
        result = request.send() => result.map_err(|_| failed)?,
    };

    // Checked before the body is read, and it returns without reading it.
    if response.status().as_u16() == 429 {
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        return Err(format!(
            "slack rate limited, retry after {retry_after} seconds"
        ));
    }

    let body = read_capped(ct, response)
        .await
        .map_err(|e| format!("reading Slack response: {e}"))?;

    let result: AuthTest =
        serde_json::from_str::<Option<crate::native::gojson::GoStruct<AuthTest>>>(&body)
            .map(|wrapped| wrapped.map_or_else(AuthTest::default, |wrapped| wrapped.0))
            .map_err(|e| format!("parsing Slack response: {e}"))?;

    if !result.ok {
        return Err(format!("slack API error: {}", result.error));
    }
    Ok(result.team)
}

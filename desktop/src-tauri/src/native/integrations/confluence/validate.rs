//! `ValidateCredentials` — `internal/integrations/confluence/validate.go`.
//!
//! The only one of the five that reads the HTTP **status**, and it reads it in
//! two steps: 401 and 403 share one sentence naming the two credential fields,
//! and any other non-200 carries the status and the body verbatim. Order
//! matters — the auth statuses are checked first, so a 401 never reports its
//! body, which would be the site's login page.
//!
//! Two things here are `validate.go`'s own rather than `tools.go`'s, and both
//! are observable: the read cap is **1 MiB** (a quarter of the tool client's),
//! and it is the body a non-200 reports. The 30-second client *is* shared,
//! because both files declare the same timeout.
//!
//! `ValidateSiteURL` is not re-implemented: [`super::validate_site_url`] is
//! already its port, `%q`-rendered scheme included, and it is what the MCP
//! server's own `Start` calls.
//!
//! The response is decoded into `confluenceSpacesResponse` and **thrown away**.
//! That is not dead code to delete: a 200 carrying non-JSON is a failure, and
//! dropping the decode would turn it into a success.

use crate::claude::CancellationToken;

use super::client::{http_client, read_capped_at};

/// `io.LimitReader(resp.Body, 1*1024*1024)` — validate.go's own cap.
const MAX_VALIDATE_BYTES: usize = 1024 * 1024;

/// `confluenceSpacesResponse`. Only its *shape* is used — see the module header
/// — so every field is unread by design: they exist so that a response of the
/// wrong shape fails the decode exactly where Go's fails.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
#[allow(dead_code)]
struct SpacesResponse {
    results: crate::native::gojson::GoList<Space>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
#[allow(dead_code)]
struct Space {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    id: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    key: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    name: String,
}

/// `ValidateCredentials(ctx, siteURL, email, apiToken)`.
pub async fn validate_credentials(
    ct: &CancellationToken,
    site_url: &str,
    email: &str,
    api_token: &str,
) -> Result<(), String> {
    // Go returns `ValidateSiteURL`'s error unwrapped, so its sentence is the
    // whole message.
    let clean = super::validate_site_url(site_url)?;

    let failed = "calling confluence API: request failed".to_string();
    let url = reqwest::Url::parse(&format!("{clean}/wiki/api/v2/spaces?limit=1"))
        // `http.NewRequestWithContext` failing is `creating confluence request:
        // %w`; the wording is `net/http`'s, so this carries reqwest's.
        .map_err(|e| format!("creating confluence request: {e}"))?;

    let request = http_client()
        .ok_or_else(|| failed.clone())?
        .get(url)
        // `req.SetBasicAuth(email, apiToken)`.
        .basic_auth(email, Some(api_token))
        .header("Accept", "application/json");

    // Go discards `client.Do`'s error rather than wrapping it: the URL is the
    // customer's site and the header is a credential.
    let response = tokio::select! {
        () = ct.cancelled() => return Err(failed.clone()),
        result = request.send() => result.map_err(|_| failed)?,
    };

    let status = response.status().as_u16();
    let body = read_capped_at(ct, response, MAX_VALIDATE_BYTES)
        .await
        .map_err(|e| format!("reading confluence response: {e}"))?;

    if status == 401 || status == 403 {
        return Err("invalid credentials: check email and API token".to_string());
    }
    if status != 200 {
        return Err(format!("confluence API returned status {status}: {body}"));
    }

    // `json.Unmarshal` into a struct: a bare `null` leaves it zeroed and
    // succeeds, and a JSON array is a type error to Go but decodes positionally
    // in serde — hence the `Option` and `GoStruct` this codebase uses for both.
    serde_json::from_str::<Option<crate::native::gojson::GoStruct<SpacesResponse>>>(&body)
        .map_err(|e| format!("parsing confluence response: {e}"))?;
    Ok(())
}

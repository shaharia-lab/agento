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
//! # Its refusals are not all reproducible, and now they are visible
//!
//! `super::validate_site_url` reproduces the *classification* of `url.Parse`'s
//! own refusals but not their wording — that is `net/url`'s vocabulary over the
//! caller's input, and its header records the trade. It could make that trade
//! because the message was **a log line**: `Start`'s error is logged by the
//! registry and reaches neither a response nor the model.
//!
//! #318 changed that: this function's error is interpolated into the 400 body
//! `auth/validate` answers. Hence [`Refusal`], which keeps the two apart: the
//! rules stated outright (HTTPS, and a hostname) carry their inherited
//! sentences under `credentials`, and the rest — this build's own `invalid site
//! URL: …` wording — is answered under `credentials.site_url`. Both are 400s
//! (#670); the second was a 500 while its text had to match another
//! implementation's. Neither has called anything yet.
//!
//! The response is decoded into `confluenceSpacesResponse` and **thrown away**.
//! That is not dead code to delete: a 200 carrying non-JSON is a failure, and
//! dropping the decode would turn it into a success.

use crate::claude::CancellationToken;
use crate::native::integrations::check::CheckFailure;

use super::client::{http_client, read_capped_at};

/// Why a validation failed, and whether this port can spell Go's sentence.
pub enum Refusal {
    /// A sentence Go produces verbatim, safe to put on the wire — carrying the
    /// outcome's kind (#521) as well as its wording.
    Reproducible(CheckFailure),
    /// This build's own wording for a site URL it will not send a request to.
    /// The caller answers it as a 400 on `credentials.site_url` (#670). It can
    /// only arise before the network call, which is why it needs no kind:
    /// nothing was asked, so nothing was refused.
    SiteUrl(String),
}

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
) -> Result<(), Refusal> {
    // Go returns `ValidateSiteURL`'s error unwrapped, so its sentence is the
    // whole message — for the two rules it states itself. The rest is
    // `url.Parse`'s wording; see the module header.
    let clean = super::validate_site_url(site_url).map_err(|e| {
        if e.starts_with("invalid site URL: ") {
            Refusal::SiteUrl(e)
        } else {
            // A site URL this build refuses outright never reached Atlassian,
            // so it says nothing about the token.
            Refusal::Reproducible(CheckFailure::unreachable(e))
        }
    })?;

    let failed = "calling confluence API: request failed".to_string();
    let url = reqwest::Url::parse(&format!("{clean}/wiki/api/v2/spaces?limit=1"))
        // The cleaned site URL does not make a request URL: the same refusal
        // as the ones above, and nothing has been called yet.
        .map_err(|e| Refusal::SiteUrl(format!("invalid site URL: {e}")))?;

    let request = http_client()
        .ok_or_else(|| Refusal::Reproducible(CheckFailure::unreachable(failed.clone())))?
        .get(url)
        // `req.SetBasicAuth(email, apiToken)`.
        .basic_auth(email, Some(api_token))
        .header("Accept", "application/json");

    // Go discards `client.Do`'s error rather than wrapping it: the URL is the
    // customer's site and the header is a credential.
    let response = tokio::select! {
        () = ct.cancelled() => {
            return Err(Refusal::Reproducible(CheckFailure::unreachable(failed.clone())))
        }
        result = request.send() => {
            result.map_err(|_| Refusal::Reproducible(CheckFailure::unreachable(failed)))?
        }
    };

    let status = response.status().as_u16();
    let body = read_capped_at(ct, response, MAX_VALIDATE_BYTES)
        .await
        .map_err(|e| {
            Refusal::Reproducible(CheckFailure::unreachable(format!(
                "reading confluence response: {e}"
            )))
        })?;

    // The only one of the five whose refusal is already singled out by status,
    // so this arm *is* the rejection — Go's own 401/403 grouping, and the same
    // grouping `CheckFailure::from_status` applies for the other four.
    if status == 401 || status == 403 {
        return Err(Refusal::Reproducible(CheckFailure::rejected(
            "invalid credentials: check email and API token",
        )));
    }
    if status != 200 {
        return Err(Refusal::Reproducible(CheckFailure::unreachable(format!(
            "confluence API returned status {status}: {body}"
        ))));
    }

    // `json.Unmarshal` into a struct: a bare `null` leaves it zeroed and
    // succeeds, and a JSON array is a type error to Go but decodes positionally
    // in serde — hence the `Option` and `GoStruct` this codebase uses for both.
    serde_json::from_str::<Option<crate::native::gojson::GoStruct<SpacesResponse>>>(&body)
        .map_err(|e| {
            Refusal::Reproducible(CheckFailure::unreachable(format!(
                "parsing confluence response: {e}"
            )))
        })?;
    Ok(())
}

//! `ValidateBotToken` — `internal/integrations/telegram/validate.go`.
//!
//! Go keeps this in its own file with its own `http.Client`, separate from
//! `tools.go`, and the port keeps that separation for a reason that is not
//! tidiness: `callTelegram` and `ValidateBotToken` hit the same API with
//! **different requests and different error sentences**, and reusing
//! [`super::client::Client::call`] here would have been wrong twice over.
//!
//! - `call` POSTs with a JSON body; this GETs with none.
//! - `call`'s parse failure is `parsing response: …`; this one is `parsing
//!   Telegram response: …`. Both strings reach the client, in the `error` field
//!   of the 400 `auth/validate` answers.
//!
//! What they *do* share is the 60-second client and the 10 MiB read cap, so
//! those come from `client` rather than being spelled a second time.
//!
//! The HTTP status is never looked at — `ok` in the envelope decides, exactly as
//! in `callTelegram`.

use crate::claude::CancellationToken;
use crate::native::integrations::check::CheckFailure;

use super::client::{api_base, http_client, read_capped, TelegramResponse};

/// `botUser` — only `username` is read, but the decode has to fail where Go's
/// fails, so the other three are declared with their Go types. `id` is an
/// `int64` there, and a `{"id":"1"}` is a type error to both languages only
/// because it is declared here.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
#[allow(dead_code)]
struct BotUser {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    id: i64,
    #[serde(
        rename = "is_bot",
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    is_bot: bool,
    #[serde(
        rename = "first_name",
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    first_name: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    username: String,
}

/// `ValidateBotToken(ctx, token)` — the bot's username on success.
///
/// The status is never looked at, so the *only* [`CheckFailure::rejected`] here
/// is the envelope's `ok:false` — a refusal Telegram delivers with HTTP 200.
/// Everything else is a transport or decode failure and changes nothing.
pub async fn validate_bot_token(
    ct: &CancellationToken,
    token: &str,
) -> Result<String, CheckFailure> {
    // Every failure between here and the response is this one sentence: Go
    // discards `client.Do`'s error rather than wrapping it, because the URL
    // carries the bot token and the error would print it.
    let failed = "calling Telegram getMe: request failed".to_string();

    let url = endpoint(token).ok_or_else(|| CheckFailure::unreachable(failed.clone()))?;
    let request = http_client()
        .ok_or_else(|| CheckFailure::unreachable(failed.clone()))?
        .get(url);

    let response = tokio::select! {
        () = ct.cancelled() => return Err(CheckFailure::unreachable(failed.clone())),
        result = request.send() => result.map_err(|_| CheckFailure::unreachable(failed))?,
    };

    let body = read_capped(ct, response)
        .await
        // `fmt.Errorf("reading Telegram response: %w", err)`.
        .map_err(|e| CheckFailure::unreachable(format!("reading Telegram response: {e}")))?;

    // `json.Unmarshal` into a struct: a bare `null` body leaves it zeroed and
    // returns no error, so Go falls through to the `!ok` branch with an empty
    // description rather than reporting a parse failure. A JSON array is a type
    // error to Go but decodes positionally in serde, hence `GoStruct`.
    let envelope =
        serde_json::from_str::<Option<crate::native::gojson::GoStruct<TelegramResponse>>>(&body)
            .map(|wrapped| wrapped.map_or_else(TelegramResponse::default, |wrapped| wrapped.0))
            // `encoding/json`'s wording is not reproducible, so this carries serde's —
            // the same pinned divergence `client::read_response` records. It cannot
            // leak the token: what is being parsed is Telegram's response.
            .map_err(|e| CheckFailure::unreachable(format!("parsing Telegram response: {e}")))?;

    if !envelope.ok {
        // Telegram refuses a bad token inside a 200 — this is the refusal.
        return Err(CheckFailure::rejected(format!(
            "telegram API error: {}",
            envelope.description
        )));
    }

    // `json.Unmarshal(tgResp.Result, &bot)`. An *absent* `result` is `nil` to
    // Go, and `json.Unmarshal(nil, …)` is `unexpected end of JSON input` — a
    // parse failure, not an empty username. `result()` renders that case as
    // `""`, which serde also rejects, so the two agree on which branch is taken
    // even though the message differs.
    let bot: BotUser =
        serde_json::from_str::<Option<crate::native::gojson::GoStruct<BotUser>>>(envelope.result())
            .map(|wrapped| wrapped.map_or_else(BotUser::default, |wrapped| wrapped.0))
            .map_err(|e| CheckFailure::unreachable(format!("parsing bot user: {e}")))?;

    Ok(bot.username)
}

/// `apiURL(token, "getMe")`, with the same rejection of a token that would
/// escape its path segment that [`super::client::Client::endpoint`] applies.
fn endpoint(token: &str) -> Option<reqwest::Url> {
    let path = format!("/bot{token}/getMe");
    let url = reqwest::Url::parse(&format!("{}{path}", api_base())).ok()?;
    (url.path() == path && url.query().is_none()).then_some(url)
}

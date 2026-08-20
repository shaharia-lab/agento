//! Trading an authorization code for a token, and storing it the way Go stores
//! it. Mirrors `oauth2.Config.Exchange` and `IntegrationConfig.SetOAuthToken`.
//!
//! # This is the half with no live diff and no vector
//!
//! The authorization URL can be pinned against Go (`super`'s vectors) because it
//! is a pure function. The exchange is a **network call to the provider**, so
//! neither a golden file nor the live parity suite can reach it — and a mistake
//! here is not a wrong byte in a response, it is an integration that silently
//! never authenticates. So the request shape is pinned instead, against a fake
//! token endpoint that records what it was sent: the same technique
//! `tests/chat_turn.rs` uses for the CLI.
//!
//! # The two provider differences that are invisible until they fail
//!
//! `oauth2` decides where the client credentials go from the endpoint's
//! `AuthStyle`, and the two providers here disagree:
//!
//! - **Google** declares `AuthStyleInParams`, so `client_id` and
//!   `client_secret` are form fields in the body and there is no
//!   `Authorization` header.
//! - **Slack** declares no style at all, which is `AuthStyleAutoDetect`: the
//!   library tries **`AuthStyleInHeader` first** — HTTP Basic, with the id and
//!   secret URL-encoded per RFC 6749 §2.3.1 — and only retries in the body if
//!   that attempt fails. Reproducing "params first" would work against a server
//!   that accepts both and fail against one that does not, which is exactly the
//!   kind of difference that shows up as "OAuth works in the web app but not the
//!   desktop one".
//!
//! # The stored shape is Go's `json.Marshal(*oauth2.Token)`
//!
//! `integrations.auth` is read by the integration MCP servers — including the
//! sidecar's, since `StartFilteredServer` is ungated and serves agent runs Go
//! answers. So the column has to hold what Go would have written:
//!
//! - `access_token` always;
//! - `token_type`, `refresh_token`, `expires_in` with `omitempty`;
//! - **`expiry` always**, because `omitempty` does not omit a struct — a zero
//!   `time.Time` marshals as `"0001-01-01T00:00:00Z"` rather than disappearing.

use std::collections::BTreeMap;

use serde::Serialize;

/// Where a provider's token endpoint wants the client credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    /// `oauth2.AuthStyleInParams` — form fields in the body.
    InParams,
    /// `oauth2.AuthStyleInHeader` — HTTP Basic.
    InHeader,
    /// `oauth2.AuthStyleAutoDetect`: header first, then params on failure.
    AutoDetect,
}

/// One provider's token endpoint.
#[derive(Debug, Clone)]
pub struct TokenEndpoint {
    pub url: String,
    pub style: AuthStyle,
}

impl TokenEndpoint {
    /// `googleoauth.Endpoint`.
    pub fn google() -> Self {
        Self {
            url: "https://oauth2.googleapis.com/token".to_string(),
            style: AuthStyle::InParams,
        }
    }

    /// Slack's v2 access endpoint, which declares no `AuthStyle`.
    pub fn slack() -> Self {
        Self {
            url: "https://slack.com/api/oauth.v2.access".to_string(),
            style: AuthStyle::AutoDetect,
        }
    }
}

/// `oauth2.Token`, in the field order Go declares — which is the order
/// `encoding/json` writes them and therefore the order stored in the column.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StoredToken {
    pub access_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub token_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    /// Always serialized: `omitempty` does not omit a `time.Time`, so Go writes
    /// `"0001-01-01T00:00:00Z"` for a token with no expiry rather than dropping
    /// the key.
    pub expiry: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub expires_in: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Go's zero `time.Time` as `encoding/json` renders it.
pub const ZERO_TIME: &str = "0001-01-01T00:00:00Z";

/// What a token endpoint answered, before it becomes a [`StoredToken`].
#[derive(Debug, Default, serde::Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    refresh_token: String,
    /// Seconds. `0` means the provider sent none, which is a token that never
    /// expires — Go leaves `Expiry` at its zero value in that case.
    #[serde(default)]
    expires_in: i64,
}

/// `oauth2.Config.Exchange`.
///
/// `now` is a parameter because `Expiry` is `now + expires_in` and a test that
/// could not fix the clock could only assert that a timestamp was *some* value.
pub async fn exchange(
    client: &reqwest::Client,
    endpoint: &TokenEndpoint,
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    now: std::time::SystemTime,
) -> Result<StoredToken, String> {
    let attempts: &[AuthStyle] = match endpoint.style {
        AuthStyle::InParams => &[AuthStyle::InParams],
        AuthStyle::InHeader => &[AuthStyle::InHeader],
        // "the first way we'll try", then "the second way we'll try".
        AuthStyle::AutoDetect => &[AuthStyle::InHeader, AuthStyle::InParams],
    };

    let mut last: Option<String> = None;
    for style in attempts {
        match post_token(
            client,
            endpoint,
            *style,
            client_id,
            client_secret,
            code,
            redirect_uri,
        )
        .await
        {
            Ok(response) => return Ok(store(response, now)),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| "exchanging code: no attempt was made".to_string()))
}

async fn post_token(
    client: &reqwest::Client,
    endpoint: &TokenEndpoint,
    style: AuthStyle,
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, String> {
    // `BTreeMap` so the body is deterministic; `url.Values.Encode` sorts too,
    // which is what the fake endpoint's assertions rely on.
    let mut form: BTreeMap<&str, &str> = BTreeMap::new();
    form.insert("grant_type", "authorization_code");
    form.insert("code", code);
    form.insert("redirect_uri", redirect_uri);

    let mut request = client.post(&endpoint.url);
    match style {
        AuthStyle::InParams | AuthStyle::AutoDetect => {
            form.insert("client_id", client_id);
            form.insert("client_secret", client_secret);
        }
        AuthStyle::InHeader => {
            // RFC 6749 §2.3.1: both halves are URL-encoded *before* base64, and
            // `oauth2` does exactly that rather than passing them raw.
            request = request.basic_auth(
                crate::native::gourl::query_escape(client_id),
                Some(crate::native::gourl::query_escape(client_secret)),
            );
        }
    }

    let response = request
        .form(&form)
        .send()
        .await
        // The cause is deliberately not interpolated, matching every other
        // ported client: the message describes the request.
        .map_err(|_| "exchanging code: request failed".to_string())?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| "exchanging code: reading response failed".to_string())?;
    if !status.is_success() {
        return Err(format!(
            "exchanging code: server response: {} {}",
            status.as_u16(),
            body.trim()
        ));
    }

    let decoded: TokenResponse = serde_json::from_str(&body)
        .map_err(|e| format!("exchanging code: parsing token response: {e}"))?;
    if decoded.access_token.is_empty() {
        // `oauth2` raises this rather than returning an empty token, and the
        // difference matters: an empty token stored would read as authenticated.
        return Err("exchanging code: server response missing access_token".to_string());
    }
    Ok(decoded)
}

/// `tokenFromInternal` plus `Token.Expiry = now + expires_in`.
fn store(response: TokenResponse, now: std::time::SystemTime) -> StoredToken {
    let expiry = if response.expires_in > 0 {
        let at = now + std::time::Duration::from_secs(response.expires_in as u64);
        let at: chrono::DateTime<chrono::Utc> = at.into();
        // `time.Time` marshals as RFC 3339 with a variable-length fraction; the
        // exchange sets a whole-second expiry, so there is never one here.
        crate::native::gotime::GoTime::from_utc(at).to_rfc3339_nano()
    } else {
        ZERO_TIME.to_string()
    };
    StoredToken {
        access_token: response.access_token,
        token_type: response.token_type,
        refresh_token: response.refresh_token,
        expiry,
        expires_in: response.expires_in,
    }
}

/// The `auth` column's bytes, as `SetOAuthToken` writes them.
pub fn encode(token: &StoredToken) -> Result<String, String> {
    let bytes = crate::native::gojson::to_vec_marshal(token)
        .map_err(|e| format!("marshaling oauth token: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("marshaling oauth token: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stored_shape_is_gos_marshalled_token() {
        let full = StoredToken {
            access_token: "at".into(),
            token_type: "Bearer".into(),
            refresh_token: "rt".into(),
            expiry: "2026-08-18T10:00:00Z".into(),
            expires_in: 3599,
        };
        assert_eq!(
            encode(&full).expect("encode"),
            r#"{"access_token":"at","token_type":"Bearer","refresh_token":"rt","expiry":"2026-08-18T10:00:00Z","expires_in":3599}"#,
            "field order is the Go struct's declaration order"
        );

        // A Slack token: no refresh, no expiry. `expiry` survives as the zero
        // time because `omitempty` does not omit a struct — getting this wrong
        // drops a key Go always writes.
        let bare = StoredToken {
            access_token: "xoxb-1".into(),
            token_type: "bot".into(),
            expiry: ZERO_TIME.into(),
            ..Default::default()
        };
        assert_eq!(
            encode(&bare).expect("encode"),
            r#"{"access_token":"xoxb-1","token_type":"bot","expiry":"0001-01-01T00:00:00Z"}"#
        );
    }

    #[test]
    fn an_expiry_is_now_plus_expires_in_and_absent_when_the_provider_sends_none() {
        // 1_800_000_000 is 2027-01-15T08:00:00Z.
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
        let with = store(
            TokenResponse {
                access_token: "at".into(),
                expires_in: 3600,
                ..Default::default()
            },
            now,
        );
        assert_eq!(with.expiry, "2027-01-15T09:00:00Z", "now + expires_in");
        assert_eq!(with.expires_in, 3600);

        let without = store(
            TokenResponse {
                access_token: "at".into(),
                ..Default::default()
            },
            now,
        );
        assert_eq!(
            without.expiry, ZERO_TIME,
            "no expires_in is a token that never expires, not one expiring now"
        );
        assert_eq!(without.expires_in, 0, "…and the field is then omitted");
    }
}

#[cfg(test)]
mod tests_wire {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What the fake token endpoint was sent.
    #[derive(Debug, Clone, Default)]
    struct Seen {
        authorization: Option<String>,
        content_type: Option<String>,
        body: String,
    }

    /// The fake's state: what it has been sent, and what it has left to answer.
    type FakeState = (Arc<Mutex<Vec<Seen>>>, Arc<Mutex<Vec<(u16, String)>>>);

    /// A token endpoint that records each request and answers what it is told
    /// to. Returns its base URL and the log.
    ///
    /// This is the only way to verify the exchange: it is a call to a provider,
    /// so there is neither a golden file nor a live diff, and the failure mode
    /// is not a wrong byte but an integration that never authenticates.
    async fn fake_endpoint(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
        use axum::extract::State;
        use axum::routing::post;

        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(replies.into_iter().collect::<Vec<_>>()));

        let state = (Arc::clone(&seen), Arc::clone(&replies));
        let app = axum::Router::new()
            .route(
                "/token",
                post(
                    |State((seen, replies)): State<FakeState>,
                     headers: axum::http::HeaderMap,
                     body: String| async move {
                        let header = |name: &str| {
                            headers
                                .get(name)
                                .and_then(|v| v.to_str().ok())
                                .map(|s| s.to_string())
                        };
                        seen.lock().expect("lock").push(Seen {
                            authorization: header("authorization"),
                            content_type: header("content-type"),
                            body,
                        });
                        let mut replies = replies.lock().expect("lock");
                        let (status, payload) = if replies.is_empty() {
                            (200, "{}".to_string())
                        } else {
                            replies.remove(0)
                        };
                        (
                            axum::http::StatusCode::from_u16(status).expect("status"),
                            payload,
                        )
                    },
                ),
            )
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/token"), seen)
    }

    fn form_pairs(body: &str) -> BTreeMap<String, String> {
        form_urlencoded::parse(body.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    #[tokio::test]
    async fn google_sends_its_credentials_in_the_body_and_no_authorization_header() {
        let (url, seen) = fake_endpoint(vec![(
            200,
            r#"{"access_token":"ya29.a0","token_type":"Bearer","refresh_token":"1//rt","expires_in":3599}"#
                .to_string(),
        )])
        .await;

        let token = exchange(
            &reqwest::Client::new(),
            &TokenEndpoint {
                url,
                style: AuthStyle::InParams,
            },
            "cid",
            "secret",
            "the-code",
            "http://localhost:4321/callback",
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
        )
        .await
        .expect("exchange");

        assert_eq!(token.access_token, "ya29.a0");
        assert_eq!(token.refresh_token, "1//rt");
        assert_eq!(token.expiry, "2027-01-15T08:59:59Z");

        let seen = seen.lock().expect("lock");
        assert_eq!(seen.len(), 1, "AuthStyleInParams makes exactly one attempt");
        assert!(
            seen[0].authorization.is_none(),
            "credentials belong in the body for Google, not in a header"
        );
        assert_eq!(
            seen[0].content_type.as_deref(),
            Some("application/x-www-form-urlencoded")
        );
        let form = form_pairs(&seen[0].body);
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("authorization_code")
        );
        assert_eq!(form.get("code").map(String::as_str), Some("the-code"));
        assert_eq!(
            form.get("redirect_uri").map(String::as_str),
            Some("http://localhost:4321/callback"),
            "the provider checks this against what the auth URL declared"
        );
        assert_eq!(form.get("client_id").map(String::as_str), Some("cid"));
        assert_eq!(
            form.get("client_secret").map(String::as_str),
            Some("secret")
        );
    }

    #[tokio::test]
    async fn slack_tries_basic_auth_first_and_falls_back_to_the_body() {
        // The first attempt is refused, exactly as a server that only accepts
        // params would refuse it.
        let (url, seen) = fake_endpoint(vec![
            (401, r#"{"error":"invalid_client"}"#.to_string()),
            (
                200,
                r#"{"access_token":"xoxb-1","token_type":"bot"}"#.to_string(),
            ),
        ])
        .await;

        let token = exchange(
            &reqwest::Client::new(),
            &TokenEndpoint {
                url,
                style: AuthStyle::AutoDetect,
            },
            "9876.5432",
            "slack-secret",
            "code",
            "http://localhost:4321/callback",
            std::time::UNIX_EPOCH,
        )
        .await
        .expect("the retry succeeds");

        assert_eq!(token.access_token, "xoxb-1");
        assert_eq!(
            token.expiry, ZERO_TIME,
            "a Slack v2 token does not expire, so Go stores the zero time"
        );

        let seen = seen.lock().expect("lock");
        assert_eq!(seen.len(), 2, "auto-detect makes two attempts");
        assert!(
            seen[0]
                .authorization
                .as_deref()
                .is_some_and(|v| v.starts_with("Basic ")),
            "the *first* attempt is Basic, which is oauth2's order: {:?}",
            seen[0].authorization
        );
        assert!(
            !form_pairs(&seen[0].body).contains_key("client_secret"),
            "…and it does not also put the secret in the body"
        );
        assert!(
            seen[1].authorization.is_none(),
            "the retry moves the credentials into the body"
        );
        assert_eq!(
            form_pairs(&seen[1].body)
                .get("client_secret")
                .map(String::as_str),
            Some("slack-secret")
        );
    }

    #[tokio::test]
    async fn a_response_with_no_access_token_is_an_error_rather_than_an_empty_token() {
        // An empty token stored would read as authenticated by
        // `IsAuthenticated`, which only checks that `auth` is non-empty.
        let (url, _seen) =
            fake_endpoint(vec![(200, r#"{"token_type":"Bearer"}"#.to_string())]).await;
        let err = exchange(
            &reqwest::Client::new(),
            &TokenEndpoint {
                url,
                style: AuthStyle::InParams,
            },
            "cid",
            "secret",
            "code",
            "http://localhost:4321/callback",
            std::time::UNIX_EPOCH,
        )
        .await
        .unwrap_err();
        assert!(err.contains("missing access_token"), "{err}");
    }
}

//! `GET /api/integrations/{id}/slack/channels` (#641): the channels a Slack
//! integration can see, for the Tasks form's channel picker.
//!
//! Rules, each enforced here:
//!
//! - **Async registry, buffered answer.** The route calls Slack, so it is a
//!   `StreamEndpoint` like `gateway_api`'s catalog route, and answers a finished
//!   document. `integrations::route_of` refuses the two extra segments, so the
//!   buffered registry never claims the path
//!   (`the_buffered_integrations_registry_does_not_claim_the_route`).
//! - **The token is the delivery token.** `registry::slack_delivery_token`
//!   applies delivery's own checks, so `is_member` is relative to the identity
//!   that will post. Unknown id → 404; not Slack, disabled, not connected or no
//!   token → 400.
//! - **Every page, bounded.** `conversations.list` with
//!   `types=public_channel,private_channel`, `exclude_archived=true` and
//!   `limit=1000`, following `response_metadata.next_cursor` for at most
//!   [`MAX_PAGES`] pages — a cursor that never empties cannot loop.
//! - **Four fields, not Slack's body.** `id`, `name`, `is_private`,
//!   `is_member`, sorted by `name` then `id`, encoded with `gojson::to_vec`.
//! - **`ok` decides; an upstream failure is a 502 sentence.** The client
//!   already treats a 500 carrying `{"ok":true}` as a success; [`readable`]
//!   turns a known `error` code into advice, and nothing it returns can carry
//!   the token, because the client's messages never do.

use axum::http::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::claude::CancellationToken;
use crate::native::gojson::{self, GoList, GoStruct};
use crate::native::gourl::Values;
use crate::native::integrations::registry::{self, SlackUnavailable};
use crate::native::{self, Answer, BoxFuture, StreamEndpoint, StreamRequest};

use super::client::{api_error_code, Client};

/// The route, recorded in `integrations::ROUTES` and so in
/// `parity/desktop_routes.json`.
pub const ROUTE: (&str, &str) = ("GET", "/api/integrations/{id}/slack/channels");

/// `10 × 1000` channels. A workspace with more shows the first 10,000 by
/// Slack's order, then sorted; search in the picker covers the rest.
pub(crate) const MAX_PAGES: usize = 10;

/// This route's entry in `native::STREAM_ENDPOINTS`.
pub const STREAM_ENDPOINT: StreamEndpoint = StreamEndpoint {
    name: "slack-channels",
    claims,
    serve,
};

/// One channel as the picker reads it. Field order is wire order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlackChannel {
    #[serde(deserialize_with = "gojson::null_is_zero_value")]
    pub id: String,
    #[serde(deserialize_with = "gojson::null_is_zero_value")]
    pub name: String,
    #[serde(deserialize_with = "gojson::null_is_zero_value")]
    pub is_private: bool,
    #[serde(deserialize_with = "gojson::null_is_zero_value")]
    pub is_member: bool,
}

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && integration_id(path).is_some()
}

/// The `{id}` of `/api/integrations/{id}/slack/channels`: one non-empty
/// segment, as every other `{id}` route here.
fn integration_id(path: &str) -> Option<&str> {
    let id = path
        .strip_prefix("/api/integrations/")?
        .strip_suffix("/slack/channels")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

fn serve(
    req: StreamRequest,
) -> BoxFuture<'static, Result<axum::http::Response<axum::body::Body>, String>> {
    Box::pin(async move {
        let Some(id) = integration_id(&req.path).map(str::to_string) else {
            return Err(format!(
                "{} {} is claimed but has no id",
                req.method, req.path
            ));
        };
        Ok(native::response(answer(&req.db_path, &id).await?))
    })
}

/// The whole route below the path match, so tests need no HTTP server of
/// Agento's own.
async fn answer(db_path: &std::path::Path, id: &str) -> Result<Answer, String> {
    let path = db_path.to_path_buf();
    let owned = id.to_string();
    let token = match crate::native::db::blocking("slack channel list token", move || {
        registry::slack_delivery_token(&path, &owned)
    })
    .await
    {
        Some(Ok(token)) => token,
        Some(Err(SlackUnavailable::NotFound)) => {
            return Answer::error(
                StatusCode::NOT_FOUND,
                &format!("integration {id:?} not found"),
            );
        }
        Some(Err(reason)) => return Answer::error(StatusCode::BAD_REQUEST, &reason.to_string()),
        None => return Err("reading the Slack integration failed".to_string()),
    };

    match list_all(&token, MAX_PAGES).await {
        Ok(channels) => {
            let body =
                gojson::to_vec(&channels).map_err(|e| format!("encoding slack channels: {e}"))?;
            Ok(Answer::json(body))
        }
        Err(e) => {
            // The id only: the message is token-free, but the id is already in
            // the access line and is all a reader needs to find the row.
            log::warn!("slack channel list for integration {id}: {e}");
            Answer::error(
                StatusCode::BAD_GATEWAY,
                &format!("listing Slack channels: {}", readable(&e)),
            )
        }
    }
}

/// Every page of `conversations.list`, up to `max_pages`, sorted by name then
/// id.
pub(crate) async fn list_all(token: &str, max_pages: usize) -> Result<Vec<SlackChannel>, String> {
    #[derive(Default, Deserialize)]
    struct Meta {
        #[serde(default, deserialize_with = "gojson::null_is_zero_value")]
        next_cursor: String,
    }
    #[derive(Default, Deserialize)]
    struct Page {
        #[serde(default)]
        channels: Option<GoList<GoStruct<SlackChannel>>>,
        #[serde(default)]
        response_metadata: Option<GoStruct<Meta>>,
    }

    let client = Client::new(token);
    // Nothing cancels the list but the client's own 60s timeout per call.
    let ct = CancellationToken::new();
    let mut out = Vec::new();
    let mut cursor = String::new();
    for _ in 0..max_pages {
        let mut values = Values::new();
        values.set("types", "public_channel,private_channel");
        values.set("exclude_archived", "true");
        values.set("limit", "1000");
        if !cursor.is_empty() {
            values.set("cursor", &cursor);
        }
        let body = client
            .call_form(&ct, "conversations.list", values.encode())
            .await?;
        // The client has already decided `ok`; a body that decoded as an
        // envelope but not as a page is Slack's shape changing, not ours.
        let page = serde_json::from_str::<Option<GoStruct<Page>>>(&body)
            .map_err(|e| format!("parsing conversations.list: {e}"))?
            .map(|page| page.0)
            .unwrap_or_default();
        out.extend(
            page.channels
                .map(|c| c.0)
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.0),
        );
        cursor = page
            .response_metadata
            .map(|m| m.0.next_cursor)
            .unwrap_or_default();
        if cursor.is_empty() {
            break;
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

/// A client error as the sentence the picker shows beneath its fallback field.
/// Known `error` codes become advice; anything else — a rate limit the client
/// already worded, a failed request, an unknown code — passes through.
fn readable(e: &str) -> String {
    let Some(code) = api_error_code(e) else {
        return e.to_string();
    };
    let sentence = match code {
        "missing_scope" => {
            "the Slack app needs channels:read (and groups:read for private channels) — add the scopes and reinstall the app"
        }
        "invalid_auth" | "token_revoked" | "account_inactive" | "not_authed" => {
            "the Slack token was rejected — reconnect the integration"
        }
        "ratelimited" => "Slack is rate limiting this workspace — try again in a minute",
        _ => return e.to_string(),
    };
    format!("{sentence} ({code})")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use rusqlite::Connection;

    use super::super::client::{api_base_lock, set_api_base};
    use super::*;

    const TOKEN: &str = "xoxb-channels-SUPER-SECRET";

    /// How the fake answers the `n`th call (from 0): status and body.
    type Reply = fn(usize) -> (u16, String);

    #[derive(Clone)]
    struct Fake {
        /// Each request's form body.
        bodies: Arc<Mutex<Vec<String>>>,
        reply: Reply,
    }

    async fn fake_handler(
        State(state): State<Fake>,
        uri: axum::http::Uri,
        body: String,
    ) -> (axum::http::StatusCode, String) {
        assert_eq!(uri.path(), "/conversations.list");
        let n = {
            let mut bodies = state.bodies.lock().expect("lock");
            bodies.push(body);
            bodies.len() - 1
        };
        let (status, body) = (state.reply)(n);
        (StatusCode::from_u16(status).expect("status"), body)
    }

    /// Points the client at a fake answering with `reply`; the caller holds
    /// `api_base_lock` for the test's duration.
    async fn fake(reply: Reply) -> Fake {
        let state = Fake {
            bodies: Arc::default(),
            reply,
        };
        let app = axum::Router::new()
            .fallback(fake_handler)
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));
        state
    }

    fn db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        for (id, kind, enabled, credentials) in [
            (
                "slack",
                "slack",
                true,
                format!(r#"{{"auth_mode":"bot_token","bot_token":"{TOKEN}"}}"#),
            ),
            (
                "off",
                "slack",
                false,
                format!(r#"{{"auth_mode":"bot_token","bot_token":"{TOKEN}"}}"#),
            ),
            (
                "tg",
                "telegram",
                true,
                r#"{"bot_token":"123:abc"}"#.to_string(),
            ),
            (
                "empty",
                "slack",
                true,
                r#"{"auth_mode":"bot_token","bot_token":""}"#.to_string(),
            ),
        ] {
            conn.execute(
                "INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                           created_at, updated_at)
                 VALUES (?1, ?1, ?2, ?3, ?4, '{}', '{}',
                         '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
                rusqlite::params![id, kind, i64::from(enabled), credentials],
            )
            .expect("insert");
        }
        file
    }

    fn body_of(answer: &Answer) -> String {
        String::from_utf8(answer.body.clone().expect("a body")).expect("utf-8")
    }

    #[test]
    fn only_a_get_on_the_exact_shape_is_claimed() {
        assert!(claims(&Method::GET, "/api/integrations/abc/slack/channels"));
        assert!(!claims(
            &Method::POST,
            "/api/integrations/abc/slack/channels"
        ));
        assert!(!claims(&Method::GET, "/api/integrations//slack/channels"));
        assert!(!claims(
            &Method::GET,
            "/api/integrations/a/b/slack/channels"
        ));
        assert!(!claims(
            &Method::GET,
            "/api/integrations/abc/slack/channels/"
        ));
        assert!(!claims(
            &Method::GET,
            "/api/integrations/abc/telegram/channels"
        ));
    }

    /// Both registries claiming one path would leave the buffered handler as a
    /// route that answers "claimed but unhandled"; `proxy.rs` asks the stream
    /// registry first, so it would never even run.
    #[test]
    fn the_buffered_integrations_registry_does_not_claim_the_route() {
        let path = "/api/integrations/abc/slack/channels";
        assert!(!(crate::native::integrations::ENDPOINT.claims)(
            &Method::GET,
            path
        ));
        assert!(crate::native::claims_stream(&Method::GET, path));
    }

    #[tokio::test]
    async fn every_page_is_joined_and_sorted_by_name_then_id() {
        let _guard = api_base_lock().await;
        let fake = fake(|n| match n {
            0 => (
                200,
                r#"{"ok":true,"channels":[
                    {"id":"C3","name":"random","is_private":false,"is_member":true,"topic":{"value":"x"}},
                    {"id":"C2","name":"general","is_private":false,"is_member":false}
                ],"response_metadata":{"next_cursor":"dGVhbTpDMg=="}}"#
                    .to_string(),
            ),
            _ => (
                200,
                r#"{"ok":true,"channels":[
                    {"id":"G1","name":"general","is_private":true,"is_member":true},
                    {"id":"C1","name":"alerts","is_private":null,"is_member":true}
                ],"response_metadata":{"next_cursor":""}}"#
                    .to_string(),
            ),
        })
        .await;

        let answer = answer(db().path(), "slack").await.expect("answered");
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(
            body_of(&answer),
            concat!(
                r#"[{"id":"C1","name":"alerts","is_private":false,"is_member":true},"#,
                r#"{"id":"C2","name":"general","is_private":false,"is_member":false},"#,
                r#"{"id":"G1","name":"general","is_private":true,"is_member":true},"#,
                r#"{"id":"C3","name":"random","is_private":false,"is_member":true}]"#,
                "\n"
            )
        );

        let bodies = fake.bodies.lock().expect("lock").clone();
        assert_eq!(
            bodies,
            vec![
                "exclude_archived=true&limit=1000&types=public_channel%2Cprivate_channel",
                "cursor=dGVhbTpDMg%3D%3D&exclude_archived=true&limit=1000&types=public_channel%2Cprivate_channel",
            ]
        );
        set_api_base(None);
    }

    #[tokio::test]
    async fn a_null_channel_list_is_an_empty_array() {
        let _guard = api_base_lock().await;
        let _fake = fake(|_| {
            (
                200,
                r#"{"ok":true,"channels":null,"response_metadata":null}"#.to_string(),
            )
        })
        .await;
        let answer = answer(db().path(), "slack").await.expect("answered");
        assert_eq!(
            (answer.status, body_of(&answer).as_str()),
            (StatusCode::OK, "[]\n")
        );
        set_api_base(None);
    }

    /// `ok` decides, not the status — the client's rule, visible through the
    /// route.
    #[tokio::test]
    async fn a_500_carrying_ok_true_is_a_success() {
        let _guard = api_base_lock().await;
        let _fake = fake(|_| {
            (
                500,
                r#"{"ok":true,"channels":[{"id":"C1","name":"a","is_private":false,"is_member":true}]}"#
                    .to_string(),
            )
        })
        .await;
        let answer = answer(db().path(), "slack").await.expect("answered");
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(
            body_of(&answer),
            "[{\"id\":\"C1\",\"name\":\"a\",\"is_private\":false,\"is_member\":true}]\n"
        );
        set_api_base(None);
    }

    #[tokio::test]
    async fn a_missing_scope_is_a_502_naming_the_scopes_and_not_the_token() {
        let _guard = api_base_lock().await;
        let _fake = fake(|_| (200, r#"{"ok":false,"error":"missing_scope"}"#.to_string())).await;
        let answer = answer(db().path(), "slack").await.expect("answered");
        assert_eq!(answer.status, StatusCode::BAD_GATEWAY);
        let body = body_of(&answer);
        assert!(body.contains("channels:read"), "{body}");
        assert!(body.contains("groups:read"), "{body}");
        assert!(body.contains("(missing_scope)"), "{body}");
        assert!(!body.contains(TOKEN), "{body}");
        set_api_base(None);
    }

    #[tokio::test]
    async fn a_rate_limit_and_an_unreachable_slack_are_502s() {
        let _guard = api_base_lock().await;
        let _fake = fake(|_| (429, String::new())).await;
        let limited = answer(db().path(), "slack").await.expect("answered");
        assert_eq!(limited.status, StatusCode::BAD_GATEWAY);
        assert!(
            body_of(&limited).contains("rate limited"),
            "{}",
            body_of(&limited)
        );

        set_api_base(Some("http://127.0.0.1:1".to_string()));
        let down = answer(db().path(), "slack").await.expect("answered");
        assert_eq!(down.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            body_of(&down),
            "{\"error\":\"listing Slack channels: calling Slack conversations.list: request failed\"}\n"
        );
        set_api_base(None);
    }

    /// A cursor that never empties stops at the cap rather than looping.
    #[tokio::test]
    async fn the_page_cap_stops_a_looping_cursor() {
        let _guard = api_base_lock().await;
        let fake = fake(|n| {
            (
                200,
                format!(
                    r#"{{"ok":true,"channels":[{{"id":"C{n}","name":"c{n}","is_private":false,"is_member":true}}],"response_metadata":{{"next_cursor":"again"}}}}"#
                ),
            )
        })
        .await;
        let channels = list_all(TOKEN, MAX_PAGES).await.expect("listed");
        assert_eq!(channels.len(), MAX_PAGES);
        assert_eq!(fake.bodies.lock().expect("lock").len(), MAX_PAGES);
        set_api_base(None);
    }

    /// Every refusal happens before any network call: the API base points at a
    /// closed port, so a call would answer 502 rather than the 404 or 400.
    #[tokio::test]
    async fn an_unusable_integration_is_refused_before_slack_is_asked() {
        let _guard = api_base_lock().await;
        set_api_base(Some("http://127.0.0.1:1".to_string()));
        let file = db();
        let cases = [
            ("nope", StatusCode::NOT_FOUND),
            ("tg", StatusCode::BAD_REQUEST),
            ("off", StatusCode::BAD_REQUEST),
            ("empty", StatusCode::BAD_REQUEST),
        ];
        for (id, status) in cases {
            let answer = answer(file.path(), id).await.expect("answered");
            assert_eq!(answer.status, status, "{id}: {}", body_of(&answer));
            assert!(!body_of(&answer).contains(TOKEN));
        }
        set_api_base(None);
    }
}

//! The in-flight OAuth flow: the loopback callback server, the state the UI
//! polls, and the token write. Mirrors `startOAuthFlow`, `startProviderCallback`,
//! `handleOAuthToken` and `GetAuthStatus` in
//! `internal/service/integration_service.go`.
//!
//! # Why this is the piece that forced the other two
//!
//! `POST …/auth/start` and `GET …/auth/status` share
//! `integrationService.oauthFlows`, a map in the process that started the flow.
//! Split them across two processes and `status` polls a map that will never be
//! populated — which is why #300 deferred them together and why they move
//! together now.
//!
//! **It also retires a workaround.** While the sidecar owned the callback
//! server, the shell could not see a token land: the browser delivers it
//! straight to a port Go opened, so `native/integrations.rs` inferred the event
//! by watching the UI *poll* `auth/status` and reloading when the row stopped
//! matching what was running (`Trigger::AuthStatusPolled`). With the flow here,
//! the shell writes the token itself and reloads directly, exactly as
//! `handleOAuthToken` does — so the inference is redundant and is removed rather
//! than left running beside the real thing.
//!
//! # Three behaviours that are easy to lose
//!
//! - **The callback server outlives the request.** `StartOAuth` returns the URL
//!   immediately; the user has not opened it yet. Go is explicit about this —
//!   its context is `WithoutCancel` plus a 10-minute deadline, with a comment
//!   warning not to `defer cancel()`. Tying the server to the HTTP request would
//!   kill it before the redirect arrives.
//! - **A failed flow is remembered.** `handleOAuthToken` stores the error on the
//!   state, and `GetAuthStatus` returns it — so the UI's poll surfaces a 500
//!   rather than reporting "not authenticated" forever.
//! - **The token is written against a re-read row.** Go loads the integration
//!   again inside `handleOAuthToken` rather than closing over the one `start`
//!   read, because ten minutes have passed and the row may have been edited.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::native::integrations::registry;
use crate::native::writes::WriteError;

use super::exchange::{self, TokenEndpoint};

/// `oauthState`. Absent from the map means no flow has been started in this
/// process for that integration.
#[derive(Debug, Clone, Default)]
struct FlowState {
    /// Set once the callback has been answered, either way. Go tracks it and
    /// never reads it; kept for the same reason the field exists there.
    #[allow(dead_code)]
    done: bool,
    authenticated: bool,
    /// Why the flow failed. `GetAuthStatus` turns this into a 500.
    err: Option<String>,
}

/// `integrationService.oauthFlows`.
fn flows() -> &'static Mutex<HashMap<String, FlowState>> {
    static FLOWS: OnceLock<Mutex<HashMap<String, FlowState>>> = OnceLock::new();
    FLOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_flows() -> std::sync::MutexGuard<'static, HashMap<String, FlowState>> {
    flows().lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a flow may stay open. `context.WithTimeout(…, 10*time.Minute)`.
const FLOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// `loadOAuthConfig`'s type check, with Go's exact wording.
fn oauth_provider(integration_type: &str) -> Option<TokenEndpoint> {
    match integration_type {
        "google" => Some(TokenEndpoint::google()),
        "slack" => Some(TokenEndpoint::slack()),
        _ => None,
    }
}

/// The client id and secret both providers store under the same two keys.
#[derive(Debug, Default, serde::Deserialize)]
struct ClientCredentials {
    #[serde(
        default,
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    client_id: String,
    #[serde(
        default,
        deserialize_with = "crate::native::gojson::null_is_zero_value"
    )]
    client_secret: String,
}

/// `StartOAuth`: the auth URL the user's browser is sent to.
///
/// Synchronous, because the seam's handler is — the listener is bound with
/// `std::net::TcpListener` (which does not need a runtime) and only the serving
/// half is spawned. Binding here rather than in the spawned task is what lets
/// the port appear in the URL this returns.
pub fn start(db_path: &std::path::Path, id: &str) -> Result<String, WriteError> {
    let Some(row) = registry::get_for_hosting(db_path, id).map_err(WriteError::Fallback)? else {
        return Err(WriteError::NotFound {
            resource: "integration".to_string(),
            id: id.to_string(),
        });
    };
    let Some(endpoint) = oauth_provider(&row.integration_type) else {
        // `loadOAuthConfig`'s `ValidationError`, wording included.
        return Err(WriteError::validation(
            "type",
            format!(
                "OAuth flow is not supported for integration type {:?}",
                row.integration_type
            ),
        ));
    };

    let creds: ClientCredentials = serde_json::from_str(&row.credentials).map_err(|e| {
        // Go's `BuildAuthURL` wraps this as "parsing <type> credentials"; the
        // handler turns any non-typed error into a 500, which the seam forwards.
        WriteError::Fallback(format!("parsing {} credentials: {e}", row.integration_type))
    })?;

    // `integrations.FreePort()`. Bound now and handed to the server, rather than
    // probed and re-bound: the gap between the two is a race a second flow can
    // land in, and the port is already in the URL by then.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| WriteError::Fallback(format!("finding free port: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| WriteError::Fallback(format!("finding free port: {e}")))?
        .port();

    let auth_url = match row.integration_type.as_str() {
        "google" => {
            let services = super::services_of(db_path, id).map_err(WriteError::Fallback)?;
            super::google_auth_url(&creds.client_id, port, &services)
        }
        _ => super::slack_auth_url(&creds.client_id, port),
    };

    // Registered before the server starts, so a callback that arrives
    // impossibly fast still finds a state to write to.
    lock_flows().insert(id.to_string(), FlowState::default());

    let spawned = spawn_callback_server(
        listener,
        CallbackContext {
            db_path: db_path.to_path_buf(),
            id: id.to_string(),
            endpoint,
            client_id: creds.client_id,
            client_secret: creds.client_secret,
            redirect_uri: super::redirect_uri(port),
        },
    );
    if let Err(e) = spawned {
        // Go's `startProviderCallback` cancels and returns the error, leaving no
        // flow behind; the map entry has to go with it or `status` would poll a
        // flow that is not running.
        lock_flows().remove(id);
        return Err(WriteError::Fallback(format!(
            "starting callback server: {e}"
        )));
    }

    log::info!(
        "oauth flow started id={id:?} type={:?}",
        row.integration_type
    );
    Ok(auth_url)
}

/// Everything the callback needs once the browser arrives.
struct CallbackContext {
    db_path: std::path::PathBuf,
    id: String,
    endpoint: TokenEndpoint,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

/// `oauthSuccessHTML`, byte for byte — it is what the user sees.
const SUCCESS_HTML: &str = "<!DOCTYPE html><html><body>\n\
<h2>Authentication successful!</h2>\n\
<p>You can close this tab and return to Agento.</p>\n\
<script>window.close();</script>\n\
</body></html>";

fn spawn_callback_server(
    listener: std::net::TcpListener,
    ctx: CallbackContext,
) -> Result<(), String> {
    use axum::extract::{Query, State};
    use axum::routing::get;

    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| "no tokio runtime on this thread".to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("setting the callback listener non-blocking: {e}"))?;

    let ctx = Arc::new(ctx);
    let app = axum::Router::new()
        .route(
            "/callback",
            get(
                |State(ctx): State<Arc<CallbackContext>>,
                 Query(params): Query<HashMap<String, String>>| async move {
                    handle_callback(ctx, params).await
                },
            ),
        )
        .with_state(Arc::clone(&ctx));

    handle.spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(e) => {
                fail_flow(&ctx.id, format!("callback listener: {e}"));
                return;
            }
        };
        // The deadline is the whole point: without it a flow the user abandons
        // leaves a listener bound for the life of the process.
        let served = tokio::time::timeout(FLOW_TIMEOUT, axum::serve(listener, app)).await;
        if served.is_err() {
            fail_flow(&ctx.id, "oauth flow timed out".to_string());
        }
    });
    Ok(())
}

/// The `/callback` handler: `callbackHandler` plus `handleOAuthToken`.
async fn handle_callback(
    ctx: Arc<CallbackContext>,
    params: HashMap<String, String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let code = params.get("code").map(String::as_str).unwrap_or_default();
    if code.is_empty() {
        // Go prefers the provider's own `error` and falls back to a fixed
        // string, then answers 400 with it.
        let reason = params
            .get("error")
            .filter(|e| !e.is_empty())
            .map(String::as_str)
            .unwrap_or("no code in callback");
        let message = format!("oauth callback error: {reason}");
        fail_flow(&ctx.id, message);
        return (
            axum::http::StatusCode::BAD_REQUEST,
            format!("Authentication failed: {reason}\n"),
        )
            .into_response();
    }

    let token = match exchange::exchange(
        &reqwest::Client::new(),
        &ctx.endpoint,
        &ctx.client_id,
        &ctx.client_secret,
        code,
        &ctx.redirect_uri,
        std::time::SystemTime::now(),
    )
    .await
    {
        Ok(token) => token,
        Err(e) => {
            fail_flow(&ctx.id, e);
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to exchange token\n",
            )
                .into_response();
        }
    };

    if let Err(e) = store_token(&ctx.db_path, &ctx.id, &token) {
        fail_flow(&ctx.id, e);
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to exchange token\n",
        )
            .into_response();
    }

    {
        let mut flows = lock_flows();
        let state = flows.entry(ctx.id.clone()).or_default();
        state.done = true;
        state.authenticated = true;
        state.err = None;
    }
    log::info!(
        "OAuth completed, starting integration server id={:?}",
        ctx.id
    );

    // `go s.registry.Reload(...)` — detached, so a slow start does not hold the
    // browser's redirect open. This is the direct reload that makes
    // `Trigger::AuthStatusPolled` unnecessary.
    let db_path = ctx.db_path.clone();
    let id = ctx.id.clone();
    tokio::spawn(async move {
        registry::reload_after_auth(&db_path, &id).await;
    });

    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        SUCCESS_HTML,
    )
        .into_response()
}

/// `handleOAuthToken`'s error arm: remember why, so the poll can report it.
fn fail_flow(id: &str, message: String) {
    log::warn!("OAuth flow failed id={id:?} error={message}");
    let mut flows = lock_flows();
    let state = flows.entry(id.to_string()).or_default();
    state.done = true;
    state.authenticated = false;
    state.err = Some(message);
}

/// `SetOAuthToken` + `store.Save`, against a **re-read** row.
///
/// Go re-loads the integration inside `handleOAuthToken` rather than using the
/// one `StartOAuth` read: ten minutes may have passed and the row may have been
/// edited or deleted in between.
fn store_token(
    db_path: &std::path::Path,
    id: &str,
    token: &exchange::StoredToken,
) -> Result<(), String> {
    let encoded = exchange::encode(token)?;
    let mut conn = crate::native::db::open_read_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin oauth token write: {e}"))?;

    let exists: bool = tx
        .query_row("SELECT 1 FROM integrations WHERE id = ?1", [id], |_| {
            Ok(true)
        })
        .ok()
        .unwrap_or(false);
    if !exists {
        // Go's `loading integration after OAuth` arm.
        return Err(format!("loading integration after OAuth: {id:?} not found"));
    }

    tx.execute(
        "UPDATE integrations SET auth = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![encoded, crate::native::gotime::now_go_text(), id],
    )
    .map_err(|e| format!("saving token: {e}"))?;
    tx.commit().map_err(|e| format!("saving token: {e}"))
}

/// `GetAuthStatus`.
///
/// An in-flight flow answers from the map; anything else falls through to the
/// stored token. The error arm is what makes a failed flow visible instead of
/// looking like "still not authenticated".
pub fn status(db_path: &std::path::Path, id: &str) -> Result<bool, WriteError> {
    let state = lock_flows().get(id).cloned();
    if let Some(state) = state {
        if let Some(err) = state.err {
            // `httpErr`'s default arm: logged, and answered as a flat 500.
            log::error!("internal server error error={err}");
            return Err(WriteError::Internal("internal server error".to_string()));
        }
        return Ok(state.authenticated);
    }

    match crate::native::integrations::get(db_path, id).map_err(WriteError::Fallback)? {
        Some(integration) => Ok(integration.authenticated),
        None => Err(WriteError::NotFound {
            resource: "integration".to_string(),
            id: id.to_string(),
        }),
    }
}

#[cfg(test)]
pub(super) fn reset_flows_for_test() {
    lock_flows().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh database with one integration, under an id **unique to the
    /// caller**.
    ///
    /// The flow map is process-global — it mirrors `integrationService.oauthFlows`,
    /// which is one map per server — so two tests sharing an id share a flow,
    /// and `cargo test` runs them in parallel. Distinct ids rather than a mutex,
    /// because the global is the thing under test.
    fn migrated(
        dir: &std::path::Path,
        id: &str,
        integration_type: &str,
        credentials: &str,
    ) -> std::path::PathBuf {
        let db = dir.join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                       created_at, updated_at)
             VALUES (?1, 'I', ?2, 1, ?3, '', '{}',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            rusqlite::params![id, integration_type, credentials],
        )
        .expect("seed");
        db
    }

    #[test]
    fn a_type_with_no_oauth_flow_is_gos_422() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "no-oauth", "telegram", r#"{"bot_token":"t"}"#);
        let err = start(&db, "no-oauth").unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            err.message(),
            r#"validation error for "type": OAuth flow is not supported for integration type "telegram""#
        );
    }

    #[test]
    fn an_unknown_integration_is_404() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "unknown-probe",
            "google",
            r#"{"client_id":"c"}"#,
        );
        let err = start(&db, "nope").unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(err.message(), r#"integration "nope" not found"#);
    }

    #[tokio::test]
    async fn starting_a_flow_answers_a_url_carrying_the_port_it_bound() {
        reset_flows_for_test();
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "bound-port",
            "google",
            r#"{"client_id":"cid","client_secret":"sec"}"#,
        );

        let url = start(&db, "bound-port").expect("start");
        // The port is the one just bound, so it cannot be asserted literally —
        // what matters is that the URL and the listener agree, which is the
        // whole reason the bind happens before the URL is built.
        let redirect = url
            .split("redirect_uri=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .expect("a redirect_uri");
        let decoded = redirect.replace("%3A", ":").replace("%2F", "/");
        let port: u16 = decoded
            .trim_start_matches("http://localhost:")
            .trim_end_matches("/callback")
            .parse()
            .expect("a port");
        assert!(port > 0);
        // The listener is live: a second bind of the same port must fail.
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port)).is_err(),
            "the callback server holds the port the URL advertises"
        );

        // …and a flow is now in flight, so `status` answers from the map rather
        // than from the stored token.
        assert!(!status(&db, "bound-port").expect("status"));
    }

    #[tokio::test]
    async fn a_failed_flow_is_remembered_as_a_500_rather_than_reported_as_unauthenticated() {
        reset_flows_for_test();
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "denied",
            "google",
            r#"{"client_id":"cid","client_secret":"sec"}"#,
        );
        start(&db, "denied").expect("start");

        fail_flow("denied", "oauth callback error: access_denied".to_string());

        let err = status(&db, "denied").unwrap_err();
        assert_eq!(
            err.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "a user who denied consent must not see 'still waiting' forever"
        );
    }

    #[test]
    fn with_no_flow_the_status_is_the_stored_token() {
        reset_flows_for_test();
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "stored", "google", r#"{"client_id":"c"}"#);
        assert!(!status(&db, "stored").expect("status"), "auth is empty");

        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute(
            "UPDATE integrations SET auth = ?1 WHERE id = 'stored'",
            [r#"{"access_token":"at","expiry":"0001-01-01T00:00:00Z"}"#],
        )
        .expect("store token");
        drop(conn);
        assert!(status(&db, "stored").expect("status"));
    }

    #[test]
    fn the_stored_token_lands_in_the_auth_column_and_touches_updated_at() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "tok", "google", r#"{"client_id":"c"}"#);
        let token = exchange::StoredToken {
            access_token: "at".into(),
            token_type: "Bearer".into(),
            refresh_token: "rt".into(),
            expiry: "2027-01-15T09:00:00Z".into(),
            expires_in: 3600,
        };
        store_token(&db, "tok", &token).expect("store");

        let conn = rusqlite::Connection::open(&db).expect("open");
        let (auth, updated): (String, String) = conn
            .query_row(
                "SELECT auth, updated_at FROM integrations WHERE id = 'tok'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row");
        assert_eq!(
            auth,
            r#"{"access_token":"at","token_type":"Bearer","refresh_token":"rt","expiry":"2027-01-15T09:00:00Z","expires_in":3600}"#
        );
        assert_ne!(
            updated, "2026-01-01 00:00:00 +0000 UTC",
            "handleOAuthToken stamps UpdatedAt"
        );
    }

    #[test]
    fn a_token_for_a_deleted_integration_is_an_error_rather_than_a_silent_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "present", "google", r#"{"client_id":"c"}"#);
        let err = store_token(&db, "gone", &exchange::StoredToken::default()).unwrap_err();
        assert!(err.contains("loading integration after OAuth"), "{err}");
    }
}

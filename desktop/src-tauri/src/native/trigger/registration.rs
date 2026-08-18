//! Registering, removing and rotating a Telegram webhook. Mirrors
//! `triggerService.{RegisterWebhook,DeleteWebhook,RegenerateSecret}`.
//!
//! # These are the routes that call Telegram, and the order is the whole design
//!
//! The issue's own trap: **fail before the call, never after.** A native handler
//! that returns `Err` forwards to Go, and Go re-runs the whole handler — so an
//! `Err` raised *after* a successful `setWebhook` registers the webhook twice,
//! the second time with whatever secret Go generates, silently invalidating the
//! one this process just stored. Every fallible step therefore happens **before**
//! the network call, and nothing after it may fail:
//!
//! 1. resolve the public URL, the row, the type and the credentials;
//! 2. read or mint the secret;
//! 3. call Telegram;
//! 4. store the outcome — and only log if that write fails.
//!
//! That rule is also why a **failed** `setWebhook` answers `WriteError::Internal`
//! rather than `Fallback`: forwarding would call Telegram a second time, and a
//! transport failure does not prove the first call did nothing — a lost response
//! to a successful registration would be re-registered under Go's secret.
//!
//! Step 4 is where Go and this port part company in one respect worth stating:
//! Go returns an error if `SetWebhookInfo` fails after a *successful*
//! registration, which would forward and re-register. Here that write failure is
//! logged and the request still answers 200, because the webhook **is**
//! registered at that point and telling the caller otherwise is the more
//! damaging lie.
//!
//! # The failure path stores its own status
//!
//! When `setWebhook` fails, Go writes `("error", err.Error())` to the row before
//! returning, so `GET …/webhook/status` can show why. That write is reproduced,
//! and it is the one write that happens on a failure.

use std::path::Path;

use serde::Serialize;

use crate::native::integrations::telegram::client::Client as TelegramClient;
use crate::native::integrations::webhook;
use crate::native::writes::WriteError;

/// `setWebhook`'s payload. Go builds it as `map[string]any`, so `json.Marshal`
/// writes the keys **sorted** — `allowed_updates`, `drop_pending_updates`,
/// `secret_token`, `url`, not the order a person would write them.
#[derive(Serialize)]
struct SetWebhook<'a> {
    allowed_updates: [&'static str; 1],
    drop_pending_updates: bool,
    secret_token: &'a str,
    url: &'a str,
}

/// `GenerateSecretToken`: 32 random bytes, hex-encoded to 64 characters.
fn generate_secret() -> String {
    // `uuid::Uuid::new_v4` is backed by `getrandom`, the same OS entropy
    // `crypto/rand` reads; two of them give the 32 bytes Go asks for.
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What `RegisterWebhook` needs, gathered before anything can fail.
///
/// `Debug` carries the bot token and the secret, so it is **test-only**: the
/// rule `registry::HostingRow` follows for the same reason.
#[cfg_attr(test, derive(Debug))]
struct Registration {
    bot_token: String,
    secret: String,
    webhook_url: String,
}

/// Everything fallible in `RegisterWebhook`, in Go's order, before the call.
fn prepare(db_path: &Path, id: &str) -> Result<Registration, WriteError> {
    // The public URL is checked first, and its absence is a 422 rather than a
    // failed registration — there is nothing to register against.
    let base = webhook::public_url(db_path);
    if base.is_empty() {
        return Err(WriteError::validation(
            "public_url",
            "public URL must be configured before registering a webhook",
        ));
    }

    let Some(row) = crate::native::integrations::registry::get_for_hosting(db_path, id)
        .map_err(WriteError::Fallback)?
    else {
        return Err(WriteError::NotFound {
            resource: "integration".to_string(),
            id: id.to_string(),
        });
    };
    if row.integration_type != "telegram" {
        return Err(WriteError::validation(
            "type",
            "webhooks are only supported for telegram integrations",
        ));
    }

    #[derive(serde::Deserialize)]
    struct Creds {
        #[serde(
            default,
            deserialize_with = "crate::native::gojson::null_is_zero_value"
        )]
        bot_token: String,
    }
    let creds: Creds = serde_json::from_str(&row.credentials).map_err(|e| {
        // Go wraps this and the handler turns it into a 500, which forwards —
        // and forwarding is safe here because nothing has been called yet.
        WriteError::Fallback(format!("parsing telegram credentials: {e}"))
    })?;

    // Reuse the stored secret if there is one, so re-registering does not
    // invalidate a webhook that is already working.
    let stored = read_secret(db_path, id).map_err(WriteError::Fallback)?;
    let secret = if stored.is_empty() {
        generate_secret()
    } else {
        stored
    };

    Ok(Registration {
        webhook_url: webhook::webhook_url(&base, id),
        bot_token: creds.bot_token,
        secret,
    })
}

fn read_secret(db_path: &Path, id: &str) -> Result<String, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    conn.query_row(
        "SELECT webhook_secret FROM integrations WHERE id = ?1",
        [id],
        |row| row.get(0),
    )
    .or_else(|e| match e {
        // `GetWebhookInfo` turns no rows into empty strings.
        rusqlite::Error::QueryReturnedNoRows => Ok(String::new()),
        other => Err(format!("getting webhook info: {other}")),
    })
}

/// `SetWebhookInfo`. Logged rather than returned by its callers after a
/// successful registration — see the module header.
fn store(db_path: &Path, id: &str, secret: &str, status: &str, error: &str) -> Result<(), String> {
    let conn = crate::native::db::open_read_write(db_path)?;
    conn.execute(
        "UPDATE integrations SET webhook_secret = ?1, webhook_status = ?2, webhook_error = ?3
         WHERE id = ?4",
        rusqlite::params![secret, status, error, id],
    )
    .map_err(|e| format!("setting webhook info for {id:?}: {e}"))?;
    Ok(())
}

/// `RegisterWebhook`.
pub async fn register(db_path: &Path, id: &str) -> Result<(), WriteError> {
    let prepared = prepare(db_path, id)?;

    // ── Nothing above this line has called Telegram; nothing below may fail. ──
    let payload = crate::native::gojson::to_vec_marshal(&SetWebhook {
        allowed_updates: ["message"],
        drop_pending_updates: false,
        secret_token: &prepared.secret,
        url: &prepared.webhook_url,
    })
    .map_err(|e| WriteError::Fallback(format!("encoding setWebhook: {e}")))?;

    let client = TelegramClient::new(&prepared.bot_token);
    let ct = tokio_util::sync::CancellationToken::new();
    match client.call(&ct, "setWebhook", payload).await {
        Ok(_) => {
            if let Err(e) = store(db_path, id, &prepared.secret, "active", "") {
                // The webhook **is** registered. Answering an error would
                // forward and register it again, under a different secret.
                log::error!("saving webhook info after a successful registration: {e}");
            }
            log::info!(
                "telegram webhook registered integration_id={id:?} url={:?}",
                prepared.webhook_url
            );
            Ok(())
        }
        Err(e) => {
            // Go records the failure so `webhook/status` can show it, then
            // returns the error. Nothing was registered, so answering an error
            // is safe.
            let message = format!("registering webhook: {e}");
            if let Err(store_err) = store(db_path, id, &prepared.secret, "error", &message) {
                log::warn!("recording webhook registration failure: {store_err}");
            }
            // **Answered here, not forwarded.** `Fallback` would have Go re-run
            // `RegisterWebhook` — a *second* `setWebhook` — and a client-side
            // failure does not prove the first one did nothing: a lost response
            // to a successful call would be re-registered under Go's own secret,
            // which is the exact hazard this module's header claims to avoid.
            // Go answers this with `httpErr`'s flat 500, so that is what goes on
            // the wire, with the reason in the log and on the row.
            log::error!("internal server error error=registering telegram webhook: {message}");
            Err(WriteError::Internal("internal server error".to_string()))
        }
    }
}

/// `DeleteWebhook`.
///
/// **A failed `deleteWebhook` is a warning, not an error** — Go logs it and
/// clears the row anyway, on the reasoning that a webhook the user asked to
/// remove should stop being *ours* whether or not Telegram agreed.
pub async fn delete(db_path: &Path, id: &str) -> Result<(), WriteError> {
    let Some(row) = crate::native::integrations::registry::get_for_hosting(db_path, id)
        .map_err(WriteError::Fallback)?
    else {
        return Err(WriteError::NotFound {
            resource: "integration".to_string(),
            id: id.to_string(),
        });
    };

    #[derive(serde::Deserialize)]
    struct Creds {
        #[serde(
            default,
            deserialize_with = "crate::native::gojson::null_is_zero_value"
        )]
        bot_token: String,
    }
    let creds: Creds = serde_json::from_str(&row.credentials)
        .map_err(|e| WriteError::Fallback(format!("parsing telegram credentials: {e}")))?;

    let client = TelegramClient::new(&creds.bot_token);
    let ct = tokio_util::sync::CancellationToken::new();
    if let Err(e) = client.call(&ct, "deleteWebhook", b"{}".to_vec()).await {
        log::warn!("failed to delete telegram webhook integration_id={id:?} error={e}");
    }

    // The clear is the one write, and its failure *is* returned — unlike the
    // register path, nothing has been left in a state a retry would duplicate.
    store(db_path, id, "", "inactive", "")
        .map_err(|e| WriteError::Fallback(format!("clearing webhook info: {e}")))?;
    log::info!("telegram webhook deleted integration_id={id:?}");
    Ok(())
}

/// `RegenerateSecret`: delete, clear, register.
///
/// The delete's failure is swallowed — Go logs and carries on, because the point
/// is to end up registered with a *new* secret and an old webhook that refused
/// to go away does not change that.
pub async fn regenerate(db_path: &Path, id: &str) -> Result<(), WriteError> {
    if let Err(e) = delete(db_path, id).await {
        log::warn!(
            "failed to delete old webhook before regenerating: {}",
            e.message()
        );
    }
    // Clearing the secret is what makes `prepare` mint a new one rather than
    // reuse the old.
    store(db_path, id, "", "", "")
        .map_err(|e| WriteError::Fallback(format!("clearing old secret: {e}")))?;
    register(db_path, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated(dir: &Path, integration_type: &str, public_url: &str) -> std::path::PathBuf {
        let db = dir.join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO integrations (id, name, type, enabled, credentials, services,
                                       created_at, updated_at)
             VALUES ('tg', 'T', ?1, 1, '{\"bot_token\":\"bot\"}', '{}',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            [integration_type],
        )
        .expect("seed");
        if !public_url.is_empty() {
            conn.execute(
                "INSERT INTO user_settings (id, public_url) VALUES (1, ?1)",
                [public_url],
            )
            .expect("seed settings");
        }
        db
    }

    #[test]
    fn a_secret_is_64_hex_characters_and_not_repeated() {
        let a = generate_secret();
        assert_eq!(a.len(), 64, "32 bytes, hex-encoded");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, generate_secret());
    }

    #[test]
    fn no_public_url_is_a_422_before_anything_is_called() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "telegram", "");
        let err = prepare(&db, "tg").unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            err.message(),
            r#"validation error for "public_url": public URL must be configured before registering a webhook"#
        );
    }

    #[test]
    fn a_non_telegram_integration_is_a_422() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "github", "https://x.example");
        let err = prepare(&db, "tg").unwrap_err();
        assert_eq!(
            err.message(),
            r#"validation error for "type": webhooks are only supported for telegram integrations"#
        );
    }

    #[test]
    fn an_unknown_integration_is_a_404() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "telegram", "https://x.example");
        let err = prepare(&db, "nope").unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn preparation_reuses_a_stored_secret_and_mints_one_otherwise() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "telegram", "https://x.example/");

        let first = prepare(&db, "tg").expect("prepare");
        assert_eq!(first.secret.len(), 64, "minted when the column is empty");
        // The trailing slash on the public URL does not double up.
        assert_eq!(first.webhook_url, "https://x.example/webhooks/telegram/tg");
        assert_eq!(first.bot_token, "bot");

        store(&db, "tg", "existing-secret", "active", "").expect("store");
        let second = prepare(&db, "tg").expect("prepare");
        assert_eq!(
            second.secret, "existing-secret",
            "re-registering must not invalidate a working webhook"
        );
    }

    /// A failed `setWebhook` records why, keeps the secret, and is answered
    /// **here** rather than forwarded.
    ///
    /// Pointed at a local fake rather than Telegram. The first version of this
    /// test claimed "no network here" and was wrong: it issued a real HTTPS POST
    /// to `api.telegram.org` from every `cargo test` run, and offline it passed
    /// for the wrong reason — a transport failure instead of the `ok:false`
    /// envelope it says it covers.
    #[tokio::test]
    async fn a_failed_registration_records_why_and_is_not_forwarded() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};

        let _guard = api_base_lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let app = axum::Router::new().fallback(|| async {
                // What Telegram answers for a bad token: 200 with ok:false.
                (
                    axum::http::StatusCode::OK,
                    r#"{"ok":false,"description":"Unauthorized"}"#,
                )
            });
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(format!("http://{addr}")));

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "telegram", "https://x.example");
        store(&db, "tg", "s3cret", "active", "").expect("store");

        let err = register(&db, "tg").await.unwrap_err();
        set_api_base(None);

        assert_eq!(
            err.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "answered here — forwarding would call setWebhook a second time"
        );
        assert_eq!(err.message(), "internal server error", "Go's httpErr body");

        let status = webhook::status(&db, "tg").expect("status");
        assert_eq!(status.status, "error");
        assert!(
            status.error.contains("Unauthorized"),
            "the provider's own reason reaches the row: {:?}",
            status.error
        );
        assert!(status.has_secret, "the secret survives a failed attempt");
    }

    #[test]
    fn the_set_webhook_payload_is_gos_sorted_map() {
        let body = crate::native::gojson::to_vec_marshal(&SetWebhook {
            allowed_updates: ["message"],
            drop_pending_updates: false,
            secret_token: "s",
            url: "https://x.example/webhooks/telegram/tg",
        })
        .expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf-8"),
            r#"{"allowed_updates":["message"],"drop_pending_updates":false,"secret_token":"s","url":"https://x.example/webhooks/telegram/tg"}"#
        );
    }
}

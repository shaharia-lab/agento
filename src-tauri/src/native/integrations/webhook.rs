//! Telegram webhook state: `GET /api/integrations/{id}/webhook/status` (#319).
//!
//! Mirrors `triggerService.GetWebhookStatus` and `SQLiteTriggerStore.GetWebhookInfo`.
//!
//! # The reason this route stayed with Go was wrong
//!
//! `native/integrations.rs` recorded it as *"asks Telegram, over the network,
//! with the bot token"*. It does not, and never did:
//! `GetWebhookStatus` reads three columns off the `integrations` row
//! (`webhook_secret`, `webhook_status`, `webhook_error`) and composes a URL from
//! the configured public URL. `internal/integrations/telegram` has no status
//! call at all — its whole surface is `RegisterWebhook`, `DeleteWebhook`,
//! `GenerateSecretToken`, `SendChatAction` and `SendReply`. The network calls
//! belong to *registration*, which is a different route.
//!
//! So this is a plain read, and the four-reads-still-Go list is one shorter.
//!
//! # What the read has to get right
//!
//! - **A missing integration is not an error.** `GetWebhookInfo` turns
//!   `sql.ErrNoRows` into four empty strings, so an unknown id answers
//!   `{"status":"inactive","url":"","has_secret":false,"error":""}` with a 200
//!   rather than a 404 — the one `{id}` route that does not check existence.
//! - **`url` is populated only while `status` is `active`**, and only when a
//!   public URL is configured. A registered webhook on an instance whose public
//!   URL was later cleared reports active with no URL, which is the state the
//!   UI needs to show.
//! - **The secret never leaves**: the column is read only to answer
//!   `has_secret`.

use std::path::Path;

use serde::Serialize;

use crate::native::db;

/// `service.WebhookStatus`, in Go's field order — **no `omitempty` anywhere**,
/// so every key is present even when empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WebhookStatus {
    /// "active", "inactive" or "error".
    pub status: String,
    /// The registered webhook URL, or empty.
    pub url: String,
    /// Whether a secret is stored. The secret itself is never exposed.
    pub has_secret: bool,
    /// The last registration error, if any.
    pub error: String,
}

/// The three columns `GetWebhookInfo` reads.
struct WebhookInfo {
    secret: String,
    status: String,
    error: String,
}

/// `GetWebhookInfo`. A missing row is empty strings rather than an error, which
/// is what makes the status of an unknown integration a 200.
fn webhook_info(db_path: &Path, id: &str) -> Result<WebhookInfo, String> {
    let conn = db::open_read_only(db_path)?;
    conn.query_row(
        "SELECT webhook_secret, webhook_status, webhook_error FROM integrations WHERE id = ?1",
        [id],
        |row| {
            Ok(WebhookInfo {
                secret: row.get(0)?,
                status: row.get(1)?,
                error: row.get(2)?,
            })
        },
    )
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(WebhookInfo {
            secret: String::new(),
            status: String::new(),
            error: String::new(),
        }),
        other => Err(format!("getting webhook info for {id:?}: {other}")),
    })
}

/// `triggerService.publicURL`: the environment wins over the stored setting, and
/// either is right-trimmed of `/`.
///
/// The trim is load-bearing rather than cosmetic — the URL is built by
/// concatenation, so a stored `https://x.example/` would otherwise produce
/// `https://x.example//webhooks/telegram/…`, which Telegram registers verbatim
/// and then calls.
pub fn public_url(db_path: &Path) -> String {
    if let Ok(from_env) = std::env::var("AGENTO_PUBLIC_URL") {
        if !from_env.is_empty() {
            return from_env.trim_end_matches('/').to_string();
        }
    }
    let Ok(conn) = db::open_read_only(db_path) else {
        return String::new();
    };
    crate::native::settings::load_stored(&conn)
        .public_url
        .trim_end_matches('/')
        .to_string()
}

/// The URL a registered webhook is reachable at.
pub fn webhook_url(base: &str, integration_id: &str) -> String {
    format!("{base}/webhooks/telegram/{integration_id}")
}

/// `GetWebhookStatus`.
pub fn status(db_path: &Path, id: &str) -> Result<WebhookStatus, String> {
    let info = webhook_info(db_path, id)?;
    let status = if info.status.is_empty() {
        "inactive".to_string()
    } else {
        info.status
    };

    let url = if status == "active" {
        let base = public_url(db_path);
        if base.is_empty() {
            String::new()
        } else {
            webhook_url(&base, id)
        }
    } else {
        String::new()
    };

    Ok(WebhookStatus {
        status,
        url,
        has_secret: !info.secret.is_empty(),
        error: info.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated(dir: &Path) -> std::path::PathBuf {
        let db = dir.join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO integrations (id, name, type, enabled, credentials, services,
                                       created_at, updated_at)
             VALUES ('tg', 'T', 'telegram', 1, '{}', '{}',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed");
        db
    }

    fn set_webhook(db: &Path, secret: &str, status: &str, error: &str) {
        let conn = rusqlite::Connection::open(db).expect("open");
        conn.execute(
            "UPDATE integrations SET webhook_secret = ?1, webhook_status = ?2,
                                     webhook_error = ?3 WHERE id = 'tg'",
            rusqlite::params![secret, status, error],
        )
        .expect("set");
    }

    fn set_public_url(db: &Path, url: &str) {
        let conn = rusqlite::Connection::open(db).expect("open");
        conn.execute(
            "INSERT INTO user_settings (id, public_url) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET public_url = excluded.public_url",
            [url],
        )
        .expect("set public url");
    }

    #[test]
    fn an_unregistered_integration_is_inactive_rather_than_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        assert_eq!(
            status(&db, "tg").expect("status"),
            WebhookStatus {
                status: "inactive".to_string(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn an_unknown_integration_is_a_200_not_a_404() {
        // `GetWebhookInfo` turns `sql.ErrNoRows` into empty strings, so this is
        // the one `{id}` route with no existence check.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        assert_eq!(
            status(&db, "no-such-id").expect("status").status,
            "inactive"
        );
    }

    #[test]
    fn the_url_appears_only_while_active_and_only_with_a_public_url() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        set_public_url(&db, "https://agento.example");

        set_webhook(&db, "s3cret", "active", "");
        let active = status(&db, "tg").expect("status");
        assert_eq!(active.url, "https://agento.example/webhooks/telegram/tg");
        assert!(active.has_secret, "…without exposing the secret itself");

        // An inactive webhook has no URL even though one could be composed.
        set_webhook(&db, "s3cret", "inactive", "");
        assert_eq!(status(&db, "tg").expect("status").url, "");

        // …and an active one on an instance with no public URL reports active
        // with no URL, which is the state the UI has to show.
        set_public_url(&db, "");
        set_webhook(&db, "s3cret", "active", "");
        let no_base = status(&db, "tg").expect("status");
        assert_eq!(no_base.status, "active");
        assert_eq!(no_base.url, "");
    }

    #[test]
    fn a_trailing_slash_on_the_public_url_does_not_double_up() {
        // `publicURL` right-trims, and the URL is built by concatenation — so
        // without the trim Telegram would be registered against `…//webhooks/…`
        // and would call exactly that.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        set_public_url(&db, "https://agento.example/");
        set_webhook(&db, "s", "active", "");
        assert_eq!(
            status(&db, "tg").expect("status").url,
            "https://agento.example/webhooks/telegram/tg"
        );
    }

    #[test]
    fn a_registration_error_is_carried_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        set_webhook(&db, "", "error", "telegram API error: bad webhook");
        let s = status(&db, "tg").expect("status");
        assert_eq!(s.status, "error");
        assert_eq!(s.error, "telegram API error: bad webhook");
        assert!(!s.has_secret);
        assert_eq!(s.url, "", "an errored webhook has no URL");
    }

    #[test]
    fn the_shape_is_gos_field_order_with_every_key_present() {
        // No `omitempty` on any field, so an empty status still carries all
        // four keys — a UI reading `has_secret` must not see it absent.
        let encoded = String::from_utf8(
            crate::native::gojson::to_vec_marshal(&WebhookStatus {
                status: "inactive".to_string(),
                ..Default::default()
            })
            .expect("encode"),
        )
        .expect("utf-8");
        assert_eq!(
            encoded,
            r#"{"status":"inactive","url":"","has_secret":false,"error":""}"#
        );
    }
}

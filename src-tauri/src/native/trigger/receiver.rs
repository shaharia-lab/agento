//! `POST /webhooks/telegram/{id}` — the inbound webhook, and the update
//! deduplication behind it. Mirrors `TelegramWebhookHandler.handleInbound` and
//! `SQLiteTriggerStore.{IsUpdateProcessed,MarkUpdateProcessed}`.
//!
//! # This route is not under `/api`, and that is deliberate
//!
//! It is mounted at the **root**, so neither guard in `guards.rs` applies to it,
//! and both would break it: the request arrives from Telegram's servers with a
//! foreign `Host`, which `validate_host` answers 403 to. Its authentication is
//! its own — a secret token in `X-Telegram-Bot-Api-Secret-Token`, compared in
//! constant time against the one stored when the webhook was registered.
//! `guards.rs` already scopes itself to `/api` and has a test saying so; this
//! module depends on that and does not re-check it.
//!
//! # Almost every failure is a 200
//!
//! An unknown integration, an inactive webhook, no stored secret, a disabled
//! integration, an undecodable body, unparseable credentials — all answer **200
//! and do nothing**. That is not laxity: Telegram *retries* a non-2xx, so a 4xx
//! for a permanent condition would turn one bad update into a retry loop. The
//! single exception is a **wrong secret**, which is 403 — the one case where
//! telling the caller it is unauthorised is right, because it is not Telegram.
//!
//! The 200 is also written **before** the update is dispatched, so a slow agent
//! run cannot hold Telegram's connection open or make it retry.

use std::path::Path;

use serde::Deserialize;

use crate::native::db;
use crate::native::gojson::{null_is_zero_value, GoStruct};

/// `trigger.TelegramUpdate`.
///
/// Every field is `null_is_zero_value`: this is JSON from Telegram decoded by
/// `encoding/json` on the Go side, where a `null` is a no-op rather than a type
/// error, and an undecodable body is a silent 200 either way.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TelegramUpdate {
    #[serde(deserialize_with = "null_is_zero_value")]
    pub update_id: i64,
    /// [`GoStruct`] for #337: every field of `TelegramMsg` has a default, so
    /// serde's derived `visit_seq` would build one from a JSON **array** of any
    /// length — `[7,{…}]` would decode to a full update and dispatch an agent
    /// run where Go's `json.Unmarshal` errors and answers a silent 200. The
    /// wrapper refuses the array shape. `Option` because a `null` message is a
    /// nil pointer to Go, which is not an error either.
    pub message: Option<GoStruct<TelegramMsg>>,
}

/// `trigger.TelegramMsg`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TelegramMsg {
    #[serde(deserialize_with = "null_is_zero_value")]
    pub message_id: i64,
    /// `GoStruct` for the same reason as `message` above; `null_is_zero_value`
    /// because a `null` chat is the zero struct to `encoding/json`.
    #[serde(deserialize_with = "null_is_zero_value")]
    pub chat: GoStruct<TelegramChat>,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub text: String,
}

/// `trigger.TelegramChat`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TelegramChat {
    #[serde(deserialize_with = "null_is_zero_value")]
    pub id: i64,
}

/// What the receiver decided, so the caller can act without re-reading rows.
#[derive(Debug, PartialEq)]
pub enum Inbound {
    /// 200, and nothing else to do.
    Ignore,
    /// 403: the secret did not match.
    Forbidden,
    /// 200, and this update should be dispatched.
    Dispatch {
        bot_token: String,
        update: TelegramUpdate,
    },
}

/// The stored webhook credentials this route authenticates against.
struct Registered {
    secret: String,
    status: String,
}

fn registered(db_path: &Path, id: &str) -> Option<Registered> {
    let conn = db::open_read_only(db_path).ok()?;
    conn.query_row(
        "SELECT webhook_secret, webhook_status FROM integrations WHERE id = ?1",
        [id],
        |row| {
            Ok(Registered {
                secret: row.get(0)?,
                status: row.get(1)?,
            })
        },
    )
    .ok()
}

/// The integration's bot token, if it is enabled and its credentials parse.
fn enabled_bot_token(db_path: &Path, id: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Creds {
        #[serde(default, deserialize_with = "null_is_zero_value")]
        bot_token: String,
    }

    let conn = db::open_read_only(db_path).ok()?;
    let (enabled, credentials): (bool, String) = conn
        .query_row(
            "SELECT enabled, credentials FROM integrations WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    if !enabled {
        return None;
    }
    serde_json::from_str::<Creds>(&credentials)
        .ok()
        .map(|c| c.bot_token)
}

/// `handleInbound`, without the HTTP.
///
/// The order is Go's, and it is load-bearing: the secret is checked **before**
/// the integration is loaded, so a caller with the wrong secret learns nothing
/// about whether the integration exists or is enabled.
pub fn receive(db_path: &Path, id: &str, header_secret: &str, body: &[u8]) -> Inbound {
    // A read failure, an unknown id, an inactive webhook or no stored secret are
    // all the same silent 200 — Go collapses them into one condition.
    let Some(reg) = registered(db_path, id) else {
        return Inbound::Ignore;
    };
    if reg.status != "active" || reg.secret.is_empty() {
        return Inbound::Ignore;
    }

    if !constant_time_eq(header_secret.as_bytes(), reg.secret.as_bytes()) {
        return Inbound::Forbidden;
    }

    let Some(bot_token) = enabled_bot_token(db_path, id) else {
        return Inbound::Ignore;
    };

    // An undecodable payload is logged at debug and answered 200 — Telegram
    // would retry a 4xx forever for a body it will keep sending.
    // `GoStruct` around the **whole** decode, not only the nested fields —
    // #337's rule 2. Wrapping `message` and `chat` alone still leaves the outer
    // update buildable from a positional array, and `[7,{…}]` would dispatch a
    // real agent run where Go's `json.Unmarshal` errors into a silent 200.
    let Ok(GoStruct(update)) = serde_json::from_slice::<GoStruct<TelegramUpdate>>(body) else {
        log::debug!("failed to decode telegram webhook payload integration_id={id:?}");
        return Inbound::Ignore;
    };

    Inbound::Dispatch { bot_token, update }
}

/// `crypto/subtle.ConstantTimeCompare`.
///
/// Go's returns 0 for unequal lengths *without* comparing, and so does this —
/// the length of a secret is not itself a secret. What must not vary with the
/// input is the comparison of two equal-length slices.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `IsUpdateProcessed` + `MarkUpdateProcessed`: `true` when this update is new
/// and has now been claimed.
///
/// Go does the two as separate statements. They are one transaction here, which
/// is a **narrowing**: two updates with the same id arriving together — Telegram
/// does retry — could both pass Go's `IsUpdateProcessed` before either marked,
/// and run the agent twice. `INSERT OR IGNORE` inside an immediate transaction
/// makes the claim atomic, and the row count says who won.
pub fn claim_update(db_path: &Path, integration_id: &str, update_id: i64) -> bool {
    let claim = || -> Result<bool, String> {
        let mut conn = db::open_read_write(db_path)?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("begin update claim: {e}"))?;
        let now = crate::native::gotime::now_go_text();
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO telegram_processed_updates
                    (integration_id, update_id, processed_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![integration_id, update_id, now],
            )
            .map_err(|e| format!("marking update as processed: {e}"))?;

        // `MarkUpdateProcessed`'s best-effort sweep, on the same 48-hour
        // horizon. A failure here is ignored exactly as Go ignores it.
        let cutoff = crate::native::gotime::go_string_from_millis(
            (chrono::Utc::now() - chrono::Duration::hours(48)).timestamp_millis(),
        );
        let _ = tx.execute(
            "DELETE FROM telegram_processed_updates WHERE processed_at < ?1",
            [&cutoff],
        );

        tx.commit()
            .map_err(|e| format!("commit update claim: {e}"))?;
        Ok(inserted > 0)
    };
    match claim() {
        Ok(claimed) => claimed,
        Err(e) => {
            // Go logs and returns false — do not run the agent on a database it
            // could not record the run against.
            log::error!(
                "failed to claim telegram update integration_id={integration_id:?} \
                 update_id={update_id} error={e}"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated(dir: &Path, enabled: bool, secret: &str, status: &str) -> std::path::PathBuf {
        let db = dir.join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO integrations (id, name, type, enabled, credentials, services,
                                       webhook_secret, webhook_status,
                                       created_at, updated_at)
             VALUES ('tg', 'T', 'telegram', ?1, '{\"bot_token\":\"botsecret\"}', '{}',
                     ?2, ?3, '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            rusqlite::params![enabled, secret, status],
        )
        .expect("seed");
        db
    }

    const UPDATE: &[u8] =
        br#"{"update_id":7,"message":{"message_id":3,"chat":{"id":-100},"text":"hello"}}"#;

    #[test]
    fn a_matching_secret_dispatches_with_the_bot_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), true, "s3cret", "active");
        match receive(&db, "tg", "s3cret", UPDATE) {
            Inbound::Dispatch { bot_token, update } => {
                assert_eq!(bot_token, "botsecret");
                assert_eq!(update.update_id, 7);
                let msg = update.message.expect("a message");
                assert_eq!(msg.text, "hello");
                assert_eq!(msg.chat.id, -100, "group ids are negative");
                assert_eq!(msg.message_id, 3);
            }
            other => panic!("expected dispatch, got {other:?}"),
        }
    }

    #[test]
    fn a_wrong_secret_is_the_only_non_200() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), true, "s3cret", "active");
        assert_eq!(receive(&db, "tg", "wrong", UPDATE), Inbound::Forbidden);
        assert_eq!(receive(&db, "tg", "", UPDATE), Inbound::Forbidden);
        // …and a *longer* guess is refused without a panic or a length leak.
        assert_eq!(
            receive(&db, "tg", "s3cret-and-more", UPDATE),
            Inbound::Forbidden
        );
    }

    #[test]
    fn every_permanent_condition_is_a_silent_200() {
        // Telegram retries a non-2xx, so a 4xx here would be a retry loop for a
        // body it will keep sending.
        let dir = tempfile::tempdir().expect("tempdir");

        let unknown = migrated(dir.path(), true, "s", "active");
        assert_eq!(receive(&unknown, "nope", "s", UPDATE), Inbound::Ignore);

        for (enabled, secret, status, why) in [
            (true, "s", "inactive", "the webhook is not registered"),
            (true, "", "active", "no secret is stored"),
            (false, "s", "active", "the integration is disabled"),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let db = migrated(dir.path(), enabled, secret, status);
            assert_eq!(receive(&db, "tg", "s", UPDATE), Inbound::Ignore, "{why}");
        }
    }

    #[test]
    fn an_undecodable_body_is_ignored_rather_than_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), true, "s3cret", "active");
        assert_eq!(receive(&db, "tg", "s3cret", b"not json"), Inbound::Ignore);
    }

    #[test]
    fn a_null_field_is_the_zero_value_rather_than_a_decode_failure() {
        // `encoding/json` treats these as no-ops, so the update still
        // dispatches — with an empty text, which the dispatcher then drops.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), true, "s3cret", "active");
        let body =
            br#"{"update_id":null,"message":{"message_id":null,"chat":{"id":null},"text":null}}"#;
        match receive(&db, "tg", "s3cret", body) {
            Inbound::Dispatch { update, .. } => {
                assert_eq!(update.update_id, 0);
                assert_eq!(update.message.expect("message").text, "");
            }
            other => panic!("expected dispatch, got {other:?}"),
        }
    }

    #[test]
    fn a_positional_array_is_refused_where_go_refuses_it() {
        // #337's over-accept. Every field has a default, so serde's derived
        // `visit_seq` would build the struct from a JSON **array** — and an
        // update that decoded from `[7,{…}]` would dispatch a real agent run
        // where Go's `json.Unmarshal` errors and answers a silent 200. The
        // an over-accept is answered rather than reported, so the
        // `GoStruct` wrapper is the only guard.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), true, "s3cret", "active");
        for body in [
            &br#"[7,{"message_id":1,"chat":{"id":5},"text":"do something"}]"#[..],
            br#"{"update_id":7,"message":[1,{"id":5},"do something"]}"#,
            br#"{"update_id":7,"message":{"message_id":1,"chat":[5],"text":"x"}}"#,
        ] {
            assert_eq!(
                receive(&db, "tg", "s3cret", body),
                Inbound::Ignore,
                "an array shape must not decode: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn an_update_with_no_message_still_decodes() {
        // Telegram sends many update kinds; the dispatcher drops the ones with
        // no message, but the receiver must not reject them.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), true, "s3cret", "active");
        match receive(&db, "tg", "s3cret", br#"{"update_id":9}"#) {
            Inbound::Dispatch { update, .. } => assert!(update.message.is_none()),
            other => panic!("expected dispatch, got {other:?}"),
        }
    }

    #[test]
    fn an_update_is_claimed_exactly_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), true, "s", "active");
        assert!(claim_update(&db, "tg", 42), "the first claim wins");
        assert!(!claim_update(&db, "tg", 42), "a retry does not");
        // A different update, and the same id under a different integration,
        // are both their own claims.
        assert!(claim_update(&db, "tg", 43));
        assert!(claim_update(&db, "other", 42));
    }

    #[test]
    fn the_sweep_drops_entries_older_than_the_horizon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), true, "s", "active");
        {
            let conn = rusqlite::Connection::open(&db).expect("open");
            conn.execute(
                "INSERT INTO telegram_processed_updates (integration_id, update_id, processed_at)
                 VALUES ('tg', 1, '2020-01-01 00:00:00 +0000 UTC')",
                [],
            )
            .expect("seed old row");
        }
        claim_update(&db, "tg", 2);

        let conn = rusqlite::Connection::open(&db).expect("open");
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM telegram_processed_updates WHERE update_id = 1",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(remaining, 0, "the 48-hour sweep runs with the claim");
    }

    #[test]
    fn the_constant_time_compare_agrees_with_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }
}

//! The notification reads: `GET /api/notifications/settings` and
//! `GET /api/notifications/log`.
//!
//! Mirrors `handleGetNotificationSettings` and `handleListNotificationLog`
//! (`internal/api/notifications.go`) over `notificationServiceImpl`
//! (`internal/service/notification_service.go`) and `SQLiteNotificationStore`
//! (`internal/storage/sqlite_notification_store.go`).
//!
//! Since #307 it also answers `PUT /api/notifications/settings` and
//! `POST /api/notifications/test`, and carries the sender they sit on:
//! [`template`] is `internal/notification/template.go` and [`smtp`] is
//! `internal/notification/smtp.go`.
//!
//! **The subscriber is not wired, and cannot be yet.** `NotificationHandler` is
//! useful because the event bus calls it when a scheduled task finishes, and the
//! task executor is #275's — it still runs in the sidecar, so the events exist
//! in a process this code is not in. What is ported is everything downstream of
//! that call: settings → message → send. Wiring it is a subscription, once
//! there is a Rust publisher to subscribe to.
//!
//! One consequence of porting the write while the subscriber is Go's, and the
//! reason `cmd/web.go` changed with this: the sidecar's `SettingsManager` holds
//! an in-memory snapshot of `user_settings` taken at boot, and its notification
//! `SettingsLoader` read from it. A native write would then have left every
//! scheduled-task email on the previous SMTP credentials until the app
//! restarted, with nothing to say so. The loader now reads the row, which is
//! what its own doc comment already promised ("so that configuration changes
//! take effect without a server restart") and what a second writer makes true.
//!
//! Three things decide the bytes, none of them visible in the Go structs:
//!
//! 1. **The settings are a JSON column, not a table.** They live in
//!    `user_settings.notification_settings` as a marshalled
//!    `notification.NotificationSettings`, which is why this module reads the
//!    row through [`super::settings::load_stored`] rather than opening its own
//!    query. Go treats `""` **and** `"{}"` as "nothing configured" and answers
//!    with the zero value; anything else that fails to parse is a 500.
//! 2. **The SMTP password is masked, but only when there is one.** Go replaces a
//!    non-empty password with `"***"` and leaves an empty one empty, so the
//!    response distinguishes "no password stored" from "password withheld" —
//!    and `PUT` reads that same sentinel back as "keep what you have". Masking
//!    unconditionally would make an unconfigured provider look configured and
//!    would round-trip `"***"` into storage as a real password.
//! 3. **An empty log is `null`, not `[]`.** `ListNotifications` accumulates into
//!    a `var entries []NotificationLogEntry` and only ever appends, so a machine
//!    that has never sent a notification gets `null` — which is most machines.
//!    See [`list_log`].

pub mod smtp;
pub mod template;

use std::path::Path;

use axum::http::Method;
use serde::{Deserialize, Serialize};

use super::db;
use super::gojson::GoStruct;
use super::gotime::GoTime;
use super::writes::{decode_body, finish, WriteError};

// ─── GET /api/notifications/settings ──────────────────────────────────────────

/// SMTP connection parameters. Mirrors `notification.SMTPConfig`.
///
/// No field carries `omitempty`, so every key is present even on an
/// unconfigured install — including `"port": 0`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtpConfig {
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub host: String,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub port: i64,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub username: String,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub password: String,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub from_address: String,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub to_addresses: String,
    /// "none", "starttls" or "ssl_tls".
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub encryption: String,
}

/// Mirrors `notification.ScheduledTasksPreferences`.
///
/// Both fields are `*bool` with `omitempty`, and the pointer is the whole
/// point: nil means "not chosen" and resolves to enabled, while a pointer to
/// `false` is a deliberate opt-out. `omitempty` omits only the nil — a pointer
/// to `false` still ships as `false`, which an `Option<bool>` reproduces and a
/// plain `bool` would not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTasksPreferences {
    /// **No `#[serde(default)]`**: a bare `Option` already gets `None` from
    /// `missing_field`, so the attribute was redundant — and its one remaining
    /// effect was to feed the derive's `visit_seq` arm, which is #337's shape.
    /// Belt-and-braces now that [`GoStruct`] wraps this at its one use site, but
    /// #336 is the precedent: an attribute left behind after its field became
    /// self-sufficient does nothing except open that arm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_finished: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_failed: Option<bool>,
}

/// Mirrors `notification.NotificationPreferences`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPreferences {
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub scheduled_tasks: GoStruct<ScheduledTasksPreferences>,
}

/// Mirrors `notification.NotificationSettings`, the shape stored in
/// `user_settings.notification_settings`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSettings {
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub enabled: bool,
    /// [`GoStruct`] because these two are the *worst* case of #337, not a
    /// borderline one: every field of `SmtpConfig` and of
    /// `ScheduledTasksPreferences` carries `#[serde(default)]` — each needs one,
    /// since `deserialize_with` makes a field required — and the derive's
    /// `visit_seq` arm errors only for a field *without* a default. So the
    /// accepted array length was **zero and upwards**, and `{"provider":[]}` was
    /// a saved SMTP configuration here and a 400 to Go.
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub provider: GoStruct<SmtpConfig>,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub preferences: GoStruct<NotificationPreferences>,
}

/// `service.maskedFieldSentinel`. Also what `PUT` reads back as "unchanged", so
/// the two directions have to agree on the exact string.
const MASKED_FIELD_SENTINEL: &str = "***";

/// `GetSettings`: the stored settings with the password masked.
///
/// An unparseable column is an error rather than a default: the user sees a
/// 500 rather than a silently empty form, which a save would then write over
/// their real configuration.
pub fn get_settings(db_path: &Path) -> Result<NotificationSettings, String> {
    let conn = db::open_read_only(db_path)?;
    let raw = super::settings::load_stored(&conn).notification_settings;
    let mut settings = decode_settings(&raw)?;
    if !settings.provider.password.is_empty() {
        settings.provider.password = MASKED_FIELD_SENTINEL.to_string();
    }
    Ok(settings)
}

/// `loadNotificationSettings`. Both `""` and `"{}"` are "nothing configured" —
/// the second because that is the column's own `DEFAULT`, so an install that
/// has never opened the notifications page stores it literally.
fn decode_settings(raw: &str) -> Result<NotificationSettings, String> {
    if raw.is_empty() || raw == "{}" {
        return Ok(NotificationSettings::default());
    }
    serde_json::from_str(raw).map_err(|e| format!("parsing notification settings: {e}"))
}

// ─── GET /api/notifications/log ───────────────────────────────────────────────

/// One delivery attempt. Mirrors `storage.NotificationLogEntry`.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationLogEntry {
    pub id: i64,
    pub event_type: String,
    pub provider: String,
    pub subject: String,
    /// "sent" or the failure marker the handler wrote.
    pub status: String,
    pub error_msg: String,
    pub created_at: GoTime,
}

/// `ListNotifications`: the most recent entries, newest first.
///
/// Returns `None` for "no rows", which is Go's **nil** slice and therefore
/// `null` on the wire — not `[]`. Go accumulates with `append` into a nil slice
/// and never allocates when the query returns nothing, and a machine that has
/// never sent a notification is the common case rather than an edge one.
pub fn list_log(db_path: &Path, limit: i64) -> Result<Option<Vec<NotificationLogEntry>>, String> {
    let conn = db::open_read_only(db_path)?;
    // No tiebreak, exactly as Go: the index this rides
    // (`idx_notification_log_created`) is the whole ordering, and adding one
    // here would reorder ties away from the rows Go returns.
    let mut stmt = conn
        .prepare(
            "SELECT id, event_type, provider, subject, status, error_msg, created_at
             FROM notification_log
             ORDER BY created_at DESC
             LIMIT ?1",
        )
        .map_err(|e| format!("preparing notification log query: {e}"))?;

    let rows = stmt
        .query_map([limit], |row| {
            let created_at: String = row.get(6)?;
            Ok(NotificationLogEntry {
                id: row.get(0)?,
                event_type: row.get(1)?,
                provider: row.get(2)?,
                subject: row.get(3)?,
                status: row.get(4)?,
                error_msg: row.get(5)?,
                created_at: GoTime::parse_any(&created_at).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::other(e)),
                    )
                })?,
            })
        })
        .map_err(|e| format!("querying notification log: {e}"))?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row.map_err(|e| format!("scanning notification log row: {e}"))?);
    }
    Ok((!entries.is_empty()).then_some(entries))
}

/// The handler's `?limit=` rule, which is **not** the one the store applies.
///
/// The handler starts at 50 and only replaces it when the value parses *and* is
/// positive — so `?limit=0`, `?limit=-1` and `?limit=abc` all mean fifty rather
/// than being rejected or meaning "none". The store's own `limit <= 0` guard is
/// unreachable from HTTP for that reason; it exists for other callers.
pub fn log_limit(query: &str) -> i64 {
    const DEFAULT_LIMIT: i64 = 50;

    // The decoding is shared (`super::query`); only the rule on top is this
    // handler's. `tasks::parse_query_int` is not reusable here because it also
    // clamps to `MAX_QUERY_LIMIT`, which this handler does not.
    match super::query::value(query, "limit").parse::<i64>() {
        Ok(n) if n > 0 => n,
        _ => DEFAULT_LIMIT,
    }
}

// ─── PUT /api/notifications/settings ──────────────────────────────────────────

/// `handleUpdateNotificationSettings` over `UpdateSettings`.
///
/// # The one column, and why that is the fix rather than a shortcut
///
/// Go's `UpdateSettings` is a read-modify-write of the **whole** `user_settings`
/// row: it takes `settingsMgr.Get()` — the sidecar's in-memory snapshot, loaded
/// at boot — sets one field on it, and saves all fourteen columns back. #305
/// reproduced what that costs once a second process writes the same row: one
/// unrelated save here reverted a natively-written hidden-project list and idle
/// threshold to the sidecar's boot values, silently.
///
/// This writes `notification_settings` and nothing else, so the hazard is not
/// reproduced — it is removed. That is a deliberate divergence from Go's
/// *mechanism*; the observable result of a save is the same, and the only way
/// to make it observably identical would be to reproduce a data-loss bug.
///
/// # The masked password
///
/// `"***"` in the incoming password means "keep what is stored" — the same
/// sentinel `get_settings` writes on the way out, so the UI can round-trip the
/// form without ever holding the real value. Storing the sentinel verbatim
/// would replace the user's password with three asterisks, and the next send
/// would fail authentication with nothing pointing at the save that did it.
fn update_settings(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let mut incoming = decode_body::<NotificationSettings>(body)?;

    let conn = db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;

    if incoming.provider.password == MASKED_FIELD_SENTINEL {
        let stored = decode_settings(&super::settings::load_stored(&conn).notification_settings)
            .map_err(|e| WriteError::Fallback(format!("loading existing settings: {e}")))?;
        incoming.provider.password = stored.provider.0.password;
    }

    // `json.Marshal`, not the response encoder: this string is stored and read
    // back by Go, so it must be the bytes Go would have written — HTML escaping
    // included, since `encoding/json` escapes and the column round-trips.
    let raw = super::gojson::to_vec_marshal(&incoming)
        .map_err(|e| WriteError::Fallback(format!("encoding notification settings: {e}")))?;
    let raw = String::from_utf8(raw)
        .map_err(|e| WriteError::Fallback(format!("notification settings are not UTF-8: {e}")))?;

    let updated = conn
        .execute(
            "UPDATE user_settings SET notification_settings = ?1 WHERE id = 1",
            [&raw],
        )
        .map_err(|e| WriteError::Fallback(format!("saving notification settings: {e}")))?;
    if updated == 0 {
        // No settings row at all. Inserting one would mean owning all fourteen
        // columns and their defaults, which this write deliberately does not —
        // see the header. Nothing was written, so the 500 is exact.
        return Err(WriteError::Fallback(
            "no user_settings row to update".to_string(),
        ));
    }

    // Go answers with a re-read rather than the request, which is how the
    // password comes back masked instead of echoed.
    let saved = get_settings(db_path).map_err(WriteError::Fallback)?;
    let body = super::gojson::to_vec(&saved)
        .map_err(|e| WriteError::Fallback(format!("encoding notification settings: {e}")))?;
    Ok(super::Answer::json(body))
}

// ─── POST /api/notifications/test ─────────────────────────────────────────────

/// `handleTestNotification` over `TestNotification`.
///
/// Sends with the stored settings **regardless of `enabled`**, which is the
/// point of the button: it lets someone verify credentials before committing to
/// turning notifications on.
///
/// Only the success path is answered with its own body. A failure is a 500 with
/// the reason in the log, because the inherited 400 carries wording from the
/// mail library and the runtime that this build cannot reproduce — see
/// `smtp.rs`. That is safe for exactly one reason: the send reports success only
/// after the server has accepted the message, so an error means nothing was
/// delivered.
fn test_notification(db_path: &Path) -> Result<super::Answer, WriteError> {
    let conn = db::open_read_only(db_path).map_err(WriteError::Fallback)?;
    let settings = decode_settings(&super::settings::load_stored(&conn).notification_settings)
        .map_err(WriteError::Fallback)?;
    drop(conn);

    // Encoded before the send. Nothing fallible may run after the message is
    // accepted, or a 500 would report a failure for mail that was delivered —
    // and invite a retry that sends it twice.
    let answer = super::gojson::to_vec(&TestResponse { status: "ok" })
        .map_err(|e| WriteError::Fallback(format!("encoding test response: {e}")))?;

    smtp::send(&settings.provider, &smtp::test_mail()).map_err(WriteError::Fallback)?;
    Ok(super::Answer::json(answer))
}

/// `map[string]string{"status": "ok"}` — one key, so nothing to sort.
#[derive(Serialize)]
struct TestResponse {
    status: &'static str,
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "notifications",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    match path {
        "/api/notifications/settings" => method == Method::GET || method == Method::PUT,
        "/api/notifications/log" => method == Method::GET,
        "/api/notifications/test" => method == Method::POST,
        _ => false,
    }
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    match (req.method, req.path) {
        (&Method::GET, "/api/notifications/settings") => {
            let body = super::gojson::to_vec(&get_settings(&ctx.db_path)?)
                .map_err(|e| format!("encoding notification settings: {e}"))?;
            Ok(super::Answer::json(body))
        }
        (&Method::GET, "/api/notifications/log") => {
            let entries = list_log(&ctx.db_path, log_limit(req.query))?;
            let body = super::gojson::to_vec(&entries)
                .map_err(|e| format!("encoding notification log: {e}"))?;
            Ok(super::Answer::json(body))
        }
        (&Method::PUT, "/api/notifications/settings") => {
            finish(update_settings(&ctx.db_path, req.body))
        }
        (&Method::POST, "/api/notifications/test") => finish(test_notification(&ctx.db_path)),
        _ => Err(format!(
            "{} {} is not a notification route",
            req.method, req.path
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    const SCHEMA: &str = "
        CREATE TABLE notification_log (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            event_type TEXT     NOT NULL,
            provider   TEXT     NOT NULL,
            subject    TEXT     NOT NULL DEFAULT '',
            status     TEXT     NOT NULL DEFAULT 'sent',
            error_msg  TEXT     NOT NULL DEFAULT '',
            created_at DATETIME NOT NULL
        );
        CREATE TABLE user_settings (
            id                         INTEGER PRIMARY KEY CHECK (id = 1),
            default_working_dir        TEXT    NOT NULL DEFAULT '',
            default_model              TEXT    NOT NULL DEFAULT '',
            onboarding_complete        INTEGER NOT NULL DEFAULT 0,
            appearance_dark_mode       INTEGER NOT NULL DEFAULT 0,
            appearance_font_size       INTEGER NOT NULL DEFAULT 0,
            appearance_font_family     TEXT    NOT NULL DEFAULT '',
            notification_settings      TEXT    NOT NULL DEFAULT '{}',
            event_bus_worker_pool_size INTEGER NOT NULL DEFAULT 3,
            public_url                 TEXT    NOT NULL DEFAULT '',
            hidden_projects            TEXT    NOT NULL DEFAULT '[]',
            idle_gap_threshold_minutes INTEGER NOT NULL DEFAULT 0,
            claude_config_dir          TEXT    NOT NULL DEFAULT '',
            claude_config_dirs         TEXT    NOT NULL DEFAULT '[]'
        );";

    /// A database on disk, since both reads take a path and open their own
    /// read-only handle — the same shape the endpoint runs in.
    fn fixture(settings_json: &str, log_rows: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute(
            "INSERT INTO user_settings (id, notification_settings) VALUES (1, ?1)",
            [settings_json],
        )
        .expect("settings row");
        if !log_rows.is_empty() {
            conn.execute_batch(log_rows).expect("log rows");
        }
        file
    }

    const CONFIGURED: &str = r#"{
        "enabled": true,
        "provider": {
            "host": "smtp.example.com", "port": 587, "username": "mailer",
            "password": "hunter2", "from_address": "a@example.com",
            "to_addresses": "b@example.com", "encryption": "starttls"
        },
        "preferences": {"scheduled_tasks": {"on_failed": false}}
    }"#;

    /// The mask is the whole reason this endpoint is not just a column read.
    #[test]
    fn a_stored_password_is_replaced_by_the_sentinel() {
        let file = fixture(CONFIGURED, "");
        let settings = get_settings(file.path()).expect("settings");
        assert_eq!(settings.provider.password, MASKED_FIELD_SENTINEL);
        // Everything beside it is untouched.
        assert_eq!(settings.provider.host, "smtp.example.com");
        assert_eq!(settings.provider.port, 587);
        assert_eq!(settings.provider.username, "mailer");
        assert!(settings.enabled);
    }

    /// Masking unconditionally would make an unconfigured provider look
    /// configured — and `PUT` reads the sentinel back as "keep the stored
    /// password", so a blind mask would eventually store `"***"` as one.
    #[test]
    fn an_absent_password_stays_empty_rather_than_masked() {
        let file = fixture(
            r#"{"enabled":false,"provider":{"host":"smtp.example.com"}}"#,
            "",
        );
        let settings = get_settings(file.path()).expect("settings");
        assert_eq!(settings.provider.password, "");
    }

    /// Both spellings of "nothing configured". `{}` is the column's own
    /// `DEFAULT`, so it is what an install that never opened the page holds.
    #[test]
    fn the_two_unconfigured_spellings_answer_with_the_zero_value() {
        for raw in ["", "{}"] {
            let file = fixture(raw, "");
            assert_eq!(
                get_settings(file.path()).expect("settings"),
                NotificationSettings::default(),
                "{raw:?}"
            );
        }
    }

    /// A 500. Degrading to the zero value instead would show an empty form that
    /// a save would then write over the user's real SMTP credentials.
    #[test]
    fn an_unparseable_column_is_an_error_not_a_default() {
        let file = fixture("{not json", "");
        assert!(get_settings(file.path()).is_err());
    }

    /// The other half of the pair, which behaves oppositely and looks the same:
    /// `json.Unmarshal` treats `null` as a no-op for every type here, so Go
    /// answers 200 with the zero value. Rejecting it would fall back silently.
    #[test]
    fn a_stored_null_is_the_zero_value_not_a_parse_failure() {
        for raw in [
            r#"{"enabled":null,"provider":null,"preferences":null}"#,
            r#"{"enabled":true,"provider":null}"#,
            r#"{"provider":{"host":null,"port":null},"preferences":{"scheduled_tasks":null}}"#,
        ] {
            let file = fixture(raw, "");
            let settings = get_settings(file.path())
                .unwrap_or_else(|e| panic!("{raw} should decode, got {e}"));
            assert_eq!(settings.provider.host, "", "{raw}");
            assert_eq!(settings.provider.port, 0, "{raw}");
            assert_eq!(
                settings.preferences,
                GoStruct(NotificationPreferences::default()),
                "{raw}"
            );
        }

        // …and `enabled: true` beside a null sibling still survives, so the
        // decoder is tolerant rather than blanket-defaulting.
        let file = fixture(r#"{"enabled":true,"provider":null}"#, "");
        assert!(get_settings(file.path()).expect("settings").enabled);
    }

    /// `omitempty` on a `*bool` omits nil and keeps a pointer to `false`. The
    /// difference is the whole meaning of the field: absent is "use the
    /// default", which is enabled.
    #[test]
    fn an_explicit_false_preference_ships_and_an_absent_one_does_not() {
        let file = fixture(CONFIGURED, "");
        let body = super::super::gojson::to_vec(&get_settings(file.path()).expect("settings"))
            .expect("encode");
        let json = String::from_utf8(body).expect("utf8");
        assert!(
            json.contains(r#""scheduled_tasks":{"on_failed":false}"#),
            "{json}"
        );
        assert!(!json.contains("on_finished"), "{json}");
    }

    /// The full envelope, in the Go structs' declaration order.
    #[test]
    fn the_settings_shape_is_the_go_struct_order() {
        let file = fixture(r#"{"enabled":true,"provider":{"port":25}}"#, "");
        let body = super::super::gojson::to_vec(&get_settings(file.path()).expect("settings"))
            .expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            concat!(
                r#"{"enabled":true,"provider":{"host":"","port":25,"username":"","#,
                r#""password":"","from_address":"","to_addresses":"","encryption":""},"#,
                r#""preferences":{"scheduled_tasks":{}}}"#,
                "\n"
            )
        );
    }

    const LOG_ROWS: &str = "
        INSERT INTO notification_log (event_type, provider, subject, status, error_msg, created_at)
        VALUES ('task.finished', 'smtp', 'Agento: done', 'sent', '',
                '2026-08-01 10:00:00 +0000 UTC'),
               ('task.failed', 'smtp', 'Agento: failed', 'error', 'dial tcp: refused',
                '2026-08-02 11:30:00 +0000 UTC'),
               ('task.finished', 'smtp', 'Agento: done again', 'sent', '',
                '2026-08-03 09:15:00 +0000 UTC');";

    #[test]
    fn the_log_is_newest_first_and_respects_the_limit() {
        let file = fixture("{}", LOG_ROWS);
        let all = list_log(file.path(), 50).expect("log").expect("rows");
        assert_eq!(
            all.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![3, 2, 1],
            "newest first"
        );
        assert_eq!(all[1].status, "error");
        assert_eq!(all[1].error_msg, "dial tcp: refused");

        let one = list_log(file.path(), 1).expect("log").expect("rows");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, 3);
    }

    /// The case most installs are in, and the one a `Vec` would get wrong.
    #[test]
    fn an_empty_log_is_null_not_an_empty_array() {
        let file = fixture("{}", "");
        let entries = list_log(file.path(), 50).expect("log");
        assert!(entries.is_none());

        let body = super::super::gojson::to_vec(&entries).expect("encode");
        assert_eq!(String::from_utf8(body).expect("utf8"), "null\n");
    }

    /// A DATETIME column holds `time.Time.String()`; the wire is RFC3339Nano.
    #[test]
    fn the_log_entry_shape_is_the_go_struct_order() {
        let file = fixture("{}", LOG_ROWS);
        let entries = list_log(file.path(), 1).expect("log");
        let body = super::super::gojson::to_vec(&entries).expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            concat!(
                r#"[{"id":3,"event_type":"task.finished","provider":"smtp","#,
                r#""subject":"Agento: done again","status":"sent","error_msg":"","#,
                r#""created_at":"2026-08-03T09:15:00Z"}]"#,
                "\n"
            )
        );
    }

    /// Every non-positive and unparsable value means fifty, because the handler
    /// only *replaces* its default when the parse succeeds and is positive.
    #[test]
    fn the_limit_falls_back_to_fifty_rather_than_rejecting() {
        assert_eq!(log_limit(""), 50);
        assert_eq!(log_limit("limit="), 50);
        assert_eq!(log_limit("limit=0"), 50);
        assert_eq!(log_limit("limit=-3"), 50);
        assert_eq!(log_limit("limit=abc"), 50);
        assert_eq!(log_limit("limit=7"), 7);
        assert_eq!(log_limit("other=1&limit=7"), 7);
        // `r.URL.Query().Get` answers with the *first* value for a repeated key.
        assert_eq!(log_limit("limit=7&limit=9"), 7);
        // …and it percent-decodes, which a `strip_prefix` scan would not: this
        // is `limit=5` to Go, so it must be 5 here.
        assert_eq!(log_limit("%6cimit=5"), 5);
        assert_eq!(log_limit("limit=%35"), 5);
    }

    #[test]
    fn each_notification_route_is_claimed_for_its_own_methods() {
        assert!(claims(&Method::GET, "/api/notifications/settings"));
        assert!(claims(&Method::GET, "/api/notifications/log"));
        assert!(claims(&Method::PUT, "/api/notifications/settings"));
        assert!(claims(&Method::POST, "/api/notifications/test"));

        // The path match must not carry a method chi routes nowhere.
        assert!(!claims(&Method::GET, "/api/notifications/test"));
        assert!(!claims(&Method::POST, "/api/notifications/settings"));
        assert!(!claims(&Method::DELETE, "/api/notifications/settings"));
        assert!(!claims(&Method::PUT, "/api/notifications/log"));
        assert!(!claims(&Method::GET, "/api/notifications"));
        assert!(!claims(&Method::GET, "/api/notifications/log/"));
    }

    // ─── PUT /api/notifications/settings ──────────────────────────────────────

    /// A database built by the **real** migrations: the write path checks the
    /// recorded schema version, which the hand-written `SCHEMA` above has none
    /// of.
    fn migrated() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        super::super::migrate::apply(&mut conn).expect("migrate");
        conn.execute("INSERT INTO user_settings (id) VALUES (1)", [])
            .expect("settings row");
        file
    }

    fn stored_json(file: &tempfile::NamedTempFile) -> String {
        let conn = Connection::open(file.path()).expect("open");
        conn.query_row(
            "SELECT notification_settings FROM user_settings WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("column")
    }

    #[test]
    fn saving_settings_answers_with_the_reread_row_and_masks_the_password() {
        let file = migrated();
        let answer = update_settings(
            file.path(),
            br#"{"enabled":true,"provider":{"host":"smtp.example.com","port":587,
                 "username":"mailer","password":"hunter2","from_address":"a@example.com",
                 "to_addresses":"b@example.com","encryption":"starttls"},
                 "preferences":{"scheduled_tasks":{"on_failed":false}}}"#,
        )
        .expect("save");

        assert_eq!(answer.status, axum::http::StatusCode::OK);
        let body = String::from_utf8(answer.body.expect("body")).expect("utf-8");
        assert!(
            body.contains(r#""password":"***""#),
            "the answer is a re-read, so the password comes back masked: {body}"
        );
        assert!(body.contains(r#""host":"smtp.example.com""#), "{body}");
        // A deliberate opt-out is a `false`, not an omission.
        assert!(body.contains(r#""on_failed":false"#), "{body}");

        // …while the column holds the real one.
        assert!(stored_json(&file).contains(r#""password":"hunter2""#));
    }

    /// #337 on this route, which is the **worst** instance of it in the write
    /// surface rather than a marginal one.
    ///
    /// Every field of `SmtpConfig` and of `ScheduledTasksPreferences` carries
    /// `#[serde(default)]` — each needs one, since `deserialize_with` makes a
    /// field required — and the derive's `visit_seq` arm errors only for a field
    /// *without* a default. So the accepted array length was **zero and
    /// upwards**: `{"provider":[]}` saved an SMTP configuration where Go answers
    /// `cannot unmarshal array into Go struct field`.
    #[test]
    fn saving_settings_refuses_an_array_where_a_struct_belongs() {
        for body in [
            &br#"{"provider":[]}"#[..],
            &br#"{"provider":["smtp.example.com",587]}"#[..],
            &br#"{"preferences":[]}"#[..],
            &br#"{"preferences":{"scheduled_tasks":[]}}"#[..],
            &br#"{"preferences":{"scheduled_tasks":[true,false]}}"#[..],
        ] {
            let file = migrated();
            let err = update_settings(file.path(), body).unwrap_err();
            assert_eq!(
                err.status(),
                axum::http::StatusCode::BAD_REQUEST,
                "{}",
                String::from_utf8_lossy(body)
            );
            // Nothing was written: an over-accept only matters for the row it
            // leaves behind.
            assert!(
                !stored_json(&file).contains("smtp.example.com"),
                "{}",
                String::from_utf8_lossy(body)
            );
        }

        // The object forms all still save, including the `null`s #295 covers —
        // the check is about the array shape and nothing else.
        let file = migrated();
        update_settings(
            file.path(),
            br#"{"enabled":true,"provider":null,"preferences":{"scheduled_tasks":null}}"#,
        )
        .expect("nulls are zero values, not arrays");
    }

    /// The sentinel round-trip. The UI never holds the real password, so a save
    /// from a form that only changed the host sends `"***"` back — storing it
    /// verbatim would replace the password with three asterisks and every
    /// later send would fail authentication with nothing pointing here.
    #[test]
    fn the_masked_sentinel_keeps_the_stored_password() {
        let file = migrated();
        update_settings(
            file.path(),
            br#"{"provider":{"host":"old.example.com","password":"hunter2",
                 "from_address":"a@b.c","to_addresses":"d@e.f"}}"#,
        )
        .expect("first save");

        update_settings(
            file.path(),
            br#"{"provider":{"host":"new.example.com","password":"***",
                 "from_address":"a@b.c","to_addresses":"d@e.f"}}"#,
        )
        .expect("second save");

        let stored = stored_json(&file);
        assert!(stored.contains(r#""password":"hunter2""#), "{stored}");
        assert!(stored.contains(r#""host":"new.example.com""#), "{stored}");
    }

    /// An empty password is not masked on the way out, so it must not be read
    /// as the sentinel on the way in either — clearing a password has to work.
    #[test]
    fn an_empty_password_clears_rather_than_preserving() {
        let file = migrated();
        update_settings(
            file.path(),
            br#"{"provider":{"password":"hunter2","from_address":"a@b.c"}}"#,
        )
        .expect("first save");
        update_settings(
            file.path(),
            br#"{"provider":{"password":"","from_address":"a@b.c"}}"#,
        )
        .expect("second save");
        assert!(stored_json(&file).contains(r#""password":"""#));
    }

    /// The divergence from Go's mechanism that is the point of the port.
    ///
    /// `UpdateSettings` saves all fourteen columns from the sidecar's boot-time
    /// snapshot, which #305 reproduced reverting a natively-written
    /// hidden-project list and idle threshold. This touches one column, so a
    /// notification save cannot revert anything else.
    #[test]
    fn saving_settings_touches_no_other_column() {
        let file = migrated();
        {
            let conn = Connection::open(file.path()).expect("open");
            conn.execute(
                "UPDATE user_settings SET hidden_projects = ?1,
                     idle_gap_threshold_minutes = 45, default_model = 'claude-opus-5'
                 WHERE id = 1",
                ["[\"/home/u/secret\"]"],
            )
            .expect("seed neighbours");
        }

        update_settings(
            file.path(),
            br#"{"enabled":true,"provider":{"host":"h","from_address":"a@b.c"}}"#,
        )
        .expect("save");

        let conn = Connection::open(file.path()).expect("open");
        let (hidden, idle, model): (String, i64, String) = conn
            .query_row(
                "SELECT hidden_projects, idle_gap_threshold_minutes, default_model
                 FROM user_settings WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("row");
        assert_eq!(hidden, "[\"/home/u/secret\"]");
        assert_eq!(idle, 45);
        assert_eq!(model, "claude-opus-5");
    }

    /// No settings row at all means nothing has ever been saved. Inserting one
    /// would mean owning all fourteen columns, so this refuses — having written
    /// nothing.
    #[test]
    fn a_missing_settings_row_is_refused_rather_than_invented() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        super::super::migrate::apply(&mut conn).expect("migrate");
        drop(conn);

        let err = update_settings(file.path(), br#"{"enabled":true}"#).unwrap_err();
        assert!(matches!(err, WriteError::Fallback(_)));
    }

    #[test]
    fn a_malformed_body_is_400_and_a_null_body_is_the_zero_value() {
        let file = migrated();
        assert_eq!(
            update_settings(file.path(), b"[]").unwrap_err(),
            WriteError::InvalidBody
        );
        assert_eq!(
            update_settings(file.path(), b"").unwrap_err(),
            WriteError::InvalidBody
        );
        // Go's `json.Unmarshal(null, &v)` is a no-op, so this saves the zero
        // value rather than failing — which is a real way to turn everything off.
        update_settings(file.path(), b"null").expect("null is the zero value");
        assert_eq!(
            stored_json(&file),
            r#"{"enabled":false,"provider":{"host":"","port":0,"username":"","password":"","from_address":"","to_addresses":"","encryption":""},"preferences":{"scheduled_tasks":{}}}"#
        );
    }
}

// ─── The subscriber (#275) ────────────────────────────────────────────────────

/// The two event types `internal/scheduler` publishes, spelled as it spells
/// them — these strings reach `notification_log.event_type` and the UI renders
/// them, so a paraphrase is a divergence.
pub mod event {
    pub const TASK_FINISHED: &str = "tasks_scheduler.task_execution.finished";
    pub const TASK_FAILED: &str = "tasks_scheduler.task_execution.failed";
}

/// `humanSubject`. An unknown type falls back to the raw string.
fn human_subject(event_type: &str) -> &str {
    match event_type {
        event::TASK_FINISHED => "Scheduled Task Completed Successfully",
        event::TASK_FAILED => "Scheduled Task Execution Failed",
        other => other,
    }
}

/// `shouldSendForEvent`. A nil preference is enabled, which is what the
/// `Option` carries; an event this switch does not know is always sent.
fn should_send_for_event(event_type: &str, settings: &NotificationSettings) -> bool {
    let prefs = &settings.preferences.scheduled_tasks;
    match event_type {
        event::TASK_FINISHED => prefs.on_finished.unwrap_or(true),
        event::TASK_FAILED => prefs.on_failed.unwrap_or(true),
        _ => true,
    }
}

/// `NotificationHandler.Handle`: settings → message → send → log.
///
/// This is the subscriber this module's header said could not exist yet — the
/// publisher was the Go scheduler, in a process this code is not in. #275 moved
/// the scheduler here, so the call is direct rather than over an event bus:
/// there is one publisher and one subscriber, and an in-process bus between two
/// functions would only be a place for a subscription to be forgotten.
///
/// **Every failure is logged and swallowed**, as Go's is. A notification that
/// cannot be sent must not fail the task run that produced it — the job history
/// row is the record of the run, and the email is a courtesy on top of it.
///
/// `payload`'s order is this port's, not Go's: `Handle` ranges a `map[string]string`
/// to build the body, so Go's line order is random per send. Insertion order is
/// used here, which is strictly more readable and cannot be "wrong" against an
/// order Go does not have.
pub fn handle(db_path: &Path, event_type: &str, payload: &[(&str, String)]) {
    let settings = match get_stored_settings(db_path) {
        Ok(settings) => settings,
        Err(e) => {
            log::error!("notification: failed to load settings: {e}");
            return;
        }
    };
    if !settings.enabled || !should_send_for_event(event_type, &settings) {
        return;
    }

    let subject = template::build_subject(human_subject(event_type));
    let body = payload
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n");

    let send_err = smtp::send(
        &settings.provider,
        &smtp::Mail {
            subject: subject.clone(),
            body,
        },
    )
    .err();
    if let Some(e) = &send_err {
        log::error!("notification: failed to send event={event_type:?} error={e}");
    }

    if let Err(e) = log_notification(db_path, event_type, &subject, send_err.as_deref()) {
        log::error!("notification: failed to log delivery event={event_type:?} error={e}");
    }
}

/// The settings as `Handle` reads them — **unmasked**, unlike
/// [`get_settings`], which is the API read and replaces the password with a
/// sentinel. Sending with the sentinel would authenticate as `***`.
fn get_stored_settings(db_path: &Path) -> Result<NotificationSettings, String> {
    let conn = db::open_read_only(db_path)?;
    decode_settings(&super::settings::load_stored(&conn).notification_settings)
}

/// `SQLiteNotificationStore.LogNotification`.
///
/// `created_at` is `time.Now()` **local**, which is Go's — see
/// [`crate::native::gotime::now_go_text_local`] for why it is reproduced rather
/// than corrected to UTC.
fn log_notification(
    db_path: &Path,
    event_type: &str,
    subject: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;
    conn.execute(
        "INSERT INTO notification_log
            (event_type, provider, subject, status, error_msg, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            event_type,
            // `SMTPProvider.Name()`.
            "smtp",
            subject,
            if error.is_some() { "failed" } else { "sent" },
            error.unwrap_or(""),
            super::gotime::now_go_text_local(),
        ],
    )
    .map_err(|e| format!("inserting notification log: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod subscriber_tests {
    use super::*;

    fn settings(
        enabled: bool,
        on_finished: Option<bool>,
        on_failed: Option<bool>,
    ) -> NotificationSettings {
        let mut s = NotificationSettings {
            enabled,
            ..Default::default()
        };
        s.preferences.scheduled_tasks.on_finished = on_finished;
        s.preferences.scheduled_tasks.on_failed = on_failed;
        s
    }

    #[test]
    fn an_unset_preference_is_enabled_and_an_explicit_false_is_not() {
        let unset = settings(true, None, None);
        assert!(should_send_for_event(event::TASK_FINISHED, &unset));
        assert!(should_send_for_event(event::TASK_FAILED, &unset));

        let opted_out = settings(true, Some(false), Some(false));
        assert!(!should_send_for_event(event::TASK_FINISHED, &opted_out));
        assert!(!should_send_for_event(event::TASK_FAILED, &opted_out));

        // The two preferences are independent, which is the shape a single
        // "scheduled tasks" flag would collapse.
        let finished_only = settings(true, Some(true), Some(false));
        assert!(should_send_for_event(event::TASK_FINISHED, &finished_only));
        assert!(!should_send_for_event(event::TASK_FAILED, &finished_only));
    }

    #[test]
    fn an_unknown_event_is_sent_and_keeps_its_raw_subject() {
        let s = settings(true, Some(false), Some(false));
        assert!(should_send_for_event("something.else", &s));
        assert_eq!(human_subject("something.else"), "something.else");
    }

    #[test]
    fn the_two_known_subjects_are_gos_wording() {
        assert_eq!(
            human_subject(event::TASK_FINISHED),
            "Scheduled Task Completed Successfully"
        );
        assert_eq!(
            human_subject(event::TASK_FAILED),
            "Scheduled Task Execution Failed"
        );
    }
}

//! The notification reads: `GET /api/notifications/settings` and
//! `GET /api/notifications/log`.
//!
//! Mirrors `handleGetNotificationSettings` and `handleListNotificationLog`
//! (`internal/api/notifications.go`) over `notificationServiceImpl`
//! (`internal/service/notification_service.go`) and `SQLiteNotificationStore`
//! (`internal/storage/sqlite_notification_store.go`).
//!
//! `PUT /api/notifications/settings` and `POST /api/notifications/test` stay
//! with Go: one writes the settings row, the other opens an SMTP connection and
//! sends mail. SMTP delivery as a whole is a write-side concern and is not
//! ported here — nothing in this module talks to a mail server.
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

use std::path::Path;

use axum::http::Method;
use serde::{Deserialize, Serialize};

use super::db;
use super::gotime::GoTime;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_finished: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failed: Option<bool>,
}

/// Mirrors `notification.NotificationPreferences`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPreferences {
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub scheduled_tasks: ScheduledTasksPreferences,
}

/// Mirrors `notification.NotificationSettings`, the shape stored in
/// `user_settings.notification_settings`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSettings {
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub enabled: bool,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub provider: SmtpConfig,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub preferences: NotificationPreferences,
}

/// `service.maskedFieldSentinel`. Also what `PUT` reads back as "unchanged", so
/// the two directions have to agree on the exact string.
const MASKED_FIELD_SENTINEL: &str = "***";

/// `GetSettings`: the stored settings with the password masked.
///
/// An unparseable column is an error rather than a default. That is Go's
/// behaviour — `loadNotificationSettings` returns the decode error and the
/// handler answers 500 — and the seam turns it into a fallback, so the user
/// sees Go's 500 rather than a silently empty form that a save would overwrite
/// their real configuration with.
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

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "notifications",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET
        && (path == "/api/notifications/settings" || path == "/api/notifications/log")
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let body = match req.path {
        "/api/notifications/settings" => super::gojson::to_vec(&get_settings(&ctx.db_path)?)
            .map_err(|e| format!("encoding notification settings: {e}"))?,
        "/api/notifications/log" => {
            let entries = list_log(&ctx.db_path, log_limit(req.query))?;
            super::gojson::to_vec(&entries)
                .map_err(|e| format!("encoding notification log: {e}"))?
        }
        other => return Err(format!("{other} is not a notification read")),
    };
    Ok(super::Answer { body, probe: None })
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

    /// Go answers 500 here, which the seam turns into a fallback. Degrading to
    /// the zero value instead would show an empty form that a save would then
    /// write over the user's real SMTP credentials.
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
                NotificationPreferences::default(),
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
    fn only_the_two_notification_reads_are_claimed() {
        assert!(claims(&Method::GET, "/api/notifications/settings"));
        assert!(claims(&Method::GET, "/api/notifications/log"));
        assert!(!claims(&Method::PUT, "/api/notifications/settings"));
        assert!(!claims(&Method::POST, "/api/notifications/test"));
        assert!(!claims(&Method::GET, "/api/notifications/test"));
        assert!(!claims(&Method::GET, "/api/notifications"));
        assert!(!claims(&Method::GET, "/api/notifications/log/"));
    }
}

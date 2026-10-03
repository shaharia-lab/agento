//! `GET /api/settings/claude-retention` — Claude Code's own transcript
//! retention, read from each indexed config dir (#718) — and
//! `PUT /api/settings/claude-retention`, which can only **raise** it (#720).
//!
//! Claude Code deletes a transcript once it is older than `cleanupPeriodDays`,
//! a top-level key of `<config dir>/settings.json`. Durable history exists
//! because of that number, so Settings → Data shows it, per config dir, beside
//! Agento's own "Keep session history".
//!
//! # Why this is not [`super::get_settings`]
//!
//! That handler reads the **run** config dir alone and answers a 500 for a
//! file that is not JSON, which is right for the editor it feeds. This route
//! needs every *indexed* dir and must never fail over a file's content: a
//! broken `settings.json` is the user's to fix, and the Data pane has to keep
//! rendering while they do. So a file that cannot be decided about is an
//! entry with `source: "unknown"` and a `reason`, and the only 500 left is a
//! settings row that cannot be read.
//!
//! # What is and is not read
//!
//! Only the user-level `<config dir>/settings.json`. Claude Code also takes
//! this key from managed, project-level and `settings.local.json` files, so
//! the value it actually uses can differ; the UI names the file it read.
//!
//! The default is Claude Code's own: its settings schema describes the key as
//! "Number of days to retain chat transcripts before automatic cleanup
//! (default: 30). Minimum 1." (checked against 2.1.285). A stored `0` is
//! reported as the `0` it is — that CLI rejects it and skips cleanup, older
//! ones stopped writing transcripts — and the Data pane words that case
//! itself rather than this route translating it.
//!
//! # The write only ever raises the number
//!
//! Lowering `cleanupPeriodDays` makes Claude Code permanently delete every
//! transcript older than the new value, so the `PUT` refuses — 422, nothing
//! written — anything that would shorten retention. [`guard`] is the whole
//! rule, a pure function over what [`read_retention`] reports:
//!
//! | on disk | requested | answer |
//! |---|---|---|
//! | `unknown` (unreadable, not JSON, not an object, bad value) | any | 422: Agento does not write a file it could not read |
//! | `0` | any | 422: Claude Code rejects `0` and skips cleanup, so any number would *start* deletion |
//! | `n` (key present, or the default 30 when absent) | `< n` | 422: lower than the current value |
//! | `n` from the file | `n` | 200, nothing written |
//! | `n` | `> n`, or `n` when it is the default | 200, the key written |
//!
//! Before the guard, the request itself must name an indexed config dir and a
//! whole number from 1 to [`MAX_CLEANUP_PERIOD_DAYS`]; a missing, `null`,
//! fractional, negative or string value is a 422 on that field, never a zero
//! value. The write is [`super::patch::set_top_level_key_if`] (#719), which changes
//! that one key and no other byte, and the answer is the `GET`'s document,
//! read again after the write.
//!
//! **The guard is decided on the bytes the write replaces.** Claude Code
//! edits this file too, so [`put`] passes [`guard`] to
//! [`super::patch::set_top_level_key_if`] as its `allow`: it runs on the
//! bytes the splice is built from, and the write is refused if the file
//! differs from them just before it is replaced. What is left is `patch`'s
//! own window, between that second read and the rename, stated in its doc.

use std::io;

use axum::http::Method;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::patch::{set_top_level_key_if, PatchError};
use super::{go_json_valid, is_utf8, settings_json_path};
use crate::native::writes::{decode_body, finish, WriteError};
use crate::native::{db, gojson, settings, Answer, Ctx, Endpoint, Request};

/// What Claude Code uses when no settings file names the key.
const DEFAULT_CLEANUP_PERIOD_DAYS: i64 = 30;

/// The highest value the `PUT` accepts: a sanity bound of a hundred years.
/// Claude Code's schema states a minimum of 1 and no maximum (2.1.285).
const MAX_CLEANUP_PERIOD_DAYS: i64 = 36_500;

const PATH: &str = "/api/settings/claude-retention";

/// The route this module adds, which Go never had.
///
/// An owner of `parity/desktop_routes.json`, whose assertion is set equality
/// over the union of every owner's const.
pub const ROUTES: &[(&str, &str)] = &[("GET", PATH), ("PUT", PATH)];

/// This module's entry in `native::ENDPOINTS`. The `PUT` is the only route
/// that calls [`super::patch::set_top_level_key_if`], and its `allow` is
/// [`guard`]: there is no general "set one key" route, because one
/// would be a way to lower `cleanupPeriodDays`.
pub const ENDPOINT: Endpoint = Endpoint {
    name: "claude retention",
    claims,
    serve,
};

/// Where an entry's number came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    /// The key is present and is a whole, non-negative number.
    Settings,
    /// The file or the key is absent, so Claude Code's default applies.
    Default,
    /// The file could not be decided about; `reason` says why.
    Unknown,
}

/// One config dir's answer, fields in wire order.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct RetentionEntry {
    config_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_period_days: Option<i64>,
    source: Source,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct RetentionResponse {
    dirs: Vec<RetentionEntry>,
}

/// The one key this route reads; everything else in the file is skipped
/// without being materialized.
#[derive(Deserialize)]
struct SettingsFile {
    #[serde(default, rename = "cleanupPeriodDays", deserialize_with = "present")]
    cleanup_period_days: Option<Value>,
}

/// A present key as `Some`, **including a `null`**. A plain `Option<Value>`
/// decodes `null` as `None`, which would report a key Claude Code rejects as
/// the default.
fn present<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(de).map(Some)
}

/// Read one config dir's `cleanupPeriodDays`. Never fails and never writes.
fn read_retention(dir: &str) -> RetentionEntry {
    match std::fs::read(settings_json_path(dir)) {
        Ok(data) => retention_of(dir, Some(&data)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => retention_of(dir, None),
        Err(e) => RetentionEntry {
            config_dir: dir.to_string(),
            cleanup_period_days: None,
            source: Source::Unknown,
            reason: Some(format!("the file could not be read ({e})")),
        },
    }
}

/// What a `settings.json` holding `data` says about `cleanupPeriodDays`;
/// `None` is no file. Pure, so the `PUT`'s guard can be decided on the very
/// bytes the write replaces.
fn retention_of(dir: &str, data: Option<&[u8]>) -> RetentionEntry {
    let entry = |days: Option<i64>, source: Source, reason: Option<String>| RetentionEntry {
        config_dir: dir.to_string(),
        cleanup_period_days: days,
        source,
        reason,
    };
    let default = || entry(Some(DEFAULT_CLEANUP_PERIOD_DAYS), Source::Default, None);
    let unknown = |reason: String| entry(None, Source::Unknown, Some(reason));

    let Some(data) = data else {
        return default();
    };
    if !is_utf8(data) {
        return unknown("the file is not valid UTF-8".to_string());
    }
    if !go_json_valid(data) {
        return unknown("the file is not valid JSON".to_string());
    }
    if data.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return unknown("the file is not a JSON object".to_string());
    }
    let file: SettingsFile = match serde_json::from_slice(data) {
        Ok(file) => file,
        // A valid object serde still refuses: the key twice, or a value nested
        // past its recursion limit.
        Err(e) => return unknown(format!("`cleanupPeriodDays` could not be read ({e})")),
    };
    match file.cleanup_period_days {
        None => default(),
        Some(value) => match whole_days(&value) {
            Some(days) => entry(Some(days), Source::Settings, None),
            None => unknown("`cleanupPeriodDays` is not a whole, non-negative number".to_string()),
        },
    }
}

/// A JSON value as a whole, non-negative number of days.
///
/// `90.0` counts: Claude Code reads the file as JavaScript, where it is the
/// integer 90. Anything past 2^53 is not a whole number there either.
fn whole_days(value: &Value) -> Option<i64> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    let number = value.as_number()?;
    if let Some(days) = number.as_i64() {
        return (days >= 0).then_some(days);
    }
    let days = number.as_f64()?;
    (days.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&days)).then_some(days as i64)
}

fn response_for(dirs: &[String]) -> RetentionResponse {
    RetentionResponse {
        dirs: dirs.iter().map(|dir| read_retention(dir)).collect(),
    }
}

/// The `PUT` body. Both fields are checked by hand rather than by serde, so
/// that a missing or `null` value is a 422 naming the field and not a zero
/// value.
#[derive(Deserialize)]
struct RetentionRequest {
    #[serde(default, deserialize_with = "present")]
    config_dir: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    cleanup_period_days: Option<Value>,
}

/// What [`guard`] allows.
#[derive(Debug, PartialEq, Eq)]
enum Write {
    /// Write the requested value.
    Apply,
    /// The file already says exactly this; write nothing.
    Noop,
}

/// The raise-only rule (see the module doc's table). `current` is
/// [`read_retention`]'s answer for the dir about to be written.
fn guard(current: &RetentionEntry, requested: i64) -> Result<Write, WriteError> {
    let days = match (current.source, current.cleanup_period_days) {
        (Source::Unknown, _) | (_, None) => {
            let reason = current
                .reason
                .as_deref()
                .unwrap_or("the file could not be read");
            return Err(WriteError::validation(
                "config_dir",
                format!("{reason}; Agento does not write a settings.json it could not read"),
            ));
        }
        (_, Some(days)) => days,
    };
    if days == 0 {
        return Err(WriteError::validation(
            "cleanup_period_days",
            "the current value is 0, which Claude Code rejects, so it never cleans up; \
             any number would start deleting transcripts, so Agento leaves it as it is",
        ));
    }
    if requested < days {
        return Err(WriteError::validation(
            "cleanup_period_days",
            format!(
                "{requested} is lower than the current {days}; \
                 Agento only extends Claude Code's retention"
            ),
        ));
    }
    if requested == days && current.source == Source::Settings {
        return Ok(Write::Noop);
    }
    Ok(Write::Apply)
}

/// The requested number of days: a whole number from 1 to
/// [`MAX_CLEANUP_PERIOD_DAYS`].
fn requested_days(value: Option<&Value>) -> Result<i64, WriteError> {
    let invalid = |message: String| WriteError::validation("cleanup_period_days", message);
    let value = match value {
        None | Some(Value::Null) => return Err(invalid("is required".to_string())),
        Some(value) => value,
    };
    match whole_days(value) {
        Some(days) if (1..=MAX_CLEANUP_PERIOD_DAYS).contains(&days) => Ok(days),
        _ => Err(invalid(format!(
            "must be a whole number from 1 to {MAX_CLEANUP_PERIOD_DAYS}, got {value}"
        ))),
    }
}

/// A refused or failed single-key write, as the route answers it.
fn patch_error(e: PatchError) -> WriteError {
    match e {
        PatchError::ChangedUnderneath => WriteError::ConflictMessage(e.to_string()),
        PatchError::DirNotIndexed
        | PatchError::NotUtf8
        | PatchError::NotJson
        | PatchError::NotAnObject
        | PatchError::DuplicateKey => WriteError::validation("config_dir", e.to_string()),
        PatchError::InvalidValue | PatchError::Io(_) => {
            WriteError::Fallback(format!("writing cleanupPeriodDays: {e}"))
        }
    }
}

/// `PUT`: decode, check the request, apply [`guard`], write, read back.
/// Every refusal happens before anything is written.
fn put(dirs: &[String], body: &[u8]) -> Result<Answer, WriteError> {
    put_with(dirs, body, || {})
}

/// [`put`] with its seam: `before_write` runs after the request has passed
/// the guard and before the write reads the file. Tests use it to stand in
/// for Claude Code editing the file in between.
fn put_with(
    dirs: &[String],
    body: &[u8],
    before_write: impl FnOnce(),
) -> Result<Answer, WriteError> {
    let request: RetentionRequest = decode_body(body)?;
    let days = requested_days(request.cleanup_period_days.as_ref())?;
    let dir = match request.config_dir {
        Some(Value::String(dir)) if dirs.contains(&dir) => dir,
        _ => {
            return Err(WriteError::validation(
                "config_dir",
                "must be one of the config dirs Agento indexes",
            ))
        }
    };
    // Refused here first, so a file that cannot even be read is a 422 with
    // its reason rather than the write's I/O failure.
    guard(&read_retention(&dir), days)?;
    before_write();
    // Then decided again on the bytes the write replaces: Claude Code edits
    // this file too, and a value it raised (or set to 0) since the read above
    // must not be overwritten with a lower one.
    let mut refused = None;
    set_top_level_key_if(
        dirs,
        &dir,
        "cleanupPeriodDays",
        &days.to_string(),
        |data| match guard(&retention_of(&dir, data), days) {
            Ok(write) => write == Write::Apply,
            Err(e) => {
                refused = Some(e);
                false
            }
        },
    )
    .map_err(patch_error)?;
    if let Some(e) = refused {
        return Err(e);
    }
    let body = gojson::to_vec(&response_for(dirs))
        .map_err(|e| WriteError::Fallback(format!("encoding claude retention: {e}")))?;
    Ok(Answer::json(body))
}

fn claims(method: &Method, path: &str) -> bool {
    path == PATH && (method == Method::GET || method == Method::PUT)
}

fn serve(ctx: &Ctx, req: &Request) -> Result<Answer, String> {
    let dirs = {
        let conn = db::open_read_only(&ctx.db_path)?;
        settings::indexed_claude_config_dirs(&conn)
    };
    if req.method == Method::PUT {
        return finish(put(&dirs, req.body));
    }
    let body = gojson::to_vec(&response_for(&dirs))
        .map_err(|e| format!("encoding claude retention: {e}"))?;
    Ok(Answer::json(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// A config dir whose `settings.json` holds `content`.
    fn dir_with(content: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("settings.json"), content).expect("settings.json");
        dir
    }

    fn read(dir: &tempfile::TempDir) -> RetentionEntry {
        read_retention(&dir.path().to_string_lossy())
    }

    fn unknown_reason(content: &[u8]) -> String {
        let entry = read(&dir_with(content));
        assert_eq!(entry.source, Source::Unknown, "{entry:?}");
        assert_eq!(entry.cleanup_period_days, None, "{entry:?}");
        entry.reason.expect("an unknown entry explains itself")
    }

    #[test]
    fn a_stored_whole_number_is_reported_as_settings() {
        for (content, days) in [
            (&br#"{"model":"opus","cleanupPeriodDays":90}"#[..], 90),
            (br#"{"cleanupPeriodDays": 0}"#, 0),
            (
                br#"{"cleanupPeriodDays":3650,"hooks":{"a":[1,{"b":2}]}}"#,
                3650,
            ),
            (br#"{"cleanupPeriodDays":90.0}"#, 90),
            (br#"{"cleanupPeriodDays":1e2}"#, 100),
        ] {
            let entry = read(&dir_with(content));
            assert_eq!(entry.source, Source::Settings, "{entry:?}");
            assert_eq!(entry.cleanup_period_days, Some(days), "{entry:?}");
            assert_eq!(entry.reason, None, "{entry:?}");
        }
    }

    #[test]
    fn a_missing_file_or_a_missing_key_is_the_default() {
        let empty = tempfile::tempdir().expect("temp dir");
        let missing_dir = empty.path().join("never-created");
        for entry in [
            read(&empty),
            read_retention(&missing_dir.to_string_lossy()),
            read(&dir_with(b"{}")),
            read(&dir_with(
                br#"{"model":"opus","nested":{"cleanupPeriodDays":5}}"#,
            )),
        ] {
            assert_eq!(entry.source, Source::Default, "{entry:?}");
            assert_eq!(entry.cleanup_period_days, Some(30), "{entry:?}");
            assert_eq!(entry.reason, None, "{entry:?}");
        }
    }

    #[test]
    fn a_value_that_is_not_a_whole_non_negative_number_is_an_explained_unknown() {
        for content in [
            &br#"{"cleanupPeriodDays":"90"}"#[..],
            br#"{"cleanupPeriodDays":-1}"#,
            br#"{"cleanupPeriodDays":1.5}"#,
            br#"{"cleanupPeriodDays":null}"#,
            br#"{"cleanupPeriodDays":true}"#,
            br#"{"cleanupPeriodDays":[30]}"#,
            br#"{"cleanupPeriodDays":1e300}"#,
        ] {
            assert_eq!(
                unknown_reason(content),
                "`cleanupPeriodDays` is not a whole, non-negative number",
                "{}",
                String::from_utf8_lossy(content)
            );
        }
    }

    #[test]
    fn a_file_that_cannot_be_decided_about_is_an_explained_unknown() {
        assert_eq!(unknown_reason(b"not json"), "the file is not valid JSON");
        assert_eq!(unknown_reason(b""), "the file is not valid JSON");
        assert_eq!(
            unknown_reason(br#"{"cleanupPeriodDays":90"#),
            "the file is not valid JSON"
        );
        assert_eq!(unknown_reason(b" [30]"), "the file is not a JSON object");
        assert_eq!(unknown_reason(b"30"), "the file is not a JSON object");
        assert_eq!(
            unknown_reason(b"{\"a\":\"\xff\"}"),
            "the file is not valid UTF-8"
        );
        assert!(
            unknown_reason(br#"{"cleanupPeriodDays":1,"cleanupPeriodDays":2}"#)
                .starts_with("`cleanupPeriodDays` could not be read (duplicate field"),
        );
    }

    /// A directory where the file should be: `read` fails with something other
    /// than `NotFound` on every platform, without needing a permission bit.
    #[test]
    fn an_unreadable_file_is_an_explained_unknown() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("settings.json")).expect("a directory in the way");
        let entry = read(&dir);
        assert_eq!(entry.source, Source::Unknown, "{entry:?}");
        assert!(
            entry
                .reason
                .as_deref()
                .is_some_and(|r| r.starts_with("the file could not be read (")),
            "{entry:?}"
        );
    }

    #[test]
    fn each_dir_is_answered_in_the_order_given() {
        let ninety = dir_with(br#"{"cleanupPeriodDays":90}"#);
        let no_key = dir_with(br#"{"model":"opus"}"#);
        let dirs = [
            ninety.path().to_string_lossy().into_owned(),
            no_key.path().to_string_lossy().into_owned(),
        ];
        let response = response_for(&dirs);
        assert_eq!(
            response.dirs,
            vec![
                RetentionEntry {
                    config_dir: dirs[0].clone(),
                    cleanup_period_days: Some(90),
                    source: Source::Settings,
                    reason: None,
                },
                RetentionEntry {
                    config_dir: dirs[1].clone(),
                    cleanup_period_days: Some(30),
                    source: Source::Default,
                    reason: None,
                },
            ]
        );
    }

    /// The envelope, byte for byte: key order, the two omitted keys, and the
    /// trailing newline `gojson::to_vec` ends every body with.
    #[test]
    fn the_envelope_is_pinned_byte_for_byte() {
        let response = RetentionResponse {
            dirs: vec![
                RetentionEntry {
                    config_dir: "/home/u/.claude".to_string(),
                    cleanup_period_days: Some(90),
                    source: Source::Settings,
                    reason: None,
                },
                RetentionEntry {
                    config_dir: "/home/u/.claude-work".to_string(),
                    cleanup_period_days: Some(30),
                    source: Source::Default,
                    reason: None,
                },
                RetentionEntry {
                    config_dir: "/home/u/.claude-old".to_string(),
                    cleanup_period_days: None,
                    source: Source::Unknown,
                    reason: Some("the file is not valid JSON".to_string()),
                },
            ],
        };
        assert_eq!(
            String::from_utf8(gojson::to_vec(&response).expect("encode")).expect("utf-8"),
            concat!(
                r#"{"dirs":["#,
                r#"{"config_dir":"/home/u/.claude","cleanup_period_days":90,"source":"settings"},"#,
                r#"{"config_dir":"/home/u/.claude-work","cleanup_period_days":30,"source":"default"},"#,
                r#"{"config_dir":"/home/u/.claude-old","source":"unknown","reason":"the file is not valid JSON"}"#,
                "]}\n",
            )
        );
        assert_eq!(
            String::from_utf8(gojson::to_vec(&response_for(&[])).expect("encode")).expect("utf-8"),
            "{\"dirs\":[]}\n",
            "an empty list is an array, never null: the default dir is always indexed"
        );
    }

    #[test]
    fn only_a_get_or_a_put_on_the_exact_path_is_claimed() {
        assert!(claims(&Method::GET, PATH));
        assert!(claims(&Method::PUT, PATH));
        for method in [Method::POST, Method::PATCH, Method::DELETE] {
            assert!(!claims(&method, PATH), "{method}");
        }
        for method in [Method::GET, Method::PUT] {
            assert!(!claims(&method, "/api/settings/claude-retention/"));
            assert!(!claims(&method, "/api/settings/claude-retention/x"));
        }
    }

    /// Every file under a dir, with its bytes and modification time.
    fn snapshot(
        root: &std::path::Path,
    ) -> Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)> {
        let mut out = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("listing") {
                let path = entry.expect("entry").path();
                let meta = std::fs::metadata(&path).expect("metadata");
                let modified = meta.modified().expect("mtime");
                if meta.is_dir() {
                    out.push((path.clone(), Vec::new(), modified));
                    pending.push(path);
                } else {
                    out.push((path.clone(), std::fs::read(&path).expect("bytes"), modified));
                }
            }
        }
        out.sort();
        out
    }

    /// The route end to end, over a migrated database and a scratch `HOME`:
    /// the default dir first, then the stored extra dirs, and nothing under
    /// any of them created or modified by the read — including the default
    /// dir, which has no `settings.json` and must not gain one.
    #[test]
    fn the_route_answers_every_indexed_dir_and_writes_nothing() {
        let _env = crate::paths::tests::env_lock();
        let home = tempfile::tempdir().expect("temp dir");
        let _home_var = crate::paths::tests::EnvVar::set("HOME", home.path());
        let _dir_var = crate::paths::tests::EnvVar::unset("CLAUDE_CONFIG_DIR");

        let default_dir = home.path().join(".claude");
        let work = home.path().join(".claude-work");
        let broken = home.path().join(".claude-broken");
        for dir in [&default_dir, &work, &broken] {
            std::fs::create_dir_all(dir.join("projects")).expect("config dir");
        }
        std::fs::write(work.join("settings.json"), br#"{"cleanupPeriodDays":90}"#).expect("work");
        std::fs::write(broken.join("settings.json"), b"{oops").expect("broken");

        let db_path = home.path().join("agento.db");
        {
            let mut conn = rusqlite::Connection::open(&db_path).expect("open");
            crate::native::migrate::apply(&mut conn).expect("migrate");
            conn.execute(
                "INSERT INTO user_settings (id, claude_config_dirs) VALUES (1, ?1)",
                [serde_json::to_string(&[&work, &broken]).expect("dirs")],
            )
            .expect("settings row");
        }

        let config_dirs = [&default_dir, &work, &broken];
        let before: Vec<_> = config_dirs.iter().map(|d| snapshot(d)).collect();

        let answer = serve(
            &Ctx { db_path },
            &Request {
                method: &Method::GET,
                path: PATH,
                query: "",
                content_type: "",
                secret_token: "",
                body: &[],
            },
        )
        .expect("the route answers");

        assert_eq!(answer.status, StatusCode::OK);
        assert!(!answer.text, "served as application/json");
        let show = |p: &std::path::Path| serde_json::to_string(&p.to_string_lossy()).expect("path");
        assert_eq!(
            String::from_utf8(answer.body.expect("a body")).expect("utf-8"),
            format!(
                concat!(
                    r#"{{"dirs":[{{"config_dir":{},"cleanup_period_days":30,"source":"default"}},"#,
                    r#"{{"config_dir":{},"cleanup_period_days":90,"source":"settings"}},"#,
                    r#"{{"config_dir":{},"source":"unknown","reason":"the file is not valid JSON"}}]}}"#,
                    "\n"
                ),
                show(&default_dir),
                show(&work),
                show(&broken),
            )
        );

        let after: Vec<_> = config_dirs.iter().map(|d| snapshot(d)).collect();
        assert_eq!(
            before, after,
            "a read must leave every config dir as it found it"
        );
    }
    /// A key written by `patch` reads back as that number from the file, and
    /// a second indexed dir keeps its own answer.
    #[test]
    fn a_patched_key_reads_back_as_settings() {
        let work = dir_with(br#"{"model":"opus"}"#);
        let other = dir_with(br#"{"cleanupPeriodDays":14}"#);
        let dirs = [
            work.path().to_string_lossy().into_owned(),
            other.path().to_string_lossy().into_owned(),
        ];
        super::super::patch::set_top_level_key(&dirs, &dirs[0], "cleanupPeriodDays", "365")
            .expect("write");

        let response = response_for(&dirs);
        assert_eq!(response.dirs[0].source, Source::Settings);
        assert_eq!(response.dirs[0].cleanup_period_days, Some(365));
        assert_eq!(response.dirs[1].cleanup_period_days, Some(14));
    }

    /// With no home at all the default dir is `/root/.claude`, which a normal
    /// user cannot read. That is still an answer, not a failure: the route
    /// leads with that dir and says `default` or an explained `unknown`.
    #[test]
    fn without_a_home_the_route_still_answers_for_the_fallback_dir() {
        let _env = crate::paths::tests::env_lock();
        let scratch = tempfile::tempdir().expect("temp dir");
        let _home_var = crate::paths::tests::EnvVar::unset("HOME");
        let _profile_var = crate::paths::tests::EnvVar::unset("USERPROFILE");
        let _dir_var = crate::paths::tests::EnvVar::unset("CLAUDE_CONFIG_DIR");

        let db_path = scratch.path().join("agento.db");
        {
            let mut conn = rusqlite::Connection::open(&db_path).expect("open");
            crate::native::migrate::apply(&mut conn).expect("migrate");
        }

        let answer = serve(
            &Ctx { db_path },
            &Request {
                method: &Method::GET,
                path: PATH,
                query: "",
                content_type: "",
                secret_token: "",
                body: &[],
            },
        )
        .expect("the route answers");
        assert_eq!(answer.status, StatusCode::OK);

        let body: Value = serde_json::from_slice(&answer.body.expect("a body")).expect("json");
        let dirs = body["dirs"].as_array().expect("dirs");
        assert_eq!(dirs.len(), 1, "{body}");
        assert_eq!(dirs[0]["config_dir"], "/root/.claude", "{body}");
        match dirs[0]["source"].as_str() {
            Some("default") => assert_eq!(dirs[0]["cleanup_period_days"], 30, "{body}"),
            Some("unknown") => assert!(dirs[0]["reason"].is_string(), "{body}"),
            // Only a test run as root, on a machine whose root has set the key.
            Some("settings") => assert!(dirs[0]["cleanup_period_days"].is_u64(), "{body}"),
            other => panic!("unexpected source {other:?} in {body}"),
        }
    }

    fn entry(days: Option<i64>, source: Source) -> RetentionEntry {
        RetentionEntry {
            config_dir: "/home/u/.claude".to_string(),
            cleanup_period_days: days,
            source,
            reason: (source == Source::Unknown).then(|| "the file is not valid JSON".to_string()),
        }
    }

    /// The raise-only rule as a table: every refusal and both accept arms.
    #[test]
    fn the_guard_only_ever_lets_the_value_rise() {
        use Source::{Default, Settings, Unknown};
        let allowed = [
            (entry(Some(90), Settings), 91, Write::Apply),
            (entry(Some(90), Settings), 36_500, Write::Apply),
            (entry(Some(90), Settings), 90, Write::Noop),
            (entry(Some(1), Settings), 2, Write::Apply),
            (entry(Some(30), Default), 31, Write::Apply),
            (entry(Some(30), Default), 30, Write::Apply),
        ];
        for (current, requested, expected) in allowed {
            assert_eq!(
                guard(&current, requested).expect("allowed"),
                expected,
                "{current:?} -> {requested}"
            );
        }

        let refused = [
            (
                entry(Some(90), Settings),
                60,
                "validation error for \"cleanup_period_days\": 60 is lower than the current 90; \
                 Agento only extends Claude Code's retention",
            ),
            (
                entry(Some(90), Settings),
                89,
                "validation error for \"cleanup_period_days\": 89 is lower than the current 90; \
                 Agento only extends Claude Code's retention",
            ),
            (
                entry(Some(30), Default),
                29,
                "validation error for \"cleanup_period_days\": 29 is lower than the current 30; \
                 Agento only extends Claude Code's retention",
            ),
            (
                entry(Some(0), Settings),
                365,
                "validation error for \"cleanup_period_days\": the current value is 0, which \
                 Claude Code rejects, so it never cleans up; any number would start deleting \
                 transcripts, so Agento leaves it as it is",
            ),
            (
                entry(None, Unknown),
                365,
                "validation error for \"config_dir\": the file is not valid JSON; Agento does \
                 not write a settings.json it could not read",
            ),
        ];
        for (current, requested, message) in refused {
            let e = guard(&current, requested).expect_err("refused");
            assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY, "{current:?}");
            assert_eq!(e.message(), message, "{current:?} -> {requested}");
        }
    }

    /// A scratch `HOME` with a migrated database and three indexed config
    /// dirs: the default one (no `settings.json`), `work` and `other`.
    struct Scratch {
        // Declared before the lock so the variables are restored under it.
        _vars: [crate::paths::tests::EnvVar; 2],
        _env: std::sync::MutexGuard<'static, ()>,
        home: tempfile::TempDir,
    }

    impl Scratch {
        fn new(work: Option<&[u8]>, other: &[u8]) -> Self {
            let env = crate::paths::tests::env_lock();
            let home = tempfile::tempdir().expect("temp dir");
            let vars = [
                crate::paths::tests::EnvVar::set("HOME", home.path()),
                crate::paths::tests::EnvVar::unset("CLAUDE_CONFIG_DIR"),
            ];
            let scratch = Self {
                _vars: vars,
                _env: env,
                home,
            };
            for dir in ["", "-work", "-other"] {
                std::fs::create_dir_all(scratch.dir(dir)).expect("config dir");
            }
            if let Some(work) = work {
                std::fs::write(scratch.file("-work"), work).expect("work");
            }
            std::fs::write(scratch.file("-other"), other).expect("other");
            let mut conn = rusqlite::Connection::open(scratch.db()).expect("open");
            crate::native::migrate::apply(&mut conn).expect("migrate");
            conn.execute(
                "INSERT INTO user_settings (id, claude_config_dirs) VALUES (1, ?1)",
                [
                    serde_json::to_string(&[scratch.dir("-work"), scratch.dir("-other")])
                        .expect("dirs"),
                ],
            )
            .expect("settings row");
            scratch
        }

        fn db(&self) -> std::path::PathBuf {
            self.home.path().join("agento.db")
        }

        /// `~/.claude<suffix>`.
        fn dir(&self, suffix: &str) -> String {
            self.home
                .path()
                .join(format!(".claude{suffix}"))
                .to_string_lossy()
                .into_owned()
        }

        fn file(&self, suffix: &str) -> std::path::PathBuf {
            std::path::Path::new(&self.dir(suffix)).join("settings.json")
        }

        fn everything(&self) -> Vec<Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)>> {
            ["", "-work", "-other"]
                .iter()
                .map(|suffix| snapshot(std::path::Path::new(&self.dir(suffix))))
                .collect()
        }

        fn put(&self, body: &str) -> (StatusCode, String) {
            let answer = serve(
                &Ctx { db_path: self.db() },
                &Request {
                    method: &Method::PUT,
                    path: PATH,
                    query: "",
                    content_type: "application/json",
                    secret_token: "",
                    body: body.as_bytes(),
                },
            )
            .expect("the route answers");
            assert!(!answer.text, "served as application/json");
            (
                answer.status,
                String::from_utf8(answer.body.expect("a body")).expect("utf-8"),
            )
        }

        fn put_days(&self, suffix: &str, days: &str) -> (StatusCode, String) {
            let dir = serde_json::to_string(&self.dir(suffix)).expect("dir");
            self.put(&format!(
                r#"{{"config_dir":{dir},"cleanup_period_days":{days}}}"#
            ))
        }

        /// A request that must be refused with `status` and leave every file
        /// under every config dir with the bytes and mtime it had.
        fn refuses(&self, status: StatusCode, body: &str, error: &str) {
            let before = self.everything();
            let (got, answer) = self.put(body);
            assert_eq!(got, status, "{body}: {answer}");
            assert_eq!(
                answer,
                format!("{}\n", serde_json::json!({ "error": error })),
                "{body}"
            );
            assert_eq!(before, self.everything(), "{body} must write nothing");
        }
    }

    const LOWER: &str = "; Agento only extends Claude Code's retention";

    /// A value lower than the effective one is a 422 that leaves bytes and
    /// mtime alone: key present (90 → 60), key absent (30 → 20), file absent.
    #[test]
    fn a_lower_value_is_refused_and_writes_nothing() {
        for (work, requested, current) in [
            (
                Some(&br#"{"model":"opus","cleanupPeriodDays":90}"#[..]),
                60,
                90,
            ),
            (Some(&br#"{"model":"opus"}"#[..]), 20, 30),
            (None, 20, 30),
        ] {
            let scratch = Scratch::new(work, br#"{"cleanupPeriodDays":14}"#);
            let dir = serde_json::to_string(&scratch.dir("-work")).expect("dir");
            scratch.refuses(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!(r#"{{"config_dir":{dir},"cleanup_period_days":{requested}}}"#),
                &format!(
                    "validation error for \"cleanup_period_days\": \
                     {requested} is lower than the current {current}{LOWER}"
                ),
            );
            assert_eq!(scratch.file("-work").exists(), work.is_some());
        }
    }

    #[test]
    fn a_stored_zero_and_an_unreadable_file_are_never_written() {
        let scratch = Scratch::new(Some(br#"{"cleanupPeriodDays":0}"#), b"{oops");
        let work = serde_json::to_string(&scratch.dir("-work")).expect("dir");
        let other = serde_json::to_string(&scratch.dir("-other")).expect("dir");
        scratch.refuses(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!(r#"{{"config_dir":{work},"cleanup_period_days":365}}"#),
            "validation error for \"cleanup_period_days\": the current value is 0, which \
             Claude Code rejects, so it never cleans up; any number would start deleting \
             transcripts, so Agento leaves it as it is",
        );
        scratch.refuses(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!(r#"{{"config_dir":{other},"cleanup_period_days":365}}"#),
            "validation error for \"config_dir\": the file is not valid JSON; Agento does \
             not write a settings.json it could not read",
        );
    }

    #[test]
    fn a_request_that_is_not_a_dir_and_a_whole_number_in_range_is_refused() {
        let scratch = Scratch::new(Some(br#"{"cleanupPeriodDays":90}"#), b"{}");
        let work = serde_json::to_string(&scratch.dir("-work")).expect("dir");
        let not_indexed =
            serde_json::to_string(&scratch.home.path().join("elsewhere").to_string_lossy())
                .expect("dir");
        let dir_error = "validation error for \"config_dir\": \
                         must be one of the config dirs Agento indexes";
        for config_dir in [not_indexed.as_str(), "null", "7", r#""""#] {
            scratch.refuses(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!(r#"{{"config_dir":{config_dir},"cleanup_period_days":365}}"#),
                dir_error,
            );
        }
        scratch.refuses(
            StatusCode::UNPROCESSABLE_ENTITY,
            r#"{"cleanup_period_days":365}"#,
            dir_error,
        );
        assert!(
            !scratch.home.path().join("elsewhere").exists(),
            "a dir that is not indexed is not created"
        );

        for days in ["1.5", "-1", "0", r#""365""#, "36501", "true", "[365]"] {
            scratch.refuses(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!(r#"{{"config_dir":{work},"cleanup_period_days":{days}}}"#),
                &format!(
                    "validation error for \"cleanup_period_days\": \
                     must be a whole number from 1 to 36500, got {days}"
                ),
            );
        }
        let required = "validation error for \"cleanup_period_days\": is required";
        scratch.refuses(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!(r#"{{"config_dir":{work},"cleanup_period_days":null}}"#),
            required,
        );
        scratch.refuses(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!(r#"{{"config_dir":{work}}}"#),
            required,
        );
        for malformed in ["", "not json", "[]", "365"] {
            scratch.refuses(StatusCode::BAD_REQUEST, malformed, "invalid JSON body");
        }
    }

    /// A higher value changes that one key and nothing else, in that one dir,
    /// and the answer is the `GET`'s document read after the write.
    #[test]
    fn a_higher_value_changes_only_that_key_in_only_that_dir() {
        let scratch = Scratch::new(
            Some(b"{\n  \"model\": \"opus\",\n  \"cleanupPeriodDays\": 90,\n  \"n\": 1e2\n}\n"),
            br#"{"cleanupPeriodDays":14}"#,
        );
        let others = |scratch: &Scratch| {
            [
                snapshot(std::path::Path::new(&scratch.dir(""))),
                snapshot(std::path::Path::new(&scratch.dir("-other"))),
            ]
        };
        let untouched = others(&scratch);

        let (status, body) = scratch.put_days("-work", "365");
        assert_eq!(status, StatusCode::OK, "{body}");
        let show = |suffix: &str| serde_json::to_string(&scratch.dir(suffix)).expect("dir");
        assert_eq!(
            body,
            format!(
                concat!(
                    r#"{{"dirs":[{{"config_dir":{},"cleanup_period_days":30,"source":"default"}},"#,
                    r#"{{"config_dir":{},"cleanup_period_days":365,"source":"settings"}},"#,
                    r#"{{"config_dir":{},"cleanup_period_days":14,"source":"settings"}}]}}"#,
                    "\n"
                ),
                show(""),
                show("-work"),
                show("-other"),
            )
        );
        assert_eq!(
            std::fs::read(scratch.file("-work")).expect("work"),
            b"{\n  \"model\": \"opus\",\n  \"cleanupPeriodDays\": 365,\n  \"n\": 1e2\n}\n"
        );
        assert_eq!(
            untouched,
            others(&scratch),
            "every other config dir is left as it was"
        );
    }

    /// The value the file already holds is a 200 that writes nothing; the
    /// default made explicit, and a file that does not exist yet, are writes.
    #[test]
    fn an_equal_value_writes_nothing_and_an_absent_key_or_file_is_written() {
        let scratch = Scratch::new(Some(br#"{"cleanupPeriodDays":90}"#), br#"{"model":"opus"}"#);
        let before = scratch.everything();
        let (status, body) = scratch.put_days("-work", "90");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            before,
            scratch.everything(),
            "an equal value writes nothing"
        );

        let (status, body) = scratch.put_days("-other", "90.0");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            std::fs::read(scratch.file("-other")).expect("other"),
            br#"{"model":"opus","cleanupPeriodDays":90}"#
        );

        let (status, body) = scratch.put_days("", "365");
        assert_eq!(status, StatusCode::OK, "{body}");
        let written = read_retention(&scratch.dir(""));
        assert_eq!(written.cleanup_period_days, Some(365));
        assert_eq!(written.source, Source::Settings);
    }

    /// Claude Code edits the file after the request passed the guard: the
    /// rule is decided again on the bytes the write replaces, so a value it
    /// raised, or set to `0`, is not overwritten with a lower one.
    #[test]
    fn a_value_raised_or_zeroed_before_the_write_is_not_lowered() {
        for (edited, error) in [
            (
                &br#"{"cleanupPeriodDays":400}"#[..],
                "validation error for \"cleanup_period_days\": 365 is lower than the \
                 current 400; Agento only extends Claude Code's retention",
            ),
            (
                br#"{"cleanupPeriodDays":0}"#,
                "validation error for \"cleanup_period_days\": the current value is 0, which \
                 Claude Code rejects, so it never cleans up; any number would start deleting \
                 transcripts, so Agento leaves it as it is",
            ),
            (
                b"{oops",
                "validation error for \"config_dir\": the file is not valid JSON; Agento does \
                 not write a settings.json it could not read",
            ),
        ] {
            let scratch = Scratch::new(Some(br#"{"cleanupPeriodDays":90}"#), b"{}");
            let dirs = [scratch.dir(""), scratch.dir("-work"), scratch.dir("-other")];
            let body = format!(
                r#"{{"config_dir":{},"cleanup_period_days":365}}"#,
                serde_json::to_string(&dirs[1]).expect("dir")
            );
            let refused = put_with(&dirs, body.as_bytes(), || {
                std::fs::write(scratch.file("-work"), edited).expect("claude code writes");
            })
            .expect_err("refused");
            assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(refused.message(), error);
            assert_eq!(std::fs::read(scratch.file("-work")).expect("work"), edited);
        }
    }

    #[test]
    fn a_refused_patch_is_a_422_a_race_is_a_409_and_an_io_failure_is_a_500() {
        for refused in [
            PatchError::DirNotIndexed,
            PatchError::NotUtf8,
            PatchError::NotJson,
            PatchError::NotAnObject,
            PatchError::DuplicateKey,
        ] {
            assert_eq!(
                patch_error(refused.clone()).status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{refused:?}"
            );
        }
        let race = patch_error(PatchError::ChangedUnderneath);
        assert_eq!(race.status(), StatusCode::CONFLICT);
        assert_eq!(
            race.message(),
            "settings.json changed while it was being edited; try again"
        );
        for broken in [
            PatchError::Io("disk full".to_string()),
            PatchError::InvalidValue,
        ] {
            assert_eq!(
                patch_error(broken).status(),
                StatusCode::INTERNAL_SERVER_ERROR
            );
        }
    }
}

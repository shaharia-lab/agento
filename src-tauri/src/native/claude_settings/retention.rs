//! `GET /api/settings/claude-retention` — Claude Code's own transcript
//! retention, read from each indexed config dir and never written (#718).
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

use std::io;

use axum::http::Method;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::{go_json_valid, is_utf8, settings_json_path};
use crate::native::{db, gojson, settings, Answer, Ctx, Endpoint, Request};

/// What Claude Code uses when no settings file names the key.
const DEFAULT_CLEANUP_PERIOD_DAYS: i64 = 30;

const PATH: &str = "/api/settings/claude-retention";

/// The route this module adds, which Go never had.
///
/// An owner of `parity/desktop_routes.json`, whose assertion is set equality
/// over the union of every owner's const.
pub const ROUTES: &[(&str, &str)] = &[("GET", PATH)];

/// This module's entry in `native::ENDPOINTS`. GET-only on purpose: writing
/// the key is #719's, under its own route.
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
    let entry = |days: Option<i64>, source: Source, reason: Option<String>| RetentionEntry {
        config_dir: dir.to_string(),
        cleanup_period_days: days,
        source,
        reason,
    };
    let default = || entry(Some(DEFAULT_CLEANUP_PERIOD_DAYS), Source::Default, None);
    let unknown = |reason: String| entry(None, Source::Unknown, Some(reason));

    let data = match std::fs::read(settings_json_path(dir)) {
        Ok(data) => data,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return default(),
        Err(e) => return unknown(format!("the file could not be read ({e})")),
    };
    if !is_utf8(&data) {
        return unknown("the file is not valid UTF-8".to_string());
    }
    if !go_json_valid(&data) {
        return unknown("the file is not valid JSON".to_string());
    }
    if data.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return unknown("the file is not a JSON object".to_string());
    }
    let file: SettingsFile = match serde_json::from_slice(&data) {
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

fn claims(method: &Method, path: &str) -> bool {
    path == PATH && method == Method::GET
}

fn serve(ctx: &Ctx, _req: &Request) -> Result<Answer, String> {
    let dirs = {
        let conn = db::open_read_only(&ctx.db_path)?;
        settings::indexed_claude_config_dirs(&conn)
    };
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
    fn only_a_get_on_the_exact_path_is_claimed() {
        assert!(claims(&Method::GET, PATH));
        for method in [Method::PUT, Method::POST, Method::PATCH, Method::DELETE] {
            assert!(!claims(&method, PATH), "{method}");
        }
        assert!(!claims(&Method::GET, "/api/settings/claude-retention/"));
        assert!(!claims(&Method::GET, "/api/settings/claude-retention/x"));
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
}

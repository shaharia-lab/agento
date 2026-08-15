//! The user preferences a read has to honour, and the Claude config dirs they
//! scope it to.
//!
//! Go keeps both as process-wide snapshots (`claudesessions.dataSettings`,
//! `config.claudeDirs`) installed during startup wiring, because the readers —
//! the scanner, nine insight processors, the journey builder, the sessions list
//! — have no settings dependency and must all agree. A ported endpoint has no
//! startup wiring to hook into, so it reads the same row from SQLite on demand.
//! That is not merely convenient: the settings row is the authority, and
//! re-reading it means a preference saved through the Go server is honoured by
//! the very next native request rather than at some later restart.
//!
//! Getting this wrong does not fail loudly. A hidden project that is not
//! filtered out simply appears — in a list the user has told us to leave it out
//! of, and in totals the Go server computes without it.
//!
//! Since #266 this module also answers `GET /api/settings`, which is the whole
//! row rather than the four columns a session read cares about. **One reader,
//! not two**: [`load_stored`] is the single `SELECT`, [`load`] narrows it to
//! [`DataSettings`] and [`resolve`] applies the defaults and env overrides the
//! way `config.SettingsManager` does. A second reader would drift, and the drift
//! would show as the settings page disagreeing with the sessions list about
//! which projects are hidden.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::http::Method;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::paths;

/// The Data & Analytics preferences a session read depends on.
#[derive(Debug, Clone, Default)]
pub struct DataSettings {
    /// Decoded project paths excluded from every figure Agento reports.
    /// Matched against `claude_session_cache.project_path` exactly.
    pub hidden_projects: Vec<String>,
    /// Every Claude config dir Agento indexes, default first. Order is
    /// load-bearing on the Go side (it decides which dir owns a session present
    /// in two); here it only has to contain the same set.
    pub indexed_config_dirs: Vec<String>,
    /// The user's definition of "still working", in milliseconds — the largest
    /// gap between two consecutive transcript events that still counts as
    /// continuous work.
    ///
    /// Every consumer of "how long did this actually run" shares this one
    /// value — the scanner, the insight processors and the journey builder — so
    /// no two pages can disagree about what active time means.
    pub idle_gap_ms: i64,
}

/// Ten minutes: long enough to keep reading a long reply or manually testing a
/// change inside a sitting, short enough to exclude everything a person would
/// not call working time. `config.DefaultIdleGapThresholdMinutes`.
pub const DEFAULT_IDLE_GAP_MS: i64 = 10 * 60 * 1000;

impl DataSettings {
    /// `config.IsIndexedClaudeDir`: whether a cached row's config dir is still
    /// one Agento reports on.
    ///
    /// An **empty** dir is always indexed. Migration 27 defaults the column to
    /// `''` rather than to the default dir, because a home directory is not a
    /// SQL constant — so every reader gives empty the meaning "the default
    /// dir", and treating it as un-indexed would hide the whole pre-migration
    /// corpus.
    pub fn is_indexed_config_dir(&self, dir: &str) -> bool {
        let dir = normalize(dir);
        if dir.is_empty() {
            return true;
        }
        self.indexed_config_dirs.contains(&dir)
    }
}

/// The persisted settings row, exactly as `storage.SQLiteSettingsStore.Load`
/// returns it — before `SettingsManager` fills defaults or the environment
/// overrides anything. Field order is `config.UserSettings`'s declaration
/// order, which is what decides the key order on the wire.
///
/// The two string-list columns are `Option` rather than `Vec` because Go's
/// `decodeStringList` returns a **nil** slice for a blank or unparseable column
/// and a non-nil empty one for a stored `[]` — and a nil slice marshals as
/// `null` while an empty one marshals as `[]`. The distinction is stored, so it
/// travels.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UserSettings {
    pub default_working_dir: String,
    pub default_model: String,
    pub onboarding_complete: bool,
    pub appearance_dark_mode: bool,
    pub appearance_font_size: i64,
    pub appearance_font_family: String,
    pub notification_settings: String,
    pub event_bus_worker_pool_size: i64,
    pub public_url: String,
    pub hidden_projects: Option<Vec<String>>,
    pub idle_gap_threshold_minutes: i64,
    pub claude_config_dir: String,
    pub claude_config_dirs: Option<Vec<String>>,
}

/// Read the settings row as stored. A missing row, a read error, or malformed
/// JSON all degrade to the zero value rather than failing — exactly as Go's
/// store returns zero-value settings for `sql.ErrNoRows` and its snapshot
/// starts at its defaults before `ApplyDataSettings` runs.
pub fn load_stored(conn: &Connection) -> UserSettings {
    let row: Option<UserSettings> = conn
        .query_row(
            "SELECT COALESCE(default_working_dir, ''),
                    COALESCE(default_model, ''),
                    COALESCE(onboarding_complete, 0),
                    COALESCE(appearance_dark_mode, 0),
                    COALESCE(appearance_font_size, 0),
                    COALESCE(appearance_font_family, ''),
                    COALESCE(notification_settings, ''),
                    COALESCE(event_bus_worker_pool_size, 0),
                    COALESCE(public_url, ''),
                    COALESCE(hidden_projects, ''),
                    COALESCE(idle_gap_threshold_minutes, 0),
                    COALESCE(claude_config_dir, ''),
                    COALESCE(claude_config_dirs, '')
             FROM user_settings WHERE id = 1",
            [],
            |r| {
                let onboarding: i64 = r.get(2)?;
                let dark_mode: i64 = r.get(3)?;
                let hidden_raw: String = r.get(9)?;
                let extra_raw: String = r.get(12)?;
                Ok(UserSettings {
                    default_working_dir: r.get(0)?,
                    default_model: r.get(1)?,
                    onboarding_complete: onboarding != 0,
                    appearance_dark_mode: dark_mode != 0,
                    appearance_font_size: r.get(4)?,
                    appearance_font_family: r.get(5)?,
                    notification_settings: r.get(6)?,
                    event_bus_worker_pool_size: r.get(7)?,
                    public_url: r.get(8)?,
                    hidden_projects: decode_string_list(&hidden_raw),
                    idle_gap_threshold_minutes: r.get(10)?,
                    claude_config_dir: r.get(11)?,
                    claude_config_dirs: decode_string_list(&extra_raw),
                })
            },
        )
        .optional()
        .unwrap_or_else(|e| {
            log::warn!("native settings: reading user_settings failed: {e}");
            None
        });

    row.unwrap_or_default()
}

/// Narrow the stored row to the preferences a session read depends on.
pub fn load(conn: &Connection) -> DataSettings {
    from_stored(&load_stored(conn))
}

/// The [`DataSettings`] view of a stored row.
fn from_stored(stored: &UserSettings) -> DataSettings {
    DataSettings {
        hidden_projects: stored.hidden_projects.clone().unwrap_or_default(),
        indexed_config_dirs: claude_config_dirs(
            &stored.claude_config_dir,
            stored.claude_config_dirs.as_deref().unwrap_or_default(),
        ),
        // Zero is "unset", not "no idle time at all": the column defaults to 0
        // and the setting is bounded to 1–240 minutes when the user does set it.
        idle_gap_ms: if stored.idle_gap_threshold_minutes > 0 {
            stored.idle_gap_threshold_minutes * 60 * 1000
        } else {
            DEFAULT_IDLE_GAP_MS
        },
    }
}

/// Decode a JSON string array column the way `storage.decodeStringList` does:
/// blank or unparseable is **nil** (`null` on the wire), a stored `[]` is an
/// empty slice (`[]` on the wire).
fn decode_string_list(raw: &str) -> Option<Vec<String>> {
    if raw.trim().is_empty() {
        return None;
    }
    match serde_json::from_str::<Vec<String>>(raw) {
        Ok(values) => Some(values),
        Err(e) => {
            log::warn!("native settings: malformed string array {raw:?}: {e}");
            None
        }
    }
}

/// Every config dir Agento indexes, mirroring `config.ClaudeConfigDirs`:
/// the default dir, the dir a run targets, then the user's extras, deduped.
///
/// **Reading is a set, running is a choice.** This is the union the sessions
/// list is scoped to — analytics is retrospective, and a machine with two
/// accounts wants both corpora in every total.
fn claude_config_dirs(run_override: &str, extra: &[String]) -> Vec<String> {
    let mut dirs = Vec::with_capacity(extra.len() + 2);
    dirs.push(default_claude_config_dir());
    dirs.push(run_config_dir(run_override));
    dirs.extend(extra.iter().cloned());

    let mut out: Vec<String> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let normalized = match absolute_dir(&normalize(&dir)) {
            Some(d) => d,
            None => continue,
        };
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

/// `~/.claude`, or `/root/.claude` when there is no home — the fallback Go's
/// `DefaultClaudeConfigDir` uses.
fn default_claude_config_dir() -> String {
    paths::home()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".claude")
        .to_string_lossy()
        .into_owned()
}

/// The single dir a run targets: `CLAUDE_CONFIG_DIR` first, then the stored
/// global setting, then the default. The environment comes first because it is
/// what the surrounding environment has already chosen for every subprocess.
fn run_config_dir(stored: &str) -> String {
    if let Ok(env) = std::env::var("CLAUDE_CONFIG_DIR") {
        if let Some(dir) = absolute_dir(&normalize(&env)) {
            return dir;
        }
    }
    if let Some(dir) = absolute_dir(&normalize(stored)) {
        return dir;
    }
    default_claude_config_dir()
}

/// Expand a leading `~` and clean the path, as `NormalizeClaudeConfigDir` does.
/// Blank stays blank so "not set" is distinguishable from a real value.
///
/// This is not tidiness: the dirs are deduplicated by string comparison and
/// recorded on cached rows, so `~/.claude`, `$HOME/.claude` and `$HOME/.claude/`
/// must collapse to one value or the same corpus is attributed to two dirs.
fn normalize(p: &str) -> String {
    let p = p.trim();
    if p.is_empty() {
        return String::new();
    }
    let expanded = if p == "~" {
        paths::home().map(|h| h.to_string_lossy().into_owned())
    } else if let Some(rest) = p.strip_prefix("~/") {
        paths::home().map(|h| h.join(rest).to_string_lossy().into_owned())
    } else {
        None
    };
    clean(&expanded.unwrap_or_else(|| p.to_string()))
}

/// `filepath.Clean` for the subset of paths a config dir can be: collapse
/// repeated separators, resolve `.` and `..`, and drop a trailing separator.
fn clean(p: &str) -> String {
    if p.is_empty() {
        return String::new();
    }
    let rooted = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if let Some(last) = parts.last() {
                    if *last != ".." {
                        parts.pop();
                        continue;
                    }
                }
                if !rooted {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// Absolute paths only. A relative config dir means two different things at
/// once — Agento resolves it against the server's working directory, Claude
/// Code against the subprocess's — so Go drops it and so does this.
fn absolute_dir(p: &str) -> Option<String> {
    if p.is_empty() || !Path::new(p).is_absolute() {
        return None;
    }
    Some(p.to_string())
}

// ─── GET /api/settings ────────────────────────────────────────────────────────

/// What `GET /api/settings` answers with. Mirrors `api.settingsResponse`.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsResponse {
    pub settings: UserSettings,
    /// Field name → the environment variable pinning it. Never nil on the Go
    /// side (`make`d unconditionally), so an unlocked install ships `{}` rather
    /// than `null` — and a `BTreeMap` because Go marshals map keys sorted.
    pub locked: BTreeMap<String, String>,
    pub model_from_env: bool,
}

/// `config.defaultModel`.
const DEFAULT_MODEL: &str = "sonnet";

/// Resolve the row into the answer `SettingsManager` gives, in its order:
/// load the store, fill the two defaults, then apply the environment.
///
/// The manager does this once during startup wiring and holds it in memory; a
/// ported read has no startup to hook into, so it redoes the resolution per
/// request. That is not just equivalent, it is slightly fresher — a value saved
/// through the Go server is visible to the very next native read.
pub fn resolve(stored: UserSettings) -> SettingsResponse {
    let mut settings = stored;
    let locked = locked_fields();

    // `SettingsManager.load` records whether the model was explicitly stored
    // *before* filling the default, and `applyEnvOverrides` reads that flag.
    let model_in_file = !settings.default_model.is_empty();
    if settings.default_working_dir.is_empty() {
        settings.default_working_dir = default_working_dir();
    }
    if settings.default_model.is_empty() {
        settings.default_model = DEFAULT_MODEL.to_string();
    }

    // `applyEnvOverrides`. AGENTO_DEFAULT_MODEL locks the field;
    // ANTHROPIC_DEFAULT_SONNET_MODEL is a soft default that only applies when
    // the user has not chosen one, and neither locks nor is reported in
    // `locked` — but both make the displayed model environment-derived.
    let mut model_from_env = false;
    if let Some(model) = env_value("AGENTO_DEFAULT_MODEL") {
        settings.default_model = model;
        model_from_env = true;
    } else if let Some(sonnet) = env_value("ANTHROPIC_DEFAULT_SONNET_MODEL") {
        if !model_in_file {
            settings.default_model = sonnet;
            model_from_env = true;
        }
    }
    if let Some(dir) = env_value("AGENTO_WORKING_DIR") {
        settings.default_working_dir = dir;
    }
    if let Some(url) = env_value("AGENTO_PUBLIC_URL") {
        settings.public_url = url;
    }
    if let Some(dir) = claude_config_dir_from_env() {
        settings.claude_config_dir = dir;
    }

    SettingsResponse {
        settings,
        locked,
        model_from_env,
    }
}

/// `SettingsManager.detectLockedFields`.
///
/// Go gates the first three on the matching `AppConfig` field also being
/// non-empty, but each of those fields is populated from exactly this variable
/// and nothing else, so the environment check alone is the same condition.
/// `default_model` is the one to watch: its `AppConfig` field falls back to
/// `ANTHROPIC_DEFAULT_SONNET_MODEL` and then to `"sonnet"`, so it is *always*
/// non-empty — which is why the lock still turns only on `AGENTO_DEFAULT_MODEL`.
fn locked_fields() -> BTreeMap<String, String> {
    let mut locked = BTreeMap::new();
    for (field, var) in [
        ("default_model", "AGENTO_DEFAULT_MODEL"),
        ("default_working_dir", "AGENTO_WORKING_DIR"),
        ("public_url", "AGENTO_PUBLIC_URL"),
    ] {
        if env_value(var).is_some() {
            locked.insert(field.to_string(), var.to_string());
        }
    }
    // CLAUDE_CONFIG_DIR is Claude Code's own variable rather than one of ours,
    // so its presence in the environment is the whole condition — and it is
    // compared *normalized*, since a value of "   " is not a choice.
    if claude_config_dir_from_env().is_some() {
        locked.insert(
            "claude_config_dir".to_string(),
            "CLAUDE_CONFIG_DIR".to_string(),
        );
    }
    locked
}

/// An environment variable, or `None` when unset **or empty** — Go's checks are
/// all `os.Getenv(x) != ""`, so an exported-but-blank variable locks nothing.
fn env_value(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// `config.ClaudeConfigDirFromEnv`: the normalized value, or `None` when blank.
fn claude_config_dir_from_env() -> Option<String> {
    let normalized = normalize(&std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_default());
    (!normalized.is_empty()).then_some(normalized)
}

/// `config.DefaultWorkingDir`: `<temp>/agento/work`. The temp dir is resolved
/// rather than hardcoded because Go's `os.TempDir` honours `TMPDIR`, and the
/// value is shown to the user in the settings form.
fn default_working_dir() -> String {
    std::env::temp_dir()
        .join("agento")
        .join("work")
        .to_string_lossy()
        .into_owned()
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "settings",
    claims,
    serve,
};

/// The read only. `PUT /api/settings` writes the row and then re-applies the
/// process-wide snapshots and kicks off a rescan, none of which Rust can do
/// while the Go server owns the database — so it stays with Go, and so does
/// `/api/settings/claude-config-dirs`, which is a filesystem probe rather than
/// a read of this row.
fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && path == "/api/settings"
}

fn serve(ctx: &super::Ctx, _req: &super::Request) -> Result<super::Answer, String> {
    let conn = super::db::open_read_only(&ctx.db_path)?;
    let body = super::gojson::to_vec(&resolve(load_stored(&conn)))
        .map_err(|e| format!("encoding settings: {e}"))?;
    Ok(super::Answer { body, probe: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_matches_filepath_clean() {
        assert_eq!(clean("/home/u/.claude/"), "/home/u/.claude");
        assert_eq!(clean("/home//u/./.claude"), "/home/u/.claude");
        assert_eq!(clean("/home/u/x/../.claude"), "/home/u/.claude");
        assert_eq!(clean("/"), "/");
    }

    #[test]
    fn relative_dirs_are_dropped_not_resolved() {
        assert_eq!(absolute_dir("relative/dir"), None);
        assert_eq!(absolute_dir(""), None);
        assert_eq!(
            absolute_dir("/var/lib/claude"),
            Some("/var/lib/claude".to_string())
        );
    }

    #[test]
    fn the_default_dir_leads_and_duplicates_collapse() {
        let home = paths::home().expect("a home directory");
        let default = home.join(".claude").to_string_lossy().into_owned();

        // The same dir spelled three ways must appear once.
        let dirs = claude_config_dirs("~/.claude", &[format!("{default}/"), default.clone()]);
        assert_eq!(dirs, vec![default]);
    }

    #[test]
    fn extra_dirs_follow_the_default() {
        let home = paths::home().expect("a home directory");
        let default = home.join(".claude").to_string_lossy().into_owned();

        let dirs = claude_config_dirs("", &["/var/lib/claude-work".to_string()]);
        assert_eq!(dirs, vec![default, "/var/lib/claude-work".to_string()]);
    }

    /// `internal/storage/sqlite.go`'s `user_settings`, with the columns the
    /// later migrations appended. Only the ones the read touches.
    const SCHEMA: &str = "
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

    fn fixture(row: Option<&str>) -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory database");
        conn.execute_batch(SCHEMA).expect("schema");
        if let Some(insert) = row {
            conn.execute_batch(insert).expect("row");
        }
        conn
    }

    /// The read is positional, and the struct is not filled in the `SELECT`'s
    /// order — the two bools and the two JSON columns are pulled out first. So
    /// every value gets a **distinct** marker here: two adjacent `TEXT` columns
    /// swapped would compile, pass every other test in this file, and only
    /// surface in the live parity run, which CI does not run.
    #[test]
    fn every_column_lands_on_its_own_field() {
        let conn = fixture(Some(
            r#"INSERT INTO user_settings
                 (id, default_working_dir, default_model, onboarding_complete,
                  appearance_dark_mode, appearance_font_size, appearance_font_family,
                  notification_settings, event_bus_worker_pool_size, public_url,
                  hidden_projects, idle_gap_threshold_minutes,
                  claude_config_dir, claude_config_dirs)
               VALUES (1, 'working-dir', 'the-model', 1,
                       1, 13, 'the-font',
                       'the-notifications', 7, 'the-url',
                       '["/hidden/one"]', 25,
                       '/run/dir', '["/extra/dir"]')"#,
        ));

        let stored = load_stored(&conn);
        assert_eq!(stored.default_working_dir, "working-dir");
        assert_eq!(stored.default_model, "the-model");
        assert!(stored.onboarding_complete);
        assert!(stored.appearance_dark_mode);
        assert_eq!(stored.appearance_font_size, 13);
        assert_eq!(stored.appearance_font_family, "the-font");
        assert_eq!(stored.notification_settings, "the-notifications");
        assert_eq!(stored.event_bus_worker_pool_size, 7);
        assert_eq!(stored.public_url, "the-url");
        assert_eq!(
            stored.hidden_projects,
            Some(vec!["/hidden/one".to_string()])
        );
        assert_eq!(stored.idle_gap_threshold_minutes, 25);
        assert_eq!(stored.claude_config_dir, "/run/dir");
        assert_eq!(
            stored.claude_config_dirs,
            Some(vec!["/extra/dir".to_string()])
        );

        // The narrowed view must agree with the row it was derived from — one
        // reader is the point.
        let data = from_stored(&stored);
        assert_eq!(data.hidden_projects, vec!["/hidden/one".to_string()]);
        assert_eq!(data.idle_gap_ms, 25 * 60 * 1000);
        assert!(data.indexed_config_dirs.contains(&"/extra/dir".to_string()));
    }

    /// No row at all is Go's `sql.ErrNoRows`, which its store answers with
    /// zero-value settings rather than an error — and the zero value is where
    /// the two lists are nil, so they ship as `null`.
    #[test]
    fn an_install_with_no_row_reads_as_the_zero_value() {
        let stored = load_stored(&fixture(None));
        assert_eq!(stored.default_model, "");
        assert_eq!(stored.hidden_projects, None);
        assert_eq!(stored.claude_config_dirs, None);
        // And the derived view still resolves the threshold to the default.
        assert_eq!(from_stored(&stored).idle_gap_ms, DEFAULT_IDLE_GAP_MS);
    }

    /// A read failure is not a 500: the table can be missing entirely on a
    /// database the Go server has not migrated yet, and the endpoint answers
    /// with defaults exactly as the snapshot does before `ApplyDataSettings`.
    #[test]
    fn a_missing_table_degrades_rather_than_failing() {
        let conn = Connection::open_in_memory().expect("in-memory database");
        let stored = load_stored(&conn);
        assert_eq!(stored.hidden_projects, None);
        assert_eq!(from_stored(&stored).idle_gap_ms, DEFAULT_IDLE_GAP_MS);
    }

    /// A nil list is `null` and an empty one is `[]`, and only the stored `[]`
    /// produces the second. Collapsing the two would change the wire for every
    /// install that has never hidden a project.
    #[test]
    fn a_malformed_settings_column_degrades_to_nil_not_empty() {
        assert_eq!(decode_string_list("not json"), None);
        assert_eq!(decode_string_list(""), None);
        assert_eq!(decode_string_list("  "), None);
        assert_eq!(decode_string_list("[]"), Some(Vec::new()));
        assert_eq!(
            decode_string_list(r#"["/a","/b"]"#),
            Some(vec!["/a".to_string(), "/b".to_string()])
        );
    }

    /// The whole envelope, encoded the way the handler encodes it. Key order is
    /// the Go struct's declaration order; `locked` is `{}` rather than `null`.
    #[test]
    fn the_response_shape_is_the_go_envelope() {
        let stored = UserSettings {
            default_working_dir: "/tmp/agento/work".into(),
            default_model: "opus".into(),
            onboarding_complete: true,
            appearance_font_size: 13,
            appearance_font_family: "Inter".into(),
            notification_settings: "{}".into(),
            event_bus_worker_pool_size: 3,
            hidden_projects: Some(vec!["/home/u/secret".into()]),
            claude_config_dirs: Some(Vec::new()),
            ..Default::default()
        };
        let body = super::super::gojson::to_vec(&SettingsResponse {
            settings: stored,
            locked: BTreeMap::new(),
            model_from_env: false,
        })
        .expect("encode");

        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            concat!(
                r#"{"settings":{"default_working_dir":"/tmp/agento/work","default_model":"opus","#,
                r#""onboarding_complete":true,"appearance_dark_mode":false,"#,
                r#""appearance_font_size":13,"appearance_font_family":"Inter","#,
                r#""notification_settings":"{}","event_bus_worker_pool_size":3,"#,
                r#""public_url":"","hidden_projects":["/home/u/secret"],"#,
                r#""idle_gap_threshold_minutes":0,"claude_config_dir":"","#,
                r#""claude_config_dirs":[]},"locked":{},"model_from_env":false}"#,
                "\n"
            )
        );
    }

    /// A nil list must reach the wire as `null`, which is what an install with
    /// no settings row at all produces.
    #[test]
    fn absent_lists_are_null_not_empty_arrays() {
        let body = super::super::gojson::to_vec(&UserSettings::default()).expect("encode");
        let json = String::from_utf8(body).expect("utf8");
        assert!(json.contains(r#""hidden_projects":null"#), "{json}");
        assert!(json.contains(r#""claude_config_dirs":null"#), "{json}");
    }

    /// The two defaults `SettingsManager.load` fills, and the flag that is set
    /// only when the *environment* chose the model.
    /// The two defaults `SettingsManager.load` fills, and the flag that is set
    /// only when the *environment* chose the model.
    ///
    /// Guarded on the variables rather than asserted flat: these run on a
    /// developer's machine, and one that exports `AGENTO_DEFAULT_MODEL` would
    /// otherwise fail a test about the built-in default.
    #[test]
    fn an_empty_row_resolves_to_the_built_in_defaults() {
        let resolved = resolve(UserSettings::default());

        match env_value("AGENTO_DEFAULT_MODEL")
            .or_else(|| env_value("ANTHROPIC_DEFAULT_SONNET_MODEL"))
        {
            Some(from_env) => {
                assert_eq!(resolved.settings.default_model, from_env);
                assert!(resolved.model_from_env);
            }
            None => {
                assert_eq!(resolved.settings.default_model, DEFAULT_MODEL);
                assert!(!resolved.model_from_env);
            }
        }

        if env_value("AGENTO_WORKING_DIR").is_none() {
            assert_eq!(resolved.settings.default_working_dir, default_working_dir());
        }
    }

    /// A stored model is not overwritten by the soft default, and that is
    /// exactly the case `modelInFile` exists to protect.
    #[test]
    fn a_stored_model_survives_and_is_not_environment_derived() {
        let stored = UserSettings {
            default_model: "haiku".into(),
            ..Default::default()
        };
        let resolved = resolve(stored);
        if env_value("AGENTO_DEFAULT_MODEL").is_none() {
            assert_eq!(resolved.settings.default_model, "haiku");
            assert!(!resolved.model_from_env);
        }
    }

    #[test]
    fn only_the_settings_read_is_claimed() {
        assert!(claims(&Method::GET, "/api/settings"));
        assert!(!claims(&Method::PUT, "/api/settings"));
        // A filesystem probe, not a read of this row — it stays with Go.
        assert!(!claims(&Method::GET, "/api/settings/claude-config-dirs"));
        assert!(!claims(&Method::GET, "/api/settings/"));
    }
}

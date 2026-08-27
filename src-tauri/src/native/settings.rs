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
use serde::{Deserialize, Serialize};

use super::gojson::null_is_zero_value;
use super::writes::WriteError;
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
///
/// It doubles as the `PUT` request body, because Go decodes the request into
/// the very same `config.UserSettings` it answers with. The scalars go through
/// [`null_is_zero_value`] for the reason every decoded Go struct here does — a
/// JSON `null` is a no-op to `encoding/json`, so `{"public_url":null}` is a
/// successful decode of the zero value and must not 400.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    #[serde(deserialize_with = "null_is_zero_value")]
    pub default_working_dir: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub default_model: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub onboarding_complete: bool,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub appearance_dark_mode: bool,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub appearance_font_size: i64,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub appearance_font_family: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub notification_settings: String,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub event_bus_worker_pool_size: i64,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub public_url: String,
    pub hidden_projects: Option<Vec<String>>,
    #[serde(deserialize_with = "null_is_zero_value")]
    pub idle_gap_threshold_minutes: i64,
    #[serde(deserialize_with = "null_is_zero_value")]
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

/// `storage.decodeStringList`, shared with every other module that meets a
/// stored string-array column — see [`super::gojson::decode_string_list`].
fn decode_string_list(raw: &str) -> Option<Vec<String>> {
    super::gojson::decode_string_list(raw)
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
///
/// Joined through [`super::gopath::join`] rather than `PathBuf::join` because
/// Go's is `filepath.Join`, which **cleans**: with `HOME=/home//u` Go answers
/// `/home/u/.claude` and a `PathBuf::join` would answer `/home//u/.claude`.
/// Every other caller runs the result through [`normalize`] and so never saw
/// the difference; `GET /api/settings/claude-config-dirs` puts it on the wire
/// raw as `default`, where it would disagree with its own `indexed[0]`.
pub fn default_claude_config_dir() -> String {
    let home = paths::home()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .to_string_lossy()
        .into_owned();
    super::gopath::join(&[&home, ".claude"])
}

/// The single dir a run targets: `CLAUDE_CONFIG_DIR` first, then the stored
/// global setting, then the default. The environment comes first because it is
/// what the surrounding environment has already chosen for every subprocess.
///
/// `pub(crate)` since #276: the chat runner needs the same answer, because an
/// agent's per-run override resolves *against* this and a second copy of the
/// precedence would be a second thing to keep in step.
pub(crate) fn run_config_dir(stored: &str) -> String {
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
pub(crate) fn normalize(p: &str) -> String {
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

/// `filepath.Clean`, delegated to [`super::gopath::clean`].
///
/// This used to be a split-and-rejoin of its own, adequate for the subset of
/// paths a config dir can be. #268 needed the real thing for `GET /api/fs` and
/// pinned it to vectors generated from Go, which promptly found a case the
/// local version also got wrong (`/a/b/../c/`). One implementation, checked
/// against Go, rather than two that agree by inspection.
///
/// The empty guard stays here: `NormalizeClaudeConfigDir` answers `""` for a
/// blank input so callers can tell "not set" from a real value, while
/// `filepath.Clean("")` is `"."`. The caller above never reaches this with an
/// empty string, but the guard is what makes that safe to stop checking.
fn clean(p: &str) -> String {
    if p.is_empty() {
        return String::new();
    }
    super::gopath::clean(p)
}

/// Absolute paths only. A relative config dir means two different things at
/// once — Agento resolves it against the server's working directory, Claude
/// Code against the subprocess's — so Go drops it and so does this.
pub(crate) fn absolute_dir(p: &str) -> Option<String> {
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
/// Go's `defaultModel` (`internal/config/settings.go`), which
/// `SettingsManager.load` fills in when nothing is stored — before
/// `applyEnvOverrides` runs. Public so a caller that resolves the model through
/// [`resolve`] can assert against the same constant rather than a literal.
pub(crate) const DEFAULT_MODEL: &str = "sonnet";

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
pub(crate) fn env_value(name: &str) -> Option<String> {
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

// ─── GET /api/settings/claude-config-dirs ─────────────────────────────────────

/// `api.claudeConfigDirsResponse`: what the config-dir editor is drawn from.
///
/// Not a read of the settings row — or not only one. `indexed` is the resolved
/// union the scanner walks, but `candidates` is a **filesystem probe**, which is
/// why #266 left this route with Go while taking `GET /api/settings`.
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeConfigDirsResponse {
    /// The resolved set, default first. Never empty: the default dir is always
    /// in it, so this ships an array rather than `null`.
    pub indexed: Vec<String>,
    /// Dirs that look like config dirs but are not configured yet.
    ///
    /// `Option` because Go distinguishes the two empties here and the
    /// distinction reaches the wire: a home directory that cannot be listed
    /// returns a **nil** slice (`null`), while one with nothing to suggest
    /// returns `make([]string, 0, …)` (`[]`).
    pub candidates: Option<Vec<String>>,
    /// `default` is a Rust keyword, so the field is renamed rather than raw —
    /// the wire name is Go's.
    #[serde(rename = "default")]
    pub default_dir: String,
}

/// `handleClaudeConfigDirs`.
///
/// The row is read **through [`resolve`]**, not raw. Go answers `indexed` from
/// the `config.claudeDirs` snapshot, which `ApplyClaudeDirs` installs from the
/// *env-resolved* settings — and `applyEnvOverrides` overwrites
/// `ClaudeConfigDir` with `ClaudeConfigDirFromEnv()` whenever that is non-blank,
/// **including when the env value is relative**. So with a relative
/// `CLAUDE_CONFIG_DIR` exported and an absolute value in the column, Go drops
/// both (the env wins, then `absoluteDir` discards it) and falls back to the
/// default dir; reading the raw row would drop only the env value and add a dir
/// Go does not index.
pub fn claude_config_dirs_response(conn: &Connection) -> ClaudeConfigDirsResponse {
    let stored = resolve(load_stored(conn)).settings;
    let indexed = claude_config_dirs(
        &stored.claude_config_dir,
        stored.claude_config_dirs.as_deref().unwrap_or_default(),
    );
    let candidates = discover_candidate_claude_dirs(&indexed);
    ClaudeConfigDirsResponse {
        indexed,
        candidates,
        default_dir: default_claude_config_dir(),
    }
}

/// `config.DiscoverCandidateClaudeDirs`: config dirs sitting beside the default
/// one that are not configured yet, so Settings can offer them instead of
/// asking for a typed absolute path.
///
/// The rule is deliberately narrow, and each clause earns its place:
///
/// - a **sibling of the default dir** — anywhere else is added by hand, because
///   suggesting is not discovering;
/// - whose name starts with `.claude`;
/// - which contains a `projects` **directory** — the only filter beyond the
///   prefix, and so the only thing that keeps a directory which merely *looks*
///   like a config dir (the `.claude-backup` shape) out.
///
/// Go's own comment says the `projects` check "is what keeps `.claude-backup`
/// and `.claude.bak` out". That is wrong about the second: the prefix match is
/// literal, so `.claude.bak` **is** suggested whenever it has a `projects`
/// directory — as the vectors below pin. Nothing distinguishes a name; only the
/// `projects` directory does.
///
/// Two Go details that a natural Rust rewrite gets wrong: `os.ReadDir`'s
/// `DirEntry.IsDir` does **not** follow symlinks (so a symlink to a directory is
/// not a candidate) while the `projects` check is an `os.Stat`, which does; and
/// a failed listing is a nil slice rather than an empty one.
fn discover_candidate_claude_dirs(configured: &[String]) -> Option<Vec<String>> {
    let parent = super::gopath::dir(&default_claude_config_dir());
    let entries = std::fs::read_dir(&parent).ok()?;

    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        // Go's `os.ReadDir` returns whatever it read *plus* the error, and the
        // handler discards both. A per-entry failure here is the same class of
        // "we could not look", so it answers the same way rather than quietly
        // suggesting a shorter list.
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(".claude") {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => {}
            _ => continue,
        }
        let candidate = super::gopath::join(&[&parent, &name]);
        if configured.contains(&candidate) {
            continue;
        }
        let projects = super::gopath::join(&[&candidate, "projects"]);
        match std::fs::metadata(&projects) {
            Ok(meta) if meta.is_dir() => {}
            _ => continue,
        }
        out.push(candidate);
    }
    // `sort.Strings` is a byte-order sort, and so is `Vec<String>::sort`.
    out.sort();
    Some(out)
}

// ─── PUT /api/settings ────────────────────────────────────────────────────────
//
// Written, tested, and deliberately **not claimed** — see `claims` below for
// why, and `desktop/CLAUDE.md`. What follows is `handleUpdateSettings` plus
// `SettingsManager.Update`, `SQLiteSettingsStore.Save` and
// `Server.applyDataSettings`, in Go's order.

/// `config.{Min,Max}IdleGapThresholdMinutes`.
const MIN_IDLE_GAP_MINUTES: i64 = 1;
const MAX_IDLE_GAP_MINUTES: i64 = 240;

/// `handleUpdateSettings`.
///
/// Every failure is a **400** carrying the error's own text: the handler writes
/// `s.writeError(w, http.StatusBadRequest, err.Error())` for anything `Update`
/// returns. That is not the 409 the monitoring path answers for an env-locked
/// write, and it is not the 422 the service layer's `ValidationError` produces —
/// `SettingsManager` returns plain `fmt.Errorf` values and the handler flattens
/// them all to 400.
///
/// One exception, and it is not a status: a failure of the machinery rather
/// than of the request — opening the database, verifying the migrations, the
/// `INSERT`, or encoding the answer — is a [`WriteError::Fallback`], answered
/// as the default 500 since #278 (those wrap driver and `os` errors whose Go
/// text is not reproducible; the reason goes to the log). The first three fail
/// with nothing written; the encode happens after the row is saved, which is
/// harmless — a `PUT` replaces the whole row, so a retry is idempotent.
pub fn update(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    update_with(db_path, body, super::scan::force_scan)
}

/// The handler, with the rescan as a parameter.
///
/// The seam exists for the tests: a scan walks the developer's real `~/.claude`,
/// so a unit test of the *save* would otherwise spend minutes reading a corpus
/// it has nothing to say about — and the trigger rules are worth asserting
/// directly rather than inferring from a side effect.
fn update_with(
    db_path: &Path,
    body: &[u8],
    rescan: impl FnOnce(PathBuf),
) -> Result<super::Answer, WriteError> {
    // Decoded first, exactly as Go does: a malformed body is a 400 before the
    // database is opened, let alone written.
    let incoming: UserSettings = super::writes::decode_body(body)?;

    let conn = super::db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;

    // `m.settings`, the manager's in-memory current value. Reconstructed rather
    // than remembered: `resolve` is `load()` + `applyEnvOverrides`, which is
    // precisely how the manager arrived at it during startup wiring.
    let current = resolve(load_stored(&conn)).settings;
    let previous_idle_gap = current.idle_gap_threshold_minutes;
    let previous_dirs = claude_config_dirs(
        &current.claude_config_dir,
        current.claude_config_dirs.as_deref().unwrap_or_default(),
    );

    let saved = apply_update(incoming, &current)?;
    save(&conn, &saved).map_err(WriteError::Fallback)?;
    drop(conn);

    apply_data_settings(db_path, &saved, previous_idle_gap, &previous_dirs, rescan);

    // **The stored row, not a resolution of it.** `Update` assigns `incoming`
    // wholesale to `m.settings` and the handler answers `Get()`, so no default
    // is refilled: a `PUT` sending `"default_model":""` is answered with `""`,
    // where the very next `GET` answers `"sonnet"`. Likewise a normalized-away
    // dir list is `null` here and `[]` on the next read, because `Save` writes
    // `[]` for a nil slice and `decodeStringList` reads that back as non-nil.
    let model_from_env = model_from_env(&saved);
    let body = super::gojson::to_vec(&SettingsResponse {
        settings: saved,
        locked: locked_fields(),
        model_from_env,
    })
    .map_err(|e| WriteError::Fallback(format!("encoding settings: {e}")))?;
    Ok(super::Answer::json(body))
}

/// `SettingsManager.Update` up to the point it persists: lock, validate,
/// normalize. The order is Go's, and it is observable — a payload that both
/// changes a locked field and carries an out-of-range threshold is answered
/// with the lock message.
fn apply_update(
    mut incoming: UserSettings,
    current: &UserSettings,
) -> Result<UserSettings, WriteError> {
    apply_locked_fields(&mut incoming, current)?;
    validate_idle_gap_threshold(incoming.idle_gap_threshold_minutes)?;
    validate_claude_config_dirs(&incoming, current)?;

    incoming.claude_config_dir = normalize(&incoming.claude_config_dir);
    incoming.claude_config_dirs = normalize_claude_config_dirs(&incoming.claude_config_dirs);
    Ok(incoming)
}

/// `SettingsManager.applyLockedFields`: refuse a change to an env-locked field,
/// and pin every locked field to what the environment chose.
///
/// **A blank incoming value is never a conflict.** The settings form posts the
/// whole object back from every tab, so a client that does not know about a
/// field must not be read as asking to clear it — it is pinned instead.
///
/// The field order is Go's slice order and is load-bearing: a body conflicting
/// on two locked fields at once reports the first of these, not the first in
/// the JSON.
fn apply_locked_fields(
    incoming: &mut UserSettings,
    current: &UserSettings,
) -> Result<(), WriteError> {
    let locked = locked_fields();
    for field in [
        "default_model",
        "default_working_dir",
        "public_url",
        "claude_config_dir",
    ] {
        let Some(env_var) = locked.get(field) else {
            continue;
        };
        let (incoming_value, current_value) = match field {
            "default_model" => (&mut incoming.default_model, &current.default_model),
            "default_working_dir" => (
                &mut incoming.default_working_dir,
                &current.default_working_dir,
            ),
            "public_url" => (&mut incoming.public_url, &current.public_url),
            _ => (&mut incoming.claude_config_dir, &current.claude_config_dir),
        };
        // `claude_config_dir` compares **normalized**, so `~/.claude` and
        // `$HOME/.claude` are not read as a conflicting change; the other three
        // are plain string equality.
        let same = if field == "claude_config_dir" {
            normalize(incoming_value) == normalize(current_value)
        } else {
            incoming_value == current_value
        };
        if !incoming_value.is_empty() && !same {
            return Err(WriteError::BadRequest(format!(
                "{field} is locked by environment variable {env_var}"
            )));
        }
        incoming_value.clone_from(current_value);
    }
    Ok(())
}

/// `config.validateIdleGapThreshold`.
///
/// Zero is allowed and means "not chosen": the settings form for any other tab
/// posts the whole object back, and a client that does not know about the field
/// must not be read as asking for a zero-length sitting. Every reader resolves
/// zero to the default.
fn validate_idle_gap_threshold(minutes: i64) -> Result<(), WriteError> {
    if minutes == 0 || (MIN_IDLE_GAP_MINUTES..=MAX_IDLE_GAP_MINUTES).contains(&minutes) {
        return Ok(());
    }
    Err(WriteError::BadRequest(format!(
        "idle_gap_threshold_minutes must be between {MIN_IDLE_GAP_MINUTES} and \
         {MAX_IDLE_GAP_MINUTES} minutes, got {minutes}"
    )))
}

/// `config.validateClaudeConfigDirs`.
///
/// **Only values the caller is actually changing are checked.** A directory that
/// existed when it was stored can stop existing — an unmounted volume, or a
/// `CLAUDE_CONFIG_DIR` exported in a shell profile that Claude Code has not
/// created yet — and validating an unchanged value would then reject every save,
/// including saves of unrelated fields, naming a field the user was not touching
/// and (when env-locked) cannot even edit.
fn validate_claude_config_dirs(
    incoming: &UserSettings,
    current: &UserSettings,
) -> Result<(), WriteError> {
    if normalize(&incoming.claude_config_dir) != normalize(&current.claude_config_dir) {
        validate_claude_config_dir(&incoming.claude_config_dir)?;
    }

    let existing: Vec<String> = current
        .claude_config_dirs
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|d| normalize(d))
        .collect();
    for dir in incoming.claude_config_dirs.as_deref().unwrap_or_default() {
        // Blank entries are dropped rather than rejected, so a half-filled row
        // in the UI is not an error the user has to clear before saving
        // anything else.
        if dir.trim().is_empty() {
            continue;
        }
        if existing.contains(&normalize(dir)) {
            continue;
        }
        validate_claude_config_dir(dir)?;
    }
    Ok(())
}

/// `config.ValidateClaudeConfigDir`.
///
/// Go validates the **normalized** path and its messages quote that form, so
/// `~/nope` is reported as the expanded path. A blank value is valid and means
/// "use the default".
fn validate_claude_config_dir(raw: &str) -> Result<(), WriteError> {
    let normalized = normalize(raw);
    if normalized.is_empty() {
        return Ok(());
    }
    if !Path::new(&normalized).is_absolute() {
        return Err(WriteError::BadRequest(format!(
            "claude config dir must be an absolute path, got {normalized:?}"
        )));
    }
    match std::fs::metadata(&normalized) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(WriteError::BadRequest(format!(
            "claude config dir {normalized:?} is not a directory"
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(WriteError::BadRequest(format!(
            "claude config dir {normalized:?} does not exist"
        ))),
        // Go wraps the underlying `os.Stat` error, and the handler flattens it
        // to a 400 carrying that text. The Go runtime's wording is not
        // reproducible, and since #278 there is no sidecar to supply it — so
        // this is the same 400 class with this build's own wording.
        Err(e) => Err(WriteError::BadRequest(format!(
            "claude config dir {normalized:?} is not readable: {e}"
        ))),
    }
}

/// `config.normalizeClaudeConfigDirs`: normalize, drop blanks and duplicates.
///
/// An input that reduces to nothing becomes **nil**, not an empty slice, and the
/// difference is on the wire — `Save` writes `[]` for both, but the `PUT`
/// response is the in-memory value, so it ships `null`.
fn normalize_claude_config_dirs(dirs: &Option<Vec<String>>) -> Option<Vec<String>> {
    let dirs = dirs.as_deref()?;
    if dirs.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let normalized = normalize(dir);
        if normalized.is_empty() || out.contains(&normalized) {
            continue;
        }
        out.push(normalized);
    }
    (!out.is_empty()).then_some(out)
}

/// `storage.SQLiteSettingsStore.Save`, one row, `id = 1`.
///
/// `encodeStringList` writes `[]` for a nil slice, which is why the column can
/// read back non-nil after a `PUT` that sent `null`.
fn save(conn: &Connection, settings: &UserSettings) -> Result<(), String> {
    let notification_settings = if settings.notification_settings.is_empty() {
        "{}"
    } else {
        &settings.notification_settings
    };
    conn.execute(
        "INSERT INTO user_settings
            (id, default_working_dir, default_model, onboarding_complete,
             appearance_dark_mode, appearance_font_size, appearance_font_family,
             notification_settings, event_bus_worker_pool_size, public_url,
             hidden_projects, idle_gap_threshold_minutes,
             claude_config_dir, claude_config_dirs)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET
            default_working_dir = excluded.default_working_dir,
            default_model = excluded.default_model,
            onboarding_complete = excluded.onboarding_complete,
            appearance_dark_mode = excluded.appearance_dark_mode,
            appearance_font_size = excluded.appearance_font_size,
            appearance_font_family = excluded.appearance_font_family,
            notification_settings = excluded.notification_settings,
            event_bus_worker_pool_size = excluded.event_bus_worker_pool_size,
            public_url = excluded.public_url,
            hidden_projects = excluded.hidden_projects,
            idle_gap_threshold_minutes = excluded.idle_gap_threshold_minutes,
            claude_config_dir = excluded.claude_config_dir,
            claude_config_dirs = excluded.claude_config_dirs",
        rusqlite::params![
            settings.default_working_dir,
            settings.default_model,
            i64::from(settings.onboarding_complete),
            i64::from(settings.appearance_dark_mode),
            settings.appearance_font_size,
            settings.appearance_font_family,
            notification_settings,
            settings.event_bus_worker_pool_size,
            settings.public_url,
            encode_string_list(&settings.hidden_projects),
            settings.idle_gap_threshold_minutes,
            settings.claude_config_dir,
            encode_string_list(&settings.claude_config_dirs),
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("persisting settings: saving settings: {e}"))
}

/// `storage.encodeStringList`: a nil or empty list is the two bytes `[]`, so the
/// `NOT NULL` column always holds valid JSON.
fn encode_string_list(values: &Option<Vec<String>>) -> String {
    match values {
        Some(v) if !v.is_empty() => serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string()),
        _ => "[]".to_string(),
    }
}

/// `Server.applyDataSettings`, minus the half Rust does not need.
///
/// Go installs the saved preferences into two process-wide snapshots
/// (`claudesessions.dataSettings`, `config.claudeDirs`) because its readers have
/// no settings dependency. **This port keeps no such snapshot**: every native
/// reader calls [`load`] against the row it is already holding a connection to,
/// so the write *is* the install. That is the whole reason this function is
/// three lines rather than thirty.
///
/// What is left is the scan, and Go's rules for it: hiding a project takes
/// effect on the next read because it is a filter over cached rows, while a new
/// threshold or a newly added config dir is not — the durations are stored per
/// transcript and a dir that was never walked has no rows to filter.
///
/// `force_scan`, not `ensure_scan`: Go calls `Cache.EnsureScan`, which admits a
/// scan outright, where `ensure_scan` is `ensureFresh` and would ask the
/// staleness markers first. The threshold branch would pass that gate anyway,
/// but the config-dir branch would **not** — no marker records which dirs were
/// walked — and a newly added account would then sit unindexed until the TTL.
fn apply_data_settings(
    db_path: &Path,
    saved: &UserSettings,
    previous_idle_gap: i64,
    previous_dirs: &[String],
    rescan: impl FnOnce(PathBuf),
) {
    if saved.idle_gap_threshold_minutes != previous_idle_gap {
        log::info!(
            "claude sessions: idle-gap threshold changed; recomputing durations \
             (from {previous_idle_gap} to {})",
            saved.idle_gap_threshold_minutes
        );
        rescan(db_path.to_path_buf());
        return;
    }
    let dirs = claude_config_dirs(
        &saved.claude_config_dir,
        saved.claude_config_dirs.as_deref().unwrap_or_default(),
    );
    // Order-sensitive on purpose: it decides which dir wins a session present in
    // two of them (`claim_session`), so a reorder is a real change.
    if dirs != previous_dirs {
        log::info!("claude sessions: config dirs changed; indexing {dirs:?}");
        rescan(db_path.to_path_buf());
    }
}

/// `SettingsManager.modelFromEnv`, recomputed from the row.
///
/// Go records this **once, at startup**, from the row as it was then. Nothing in
/// the port observes that moment, so this answers the question a fresh boot on
/// the current row would — the same convention [`resolve`] already uses for
/// `GET /api/settings`, and the same one-case caveat: with only
/// `ANTHROPIC_DEFAULT_SONNET_MODEL` set, a model stored *after* boot makes Go's
/// flag stale-true while this says false.
fn model_from_env(stored: &UserSettings) -> bool {
    env_value("AGENTO_DEFAULT_MODEL").is_some()
        || (env_value("ANTHROPIC_DEFAULT_SONNET_MODEL").is_some()
            && stored.default_model.is_empty())
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "settings",
    claims,
    serve,
};

/// The two reads and, since #278, the write.
///
/// **`PUT /api/settings` was written, unit-tested against Go's literal answers,
/// and deliberately left unclaimed until the cut-over** — the `migrate::apply`
/// precedent from #274. The blocker was the sidecar's own in-memory snapshot:
/// `config.claudeDirs`, `claudesessions.dataSettings` and
/// `SettingsManager.settings` were read by routes Go still served, and worst of
/// all `notificationServiceImpl.UpdateSettings` was a read-modify-write over
/// `settingsMgr.Get()` that persisted the **whole** `user_settings` row — one
/// unrelated `PUT /api/notifications/settings` silently reverted a native
/// settings write, reproduced against a live parity instance. That blocker
/// died with the sidecar, so the route is claimed in the same change that
/// removed it — one commit, one owner, no window in which two processes
/// disagree about the row.
///
/// `/api/settings/claude-config-dirs` is claimed and answered on **every**
/// platform. It answered a 501 on Windows until #374, because the probe is
/// `filepath` arithmetic and `native/gopath.rs` carried only the Unix rules;
/// it now carries both, selected by target and pinned on every host by
/// `parity/gopath_windows_vectors.json`.
fn claims(method: &Method, path: &str) -> bool {
    match path {
        "/api/settings" => method == Method::GET || method == Method::PUT,
        "/api/settings/claude-config-dirs" => method == Method::GET,
        _ => false,
    }
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    // The config-dir probe is `filepath` arithmetic on the real filesystem, and
    // it answered a 501 on Windows until #374: this port carried only the Unix
    // `filepath`, so `gopath::dir` found no `/` in `C:\Users\u\.claude` and
    // answered `"."` — listing the process working directory instead of the
    // home directory — while `gopath::join` built `C:\Users\u/.claude-work`,
    // which no `configured` entry could ever match. Both are now the
    // target's rules (`parity/gopath_windows_vectors.json` pins them), so the
    // probe is answered everywhere and the gate is gone.
    if req.method == Method::PUT {
        return super::writes::finish(update(&ctx.db_path, req.body));
    }
    let conn = super::db::open_read_only(&ctx.db_path)?;
    let body = if req.path == "/api/settings/claude-config-dirs" {
        super::gojson::to_vec(&claude_config_dirs_response(&conn))
            .map_err(|e| format!("encoding claude config dirs: {e}"))?
    } else {
        super::gojson::to_vec(&resolve(load_stored(&conn)))
            .map_err(|e| format!("encoding settings: {e}"))?
    };
    Ok(super::Answer::json(body))
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
        // `run_config_dir` reads `CLAUDE_CONFIG_DIR` and `paths::home` reads
        // `HOME`, both of which the locked-field tests below swap.
        let _env = crate::paths::tests::env_lock();
        let home = paths::home().expect("a home directory");
        let default = home.join(".claude").to_string_lossy().into_owned();

        // The same dir spelled three ways must appear once.
        let dirs = claude_config_dirs("~/.claude", &[format!("{default}/"), default.clone()]);
        assert_eq!(dirs, vec![default]);
    }

    #[test]
    fn extra_dirs_follow_the_default() {
        let _env = crate::paths::tests::env_lock();
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

    // ─── GET /api/settings/claude-config-dirs ─────────────────────────────────

    use crate::paths::tests::EnvVar;

    /// `desktop/parity/claude_dirs_vectors.json`, generated from Go by
    /// `desktop/parity/claude_dirs_parity_test.go`.
    ///
    /// A shared *primitive* rather than a response, so it takes the vector form
    /// `desktop/CLAUDE.md`'s checklist names — the same arrangement
    /// `gopath_vectors.json` uses. It has to be the vector form here: the four
    /// exclusion shapes (a `.claude*` symlink, a `.claude*` dir with no
    /// `projects`, one whose `projects` is a *file*, a plain file with the
    /// prefix) exist in no real `$HOME`, so the live parity diff structurally
    /// cannot re-verify them, and a hand-transcribed literal would pin only what
    /// its author believed Go does. Both languages now assert against what Go
    /// actually answered, and a change to Go's rule fails Go's own suite.
    #[derive(Deserialize)]
    struct DirVectorSymlink {
        link: String,
        target: String,
    }

    #[derive(Deserialize)]
    struct DirVectorLayout {
        dirs: Vec<String>,
        files: Vec<String>,
        symlinks: Vec<DirVectorSymlink>,
    }

    #[derive(Deserialize)]
    struct DirVectorCase {
        name: String,
        claude_config_dir_env: String,
        run_dir: String,
        extra: Vec<String>,
        indexed: Vec<String>,
        candidates: Vec<String>,
    }

    #[derive(Deserialize)]
    struct DirVectors {
        layout: DirVectorLayout,
        cases: Vec<DirVectorCase>,
    }

    /// Baked in rather than read at run time: the file is a build input, and a
    /// missing one should fail the compile rather than one test.
    const DIR_VECTORS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../parity/claude_dirs_vectors.json"
    ));

    /// The `$HOME` token the vectors record, since the home directory is a
    /// fresh temp dir on both sides.
    fn expand(path: &str, home: &str) -> String {
        path.replace("$HOME", home)
    }

    fn expand_all(paths: &[String], home: &str) -> Vec<String> {
        paths.iter().map(|p| expand(p, home)).collect()
    }

    /// Build the vectors' layout — the same tree the Go test built.
    #[cfg(unix)]
    fn build_layout(layout: &DirVectorLayout, root: &Path) {
        for dir in &layout.dirs {
            std::fs::create_dir_all(root.join(dir)).expect("layout dir");
        }
        for file in &layout.files {
            std::fs::write(root.join(file), "").expect("layout file");
        }
        for link in &layout.symlinks {
            std::os::unix::fs::symlink(root.join(&link.target), root.join(&link.link))
                .expect("layout symlink");
        }
    }

    /// Both halves of the endpoint, against Go's own answers over a home
    /// directory built from the vectors.
    ///
    /// `#[cfg(unix)]` because the layout needs a **symlink**, which is one of
    /// the four exclusions this pins (`os.ReadDir`'s `IsDir` does not follow
    /// one) and which needs a privilege on Windows that a CI runner does not
    /// have. The rule itself is no longer Unix-only — [`serve`] answers this
    /// route everywhere since #374 — and the `filepath` arithmetic underneath
    /// is pinned for both targets by `parity/gopath_windows_vectors.json`.
    #[test]
    #[cfg(unix)]
    fn the_candidate_probe_matches_gos_discovery_rule() {
        let _env = crate::paths::tests::env_lock();
        let vectors: DirVectors =
            serde_json::from_str(DIR_VECTORS).expect("parsing claude dir vectors");
        assert!(vectors.cases.len() >= 5, "vectors look truncated");

        let home = tempfile::tempdir().expect("tempdir");
        build_layout(&vectors.layout, home.path());
        let root = home.path().to_string_lossy().into_owned();
        let _home_var = EnvVar::set("HOME", home.path());

        for case in &vectors.cases {
            let _dir_var = match case.claude_config_dir_env.as_str() {
                "" => EnvVar::unset("CLAUDE_CONFIG_DIR"),
                value => EnvVar::set("CLAUDE_CONFIG_DIR", expand(value, &root)),
            };

            let indexed = claude_config_dirs(
                &expand(&case.run_dir, &root),
                &expand_all(&case.extra, &root),
            );
            assert_eq!(
                indexed,
                expand_all(&case.indexed, &root),
                "indexed — {}",
                case.name
            );
            assert_eq!(
                discover_candidate_claude_dirs(&indexed),
                Some(expand_all(&case.candidates, &root)),
                "candidates — {}",
                case.name
            );
        }
    }

    /// A home that cannot be listed is `null`, and one with nothing to suggest
    /// is `[]`. Both reach the wire, and only the second is an empty array.
    #[test]
    fn an_unlistable_home_is_null_and_an_empty_one_is_an_empty_array() {
        let _env = crate::paths::tests::env_lock();

        let empty = tempfile::tempdir().expect("tempdir");
        let _home_var = EnvVar::set("HOME", empty.path());
        assert_eq!(discover_candidate_claude_dirs(&[]), Some(Vec::new()));

        // `~/.claude`'s parent is the home directory, so a home that does not
        // exist is the unlistable case.
        let _gone = EnvVar::set("HOME", empty.path().join("gone"));
        assert_eq!(discover_candidate_claude_dirs(&[]), None);
    }

    /// `default` is `filepath.Join(home, ".claude")`, and `filepath.Join`
    /// **cleans** — `PathBuf::join` does not. This is the one caller whose
    /// output reaches the wire without passing through [`normalize`], so a
    /// non-clean `HOME` would put a `default` on the wire that disagrees with
    /// the `indexed[0]` beside it.
    #[test]
    fn the_default_dir_is_cleaned_like_filepath_join() {
        let _env = crate::paths::tests::env_lock();
        let _home_var = EnvVar::set("HOME", "/home//u/");
        assert_eq!(default_claude_config_dir(), "/home/u/.claude");
        assert_eq!(
            claude_config_dirs("", &[]),
            vec!["/home/u/.claude".to_string()],
            "`default` and `indexed[0]` are the same dir and must be spelled alike"
        );
    }

    /// `indexed` is resolved from the **env-resolved** settings, not the raw
    /// row: `applyEnvOverrides` overwrites `claude_config_dir` with
    /// `CLAUDE_CONFIG_DIR` whenever that is non-blank — *including when the env
    /// value is relative* — and `ApplyClaudeDirs` installs what is left. So a
    /// relative env value drops the stored dir with it, and reading the raw row
    /// would index a dir Go does not.
    #[test]
    fn a_relative_env_dir_drops_the_stored_run_dir_too() {
        let _env = crate::paths::tests::env_lock();
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".claude").join("projects")).expect("default dir");
        let stored = home.path().join(".claude-work");
        std::fs::create_dir_all(stored.join("projects")).expect("stored dir");

        let _home_var = EnvVar::set("HOME", home.path());
        let _dir_var = EnvVar::set("CLAUDE_CONFIG_DIR", "relative/dir");

        let conn = fixture(Some(&format!(
            "INSERT INTO user_settings (id, claude_config_dir) VALUES (1, '{}')",
            stored.display()
        )));
        let response = claude_config_dirs_response(&conn);
        assert_eq!(
            response.indexed,
            vec![home.path().join(".claude").to_string_lossy().into_owned()],
            "the relative env value replaces the stored one before either is resolved"
        );
        assert!(
            response
                .candidates
                .expect("listable")
                .contains(&stored.to_string_lossy().into_owned()),
            "and the dropped dir is offered as a candidate, exactly as Go offers it"
        );
    }

    /// The envelope, byte for byte — including `default`, which is a Rust
    /// keyword and so is the one field name a rename could quietly drop.
    #[test]
    fn the_config_dirs_envelope_is_gos() {
        let body = super::super::gojson::to_vec(&ClaudeConfigDirsResponse {
            indexed: vec!["/home/u/.claude".into(), "/home/u/.claude-work".into()],
            candidates: Some(vec!["/home/u/.claude-alpha".into()]),
            default_dir: "/home/u/.claude".into(),
        })
        .expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            concat!(
                r#"{"indexed":["/home/u/.claude","/home/u/.claude-work"],"#,
                r#""candidates":["/home/u/.claude-alpha"],"default":"/home/u/.claude"}"#,
                "\n"
            )
        );

        let body = super::super::gojson::to_vec(&ClaudeConfigDirsResponse {
            indexed: vec!["/home/u/.claude".into()],
            candidates: None,
            default_dir: "/home/u/.claude".into(),
        })
        .expect("encode");
        assert!(String::from_utf8(body)
            .expect("utf8")
            .contains(r#""candidates":null"#));
    }

    // ─── PUT /api/settings ────────────────────────────────────────────────────
    //
    // Written but unclaimed, so there is no live diff to run against it. Every
    // expectation below is instead a **literal captured from a Go server built
    // from this checkout**, driven with the same request body — the convention
    // #274 established for the write path.

    fn migrated_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        super::super::migrate::apply(&mut conn).expect("migrate");
        file
    }

    /// These tests assert the *unlocked* answers. A developer whose shell
    /// exports one of the four variables would otherwise fail them for a reason
    /// that is not a bug.
    fn nothing_is_locked() -> bool {
        locked_fields().is_empty()
    }

    /// A `PUT`, with the rescan stubbed out — see [`put_recording_rescan`] for
    /// the variant that asserts on it.
    fn put(db: &std::path::Path, body: &str) -> (axum::http::StatusCode, String) {
        put_recording_rescan(db, body).0
    }

    /// The same, reporting whether the save asked for a rescan.
    fn put_recording_rescan(
        db: &std::path::Path,
        body: &str,
    ) -> ((axum::http::StatusCode, String), bool) {
        let mut rescanned = false;
        let result = update_with(db, body.as_bytes(), |_| rescanned = true);
        let answered = match super::super::writes::finish(result) {
            Ok(answer) => (
                answer.status,
                String::from_utf8(answer.body.unwrap_or_default()).expect("utf8"),
            ),
            Err(reason) => panic!("forwarded rather than answered: {reason}"),
        };
        (answered, rescanned)
    }

    /// The whole happy path, byte for byte against Go.
    ///
    /// Note what is **not** in the answer: `Update` assigns the incoming struct
    /// to `m.settings` wholesale and the handler answers `Get()`, so nothing is
    /// re-defaulted — and `claude_config_dirs` comes back `null` for a request
    /// that sent `[]`, because `normalizeClaudeConfigDirs` collapses an empty
    /// list to a nil slice while `Save` still writes `[]` to the column.
    #[test]
    fn a_full_save_answers_gos_bytes() {
        let _env = crate::paths::tests::env_lock();
        if !nothing_is_locked() {
            return;
        }
        let file = migrated_db();
        let (status, body) = put(
            file.path(),
            concat!(
                r#"{"default_working_dir":"/tmp/agento/work","default_model":"opus","#,
                r#""onboarding_complete":true,"appearance_dark_mode":true,"#,
                r#""appearance_font_size":13,"appearance_font_family":"Inter","#,
                r#""notification_settings":"{\"enabled\":false}","event_bus_worker_pool_size":3,"#,
                r#""public_url":"https://agento.example","#,
                r#""hidden_projects":["/home/u/secret","/home/u/other"],"#,
                r#""idle_gap_threshold_minutes":25,"claude_config_dir":"","#,
                r#""claude_config_dirs":[]}"#
            ),
        );
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(
            body,
            concat!(
                r#"{"settings":{"default_working_dir":"/tmp/agento/work","default_model":"opus","#,
                r#""onboarding_complete":true,"appearance_dark_mode":true,"#,
                r#""appearance_font_size":13,"appearance_font_family":"Inter","#,
                r#""notification_settings":"{\"enabled\":false}","event_bus_worker_pool_size":3,"#,
                r#""public_url":"https://agento.example","#,
                r#""hidden_projects":["/home/u/secret","/home/u/other"],"#,
                r#""idle_gap_threshold_minutes":25,"claude_config_dir":"","#,
                r#""claude_config_dirs":null},"locked":{},"model_from_env":false}"#,
                "\n"
            )
        );

        // …and the row it wrote reads back the way the next `GET` will read it:
        // `[]` where the answer said `null`, and `{}` for a blank
        // `notification_settings`.
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let stored = load_stored(&conn);
        assert_eq!(stored.claude_config_dirs, Some(Vec::new()));
        assert_eq!(
            stored.hidden_projects,
            Some(vec![
                "/home/u/secret".to_string(),
                "/home/u/other".to_string()
            ]),
            "the stored order is the order the client sent"
        );
        assert_eq!(stored.idle_gap_threshold_minutes, 25);
    }

    /// A partial body is not a patch: Go decodes into a zero-valued struct and
    /// stores the lot, so every field the client omitted is cleared. An unknown
    /// key is ignored and an explicit `null` is the zero value.
    #[test]
    fn an_omitted_field_is_cleared_and_a_null_is_its_zero_value() {
        let _env = crate::paths::tests::env_lock();
        if !nothing_is_locked() {
            return;
        }
        let file = migrated_db();
        let (status, body) = put(
            file.path(),
            r#"{"surprise":1,"public_url":null,"hidden_projects":null,"appearance_font_size":null,"idle_gap_threshold_minutes":7}"#,
        );
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(
            body,
            concat!(
                r#"{"settings":{"default_working_dir":"","default_model":"","#,
                r#""onboarding_complete":false,"appearance_dark_mode":false,"#,
                r#""appearance_font_size":0,"appearance_font_family":"","#,
                r#""notification_settings":"","event_bus_worker_pool_size":0,"#,
                r#""public_url":"","hidden_projects":null,"#,
                r#""idle_gap_threshold_minutes":7,"claude_config_dir":"","#,
                r#""claude_config_dirs":null},"locked":{},"model_from_env":false}"#,
                "\n"
            )
        );
    }

    /// Go's decoder is lenient about `null` and strict about everything else.
    /// A `null` **body** is a documented no-op that reaches the handler as the
    /// zero value and saves; an array is a decode error and 400s.
    #[test]
    fn a_null_body_saves_and_an_array_body_is_rejected() {
        let _env = crate::paths::tests::env_lock();
        if !nothing_is_locked() {
            return;
        }
        let file = migrated_db();
        let (status, body) = put(file.path(), "null");
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains(r#""idle_gap_threshold_minutes":0"#), "{body}");

        let (status, body) = put(file.path(), r#"["x"]"#);
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body, "{\"error\":\"invalid JSON body\"}\n");

        let (status, body) = put(file.path(), "not json");
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body, "{\"error\":\"invalid JSON body\"}\n");
    }

    /// Every rejection is a **400** carrying the error's own text — not the 409
    /// the monitoring path answers for an env-locked write, and not the 422 the
    /// service layer's `ValidationError` produces. `SettingsManager` returns
    /// plain errors and the handler flattens them all.
    #[test]
    fn the_validation_failures_are_400s_with_gos_wording() {
        let _env = crate::paths::tests::env_lock();
        if !nothing_is_locked() {
            return;
        }
        let file = migrated_db();
        let scratch = tempfile::tempdir().expect("tempdir");
        let missing = scratch.path().join("nope");
        let not_a_dir = scratch.path().join("notadir");
        std::fs::write(&not_a_dir, "").expect("file");

        for (body, expected) in [
            (
                r#"{"idle_gap_threshold_minutes":500}"#.to_string(),
                "idle_gap_threshold_minutes must be between 1 and 240 minutes, got 500".to_string(),
            ),
            (
                r#"{"idle_gap_threshold_minutes":-1}"#.to_string(),
                "idle_gap_threshold_minutes must be between 1 and 240 minutes, got -1".to_string(),
            ),
            (
                r#"{"claude_config_dir":"relative/dir"}"#.to_string(),
                "claude config dir must be an absolute path, got \\\"relative/dir\\\"".to_string(),
            ),
            (
                format!(r#"{{"claude_config_dir":"{}"}}"#, missing.display()),
                format!(
                    "claude config dir \\\"{}\\\" does not exist",
                    missing.display()
                ),
            ),
            (
                format!(r#"{{"claude_config_dir":"{}"}}"#, not_a_dir.display()),
                format!(
                    "claude config dir \\\"{}\\\" is not a directory",
                    not_a_dir.display()
                ),
            ),
            (
                format!(r#"{{"claude_config_dirs":["{}"]}}"#, missing.display()),
                format!(
                    "claude config dir \\\"{}\\\" does not exist",
                    missing.display()
                ),
            ),
        ] {
            let (status, answer) = put(file.path(), &body);
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(answer, format!("{{\"error\":\"{expected}\"}}\n"), "{body}");
        }

        // Zero is "not chosen", not a zero-length sitting — the whole reason the
        // bound starts at 1 and zero is still accepted.
        let (status, _) = put(file.path(), r#"{"idle_gap_threshold_minutes":0}"#);
        assert_eq!(status, axum::http::StatusCode::OK);
    }

    /// Nothing may be written before a rejection, or the 500 would be reporting
    /// a change that partly landed. Cheaper to pin than to re-derive.
    #[test]
    fn a_rejected_save_leaves_the_row_untouched() {
        let _env = crate::paths::tests::env_lock();
        if !nothing_is_locked() {
            return;
        }
        let file = migrated_db();
        put(file.path(), r#"{"public_url":"https://kept.example"}"#);
        let (status, _) = put(
            file.path(),
            r#"{"public_url":"https://clobbered.example","idle_gap_threshold_minutes":9999}"#,
        );
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);

        let conn = rusqlite::Connection::open(file.path()).expect("open");
        assert_eq!(load_stored(&conn).public_url, "https://kept.example");
    }

    /// A dir already in the stored list is **not** re-validated, so an unmounted
    /// volume cannot block a save of some unrelated field.
    #[test]
    fn an_unchanged_dir_is_not_revalidated_when_it_stops_existing() {
        let _env = crate::paths::tests::env_lock();
        if !nothing_is_locked() {
            return;
        }
        let file = migrated_db();
        let scratch = tempfile::tempdir().expect("tempdir");
        let volume = scratch.path().join("volume");
        std::fs::create_dir_all(&volume).expect("dir");

        let listed = format!(r#"["{}"]"#, volume.display());
        let (status, _) = put(
            file.path(),
            &format!(r#"{{"claude_config_dirs":{listed}}}"#),
        );
        assert_eq!(status, axum::http::StatusCode::OK);

        std::fs::remove_dir(&volume).expect("unmount");
        let (status, body) = put(
            file.path(),
            &format!(r#"{{"appearance_font_size":15,"claude_config_dirs":{listed}}}"#),
        );
        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "an unchanged dir must not be re-validated: {body}"
        );

        // …but adding a *new* missing one still fails.
        let (status, _) = put(
            file.path(),
            &format!(
                r#"{{"claude_config_dirs":["{}","{}"]}}"#,
                volume.display(),
                scratch.path().join("brand-new").display()
            ),
        );
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    }

    /// The `locked` map is a **400**, and it is the message Go writes.
    ///
    /// Three properties in one, because they share an environment: a blank
    /// incoming value is pinned rather than treated as a request to clear;
    /// `claude_config_dir` is compared *normalized*, so a trailing slash is not
    /// a change; and the field order is Go's slice order, so a body conflicting
    /// on two locked fields reports `public_url` rather than whichever came
    /// first in the JSON.
    #[test]
    fn a_locked_field_is_a_400_and_a_blank_one_is_pinned() {
        let _env = crate::paths::tests::env_lock();
        let scratch = tempfile::tempdir().expect("tempdir");
        let pinned = scratch.path().join("pinned");
        let other = scratch.path().join("other");
        std::fs::create_dir_all(&pinned).expect("dir");
        std::fs::create_dir_all(&other).expect("dir");

        // RAII, not a trailing restore: a failed assertion below panics past
        // any epilogue, and these two variables would then leak into the rest
        // of the binary — where six other tests skip themselves when anything
        // is locked, silently turning one failure into seven.
        let _url_var = EnvVar::set("AGENTO_PUBLIC_URL", "https://example.test");
        let _dir_var = EnvVar::set("CLAUDE_CONFIG_DIR", pinned.to_string_lossy().as_ref());

        let file = migrated_db();
        let (status, body) = put(
            file.path(),
            &format!(r#"{{"claude_config_dir":"{}"}}"#, other.display()),
        );
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            "{\"error\":\"claude_config_dir is locked by environment variable CLAUDE_CONFIG_DIR\"}\n"
        );

        let (status, body) = put(file.path(), r#"{"public_url":"https://other.test"}"#);
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            body.contains("public_url is locked by environment variable AGENTO_PUBLIC_URL"),
            "{body}"
        );

        // Both at once reports `public_url`: it comes first in Go's slice.
        let (_, body) = put(
            file.path(),
            &format!(
                r#"{{"public_url":"https://other.test","claude_config_dir":"{}"}}"#,
                other.display()
            ),
        );
        assert!(body.contains("public_url is locked"), "{body}");

        // A trailing slash normalizes to the same dir, so it saves — and both
        // locked fields come back pinned to what the environment chose.
        let (status, body) = put(
            file.path(),
            &format!(r#"{{"claude_config_dir":"{}/"}}"#, pinned.display()),
        );
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert!(
            body.contains(&format!(r#""claude_config_dir":"{}""#, pinned.display())),
            "{body}"
        );
        assert!(
            body.contains(r#""public_url":"https://example.test""#),
            "{body}"
        );
        assert!(
            body.contains(
                r#""locked":{"claude_config_dir":"CLAUDE_CONFIG_DIR","public_url":"AGENTO_PUBLIC_URL"}"#
            ),
            "{body}"
        );

        // A blank value is pinned rather than read as "clear it".
        let (status, body) = put(file.path(), r#"{"claude_config_dir":""}"#);
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(
            body.contains(&format!(r#""claude_config_dir":"{}""#, pinned.display())),
            "{body}"
        );
    }

    /// `applyDataSettings`'s three cases, which are not symmetrical.
    ///
    /// Hiding a project is a filter over cached rows and takes effect on the
    /// next read, so it must **not** cost a corpus walk. A threshold change
    /// must, because active duration is stored per transcript. Adding a config
    /// dir must too, and for a different reason: that dir has never been walked,
    /// so there are no rows to filter. Removing one needs no scan either, but
    /// the comparison is on the resolved set, so it is one rule.
    #[test]
    fn only_a_threshold_or_a_config_dir_change_asks_for_a_rescan() {
        let _env = crate::paths::tests::env_lock();
        if !nothing_is_locked() {
            return;
        }
        let file = migrated_db();
        let scratch = tempfile::tempdir().expect("tempdir");
        let extra = scratch.path().join("extra");
        std::fs::create_dir_all(&extra).expect("dir");

        // A first save that changes nothing about time or dirs.
        let (_, rescanned) =
            put_recording_rescan(file.path(), r#"{"public_url":"https://a.test"}"#);
        assert!(!rescanned, "an ordinary save must not walk the corpus");

        let (_, rescanned) = put_recording_rescan(
            file.path(),
            r#"{"hidden_projects":["/home/u/secret"],"public_url":"https://a.test"}"#,
        );
        assert!(!rescanned, "hiding a project is a filter, not a re-read");

        let (_, rescanned) = put_recording_rescan(
            file.path(),
            r#"{"hidden_projects":["/home/u/secret"],"idle_gap_threshold_minutes":25}"#,
        );
        assert!(
            rescanned,
            "a moved threshold restates every stored duration"
        );

        let (_, rescanned) = put_recording_rescan(
            file.path(),
            &format!(
                r#"{{"idle_gap_threshold_minutes":25,"claude_config_dirs":["{}"]}}"#,
                extra.display()
            ),
        );
        assert!(
            rescanned,
            "a dir that was never walked has no rows to filter"
        );

        // …and saving the very same thing again is free.
        let (_, rescanned) = put_recording_rescan(
            file.path(),
            &format!(
                r#"{{"idle_gap_threshold_minutes":25,"claude_config_dirs":["{}"]}}"#,
                extra.display()
            ),
        );
        assert!(!rescanned, "an unchanged save must cost nothing");
    }

    /// `normalizeClaudeConfigDirs`: normalize, drop blanks, drop duplicates, and
    /// answer **nil** rather than `[]` when nothing survives.
    #[test]
    fn the_stored_dir_list_is_normalized_and_collapses_to_nil() {
        assert_eq!(normalize_claude_config_dirs(&None), None);
        assert_eq!(normalize_claude_config_dirs(&Some(Vec::new())), None);
        assert_eq!(
            normalize_claude_config_dirs(&Some(vec!["  ".into(), "".into()])),
            None
        );
        assert_eq!(
            normalize_claude_config_dirs(&Some(vec![
                "/var/lib/claude/".into(),
                "/var/lib/claude".into(),
                " /var/lib/other ".into(),
            ])),
            Some(vec![
                "/var/lib/claude".to_string(),
                "/var/lib/other".to_string()
            ])
        );
    }

    #[test]
    fn the_two_reads_and_the_write_are_claimed() {
        assert!(claims(&Method::GET, "/api/settings"));
        assert!(claims(&Method::GET, "/api/settings/claude-config-dirs"));
        // Claimed since the cut-over (#278): the sidecar snapshot that kept
        // this unwired died with the sidecar. See `claims`.
        assert!(claims(&Method::PUT, "/api/settings"));
        assert!(!claims(&Method::PUT, "/api/settings/claude-config-dirs"));
        assert!(!claims(&Method::POST, "/api/settings"));
        assert!(!claims(&Method::GET, "/api/settings/"));
    }
}

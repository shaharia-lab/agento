//! The gateway's settings model, its SQLite storage, and the mapping onto
//! `ferrox-providers`' own config types (#422).
//!
//! # The credential rule, which is the whole security story of this file
//!
//! `gateway_providers.api_key` is a provider secret in plaintext at rest, on the
//! same terms as `integrations.credentials`: the protection is perimeter-only
//! (loopback bind, directory permissions, the `/api` bearer token). What keeps
//! it from leaking *outward* is a reader discipline copied from
//! `native/integrations/registry.rs`, and it is three rules rather than one:
//!
//! 1. **Two projections, and the public one is not built on the secret one.**
//!    [`PROVIDER_COLUMNS_PUBLIC`] does not name `api_key` at all;
//!    [`PROVIDER_COLUMNS_SECRET`] does and is the only thing that does. Sharing
//!    a base would put the column one `SELECT` away from every future read.
//! 2. **The row type that carries it derives neither `Serialize` nor `Debug`.**
//!    A `{row:?}` in a log line is the same leak with a longer fuse, which is
//!    why the absence of `Debug` matters as much as the absence of `Serialize`.
//! 3. **The ferrox value is built at the point of use and dropped.**
//!    `ferrox_providers::config::ProviderConfig` derives *both* `Debug` and
//!    `Serialize` and holds `api_key: Option<String>`, so holding one on a
//!    long-lived struct would re-open what rule 2 closes.
//!
//! A malformed stored value reports **line and column only**, never serde's own
//! message, which quotes the offending text — `native/integration_credentials.rs`
//! established that and the reason applies identically here.
//!
//! # What this module does not do
//!
//! It computes and stores; it starts nothing. There is no listener here (#424)
//! and no usage recording (#425). Every function takes a `&Path` to the
//! database and opens its own connection through [`crate::native::db`] — never a
//! bare `rusqlite::Connection::open`, which opens `READWRITE|CREATE` and was
//! observed to reset the WAL of a database another process holds.

use std::path::Path;

use ferrox_providers::config::{DefaultsConfig, ProviderConfig, TimeoutsConfig};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::native::{db, gotime};

/// The default port the gateway binds when nothing has been configured.
///
/// Above the dynamic-port floor and clear of the app's own dev listener (8991)
/// and the installed server's historical 8990, so a first run does not collide
/// with an Agento the user already has open.
pub const DEFAULT_PORT: u16 = 8880;

// ─── Provider types ───────────────────────────────────────────────────────────

/// Which upstream a provider row talks to.
///
/// **Four, not five.** `ferrox_providers::config::ProviderType` also has
/// `Bedrock`, and this deliberately does not: the AWS SDK behind that adapter
/// pins Rust 1.94.1 against this crate's declared 1.88 floor, so the `bedrock`
/// feature is off in `Cargo.toml` and a stored `bedrock` row would name an
/// adapter that was never compiled in. Refusing it here is what makes that a
/// validation error rather than a runtime surprise in #424.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Anthropic,
    Openai,
    Gemini,
    /// Z.AI GLM — fully OpenAI-compatible, and served by the OpenAI adapter
    /// with a custom `base_url`.
    Glm,
}

impl ProviderType {
    /// The stored spelling, which is also what the control API will put on the
    /// wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Gemini => "gemini",
            Self::Glm => "glm",
        }
    }

    /// Parse the stored spelling. `None` for anything else — including
    /// `bedrock`, which is a real ferrox provider type this build cannot serve,
    /// and including any casing but lower.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
            "gemini" => Some(Self::Gemini),
            "glm" => Some(Self::Glm),
            _ => None,
        }
    }

    /// The ferrox variant this maps onto.
    fn to_ferrox(self) -> ferrox_providers::config::ProviderType {
        use ferrox_providers::config::ProviderType as F;
        match self {
            Self::Anthropic => F::Anthropic,
            Self::Openai => F::OpenAI,
            Self::Gemini => F::Gemini,
            Self::Glm => F::Glm,
        }
    }
}

// ─── Settings ─────────────────────────────────────────────────────────────────

/// The single-row `gateway_settings` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewaySettings {
    pub enabled: bool,
    pub port: u16,
    pub start_with_app: bool,
}

impl Default for GatewaySettings {
    /// **Disabled**, which is what "ships in an ordinary release, zero cost when
    /// off" means: a fresh install binds no port until the user asks for one.
    /// `start_with_app` defaults *on* because it only takes effect once
    /// `enabled` is true, and a gateway that dies on every restart is not a
    /// gateway.
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            start_with_app: true,
        }
    }
}

impl GatewaySettings {
    /// Reject a configuration this build cannot serve.
    ///
    /// Port 0 is the one real case. It is what the OS reads as "assign me
    /// anything", which is unusable here for a reason specific to this feature:
    /// the port is what a user pastes into a tool config, so it has to be the
    /// number they chose and it has to survive a restart.
    pub fn validate(&self) -> Result<(), String> {
        if self.port == 0 {
            return Err("port must be between 1 and 65535".to_string());
        }
        Ok(())
    }
}

/// Read the settings row, or the defaults when nothing has been stored.
///
/// A missing row is not an error: the migration creates the table empty, so
/// "never configured" is the ordinary state of a fresh install and answering
/// [`GatewaySettings::default`] is what makes the first read work.
pub fn load_settings(db_path: &Path) -> Result<GatewaySettings, String> {
    let conn = db::open_read_only(db_path)?;
    load_settings_from(&conn)
}

fn load_settings_from(conn: &Connection) -> Result<GatewaySettings, String> {
    conn.query_row(
        "SELECT enabled, port, start_with_app FROM gateway_settings WHERE id = 1",
        [],
        |row| {
            let port: i64 = row.get(1)?;
            Ok(GatewaySettings {
                enabled: row.get::<_, i64>(0)? != 0,
                // Clamped rather than cast: the column is INTEGER and SQLite
                // will store whatever was written to it, so a value outside
                // `u16` has to become something rather than wrap silently.
                port: u16::try_from(port).unwrap_or(DEFAULT_PORT),
                start_with_app: row.get::<_, i64>(2)? != 0,
            })
        },
    )
    .optional()
    .map_err(|e| format!("reading gateway settings: {e}"))
    .map(Option::unwrap_or_default)
}

/// Write the settings row, creating it on first save.
pub fn store_settings(db_path: &Path, settings: &GatewaySettings) -> Result<(), String> {
    settings.validate()?;
    let mut conn = db::open_read_write(db_path)?;
    // The guard every native write applies: refuse a database whose schema is
    // not the one this build compiled against, in either direction. These three
    // writes are not on the `/api` seam, but the reason for the check is the
    // schema rather than the route.
    crate::native::migrate::verify(&conn)?;
    let now = gotime::now_go_text();
    let tx = conn
        .transaction()
        .map_err(|e| format!("beginning gateway settings write: {e}"))?;
    tx.execute(
        "INSERT INTO gateway_settings (id, enabled, port, start_with_app, created_at, updated_at)\n         VALUES (1, ?1, ?2, ?3, ?4, ?4)\n         ON CONFLICT(id) DO UPDATE SET\n             enabled = excluded.enabled,\n             port = excluded.port,\n             start_with_app = excluded.start_with_app,\n             updated_at = excluded.updated_at",
        rusqlite::params![
            settings.enabled as i64,
            settings.port as i64,
            settings.start_with_app as i64,
            now,
        ],
    )
    .map_err(|e| format!("writing gateway settings: {e}"))?;
    tx.commit()
        .map_err(|e| format!("committing gateway settings: {e}"))
}

// ─── Providers ────────────────────────────────────────────────────────────────

/// The projection that reads the API key. **The only one that does.**
///
/// Deliberately not a superset built from [`PROVIDER_COLUMNS_PUBLIC`]: the point
/// of that constant is that the secret is not in it, and a shared base would
/// make the two drift into one.
const PROVIDER_COLUMNS_SECRET: &str = "SELECT id, name, type, api_key, base_url,
            connect_secs, ttfb_secs, idle_secs, enabled
     FROM gateway_providers";

/// The projection everything *except* server construction uses — the control
/// API (#426) and the UI (#427) read through this.
///
/// `api_key` is replaced by a boolean computed in SQL, so a caller can render
/// "configured" without the value existing in this process to be echoed. Same
/// shape as `integrations`' `authenticated` column, for the same reason.
const PROVIDER_COLUMNS_PUBLIC: &str = "SELECT id, name, type,
            (api_key IS NOT NULL AND api_key != '') AS has_api_key,
            base_url, connect_secs, ttfb_secs, idle_secs, enabled
     FROM gateway_providers";

/// A provider row **including its API key**.
///
/// Derives neither `Serialize` nor `Debug`, and that is not stylistic: the
/// first would let it reach a response body and the second a log line. Only a
/// `&str` leaves it, and the one consumer that needs the key is
/// [`Self::to_ferrox`], which builds a value and hands it straight to an adapter
/// constructor.
pub struct ProviderRow {
    pub id: String,
    pub name: String,
    pub provider_type: ProviderType,
    api_key: String,
    pub base_url: String,
    pub timeouts: Timeouts,
    pub enabled: bool,
}

/// The per-provider timeout triple, mirroring `ferrox_providers`'
/// `TimeoutsConfig` with the same defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeouts {
    pub connect_secs: u64,
    pub ttfb_secs: u64,
    pub idle_secs: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        let d = TimeoutsConfig::default();
        Self {
            connect_secs: d.connect_secs,
            ttfb_secs: d.ttfb_secs,
            idle_secs: d.idle_secs,
        }
    }
}

impl Timeouts {
    fn to_ferrox(self) -> TimeoutsConfig {
        TimeoutsConfig {
            connect_secs: self.connect_secs,
            ttfb_secs: self.ttfb_secs,
            idle_secs: self.idle_secs,
        }
    }
}

/// A provider row **without** its API key — what a read answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: ProviderType,
    /// Whether a key is stored, computed in SQL. Never the key.
    pub has_api_key: bool,
    pub base_url: String,
    pub timeouts: Timeouts,
    pub enabled: bool,
}

impl ProviderRow {
    /// Build the `ferrox-providers` configuration for this provider.
    ///
    /// Returns an owned value the caller is expected to hand to an adapter and
    /// drop — see rule 3 in the module header. `base_url` maps to `None` when
    /// empty rather than to `Some("")`, because ferrox reads `None` as "use the
    /// adapter's own default endpoint" and an empty string is a URL that
    /// resolves nowhere.
    pub fn to_ferrox(&self) -> ProviderConfig {
        ProviderConfig {
            name: self.name.clone(),
            provider_type: self.provider_type.to_ferrox(),
            api_key: non_empty(&self.api_key),
            base_url: non_empty(&self.base_url),
            aws: None,
            timeouts: Some(self.timeouts.to_ferrox()),
            circuit_breaker: None,
        }
    }

    /// The redacted view of this row.
    pub fn to_summary(&self) -> ProviderSummary {
        ProviderSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            provider_type: self.provider_type,
            has_api_key: !self.api_key.is_empty(),
            base_url: self.base_url.clone(),
            timeouts: self.timeouts,
            enabled: self.enabled,
        }
    }
}

/// `""` is "not set", `Some(v)` is a real value.
///
/// The columns are `NOT NULL DEFAULT ''` rather than nullable, so this is the
/// single place the empty-versus-absent distinction is made, and it is made the
/// same way for both optional strings.
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn scan_provider_secret(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderRow> {
    let raw_type: String = row.get(2)?;
    Ok(ProviderRow {
        id: row.get(0)?,
        name: row.get(1)?,
        // An unrecognized stored type cannot be represented, and guessing would
        // mean building an adapter the row did not ask for. `FromSqlConversionFailure`
        // is the closest rusqlite error and carries no value text.
        provider_type: ProviderType::parse(&raw_type).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(UnknownProviderType),
            )
        })?,
        api_key: row.get(3)?,
        base_url: row.get(4)?,
        timeouts: Timeouts {
            connect_secs: row.get::<_, i64>(5)?.max(0) as u64,
            ttfb_secs: row.get::<_, i64>(6)?.max(0) as u64,
            idle_secs: row.get::<_, i64>(7)?.max(0) as u64,
        },
        enabled: row.get::<_, i64>(8)? != 0,
    })
}

/// Deliberately names no value — the offending text is the thing this file must
/// not put in an error string.
#[derive(Debug)]
struct UnknownProviderType;

impl std::fmt::Display for UnknownProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider type is not one this build can serve")
    }
}

impl std::error::Error for UnknownProviderType {}

/// Every enabled provider, with keys — the read that builds the registry.
pub fn load_providers(db_path: &Path) -> Result<Vec<ProviderRow>, String> {
    let conn = db::open_read_only(db_path)?;
    load_providers_from(&conn)
}

fn load_providers_from(conn: &Connection) -> Result<Vec<ProviderRow>, String> {
    let sql = format!("{PROVIDER_COLUMNS_SECRET}\n     ORDER BY name ASC");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("preparing gateway provider read: {e}"))?;
    let rows = stmt
        .query_map([], scan_provider_secret)
        .map_err(|e| format!("reading gateway providers: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("reading gateway providers: {e}"))
}

/// Every provider, redacted — the read the control API and UI use.
pub fn load_provider_summaries(db_path: &Path) -> Result<Vec<ProviderSummary>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{PROVIDER_COLUMNS_PUBLIC}\n     ORDER BY name ASC");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("preparing gateway provider read: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            let raw_type: String = row.get(2)?;
            Ok(ProviderSummary {
                id: row.get(0)?,
                name: row.get(1)?,
                provider_type: ProviderType::parse(&raw_type).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(UnknownProviderType),
                    )
                })?,
                has_api_key: row.get::<_, i64>(3)? != 0,
                base_url: row.get(4)?,
                timeouts: Timeouts {
                    connect_secs: row.get::<_, i64>(5)?.max(0) as u64,
                    ttfb_secs: row.get::<_, i64>(6)?.max(0) as u64,
                    idle_secs: row.get::<_, i64>(7)?.max(0) as u64,
                },
                enabled: row.get::<_, i64>(8)? != 0,
            })
        })
        .map_err(|e| format!("reading gateway providers: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("reading gateway providers: {e}"))
}

/// Everything a provider write needs.
///
/// A struct rather than eight positional arguments, and `api_key` is a plain
/// **required field** rather than an `Option`: a caller must say what the key
/// is, every time. That is deliberate. `PUT /api/integrations/{id}` wipes stored
/// credentials when the caller omits them — a real data-loss bug on this
/// repository's known-bugs list — and it is reachable precisely because "field
/// absent" and "field empty" are the same thing to that write. Making the field
/// mandatory means a caller that wants to preserve the existing key has to read
/// it and pass it, which is a visible act rather than an omission.
///
/// Derives neither `Serialize` nor `Debug`, for [`ProviderRow`]'s reason: it
/// carries the same secret.
pub struct ProviderInput<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub provider_type: ProviderType,
    pub api_key: &'a str,
    pub base_url: &'a str,
    pub timeouts: Timeouts,
    pub enabled: bool,
}

/// Insert or replace a provider.
pub fn store_provider(db_path: &Path, input: &ProviderInput<'_>) -> Result<(), String> {
    let ProviderInput {
        id,
        name,
        provider_type,
        api_key,
        base_url,
        timeouts,
        enabled,
    } = *input;
    if name.trim().is_empty() {
        return Err("provider name is required".to_string());
    }
    let mut conn = db::open_read_write(db_path)?;
    crate::native::migrate::verify(&conn)?;
    let now = gotime::now_go_text();
    let tx = conn
        .transaction()
        .map_err(|e| format!("beginning gateway provider write: {e}"))?;
    tx.execute(
        "INSERT INTO gateway_providers\n             (id, name, type, api_key, base_url, connect_secs, ttfb_secs, idle_secs, enabled, created_at, updated_at)\n         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)\n         ON CONFLICT(id) DO UPDATE SET\n             name = excluded.name,\n             type = excluded.type,\n             api_key = excluded.api_key,\n             base_url = excluded.base_url,\n             connect_secs = excluded.connect_secs,\n             ttfb_secs = excluded.ttfb_secs,\n             idle_secs = excluded.idle_secs,\n             enabled = excluded.enabled,\n             updated_at = excluded.updated_at",
        rusqlite::params![
            id,
            name,
            provider_type.as_str(),
            api_key,
            base_url,
            timeouts.connect_secs as i64,
            timeouts.ttfb_secs as i64,
            timeouts.idle_secs as i64,
            enabled as i64,
            now,
        ],
    )
    .map_err(|e| format!("writing gateway provider: {e}"))?;
    tx.commit()
        .map_err(|e| format!("committing gateway provider: {e}"))
}

// ─── Model aliases ────────────────────────────────────────────────────────────

/// One upstream a request can be sent to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteTarget {
    /// The `gateway_providers.name` this refers to.
    pub provider: String,
    /// The model id sent upstream, which is **not** the alias the client asked
    /// for — ferrox's `ProviderAdapter::chat` takes it separately from
    /// `req.model` for exactly this reason.
    pub model_id: String,
}

/// The stored `routing` document.
///
/// `targets` is ordered and the order is the whole meaning: the first is
/// preferred, and `fallbacks` is walked after all of them fail. A child table
/// would need a position column and a join to say the same thing less safely,
/// which is why this is JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routing {
    #[serde(default)]
    pub targets: Vec<RouteTarget>,
    #[serde(default)]
    pub fallbacks: Vec<RouteTarget>,
}

/// A `gateway_model_aliases` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAlias {
    pub id: String,
    /// What the client sends as `model`. ferrox routes on a config-driven alias
    /// table only — there is no prefix parsing — so this string is the entire
    /// routing key.
    pub alias: String,
    pub routing: Routing,
    pub enabled: bool,
}

/// Decode a stored `routing` column.
///
/// An empty column is an empty [`Routing`], not an error: the column is
/// `NOT NULL DEFAULT ''`, so a row created without routing reads as "no targets
/// configured yet" rather than as corruption.
///
/// A malformed one reports **line and column and nothing else**. serde's own
/// message quotes the offending text, and this document sits beside an API key
/// in a table whose whole discipline is that stored values do not reach a
/// message.
pub fn decode_routing(raw: &str) -> Result<Routing, String> {
    if raw.is_empty() {
        return Ok(Routing::default());
    }
    serde_json::from_str(raw).map_err(|e| {
        format!(
            "invalid routing configuration: malformed JSON at line {} column {}",
            e.line(),
            e.column()
        )
    })
}

/// Every alias, ordered by the key clients route on.
pub fn load_aliases(db_path: &Path) -> Result<Vec<ModelAlias>, String> {
    let conn = db::open_read_only(db_path)?;
    load_aliases_from(&conn)
}

fn load_aliases_from(conn: &Connection) -> Result<Vec<ModelAlias>, String> {
    let mut stmt = conn
        .prepare("SELECT id, alias, routing, enabled FROM gateway_model_aliases ORDER BY alias ASC")
        .map_err(|e| format!("preparing gateway alias read: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
            ))
        })
        .map_err(|e| format!("reading gateway aliases: {e}"))?;

    let mut out = Vec::new();
    for row in rows {
        let (id, alias, raw, enabled) = row.map_err(|e| format!("reading gateway aliases: {e}"))?;
        out.push(ModelAlias {
            id,
            alias,
            routing: decode_routing(&raw)?,
            enabled,
        });
    }
    Ok(out)
}

/// Insert or replace an alias.
pub fn store_alias(db_path: &Path, alias: &ModelAlias) -> Result<(), String> {
    if alias.alias.trim().is_empty() {
        return Err("alias is required".to_string());
    }
    let routing = serde_json::to_string(&alias.routing)
        .map_err(|e| format!("encoding routing configuration: {e}"))?;
    let mut conn = db::open_read_write(db_path)?;
    crate::native::migrate::verify(&conn)?;
    let now = gotime::now_go_text();
    let tx = conn
        .transaction()
        .map_err(|e| format!("beginning gateway alias write: {e}"))?;
    tx.execute(
        "INSERT INTO gateway_model_aliases (id, alias, routing, enabled, created_at, updated_at)\n         VALUES (?1, ?2, ?3, ?4, ?5, ?5)\n         ON CONFLICT(id) DO UPDATE SET\n             alias = excluded.alias,\n             routing = excluded.routing,\n             enabled = excluded.enabled,\n             updated_at = excluded.updated_at",
        rusqlite::params![alias.id, alias.alias, routing, alias.enabled as i64, now],
    )
    .map_err(|e| format!("writing gateway alias: {e}"))?;
    tx.commit()
        .map_err(|e| format!("committing gateway alias: {e}"))
}

// ─── Defaults ─────────────────────────────────────────────────────────────────

/// The `DefaultsConfig` a registry is built with.
///
/// Only `timeouts` is read by the adapters; `retry` and `circuit_breaker` are
/// carried by that struct because ferrox deserializes all three from one YAML
/// object. This build sets neither — v1 policy is retries plus per-alias
/// fallback chains implemented in #424's own dispatch, and circuit breakers are
/// explicitly out of v1 — so their `Default` is what ships.
pub fn defaults() -> DefaultsConfig {
    DefaultsConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database with the schema applied, in memory — `tokens.rs`'s helper.
    fn schema() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn
    }

    #[test]
    fn the_migration_creates_all_three_tables() {
        let conn = schema();
        for table in [
            "gateway_settings",
            "gateway_providers",
            "gateway_model_aliases",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("query");
            assert_eq!(count, 1, "{table} was not created");
        }
    }

    /// **The empty database answers the defaults, and the defaults are off.**
    ///
    /// A fresh install has no row, and that is the ordinary state rather than an
    /// error — the migration creates the table empty. Shipping `enabled: true`
    /// here would bind a port on every existing install at the next update.
    #[test]
    fn no_stored_row_reads_as_the_disabled_defaults() {
        let conn = schema();
        let settings = load_settings_from(&conn).expect("load");
        assert_eq!(settings, GatewaySettings::default());
        assert!(!settings.enabled, "the gateway must ship disabled");
        assert_eq!(settings.port, DEFAULT_PORT);
        assert!(settings.start_with_app);
    }

    #[test]
    fn every_provider_type_round_trips_and_bedrock_does_not() {
        for t in [
            ProviderType::Anthropic,
            ProviderType::Openai,
            ProviderType::Gemini,
            ProviderType::Glm,
        ] {
            assert_eq!(ProviderType::parse(t.as_str()), Some(t));
        }
        // The one ferrox knows and this build cannot serve, because its adapter
        // is not compiled in — see the type's own note.
        assert_eq!(ProviderType::parse("bedrock"), None);
        assert_eq!(ProviderType::parse("Anthropic"), None, "casing is exact");
        assert_eq!(ProviderType::parse(""), None);
    }

    /// **Empty and absent are different**, and the mapping onto ferrox is where
    /// it shows: `Some("")` is a URL that resolves nowhere, while `None` means
    /// "use the adapter's own endpoint".
    #[test]
    fn an_empty_optional_maps_to_none_rather_than_to_an_empty_string() {
        let row = ProviderRow {
            id: "p1".into(),
            name: "prod".into(),
            provider_type: ProviderType::Anthropic,
            api_key: String::new(),
            base_url: String::new(),
            timeouts: Timeouts::default(),
            enabled: true,
        };
        let cfg = row.to_ferrox();
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.base_url, None);

        let row = ProviderRow {
            api_key: "sk-test".into(),
            base_url: "https://example.invalid/v1".into(),
            ..row
        };
        let cfg = row.to_ferrox();
        assert_eq!(cfg.api_key.as_deref(), Some("sk-test"));
        assert_eq!(cfg.base_url.as_deref(), Some("https://example.invalid/v1"));
    }

    #[test]
    fn a_provider_round_trips_through_the_secret_projection() {
        let db = scratch_db();
        let timeouts = Timeouts {
            connect_secs: 5,
            ttfb_secs: 90,
            idle_secs: 15,
        };
        store_provider(
            db.path(),
            &ProviderInput {
                id: "p1",
                name: "prod-anthropic",
                provider_type: ProviderType::Anthropic,
                api_key: "sk-secret",
                base_url: "",
                timeouts,
                enabled: true,
            },
        )
        .expect("store");

        let rows = load_providers(db.path()).expect("load");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.name, "prod-anthropic");
        assert_eq!(row.provider_type, ProviderType::Anthropic);
        assert_eq!(row.timeouts.connect_secs, 5);
        assert_eq!(row.timeouts.ttfb_secs, 90);
        assert_eq!(row.timeouts.idle_secs, 15);
        assert!(row.enabled);

        let cfg = row.to_ferrox();
        assert_eq!(cfg.api_key.as_deref(), Some("sk-secret"));
        assert_eq!(cfg.timeouts.expect("timeouts").ttfb_secs, 90);

        // ...and a second write with the same id **updates**, rather than
        // failing on the primary key or leaving two rows. This is the
        // `ON CONFLICT` clause, which no test reached while these went through
        // a hand-written copy of the statement.
        store_provider(
            db.path(),
            &ProviderInput {
                id: "p1",
                name: "prod-anthropic",
                provider_type: ProviderType::Anthropic,
                api_key: "sk-rotated",
                base_url: "https://example.invalid/v1",
                timeouts,
                enabled: false,
            },
        )
        .expect("second store");

        let rows = load_providers(db.path()).expect("reload");
        assert_eq!(rows.len(), 1, "the upsert must not insert a second row");
        assert_eq!(rows[0].to_ferrox().api_key.as_deref(), Some("sk-rotated"));
        assert_eq!(rows[0].base_url, "https://example.invalid/v1");
        assert!(!rows[0].enabled);
    }

    /// An empty name is refused **before** anything is written.
    #[test]
    fn a_provider_with_no_name_is_refused_and_writes_nothing() {
        let db = scratch_db();
        let err = store_provider(
            db.path(),
            &ProviderInput {
                id: "p1",
                name: "   ",
                provider_type: ProviderType::Anthropic,
                api_key: "sk-secret",
                base_url: "",
                timeouts: Timeouts::default(),
                enabled: true,
            },
        )
        .expect_err("an empty name must be refused");
        assert!(err.contains("name"), "{err}");
        assert!(
            load_providers(db.path()).is_ok_and(|rows| rows.is_empty()),
            "a refused write must leave the table empty"
        );
    }

    /// **The public projection cannot return the key**, asserted against the SQL
    /// rather than against a value.
    ///
    /// A value assertion only proves the key was absent from the row that
    /// happened to be tested; the rule is about the statement. This is what
    /// fails if someone "simplifies" the two projections into one.
    #[test]
    fn the_public_projection_does_not_select_the_api_key() {
        let conn = schema();

        // Asked of the prepared statement rather than of the SQL text: a
        // substring check is both too weak and too strong here — `has_api_key`
        // contains `api_key`, so naive matching reports a leak that is not one,
        // while an aliased or reordered column could hide a real one.
        let columns = |sql: &str| -> Vec<String> {
            let stmt = conn.prepare(sql).expect("prepare");
            stmt.column_names()
                .into_iter()
                .map(str::to_string)
                .collect()
        };

        let public = columns(PROVIDER_COLUMNS_PUBLIC);
        assert!(
            !public.iter().any(|c| c == "api_key"),
            "the public projection must not select the key: {public:?}"
        );
        assert!(
            public.iter().any(|c| c == "has_api_key"),
            "it must still say whether one is set: {public:?}"
        );

        let secret = columns(PROVIDER_COLUMNS_SECRET);
        assert!(
            secret.iter().any(|c| c == "api_key"),
            "and the secret projection is the one that reads it: {secret:?}"
        );
    }

    /// The summary is what a response is built from, so the key must not be
    /// reachable through it — including through `Debug`, which is how a value
    /// reaches a log line.
    #[test]
    fn a_summary_carries_no_key_in_any_encoding() {
        let db = scratch_db();
        store_provider(
            db.path(),
            &ProviderInput {
                id: "p1",
                name: "prod",
                provider_type: ProviderType::Openai,
                api_key: "sk-do-not-leak",
                base_url: "",
                timeouts: Timeouts::default(),
                enabled: true,
            },
        )
        .expect("store");

        let summaries = load_provider_summaries(db.path()).expect("load");
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].has_api_key);

        let json = serde_json::to_string(&summaries[0]).expect("json");
        let debug = format!("{:?}", summaries[0]);
        for rendered in [json, debug] {
            assert!(
                !rendered.contains("sk-do-not-leak"),
                "a provider key reached a rendered summary: {rendered}"
            );
        }
    }

    /// A stored type this build cannot serve is refused rather than guessed at,
    /// and the refusal does not quote the value.
    #[test]
    fn an_unservable_stored_type_is_refused_without_naming_it() {
        let conn = schema();
        conn.execute(
            "INSERT INTO gateway_providers (id, name, type, created_at, updated_at)\n             VALUES ('p1', 'legacy', 'bedrock', '', '')",
            [],
        )
        .expect("insert");

        // `expect_err` is unavailable here and that is the type doing its job:
        // it needs `Debug` on the `Ok` side, and `ProviderRow` deliberately has
        // none — see the module header, rule 2.
        let err = match load_providers_from(&conn) {
            Err(e) => e,
            Ok(rows) => panic!("bedrock must be refused, got {} row(s)", rows.len()),
        };
        assert!(
            !err.contains("bedrock"),
            "the error must not quote the stored value: {err}"
        );
    }

    #[test]
    fn routing_preserves_target_order_and_the_fallback_chain() {
        let db = scratch_db();
        let alias = ModelAlias {
            id: "a1".into(),
            alias: "claude-sonnet".into(),
            routing: Routing {
                targets: vec![
                    RouteTarget {
                        provider: "primary".into(),
                        model_id: "claude-sonnet-4-6".into(),
                    },
                    RouteTarget {
                        provider: "secondary".into(),
                        model_id: "claude-sonnet-4-5".into(),
                    },
                ],
                fallbacks: vec![RouteTarget {
                    provider: "openai".into(),
                    model_id: "gpt-4o".into(),
                }],
            },
            enabled: true,
        };
        store_alias(db.path(), &alias).expect("store");

        let loaded = load_aliases(db.path()).expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], alias, "order is the meaning; it must survive");
        assert_eq!(loaded[0].routing.targets[0].provider, "primary");
        assert_eq!(loaded[0].routing.targets[1].provider, "secondary");
    }

    /// An empty column is "nothing configured", not corruption — the column is
    /// `NOT NULL DEFAULT ''`, so this is what a row created without routing
    /// reads as.
    #[test]
    fn an_empty_routing_column_is_not_a_decode_failure() {
        assert_eq!(decode_routing("").expect("empty"), Routing::default());
        assert_eq!(
            decode_routing("{}").expect("empty object"),
            Routing::default()
        );
    }

    /// **A decode failure reports a position and nothing else.**
    ///
    /// serde's own message quotes the offending text, and this column sits in a
    /// table whose other secret is an API key — a routing document is a likely
    /// place for someone to paste one by mistake.
    #[test]
    fn a_malformed_routing_document_reports_position_only() {
        let err = decode_routing(r#"{"targets": [{"provider": "sk-leaked-secret"#)
            .expect_err("must fail");
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("column"), "{err}");
        assert!(
            !err.contains("sk-leaked-secret"),
            "the error quoted the document: {err}"
        );
    }

    #[test]
    fn settings_round_trip_and_port_zero_is_refused() {
        let db = scratch_db();
        let settings = GatewaySettings {
            enabled: true,
            port: 9099,
            start_with_app: false,
        };
        store_settings(db.path(), &settings).expect("store");
        assert_eq!(load_settings(db.path()).expect("load"), settings);

        // ...and a second write updates rather than inserting a second row.
        let settings = GatewaySettings {
            port: 9100,
            ..settings
        };
        store_settings(db.path(), &settings).expect("second store");
        assert_eq!(load_settings(db.path()).expect("load"), settings);

        let conn = Connection::open(db.path()).expect("open");
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM gateway_settings", [], |r| r.get(0))
            .expect("count");
        assert_eq!(rows, 1, "the settings table holds exactly one row");

        // Port 0 is refused by the write, not merely by `validate` in isolation
        // — the check has to be wired in, which is the half a unit test of
        // `validate` alone would miss.
        let zero = GatewaySettings {
            port: 0,
            ..Default::default()
        };
        assert!(zero.validate().is_err());
        assert!(store_settings(db.path(), &zero).is_err());
        assert_eq!(
            load_settings(db.path()).expect("reload"),
            settings,
            "a refused write must not have changed the stored row"
        );
    }

    /// The defaults handed to a registry are ferrox's own, unmodified: v1 sets
    /// no retry or circuit-breaker policy here.
    #[test]
    fn the_ferrox_defaults_are_the_crates_own() {
        let d = defaults();
        assert_eq!(
            d.timeouts.connect_secs,
            TimeoutsConfig::default().connect_secs
        );
        assert_eq!(d.timeouts.ttfb_secs, TimeoutsConfig::default().ttfb_secs);
        assert_eq!(d.timeouts.idle_secs, TimeoutsConfig::default().idle_secs);
    }

    /// A temp-file database with the schema applied.
    ///
    /// A **file**, not `:memory:`, and that is the point: the public write
    /// functions take a `&Path` and open their own connection, so a test that
    /// wants to exercise the real statements has to give them a real path. The
    /// tests used to insert through hand-written copies of the production SQL
    /// instead, which verified the copy rather than the code — the upsert's
    /// `ON CONFLICT` clause, the part most likely to be wrong, was executed by
    /// nothing.
    fn scratch_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        file
    }
}

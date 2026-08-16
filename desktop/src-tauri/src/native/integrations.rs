//! The integration reads, plus the writes that have no effect outside the
//! database: `GET /api/integrations`, `GET /api/integrations/{id}`,
//! `GET /api/integrations/available-tools`,
//! `GET|POST /api/integrations/{id}/triggers`,
//! `PUT|DELETE /api/integrations/{id}/triggers/{rid}` and
//! `POST /api/integrations`.
//!
//! Mirrors `handleListIntegrations`, `handleGetIntegration`,
//! `handleAvailableTools`, `handleCreateIntegration`
//! (`internal/api/integrations.go`) and the trigger-rule handlers
//! (`internal/api/trigger_rules.go`) over `SQLiteIntegrationStore` and
//! `SQLiteTriggerStore`.
//!
//! ## Which writes moved, and the rule that decided it (#277)
//!
//! A route moves only when Rust reproduces **every** effect it has, and the
//! integration writes split cleanly on that test:
//!
//! - `PUT /api/integrations/{id}` calls `registry.Reload(id)` and `DELETE`
//!   calls `registry.Stop(id)` — they restart the **live in-process MCP
//!   server**. Rust has none to restart until #282, so both stay with Go. A
//!   native write there would persist the row and leave the running integration
//!   on stale config: an integration still using a token the user just revoked,
//!   with a 200 saying it worked.
//! - `POST /api/integrations` touches no registry at all. That was verified
//!   against the whole of `integrationService.Create` rather than inferred from
//!   its siblings, which is the point — `Create` and `Update` look alike and
//!   differ exactly here.
//! - The trigger-rule writes are safe for a different reason: the dispatcher
//!   calls `ListRules` **per inbound message**
//!   (`internal/trigger/dispatcher.go`), so there is no cached rule set for a
//!   native write to leave stale.
//!
//! ## A create is the one native path that handles a secret
//!
//! Everything below the "Secrets" note is about never *reading* credentials.
//! That cannot hold for a create, because the caller supplies them in the
//! request body and they have to reach the column. So `create` carries them as
//! a borrowed `RawValue` from decode to `INSERT`, and the response is still
//! built from [`ScrubbedIntegration`], which has no field to leak them through.
//!
//! **That guarantee is not local to this module.** It also depends on
//! `writes::decode_body` deserializing from the request's *original bytes*
//! rather than from the `serde_json::Value` it shape-checks with — going
//! through the `Value` sorts keys and respells numbers, so the captured
//! `RawValue` would be a re-serialization and Go's verbatim column would
//! differ on every blob with more than one key. `decode_body` has its own test
//! for this; the one here asserts the stored bytes end to end. Both use a
//! multi-key blob deliberately: a single-key one is a fixed point of the broken
//! round trip and proves nothing.
//!
//! ## Secrets
//!
//! `integrations.credentials` and `integrations.auth` hold **plaintext
//! secrets** — OAuth refresh tokens, bot tokens, API keys. `scrubIntegration`
//! drops both from every response, and the port goes one better: the
//! `credentials` column is **never selected**, and `auth` is reduced to a
//! boolean *in SQL* (`authenticated`) so neither value ever exists in this
//! process. A field that is not read cannot be echoed back by a later edit that
//! adds a key to the response.
//!
//! That is also why the response types here are hand-written rather than
//! derived from a row struct: a row struct would carry the secret as a field
//! and rely on `skip_serializing` to keep it off the wire, which is one
//! attribute away from a leak.
//!
//! ## What is not ported, and why
//!
//! - **`GET /{id}/auth/status`** consults `integrationService.oauthFlows`, an
//!   in-memory map of *in-progress* OAuth flows. It answers from the stored
//!   token only when no flow is running, so a native reader would report the
//!   pre-flow answer in exactly the window the endpoint exists for — while the
//!   user is completing a consent screen.
//! - **`GET /{id}/webhook/status`** asks Telegram, over the network, with the
//!   bot token.
//! - **`GET /{id}/whatsapp/*`** is not ported at all: WhatsApp is dropped from
//!   the desktop app by decision (`whatsmeow` has no Rust equivalent), so these
//!   routes must keep answering exactly as the sidecar answers them. As of
//!   #273 the desktop UI no longer calls them, so they are unreachable rather
//!   than merely unported — but they stay unclaimed, because forwarding is what
//!   makes them the sidecar's problem to answer and then to delete.
//!
//! ## A `whatsapp` row is data, not a type this code knows
//!
//! Dropping the integration does **not** mean filtering it out here. Nothing in
//! this module models an integration type: `type` is a `String` read verbatim,
//! and `available_tools` selects on `enabled`/`authenticated` alone. That is
//! deliberate on both counts.
//!
//! - A user who paired WhatsApp under the Go server has a row. Skipping it
//!   would delete history from the list, which is not what "unavailable" means;
//!   the UI renders it from the stored fields and explains itself instead
//!   (`unavailableCopy` in `views/integrations/catalog.ts`).
//! - Filtering it out of `available-tools` would also be a **parity
//!   regression**. Go's handler is type-agnostic, so a suppressed row is a
//!   byte-level divergence on an endpoint whose bar is byte-identical JSON.
//!   Agents whose allowlists name WhatsApp tools keep those entries, and while
//!   the sidecar is bundled those tools still resolve — agent execution is
//!   phase 5 and `cmd/web.go` registers the `whatsapp` starter in the binary
//!   the app ships. They stop resolving when the sidecar goes.
//!
//! ## Go's own response is not order-stable here
//!
//! `AvailableTools` ranges `cfg.Services`, a Go **map**, so two services of one
//! integration come out in either order between requests. This collects into a
//! `BTreeMap` and is therefore reproducible — strictly better, but it matches
//! only one of the orderings Go produces, which is why the live parity suite
//! re-asks. See "Go itself is not always byte-stable" in `desktop/CLAUDE.md`.

/// The GitHub integration's in-process MCP server (#312).
///
/// A submodule of a `.rs` module file rather than a sibling, which is the least
/// disruptive of the two layouts: `native/integrations.rs` stays exactly where
/// it is and keeps its history, and `native/integrations/github/` is where the
/// six ports (#312–#317) collect. The alternative — moving this file to
/// `native/integrations/mod.rs` — is a rename of a 1,400-line file for the same
/// result.
///
/// Nothing here calls it. Hosting an integration's server is the registry's
/// job, which is #311; this module still never reads a credential.
pub mod github;

use std::collections::BTreeMap;
use std::path::Path;

use axum::http::Method;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::gotime::GoTime;
use super::writes::{decode_body, finish, WriteError};

/// One integration as `scrubIntegration` renders it.
///
/// **Field order is alphabetical, not logical.** Go builds this as a
/// `map[string]any` and `encoding/json` sorts map keys, so the handler's source
/// order is not the wire order. Declaring the fields already sorted states that
/// once, here, rather than leaving it to a container's iteration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScrubbedIntegration {
    /// `IsAuthenticated()`: a stored `auth` that is present, non-empty and not
    /// the four bytes `null`. Computed in SQL so the token itself never reaches
    /// this process.
    pub authenticated: bool,
    pub created_at: GoTime,
    pub enabled: bool,
    pub id: String,
    pub name: String,
    /// A nil Go map is `null` and an empty one is `{}`, and the stored column
    /// decides which — so this is an `Option`, and the inner map is a
    /// `BTreeMap` because Go marshals map keys sorted.
    pub services: Option<BTreeMap<String, ServiceConfig>>,
    #[serde(rename = "type")]
    pub integration_type: String,
    pub updated_at: GoTime,
}

/// One enabled service of an integration. Mirrors `config.ServiceConfig`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfig {
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub enabled: bool,
    /// A nil slice is `null` and an empty one is `[]`; the stored value decides.
    /// A `null` **element** is `""`, which is Go's answer too (#295).
    ///
    /// **No `#[serde(default)]`, deliberately.** It carried one until #295 made
    /// it redundant — `Option<GoList<_>>` gets `None` from `missing_field`
    /// without it — and its only remaining effect was the derive's `visit_seq`
    /// arm, which admits an array with as many elements as the struct has
    /// defaulted fields. That is what made `{"services":{"s":[]}}` a 201 where
    /// Go answers 400. See [`super::gojson::GoList`].
    pub tools: Option<super::gojson::GoList<String>>,
}

/// One tool an enabled, authenticated integration exposes. Mirrors
/// `service.AvailableTool`, which is a struct — so this order *is* declaration
/// order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvailableTool {
    pub integration_id: String,
    pub integration_name: String,
    pub tool_name: String,
    /// `mcp__<integration id>__<tool>`.
    pub qualified_name: String,
    pub service: String,
}

/// One inbound-message rule. Mirrors `config.TriggerRule`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriggerRule {
    pub id: String,
    pub integration_id: String,
    pub name: String,
    pub agent_slug: String,
    pub enabled: bool,
    pub filter_prefix: String,
    pub filter_keywords: Option<Vec<String>>,
    pub filter_chat_ids: Option<Vec<String>>,
    pub created_at: GoTime,
    pub updated_at: GoTime,
}

/// The projection every integration read shares.
///
/// `credentials` is absent on purpose — see this module's header — and `auth`
/// is collapsed to a boolean before it leaves SQLite. `ORDER BY name ASC` is
/// the store's, and it has no tiebreak in Go either.
const INTEGRATION_COLUMNS: &str = "SELECT id, name, type, enabled,
            (auth IS NOT NULL AND auth != '' AND auth != 'null') AS authenticated,
            services, created_at, updated_at
     FROM integrations";

fn scan_integration(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScrubbedIntegration> {
    let enabled: i64 = row.get(3)?;
    let authenticated: i64 = row.get(4)?;
    let services: String = row.get(5)?;
    let created_at: String = row.get(6)?;
    let updated_at: String = row.get(7)?;
    Ok(ScrubbedIntegration {
        authenticated: authenticated != 0,
        created_at: super::gotime::from_sql_text(&created_at, 6)?,
        enabled: enabled != 0,
        id: row.get(0)?,
        name: row.get(1)?,
        services: decode_services(&services),
        integration_type: row.get(2)?,
        updated_at: super::gotime::from_sql_text(&updated_at, 7)?,
    })
}

/// `json.Unmarshal` into a `map[string]ServiceConfig`: a stored `null` leaves
/// the map nil, and so does anything unparseable — Go's store would fail the
/// whole request on the latter, but it stores what it wrote and what it wrote
/// is always an object.
fn decode_services(raw: &str) -> Option<BTreeMap<String, ServiceConfig>> {
    if raw.trim().is_empty() {
        return None;
    }
    match serde_json::from_str::<Option<BTreeMap<String, ServiceConfig>>>(raw) {
        Ok(services) => services,
        Err(e) => {
            log::warn!("native integrations: malformed services column: {e}");
            None
        }
    }
}

/// Every integration, ordered by name. `make(..., 0, len)` on the Go side, so
/// an install with none answers `[]` rather than `null`.
pub fn list(db_path: &Path) -> Result<Vec<ScrubbedIntegration>, String> {
    let conn = super::db::open_read_only(db_path)?;
    let sql = format!("{INTEGRATION_COLUMNS}\n     ORDER BY name ASC");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("preparing integration list: {e}"))?;
    let rows = stmt
        .query_map([], scan_integration)
        .map_err(|e| format!("listing integrations: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("scanning integration: {e}"))?);
    }
    Ok(out)
}

/// One integration, or `None` when no row has that id.
pub fn get(db_path: &Path, id: &str) -> Result<Option<ScrubbedIntegration>, String> {
    let conn = super::db::open_read_only(db_path)?;
    let sql = format!("{INTEGRATION_COLUMNS}\n     WHERE id = ?1");
    conn.query_row(&sql, [id], scan_integration)
        .optional()
        .map_err(|e| format!("getting integration {id:?}: {e}"))
}

/// `AvailableTools`: every tool of every **enabled and authenticated**
/// integration's **enabled** services.
///
/// Note what the filter is not: it is the integration's own two flags, not the
/// running MCP server's state, so this is a pure read of the same rows the list
/// serves. An integration that is enabled but unauthenticated contributes
/// nothing, which is why a freshly created one shows no tools.
pub fn available_tools(db_path: &Path) -> Result<Vec<AvailableTool>, String> {
    let mut tools = Vec::new();
    for cfg in list(db_path)? {
        if !cfg.enabled || !cfg.authenticated {
            continue;
        }
        let Some(services) = &cfg.services else {
            continue;
        };
        for (service_name, service) in services {
            if !service.enabled {
                continue;
            }
            for tool_name in service.tools.iter().flat_map(|tools| tools.iter()) {
                tools.push(AvailableTool {
                    integration_id: cfg.id.clone(),
                    integration_name: cfg.name.clone(),
                    tool_name: tool_name.clone(),
                    qualified_name: format!("mcp__{}__{}", cfg.id, tool_name),
                    service: service_name.clone(),
                });
            }
        }
    }
    Ok(tools)
}

/// `ListRules`: one integration's trigger rules, oldest first.
///
/// No existence check on the integration, matching Go: an unknown id is an
/// empty list rather than a 404.
pub fn list_trigger_rules(
    db_path: &Path,
    integration_id: &str,
) -> Result<Vec<TriggerRule>, String> {
    let conn = super::db::open_read_only(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, integration_id, name, agent_slug, enabled,
                    filter_prefix, filter_keywords, filter_chat_ids,
                    created_at, updated_at
             FROM trigger_rules
             WHERE integration_id = ?1
             ORDER BY created_at ASC",
        )
        .map_err(|e| format!("preparing trigger rule list: {e}"))?;
    let rows = stmt
        .query_map([integration_id], scan_trigger_rule)
        .map_err(|e| format!("listing trigger rules: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("scanning trigger rule: {e}"))?);
    }
    Ok(out)
}

/// The projection both rule reads share, so the by-id lookup the write path
/// needs cannot drift from the list.
const TRIGGER_RULE_COLUMNS: &str = "SELECT id, integration_id, name, agent_slug, enabled,
                    filter_prefix, filter_keywords, filter_chat_ids,
                    created_at, updated_at
             FROM trigger_rules";

fn scan_trigger_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<TriggerRule> {
    let enabled: i64 = row.get(4)?;
    let keywords: String = row.get(6)?;
    let chat_ids: String = row.get(7)?;
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;
    Ok(TriggerRule {
        id: row.get(0)?,
        integration_id: row.get(1)?,
        name: row.get(2)?,
        agent_slug: row.get(3)?,
        enabled: enabled != 0,
        filter_prefix: row.get(5)?,
        filter_keywords: super::gojson::decode_string_list(&keywords),
        filter_chat_ids: super::gojson::decode_string_list(&chat_ids),
        created_at: super::gotime::from_sql_text(&created_at, 8)?,
        updated_at: super::gotime::from_sql_text(&updated_at, 9)?,
    })
}

/// One rule by its own id, regardless of which integration owns it — the
/// ownership comparison is the caller's, because Go's is what produces the 403.
fn trigger_rule_by_id(
    conn: &rusqlite::Connection,
    rule_id: &str,
) -> rusqlite::Result<Option<TriggerRule>> {
    conn.query_row(
        &format!("{TRIGGER_RULE_COLUMNS} WHERE id = ?1"),
        [rule_id],
        scan_trigger_rule,
    )
    .optional()
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "integrations",
    claims,
    serve,
};

enum Route<'a> {
    List,
    AvailableTools,
    Get(&'a str),
    Triggers(&'a str),
    /// `{id}/triggers/{rid}` — the rule routes that take a rule id.
    Trigger(&'a str, &'a str),
}

/// Match the reads, plus the write routes that share their paths.
///
/// `available-tools` is checked before the `{id}` arm because it is a single
/// segment in the same position, exactly as chi's own route table orders them.
/// Every other `{id}` sibling — `auth/status`, `webhook/status`, the WhatsApp
/// tree — has a further segment and so cannot be reached here.
fn route_of(path: &str) -> Option<Route<'_>> {
    if path == "/api/integrations" {
        return Some(Route::List);
    }
    let rest = path.strip_prefix("/api/integrations/")?;
    if rest == "available-tools" {
        return Some(Route::AvailableTools);
    }
    if let Some(id) = rest.strip_suffix("/triggers") {
        return segment(id).map(Route::Triggers);
    }
    if let Some((id, tail)) = rest.split_once("/triggers/") {
        return match (segment(id), segment(tail)) {
            (Some(id), Some(rid)) => Some(Route::Trigger(id, rid)),
            _ => None,
        };
    }
    segment(rest).map(Route::Get)
}

fn segment(value: &str) -> Option<&str> {
    if value.is_empty() || value.contains('/') {
        return None;
    }
    Some(value)
}

/// Which of this module's routes are native, by method.
///
/// **The integration `{id}` writes are deliberately absent.** `PUT` calls
/// `registry.Reload(id)` and `DELETE` calls `registry.Stop(id)`
/// (`internal/service/integration_service.go`), which start and stop the *live
/// in-process MCP server* for that integration. Rust has no MCP server to
/// reload until #282, so a native write would persist the row and leave the
/// running integration on stale config — the failure would show up as an
/// integration that keeps using a revoked token, with nothing in the response
/// to suggest it. `POST /api/integrations` is safe because `Create` is a pure
/// row write: it never touches the registry, which was verified against the
/// whole function rather than assumed from its siblings.
///
/// The trigger-rule writes are safe for a different reason: the dispatcher
/// calls `ListRules` **per inbound message** (`internal/trigger/dispatcher.go`),
/// so there is no cached rule set for a native write to leave stale.
fn claims(method: &Method, path: &str) -> bool {
    match route_of(path) {
        Some(Route::List) => method == Method::GET || method == Method::POST,
        Some(Route::AvailableTools) | Some(Route::Get(_)) => method == Method::GET,
        Some(Route::Triggers(_)) => method == Method::GET || method == Method::POST,
        Some(Route::Trigger(..)) => method == Method::PUT || method == Method::DELETE,
        None => false,
    }
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let db = &ctx.db_path;
    if req.method != Method::GET {
        return match route_of(req.path) {
            Some(Route::List) => finish(create(db, req.body)),
            Some(Route::Triggers(id)) => finish(create_trigger_rule(db, id, req.body)),
            Some(Route::Trigger(id, rid)) if req.method == Method::PUT => {
                finish(update_trigger_rule(db, id, rid, req.body))
            }
            Some(Route::Trigger(id, rid)) => finish(delete_trigger_rule(db, id, rid)),
            _ => Err(format!(
                "{} {} is not an integration write",
                req.method, req.path
            )),
        };
    }
    let body = match route_of(req.path) {
        Some(Route::List) => {
            super::gojson::to_vec(&list(db)?).map_err(|e| format!("encoding integrations: {e}"))?
        }

        Some(Route::AvailableTools) => super::gojson::to_vec(&available_tools(db)?)
            .map_err(|e| format!("encoding available tools: {e}"))?,

        // Falling back lets Go answer its own 404, body and status included.
        Some(Route::Get(id)) => match get(db, id)? {
            Some(cfg) => {
                super::gojson::to_vec(&cfg).map_err(|e| format!("encoding integration: {e}"))?
            }
            None => return Err(format!("integration {id:?} not found")),
        },

        Some(Route::Triggers(id)) => super::gojson::to_vec(&list_trigger_rules(db, id)?)
            .map_err(|e| format!("encoding trigger rules: {e}"))?,

        // `claims` never admits a GET here — chi has no such route, so Go 405s
        // and forwarding is what reproduces that.
        Some(Route::Trigger(..)) | None => {
            return Err(format!("{} is not an integration read", req.path))
        }
    };
    Ok(super::Answer::json(body))
}

// ─── Writes ───────────────────────────────────────────────────────────────────

/// `CreateIntegrationRequest`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct CreateIntegrationRequest {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    name: String,
    #[serde(
        rename = "type",
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    integration_type: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    enabled: bool,
    /// `json.RawMessage`, so a literal `null` is four bytes rather than absent —
    /// which changes which validation error the caller gets. `captured_raw` is
    /// what keeps the two distinguishable.
    #[serde(deserialize_with = "super::gojson::captured_raw")]
    credentials: Option<Box<RawValue>>,
    /// A `null` **value** is the zero `ServiceConfig` to Go, not an error
    /// (#295) — the same rule `Capabilities.mcp` carries.
    services: Option<super::gojson::GoMap<ServiceConfig>>,
}

/// `handleCreateIntegration` → `integrationService.Create`.
///
/// Order is load-bearing and is Go's: name, then type, then credentials. A
/// request missing all three reports `name`.
fn create(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<CreateIntegrationRequest>(body)?;

    if req.name.is_empty() {
        return Err(WriteError::validation("name", "name is required"));
    }
    if req.integration_type.is_empty() {
        return Err(WriteError::validation("type", "type is required"));
    }

    let credentials = match super::integration_credentials::validate(
        &req.integration_type,
        req.credentials.as_deref(),
    )? {
        // Jira rewrites what it stores; everyone else stores the caller's bytes.
        Some(normalized) => normalized,
        None => req
            .credentials
            .as_ref()
            .map(|c| c.get().to_string())
            .unwrap_or_default(),
    };

    let id = uuid::Uuid::new_v4().to_string();
    // One timestamp for both columns, as Go takes one `now`.
    let now = super::gotime::now_go_text();
    // `if cfg.Services == nil { cfg.Services = make(...) }` — so an absent
    // `services` is stored and echoed as `{}`, never as `null`.
    let services = req.services.unwrap_or_default();
    let services_json = super::gojson::to_vec_marshal(&services)
        .map_err(|e| WriteError::Fallback(format!("marshaling services: {e}")))?;
    let services_json = String::from_utf8(services_json)
        .map_err(|e| WriteError::Fallback(format!("services json is not utf-8: {e}")))?;

    let conn = open_for_write(db_path)?;
    conn.execute(
        "INSERT INTO integrations (id, name, type, enabled, credentials, auth, services, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name, type = excluded.type, enabled = excluded.enabled,
            credentials = excluded.credentials, auth = excluded.auth,
            services = excluded.services, updated_at = excluded.updated_at",
        rusqlite::params![
            &id,
            &req.name,
            &req.integration_type,
            i64::from(req.enabled),
            &credentials,
            // `authJSON` stays nil on create: nothing is authenticated yet.
            None::<String>,
            &services_json,
            &now,
            &now,
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("saving integration: {e}")))?;

    let created = ScrubbedIntegration {
        authenticated: false,
        created_at: parse_written(&now)?,
        enabled: req.enabled,
        id,
        name: req.name,
        services: Some(services.0),
        integration_type: req.integration_type,
        updated_at: parse_written(&now)?,
    };
    encode_created(&created)
}

/// `CreateTriggerRuleRequest` / `UpdateTriggerRuleRequest` — identical shapes.
#[derive(Default, Deserialize)]
#[serde(default)]
struct TriggerRuleRequest {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    name: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    agent_slug: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    enabled: bool,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    filter_prefix: String,
    /// A `null` element is `""` to Go, not an error (#295).
    filter_keywords: Option<super::gojson::GoList<String>>,
    filter_chat_ids: Option<super::gojson::GoList<String>>,
}

/// `handleCreateTriggerRule` → `triggerService.CreateRule`.
fn create_trigger_rule(
    db_path: &Path,
    integration_id: &str,
    body: &[u8],
) -> Result<super::Answer, WriteError> {
    let req = decode_body::<TriggerRuleRequest>(body)?;

    // `validateTriggerRule` checks agent_slug first, and integration_id second —
    // the latter can only be empty if the path had an empty segment, which
    // `route_of` already refuses, but the order is kept so the reasoning is
    // visible rather than assumed.
    if req.agent_slug.is_empty() {
        return Err(WriteError::validation(
            "agent_slug",
            "agent_slug is required",
        ));
    }

    let conn = open_for_write(db_path)?;
    if !integration_exists(&conn, integration_id)? {
        return Err(WriteError::NotFound {
            resource: "integration".to_string(),
            id: integration_id.to_string(),
        });
    }

    // One `now` for both columns and for the response: Go takes a single
    // `time.Now()` and writes it to both, so two calls here could store a rule
    // whose `created_at` and `updated_at` differ by a nanosecond.
    let now = super::gotime::now_go_text();
    let stamp = parse_written(&now)?;
    let rule = TriggerRule {
        id: uuid::Uuid::new_v4().to_string(),
        integration_id: integration_id.to_string(),
        name: req.name,
        agent_slug: req.agent_slug,
        enabled: req.enabled,
        filter_prefix: req.filter_prefix,
        filter_keywords: req.filter_keywords.map(|list| list.0),
        filter_chat_ids: req.filter_chat_ids.map(|list| list.0),
        created_at: stamp,
        updated_at: stamp,
    };
    insert_rule(&conn, &rule, &now)?;
    encode_created(&rule)
}

/// `handleUpdateTriggerRule`.
///
/// The ownership check comes **before** the body is decoded, so a malformed
/// payload aimed at another integration's rule is a 403 rather than a 400.
fn update_trigger_rule(
    db_path: &Path,
    integration_id: &str,
    rule_id: &str,
    body: &[u8],
) -> Result<super::Answer, WriteError> {
    let conn = open_for_write(db_path)?;
    let existing = load_rule(&conn, rule_id)?;
    if existing.integration_id != integration_id {
        return Err(WriteError::Forbidden(
            "rule does not belong to this integration".to_string(),
        ));
    }

    let req = decode_body::<TriggerRuleRequest>(body)?;
    if req.agent_slug.is_empty() {
        return Err(WriteError::validation(
            "agent_slug",
            "agent_slug is required",
        ));
    }

    // `UpdateRule` keeps the stored id, integration and creation time and
    // replaces everything else — a field the caller omitted is cleared, not kept.
    let now = super::gotime::now_go_text();
    let rule = TriggerRule {
        id: existing.id,
        integration_id: existing.integration_id,
        name: req.name,
        agent_slug: req.agent_slug,
        enabled: req.enabled,
        filter_prefix: req.filter_prefix,
        filter_keywords: req.filter_keywords.map(|list| list.0),
        filter_chat_ids: req.filter_chat_ids.map(|list| list.0),
        created_at: existing.created_at,
        updated_at: parse_written(&now)?,
    };

    conn.execute(
        "UPDATE trigger_rules SET
            name = ?1, agent_slug = ?2, enabled = ?3,
            filter_prefix = ?4, filter_keywords = ?5, filter_chat_ids = ?6,
            updated_at = ?7
         WHERE id = ?8",
        rusqlite::params![
            &rule.name,
            &rule.agent_slug,
            i64::from(rule.enabled),
            &rule.filter_prefix,
            &marshal_list(&rule.filter_keywords)?,
            &marshal_list(&rule.filter_chat_ids)?,
            &now,
            &rule.id,
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("updating trigger rule: {e}")))?;

    let body = super::gojson::to_vec(&rule)
        .map_err(|e| WriteError::Fallback(format!("encoding trigger rule: {e}")))?;
    Ok(super::Answer::json(body))
}

/// `handleDeleteTriggerRule`. 204, and the same ownership check.
fn delete_trigger_rule(
    db_path: &Path,
    integration_id: &str,
    rule_id: &str,
) -> Result<super::Answer, WriteError> {
    let conn = open_for_write(db_path)?;
    let existing = load_rule(&conn, rule_id)?;
    if existing.integration_id != integration_id {
        return Err(WriteError::Forbidden(
            "rule does not belong to this integration".to_string(),
        ));
    }
    conn.execute("DELETE FROM trigger_rules WHERE id = ?1", [rule_id])
        .map_err(|e| WriteError::Fallback(format!("deleting trigger rule: {e}")))?;
    Ok(super::Answer::no_content())
}

fn insert_rule(
    conn: &rusqlite::Connection,
    rule: &TriggerRule,
    now: &str,
) -> Result<(), WriteError> {
    conn.execute(
        "INSERT INTO trigger_rules
            (id, integration_id, name, agent_slug, enabled,
             filter_prefix, filter_keywords, filter_chat_ids, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            &rule.id,
            &rule.integration_id,
            &rule.name,
            &rule.agent_slug,
            i64::from(rule.enabled),
            &rule.filter_prefix,
            &marshal_list(&rule.filter_keywords)?,
            &marshal_list(&rule.filter_chat_ids)?,
            now,
            now,
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("creating trigger rule: {e}")))?;
    Ok(())
}

/// `json.Marshal` of a `[]string`: `null` when nil, `[]` when empty.
fn marshal_list(value: &Option<Vec<String>>) -> Result<String, WriteError> {
    let bytes = super::gojson::to_vec_marshal(value)
        .map_err(|e| WriteError::Fallback(format!("marshaling filter list: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|e| WriteError::Fallback(format!("filter list is not utf-8: {e}")))
}

fn load_rule(conn: &rusqlite::Connection, rule_id: &str) -> Result<TriggerRule, WriteError> {
    trigger_rule_by_id(conn, rule_id)
        .map_err(|e| WriteError::Fallback(format!("looking up trigger rule: {e}")))?
        .ok_or_else(|| WriteError::NotFound {
            resource: "trigger_rule".to_string(),
            id: rule_id.to_string(),
        })
}

fn integration_exists(conn: &rusqlite::Connection, id: &str) -> Result<bool, WriteError> {
    conn.query_row("SELECT 1 FROM integrations WHERE id = ?1", [id], |_| {
        Ok(true)
    })
    .optional()
    .map_err(|e| WriteError::Fallback(format!("looking up integration: {e}")))
    .map(|found| found.unwrap_or(false))
}

/// Re-read a timestamp this process just formatted.
///
/// The stored text is the source of truth for both the column and the response,
/// so parsing it back is what keeps them identical rather than merely equal.
fn parse_written(text: &str) -> Result<GoTime, WriteError> {
    GoTime::parse_go_string(text)
        .map_err(|e| WriteError::Fallback(format!("re-reading written timestamp: {e}")))
}

fn open_for_write(db_path: &Path) -> Result<rusqlite::Connection, WriteError> {
    let conn = super::db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    Ok(conn)
}

fn encode_created<T: Serialize>(value: &T) -> Result<super::Answer, WriteError> {
    let body = super::gojson::to_vec(value)
        .map_err(|e| WriteError::Fallback(format!("encoding created row: {e}")))?;
    Ok(super::Answer::json_status(
        axum::http::StatusCode::CREATED,
        body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    const SCHEMA: &str = "
        CREATE TABLE integrations (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            type        TEXT NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 0,
            credentials TEXT NOT NULL DEFAULT '{}',
            auth        TEXT,
            services    TEXT NOT NULL DEFAULT '{}',
            created_at  DATETIME NOT NULL,
            updated_at  DATETIME NOT NULL
        );
        CREATE TABLE trigger_rules (
            id              TEXT PRIMARY KEY,
            integration_id  TEXT NOT NULL,
            name            TEXT NOT NULL DEFAULT '',
            agent_slug      TEXT NOT NULL,
            enabled         INTEGER NOT NULL DEFAULT 1,
            filter_prefix   TEXT NOT NULL DEFAULT '',
            filter_keywords TEXT NOT NULL DEFAULT '[]',
            filter_chat_ids TEXT NOT NULL DEFAULT '[]',
            created_at      DATETIME NOT NULL,
            updated_at      DATETIME NOT NULL
        );";

    /// The secret is a distinctive string so a leak is unmistakable in any
    /// assertion below.
    const SECRET: &str = "SUPER-SECRET-REFRESH-TOKEN";

    fn fixture() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute_batch(&format!(
            r#"
            INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                      created_at, updated_at)
            VALUES
              ('zulu-int', 'Zulu <work>', 'telegram', 1,
               '{{"bot_token":"{SECRET}"}}', '{{"access_token":"{SECRET}"}}',
               '{{"messaging":{{"enabled":true,"tools":["send_message","read_chat"]}}}}',
               '2026-08-01 10:00:00 +0000 UTC', '2026-08-02 11:00:00 +0000 UTC'),
              -- Authenticated but disabled: no tools.
              ('alpha-int', 'Alpha', 'google', 0,
               '{{"client_secret":"{SECRET}"}}', '{{"refresh_token":"{SECRET}"}}',
               '{{"gmail":{{"enabled":true,"tools":["send_email"]}}}}',
               '2026-08-01 09:00:00 +0000 UTC', '2026-08-01 09:00:00 +0000 UTC'),
              -- Enabled but never authenticated: no tools either.
              ('mike-int', 'Mike', 'github', 1,
               '{{"pat":"{SECRET}"}}', NULL,
               '{{"repos":{{"enabled":true,"tools":["list_repos"]}}}}',
               '2026-08-01 08:00:00 +0000 UTC', '2026-08-01 08:00:00 +0000 UTC'),
              -- The literal four bytes `null` are NOT authentication.
              ('november-int', 'November', 'slack', 1,
               '{{}}', 'null',
               '{{"chat":{{"enabled":true,"tools":["post"]}}}}',
               '2026-08-01 07:00:00 +0000 UTC', '2026-08-01 07:00:00 +0000 UTC');

            INSERT INTO trigger_rules (id, integration_id, name, agent_slug, enabled,
                                       filter_prefix, filter_keywords, filter_chat_ids,
                                       created_at, updated_at)
            VALUES
              ('r2', 'zulu-int', 'second', 'agent-b', 0, '/go', '["b"]', '[]',
               '2026-08-02 10:00:00 +0000 UTC', '2026-08-02 10:00:00 +0000 UTC'),
              ('r1', 'zulu-int', 'first', 'agent-a', 1, '', '["a","b"]', '["123"]',
               '2026-08-01 10:00:00 +0000 UTC', '2026-08-01 10:00:00 +0000 UTC'),
              ('r3', 'alpha-int', 'other', 'agent-c', 1, '', '[]', '[]',
               '2026-08-01 10:00:00 +0000 UTC', '2026-08-01 10:00:00 +0000 UTC');
            "#
        ))
        .expect("rows");
        file
    }

    /// The trap this endpoint exists around: neither secret column may appear
    /// anywhere in any response, in any shape.
    #[test]
    fn no_response_carries_a_credential_or_a_token() {
        let file = fixture();
        let bodies = [
            super::super::gojson::to_vec(&list(file.path()).expect("list")).expect("encode"),
            super::super::gojson::to_vec(&get(file.path(), "zulu-int").expect("get"))
                .expect("encode"),
            super::super::gojson::to_vec(&available_tools(file.path()).expect("tools"))
                .expect("encode"),
            super::super::gojson::to_vec(
                &list_trigger_rules(file.path(), "zulu-int").expect("rules"),
            )
            .expect("encode"),
        ];
        for body in bodies {
            let json = String::from_utf8(body).expect("utf8");
            assert!(!json.contains(SECRET), "a secret reached the wire: {json}");
            assert!(!json.contains("credentials"), "{json}");
            assert!(!json.contains("bot_token"), "{json}");
            assert!(!json.contains(r#""auth""#), "{json}");
        }
    }

    /// `IsAuthenticated` is "present, non-empty, and not the four bytes
    /// `null`". A stored `'null'` is the case a truthiness check gets wrong.
    #[test]
    fn authentication_is_a_stored_token_that_is_not_the_literal_null() {
        let file = fixture();
        let by_id: BTreeMap<String, bool> = list(file.path())
            .expect("list")
            .into_iter()
            .map(|i| (i.id, i.authenticated))
            .collect();
        assert!(by_id["zulu-int"]);
        assert!(by_id["alpha-int"], "disabled but still authenticated");
        assert!(!by_id["mike-int"], "a NULL column is not authentication");
        assert!(
            !by_id["november-int"],
            "the literal string `null` is not authentication"
        );
    }

    #[test]
    fn integrations_are_ordered_by_name() {
        let file = fixture();
        assert_eq!(
            list(file.path())
                .expect("list")
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Mike", "November", "Zulu <work>"]
        );
    }

    /// Both flags gate a tool, and the qualified name is built from the
    /// integration **id**, not its name.
    #[test]
    fn only_enabled_and_authenticated_integrations_expose_tools() {
        let file = fixture();
        let tools = available_tools(file.path()).expect("tools");
        assert_eq!(
            tools
                .iter()
                .map(|t| t.qualified_name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__zulu-int__send_message", "mcp__zulu-int__read_chat"],
            "only the enabled AND authenticated integration contributes"
        );
        assert_eq!(tools[0].integration_name, "Zulu <work>");
        assert_eq!(tools[0].service, "messaging");
        assert_eq!(tools[0].tool_name, "send_message");
    }

    /// A disabled *service* of an otherwise live integration contributes
    /// nothing either.
    #[test]
    fn a_disabled_service_contributes_no_tools() {
        let file = fixture();
        let conn = Connection::open(file.path()).expect("open");
        conn.execute(
            "UPDATE integrations SET services = ?1 WHERE id = 'zulu-int'",
            [r#"{"messaging":{"enabled":false,"tools":["send_message"]}}"#],
        )
        .expect("update");
        assert!(available_tools(file.path()).expect("tools").is_empty());
    }

    #[test]
    fn trigger_rules_are_oldest_first_and_scoped_to_one_integration() {
        let file = fixture();
        let rules = list_trigger_rules(file.path(), "zulu-int").expect("rules");
        assert_eq!(
            rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["r1", "r2"]
        );
        assert_eq!(rules[0].filter_keywords, Some(vec!["a".into(), "b".into()]));
        assert_eq!(rules[0].filter_chat_ids, Some(vec!["123".into()]));
        assert!(rules[0].enabled);
        assert!(!rules[1].enabled);
        assert_eq!(rules[1].filter_prefix, "/go");

        // An unknown integration is an empty list, not a failure.
        assert!(list_trigger_rules(file.path(), "nope")
            .expect("rules")
            .is_empty());
    }

    /// Key order is alphabetical because Go builds the body from a map, and the
    /// `<` is escaped because `writeJSON` uses `json.Encoder`.
    #[test]
    fn the_integration_shape_is_the_map_go_marshals() {
        let file = fixture();
        let body = super::super::gojson::to_vec(&get(file.path(), "zulu-int").expect("get"))
            .expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            concat!(
                r#"{"authenticated":true,"created_at":"2026-08-01T10:00:00Z","enabled":true,"#,
                // `<` and `>` arrive escaped: `writeJSON` uses `json.Encoder`,
                // which HTML-escapes by default.
                r#""id":"zulu-int","name":"Zulu \u003cwork\u003e","services":{"messaging":"#,
                r#"{"enabled":true,"tools":["send_message","read_chat"]}},"type":"telegram","#,
                r#""updated_at":"2026-08-02T11:00:00Z"}"#,
                "\n"
            )
        );
    }

    /// An install with no integrations answers `[]`, because the Go slice is
    /// preallocated with `make`.
    #[test]
    fn empty_collections_are_arrays_not_null() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");

        for body in [
            super::super::gojson::to_vec(&list(file.path()).expect("list")).expect("encode"),
            super::super::gojson::to_vec(&available_tools(file.path()).expect("tools"))
                .expect("encode"),
            super::super::gojson::to_vec(&list_trigger_rules(file.path(), "x").expect("rules"))
                .expect("encode"),
        ] {
            assert_eq!(String::from_utf8(body).expect("utf8"), "[]\n");
        }
    }

    /// A stored `null` services column is `null` on the wire; a stored `{}` is
    /// `{}`. The store writes both.
    #[test]
    fn a_nil_services_map_is_null_and_an_empty_one_is_an_object() {
        assert_eq!(decode_services("null"), None);
        assert_eq!(decode_services(""), None);
        assert_eq!(decode_services("{}"), Some(BTreeMap::new()));
    }

    #[test]
    fn a_missing_integration_is_none_not_an_error() {
        let file = fixture();
        assert!(get(file.path(), "nope").expect("get").is_none());
    }

    /// A row of a type the desktop app has dropped is still ordinary data.
    ///
    /// **A guard, not a proof of #273.** It passes unchanged on the commit
    /// before it: #273 removed WhatsApp from the *UI*, and the readers here
    /// never looked at `type` to begin with. It exists because the tempting
    /// follow-up is to "finish the job" by filtering the type out at this
    /// layer, and both halves of that would be wrong — the list would lose a
    /// row the user really has, and `available-tools` would stop matching Go,
    /// whose handler never looks at `type` either, on an endpoint whose bar is
    /// byte-identical JSON.
    ///
    /// Its own fixture, so the shared one's byte assertions stay put.
    #[test]
    fn a_dropped_integration_type_is_listed_like_any_other() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute_batch(
            r#"
            INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                      created_at, updated_at)
            VALUES
              ('wa-int', 'Paired phone', 'whatsapp', 1,
               '{}', '{"device_id":"abc"}',
               '{"messaging":{"enabled":true,"tools":["send_message","send_media"]}}',
               '2026-08-01 10:00:00 +0000 UTC', '2026-08-02 11:00:00 +0000 UTC');
            "#,
        )
        .expect("seed");
        drop(conn);

        let listed = list(file.path()).expect("list");
        assert_eq!(listed.len(), 1, "a whatsapp row must not be filtered out");
        assert_eq!(listed[0].integration_type, "whatsapp");
        assert!(listed[0].authenticated);

        assert!(get(file.path(), "wa-int").expect("get").is_some());

        // Its tools still reach the allowlist picker, exactly as Go reports
        // them. While the sidecar is bundled they still resolve; they stop
        // when it goes. Either way, not something this reader may paper over.
        let tools = available_tools(file.path()).expect("tools");
        let names: Vec<&str> = tools.iter().map(|t| t.tool_name.as_str()).collect();
        assert_eq!(names, ["send_message", "send_media"]);
    }

    #[test]
    fn the_reads_and_the_portable_writes_are_claimed_and_nothing_else_is() {
        assert!(claims(&Method::GET, "/api/integrations"));
        assert!(claims(&Method::GET, "/api/integrations/available-tools"));
        assert!(claims(&Method::GET, "/api/integrations/abc"));
        assert!(claims(&Method::GET, "/api/integrations/abc/triggers"));

        // The writes that moved (#277).
        assert!(claims(&Method::POST, "/api/integrations"));
        assert!(claims(&Method::POST, "/api/integrations/abc/triggers"));
        assert!(claims(&Method::PUT, "/api/integrations/abc/triggers/r1"));
        assert!(claims(&Method::DELETE, "/api/integrations/abc/triggers/r1"));

        // The integration `{id}` writes did NOT move: `PUT` reloads and
        // `DELETE` stops the live in-process MCP server, which Rust has no way
        // to do until #282. Claiming either would persist the row and leave the
        // running integration on stale config.
        assert!(!claims(&Method::PUT, "/api/integrations/abc"));
        assert!(!claims(&Method::DELETE, "/api/integrations/abc"));

        // A rule id is one segment, and the collection has no PUT/DELETE.
        assert!(!claims(&Method::PUT, "/api/integrations/abc/triggers"));
        assert!(!claims(&Method::POST, "/api/integrations/abc/triggers/r1"));
        assert!(!claims(&Method::PUT, "/api/integrations/abc/triggers/r1/x"));

        // Reads that stay with Go, each for its own reason — see the header.
        assert!(!claims(&Method::GET, "/api/integrations/abc/auth/status"));
        assert!(!claims(
            &Method::GET,
            "/api/integrations/abc/webhook/status"
        ));
        assert!(!claims(&Method::GET, "/api/integrations/abc/whatsapp/qr"));
        assert!(!claims(
            &Method::GET,
            "/api/integrations/abc/whatsapp/status"
        ));
        assert!(!claims(&Method::GET, "/api/integrations/abc/triggers/rid"));
        assert!(!claims(&Method::GET, "/api/integrations/"));
    }

    // ─── Writes ───────────────────────────────────────────────────────────────

    /// A database with the real schema, so the write path runs against the
    /// same tables and the same `schema_migrations` row the app has.
    fn migrated() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        super::super::migrate::apply(&mut conn).expect("migrate");
        file
    }

    fn body_of(answer: &super::super::Answer) -> String {
        String::from_utf8(answer.body.clone().expect("a body")).expect("utf-8")
    }

    fn stored(file: &tempfile::NamedTempFile, sql: &str) -> String {
        Connection::open(file.path())
            .expect("open")
            .query_row(sql, [], |row| row.get::<_, String>(0))
            .expect("query")
    }

    #[test]
    fn creating_an_integration_answers_201_and_stores_the_row() {
        let file = migrated();
        // A **multi-key** blob, out of alphabetical order, with a trailing-zero
        // decimal and interior whitespace. A single-key blob is a fixed point of
        // a `serde_json::Value` round trip, so it would pass even if the bytes
        // were being rebuilt — which is exactly the hole review found here.
        let answer = create(
            file.path(),
            br#"{"name":"Work","type":"telegram","enabled":true,
                 "credentials":{"zebra":"z", "bot_token":"t","rate":1.50}}"#,
        )
        .expect("create should succeed");

        assert_eq!(answer.status, axum::http::StatusCode::CREATED);
        let body = body_of(&answer);
        // Alphabetical, because Go builds the response as a map.
        assert!(
            body.starts_with(r#"{"authenticated":false,"created_at":"#),
            "{body}"
        );
        assert!(body.contains(r#""name":"Work""#), "{body}");
        assert!(body.contains(r#""type":"telegram""#), "{body}");
        // The secret is never echoed.
        assert!(
            !body.contains("bot_token"),
            "credentials must not be in the response: {body}"
        );
        // …but it is stored, verbatim: a create is the one native path that
        // necessarily handles a secret, because the caller supplies it.
        assert_eq!(
            stored(&file, "SELECT credentials FROM integrations"),
            r#"{"zebra":"z", "bot_token":"t","rate":1.50}"#,
            "Go stores the RawMessage verbatim: key order, `1.50` and the space \
             after the first comma all survive"
        );
    }

    /// `if cfg.Services == nil { cfg.Services = make(...) }`, so an omitted
    /// `services` is `{}` in both the column and the response — never `null`.
    #[test]
    fn an_omitted_services_map_becomes_an_empty_object_not_null() {
        let file = migrated();
        let answer = create(
            file.path(),
            br#"{"name":"W","type":"telegram","credentials":{"bot_token":"t"}}"#,
        )
        .expect("create");
        assert!(
            body_of(&answer).contains(r#""services":{}"#),
            "{}",
            body_of(&answer)
        );
        assert_eq!(stored(&file, "SELECT services FROM integrations"), "{}");
    }

    /// The write surface carries the same containers `Capabilities` does, so it
    /// carries the same rule (#295): a `null` service value is the zero
    /// `ServiceConfig` to Go and a `null` tool is `""`, both with no error.
    #[test]
    fn a_null_inside_services_is_a_zero_value_rather_than_a_400() {
        let file = migrated();
        let answer = create(
            file.path(),
            br#"{"name":"W","type":"telegram","credentials":{"bot_token":"t"},
                 "services":{"quiet":null,"loud":{"enabled":true,"tools":[null,"send"]}}}"#,
        )
        .expect("a body Go accepts");
        assert_eq!(answer.status, axum::http::StatusCode::CREATED);

        let stored_services = stored(&file, "SELECT services FROM integrations");
        assert!(
            stored_services.contains(r#""quiet":{"enabled":false,"tools":null}"#),
            "{stored_services}"
        );
        assert!(
            stored_services.contains(r#""tools":["","send"]"#),
            "{stored_services}"
        );

        // A whole services map that is `null` is still nil, not `{}` — the
        // nil-versus-empty distinction is untouched.
        let file = migrated();
        create(
            file.path(),
            br#"{"name":"W","type":"telegram","credentials":{"bot_token":"t"},"services":null}"#,
        )
        .expect("null map");
        assert_eq!(stored(&file, "SELECT services FROM integrations"), "{}");

        // And a wrongly-typed value still fails, which is Go's answer too —
        // including the positional arrays that `#[serde(default)]` on `tools`
        // used to admit. Dropping that attribute (redundant once `tools` is a
        // `GoList`) is what closes them.
        for services in [r#"{"s":1}"#, r#"{"s":[]}"#, r#"{"s":[true]}"#] {
            let file = migrated();
            let body = format!(
                r#"{{"name":"W","type":"telegram","credentials":{{"bot_token":"t"}},"services":{services}}}"#
            );
            let err = create(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(
                err.status(),
                axum::http::StatusCode::BAD_REQUEST,
                "{services}"
            );
        }
    }

    /// The trigger rule's two filter lists, same rule.
    #[test]
    fn a_null_filter_element_is_an_empty_string_rather_than_a_400() {
        let file = migrated();
        seed_integration(&file, "int-1");

        let created = create_trigger_rule(
            file.path(),
            "int-1",
            br#"{"name":"R","agent_slug":"a","filter_keywords":[null,"x"],"filter_chat_ids":[null]}"#,
        )
        .expect("a body Go accepts");
        assert_eq!(created.status, axum::http::StatusCode::CREATED);
        let body = body_of(&created);
        assert!(body.contains(r#""filter_keywords":["","x"]"#), "{body}");
        assert!(body.contains(r#""filter_chat_ids":[""]"#), "{body}");
    }

    /// Jira is the only type that rewrites what it stores.
    #[test]
    fn creating_a_jira_integration_stores_the_renormalised_credentials() {
        let file = migrated();
        create(
            file.path(),
            br#"{"name":"J","type":"jira","credentials":
                 {"email":"e","extra":"dropped","site_url":"https://x.net//","api_token":"t"}}"#,
        )
        .expect("create");
        assert_eq!(
            stored(&file, "SELECT credentials FROM integrations"),
            r#"{"site_url":"https://x.net","email":"e","api_token":"t"}"#,
            "jira trims the trailing slashes, re-marshals in declaration order and drops unknown keys"
        );
    }

    #[test]
    fn name_is_checked_before_type_and_type_before_credentials() {
        let file = migrated();
        let err = create(file.path(), br#"{}"#).expect_err("empty body");
        assert!(
            err.message().contains("name is required"),
            "{}",
            err.message()
        );
        let err = create(file.path(), br#"{"name":"W"}"#).expect_err("no type");
        assert!(
            err.message().contains("type is required"),
            "{}",
            err.message()
        );
        let err =
            create(file.path(), br#"{"name":"W","type":"telegram"}"#).expect_err("no credentials");
        assert!(
            err.message().contains("credentials are empty"),
            "{}",
            err.message()
        );
    }

    fn seed_integration(file: &tempfile::NamedTempFile, id: &str) {
        Connection::open(file.path())
            .expect("open")
            .execute(
                "INSERT INTO integrations (id, name, type, enabled, credentials, services,
                                           created_at, updated_at)
                 VALUES (?1, 'n', 'telegram', 1, '{}', '{}', '2026-01-01 00:00:00 +0000 UTC',
                         '2026-01-01 00:00:00 +0000 UTC')",
                [id],
            )
            .expect("seed");
    }

    #[test]
    fn a_trigger_rule_round_trips_through_create_update_and_delete() {
        let file = migrated();
        seed_integration(&file, "int-1");

        let created = create_trigger_rule(
            file.path(),
            "int-1",
            br#"{"name":"R","agent_slug":"a","enabled":true,"filter_keywords":["x"]}"#,
        )
        .expect("create rule");
        assert_eq!(created.status, axum::http::StatusCode::CREATED);
        let body = body_of(&created);
        assert!(body.contains(r#""agent_slug":"a""#), "{body}");
        assert!(body.contains(r#""filter_keywords":["x"]"#), "{body}");
        // A `TriggerRule` is a struct in Go, so this order is declaration order.
        assert!(body.starts_with(r#"{"id":"#), "{body}");

        let id = stored(&file, "SELECT id FROM trigger_rules");

        // An omitted field is cleared, not preserved — `UpdateRule` replaces.
        let updated = update_trigger_rule(file.path(), "int-1", &id, br#"{"agent_slug":"b"}"#)
            .expect("update rule");
        assert_eq!(updated.status, axum::http::StatusCode::OK);
        let body = body_of(&updated);
        assert!(body.contains(r#""agent_slug":"b""#), "{body}");
        assert!(
            body.contains(r#""name":"""#),
            "an omitted name is cleared: {body}"
        );
        // A nil Go slice is `null`; an omitted keyword list is nil, not `[]`.
        assert!(body.contains(r#""filter_keywords":null"#), "{body}");

        let deleted = delete_trigger_rule(file.path(), "int-1", &id).expect("delete rule");
        assert_eq!(deleted.status, axum::http::StatusCode::NO_CONTENT);
        assert!(deleted.body.is_none(), "a 204 carries no body at all");
    }

    /// The ownership check runs before the body is decoded, so a malformed
    /// payload aimed at another integration's rule is 403 and not 400.
    #[test]
    fn a_rule_owned_by_another_integration_is_403_before_the_body_is_read() {
        let file = migrated();
        seed_integration(&file, "int-1");
        seed_integration(&file, "int-2");
        let created =
            create_trigger_rule(file.path(), "int-1", br#"{"agent_slug":"a"}"#).expect("create");
        assert_eq!(created.status, axum::http::StatusCode::CREATED);
        let id = stored(&file, "SELECT id FROM trigger_rules");

        for body in [&br#"{"agent_slug":"b"}"#[..], b"not json at all"] {
            let err = update_trigger_rule(file.path(), "int-2", &id, body)
                .expect_err("wrong integration");
            assert_eq!(
                err.status(),
                axum::http::StatusCode::FORBIDDEN,
                "body: {body:?}"
            );
            assert_eq!(err.message(), "rule does not belong to this integration");
        }

        let err = delete_trigger_rule(file.path(), "int-2", &id).expect_err("wrong integration");
        assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn a_missing_rule_is_404_and_a_missing_integration_is_too() {
        let file = migrated();
        let err = update_trigger_rule(file.path(), "int-1", "nope", br#"{"agent_slug":"a"}"#)
            .expect_err("missing rule");
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(err.message(), r#"trigger_rule "nope" not found"#);

        let err = create_trigger_rule(file.path(), "ghost", br#"{"agent_slug":"a"}"#)
            .expect_err("missing integration");
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(err.message(), r#"integration "ghost" not found"#);
    }

    #[test]
    fn a_rule_without_an_agent_slug_is_422() {
        let file = migrated();
        seed_integration(&file, "int-1");
        let err = create_trigger_rule(file.path(), "int-1", br#"{"name":"R"}"#)
            .expect_err("no agent_slug");
        assert_eq!(err.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            err.message().contains("agent_slug is required"),
            "{}",
            err.message()
        );
    }
}

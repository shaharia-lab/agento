//! The integration reads and every integration write that is not an OAuth
//! flow, a webhook registration or WhatsApp: `GET /api/integrations`,
//! `GET|PUT|DELETE /api/integrations/{id}`,
//! `GET /api/integrations/available-tools`,
//! `GET|POST /api/integrations/{id}/triggers`,
//! `PUT|DELETE /api/integrations/{id}/triggers/{rid}` and
//! `POST /api/integrations`.
//!
//! Mirrors `handleListIntegrations`, `handleGetIntegration`,
//! `handleAvailableTools`, `handleCreateIntegration`,
//! `handleUpdateIntegration`, `handleDeleteIntegration`
//! (`internal/api/integrations.go`) and the trigger-rule handlers
//! (`internal/api/trigger_rules.go`) over `SQLiteIntegrationStore` and
//! `SQLiteTriggerStore`.
//!
//! ## Which writes moved, and the rule that decided it (#277, #311)
//!
//! A route moves only when Rust reproduces **every** effect it has, and the
//! integration writes split cleanly on that test:
//!
//! - `PUT /api/integrations/{id}` calls `registry.Reload(id)` and `DELETE`
//!   calls `registry.Stop(id)` — they restart the **live in-process MCP
//!   server**. #277 left both with Go because Rust had none to restart, and a
//!   native write would have persisted the row while the sidecar kept serving
//!   the old config: an integration still using a token the user just revoked,
//!   with a 200 saying it worked. #311 did not solve that by adding a second
//!   registry — it moved the *ownership*. The sidecar now runs with
//!   `AGENTO_INTEGRATIONS=off:<types>` and [`registry`] is the only
//!   implementation for the types named there, which is #289's flip applied a
//!   second time — per type rather than per process, because a starter is not
//!   always a pure MCP-server constructor. See `registry::HOSTED_TYPES`.
//! - `POST /api/integrations` needed none of that: `Create` touches no registry
//!   at all. That was verified against the whole of `integrationService.Create`
//!   rather than inferred from its siblings, which is the point — `Create` and
//!   `Update` look alike and differ exactly here.
//! - The trigger-rule writes are safe for a third reason: the dispatcher calls
//!   `ListRules` **per inbound message** (`internal/trigger/dispatcher.go`), so
//!   there is no cached rule set for a native write to leave stale.
//!
//! ## The writes that handle a secret, and the one that does not read one
//!
//! Everything below the "Secrets" note is about never *reading* credentials.
//! That cannot hold for a create, because the caller supplies them in the
//! request body and they have to reach the column. So `create` carries them as
//! a borrowed `RawValue` from decode to `INSERT`, and the response is still
//! built from [`ScrubbedIntegration`], which has no field to leak them through.
//! [`update`] does the same on the way in — and on the way *out* it keeps the
//! rule intact by rewriting `auth` **from itself in SQL**, so the token it
//! preserves is never a value this process holds.
//!
//! Reading a credential is [`registry`]'s job and is deliberately in a different
//! file. It has its own projection, it derives neither `Serialize` nor `Debug`,
//! and nothing that builds a response can reach it.
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
//! - **`GET /{id}/whatsapp/*`** is not ported at all: WhatsApp is dropped from
//!   the desktop app by decision (`whatsmeow` has no Rust equivalent), so these
//!   routes must keep answering exactly as the sidecar answers them. As of
//!   #273 the desktop UI no longer calls them, so they are unreachable rather
//!   than merely unported. They stay unclaimed, so they answer the unrouted
//!   404 and can simply be deleted.
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
/// [`registry`] calls it. Hosting an integration's server is the registry's
/// job, and this module still never reads a credential — [`registry`] does,
/// through a projection of its own that no response type can reach.
pub mod base_url;
pub mod confluence;
pub mod github;
pub mod google;
pub mod jira;
pub mod slack;
pub mod telegram;

/// The Start/Stop/Reload lifecycle, and the one place in the port that reads
/// `integrations.credentials` (#311).
///
/// Kept out of this file deliberately. The "Secrets" note below is a rule about
/// *this* module — the column is never selected here and `auth` is a boolean
/// before it leaves SQLite — and it stays literally true with the secrets
/// projection living next door, where nothing that builds a response can see it.
pub mod oauth;
pub mod registry;
/// `POST /api/integrations/{id}/auth/validate` — five remote validations, each
/// writing its own `auth` payload. Beside `registry` for the same reason: it is
/// the second place in the port that reads `integrations.credentials`, and this
/// module's "Secrets" rule stays literally true with it living next door.
pub mod token_validate;
pub mod webhook;

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

// ─── OAuth (#318) ─────────────────────────────────────────────────────────────

/// `map[string]bool{"authenticated": …}` — one key, so nothing to sort.
#[derive(Serialize)]
struct AuthStatusResponse {
    authenticated: bool,
}

/// `map[string]string{"auth_url": …}`.
#[derive(Serialize)]
struct AuthStartResponse {
    auth_url: String,
}

/// `handleStartOAuth`.
fn start_oauth(db_path: &Path, id: &str) -> Result<super::Answer, WriteError> {
    let auth_url = oauth::flow::start(db_path, id)?;
    let body = super::gojson::to_vec(&AuthStartResponse { auth_url })
        .map_err(|e| WriteError::Fallback(format!("encoding auth start: {e}")))?;
    Ok(super::Answer::json(body))
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
    /// `{id}/auth/start` — begins an OAuth flow and answers its URL (#318).
    AuthStart(&'a str),
    /// `{id}/auth/status` — polls the in-flight flow, or the stored token.
    AuthStatus(&'a str),
    /// `{id}/auth/validate` — validates stored token credentials against the
    /// provider and writes the type-specific `auth` payload (#318).
    AuthValidate(&'a str),
    /// `{id}/webhook/status` — the stored Telegram webhook state (#319).
    WebhookStatus(&'a str),
    /// `{id}/webhook/register` — POST registers with Telegram, DELETE removes.
    WebhookRegister(&'a str),
    /// `{id}/webhook/regenerate-secret` — rotate and re-register.
    WebhookRegenerate(&'a str),
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
    if let Some(id) = rest.strip_suffix("/auth/start") {
        return segment(id).map(Route::AuthStart);
    }
    if let Some(id) = rest.strip_suffix("/auth/status") {
        return segment(id).map(Route::AuthStatus);
    }
    if let Some(id) = rest.strip_suffix("/auth/validate") {
        return segment(id).map(Route::AuthValidate);
    }
    if let Some(id) = rest.strip_suffix("/webhook/status") {
        return segment(id).map(Route::WebhookStatus);
    }
    if let Some(id) = rest.strip_suffix("/webhook/register") {
        return segment(id).map(Route::WebhookRegister);
    }
    if let Some(id) = rest.strip_suffix("/webhook/regenerate-secret") {
        return segment(id).map(Route::WebhookRegenerate);
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
/// **The integration `{id}` writes moved in #311**, and what let them move was
/// not a new implementation but a change of *owner*. `PUT` calls
/// `registry.Reload(id)` and `DELETE` calls `registry.Stop(id)`
/// (`internal/service/integration_service.go`); until the Go half could be
/// switched off, a native write there would have persisted the row and left the
/// sidecar's server running on stale config — on an unauthenticated loopback
/// port, holding a token the user had just revoked. The sidecar now runs with
/// `AGENTO_INTEGRATIONS=off:<the types the shell hosts>` and [`registry`] is the
/// only implementation **for those types**, so both effects are reproduced
/// rather than lost. Note the switch is per type, not per process: a write for
/// a `whatsapp` row is declined rather than applied, because nothing here can
/// start or stop its connection (see [`claims`]'s caller and `writes.rs`). All
/// six of the others are hosted as of #313
/// — `github`, `confluence`, `jira`, `slack`, `telegram`, `google` — and the
/// list to read is `registry::HOSTED_TYPES`, never this comment.
///
/// `POST /api/integrations` needed none of that, because `Create` is a pure row
/// write: it never touches the registry, which was verified against the whole
/// function rather than assumed from its siblings.
///
/// The trigger-rule writes are safe for a different reason again: the dispatcher
/// calls `ListRules` **per inbound message** (`internal/trigger/dispatcher.go`),
/// so there is no cached rule set for a native write to leave stale.
fn claims(method: &Method, path: &str) -> bool {
    match route_of(path) {
        Some(Route::List) => method == Method::GET || method == Method::POST,
        Some(Route::AvailableTools) => method == Method::GET,
        Some(Route::Get(_)) => {
            method == Method::GET || method == Method::PUT || method == Method::DELETE
        }
        Some(Route::Triggers(_)) => method == Method::GET || method == Method::POST,
        Some(Route::Trigger(..)) => method == Method::PUT || method == Method::DELETE,
        Some(Route::AuthStart(_)) => method == Method::POST,
        Some(Route::AuthStatus(_)) => method == Method::GET,
        // #318's remaining half. It calls out to the provider, so
        // `token_validate`'s header states the order the registration routes
        // established: everything fallible before the call, and the one write
        // after it is answered natively because Go answers its failure with the
        // same 400 body as a failed validation.
        Some(Route::AuthValidate(_)) => method == Method::POST,
        // #319. The reason this stayed with Go — "asks Telegram, over the
        // network, with the bot token" — was simply wrong: `GetWebhookStatus`
        // reads three columns off the row and composes a URL from the public
        // URL, and `internal/integrations/telegram` has no status call at all.
        // The network belongs to *registration*, which is a different route.
        Some(Route::WebhookStatus(_)) => method == Method::GET,
        // The three that call Telegram. Every fallible step happens *before*
        // the call — an `Err` after a successful `setWebhook` answers 500 for a
        // registration that landed, inviting a retry that registers it twice
        // under a new secret. See `trigger::registration`.
        Some(Route::WebhookRegister(_)) => method == Method::POST || method == Method::DELETE,
        Some(Route::WebhookRegenerate(_)) => method == Method::POST,
        None => false,
    }
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let db = &ctx.db_path;
    if req.method != Method::GET {
        return match route_of(req.path) {
            Some(Route::List) => finish(create(db, req.body)),
            Some(Route::Get(id)) if req.method == Method::PUT => finish(update(db, id, req.body)),
            Some(Route::Get(id)) if req.method == Method::DELETE => finish(delete(db, id)),
            Some(Route::Triggers(id)) => finish(create_trigger_rule(db, id, req.body)),
            Some(Route::Trigger(id, rid)) if req.method == Method::PUT => {
                finish(update_trigger_rule(db, id, rid, req.body))
            }
            Some(Route::Trigger(id, rid)) => finish(delete_trigger_rule(db, id, rid)),
            Some(Route::AuthStart(id)) => finish(start_oauth(db, id)),
            Some(Route::AuthValidate(id)) => finish(token_validate::serve(db, id)),
            Some(Route::WebhookRegister(id)) if req.method == Method::DELETE => {
                finish(super::trigger::serve_delete(db, id))
            }
            Some(Route::WebhookRegister(id)) => finish(super::trigger::serve_register(db, id)),
            Some(Route::WebhookRegenerate(id)) => finish(super::trigger::serve_regenerate(db, id)),
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

        // `handleGetIntegration` → `httpErr` → `NotFoundError`'s own wording,
        // answered here since #278.
        Some(Route::Get(id)) => match get(db, id)? {
            Some(cfg) => {
                super::gojson::to_vec(&cfg).map_err(|e| format!("encoding integration: {e}"))?
            }
            None => {
                return super::Answer::error(
                    axum::http::StatusCode::NOT_FOUND,
                    &format!("integration {id:?} not found"),
                )
            }
        },

        Some(Route::Triggers(id)) => super::gojson::to_vec(&list_trigger_rules(db, id)?)
            .map_err(|e| format!("encoding trigger rules: {e}"))?,

        Some(Route::AuthStatus(id)) => {
            let authenticated = match oauth::flow::status(db, id) {
                Ok(authenticated) => authenticated,
                // A read, so the seam wants a `Result<_, String>`; `finish`
                // is the write path's shape. `Internal` and the typed errors
                // carry their own status; `Fallback` becomes a plain 500.
                Err(WriteError::Fallback(reason)) => return Err(reason),
                Err(e) => {
                    let body = super::gojson::to_vec(&super::writes::error_body(&e.message()))
                        .map_err(|enc| format!("encoding error body: {enc}"))?;
                    return Ok(super::Answer::json_status(e.status(), body));
                }
            };
            super::gojson::to_vec(&AuthStatusResponse { authenticated })
                .map_err(|e| format!("encoding auth status: {e}"))?
        }

        Some(Route::WebhookStatus(id)) => super::gojson::to_vec(&webhook::status(db, id)?)
            .map_err(|e| format!("encoding webhook status: {e}"))?,

        // `claims` never admits a GET here — there is no such route, so it
        // falls through to the unrouted 404.
        Some(Route::AuthStart(_))
        | Some(Route::AuthValidate(_))
        | Some(Route::WebhookRegister(_))
        | Some(Route::WebhookRegenerate(_))
        | Some(Route::Trigger(..))
        | None => return Err(format!("{} is not an integration read", req.path)),
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
    /// (#295) — the same rule `Capabilities.mcp` carries, and [`GoStruct`] is
    /// the same third rule (#337): `{"services":{"s":[true,null]}}` filled a
    /// `ServiceConfig` positionally where Go answers 400.
    services: Option<super::gojson::GoMap<super::gojson::GoStruct<ServiceConfig>>>,
}

/// Drop the [`super::gojson::GoStruct`] wrappers a decoded `services` map
/// carries.
///
/// The wrapper is a **decode-time** rule and serializes as its inner value, so
/// the stored bytes and the response bytes are the same either way. Peeling it
/// here is what keeps `ScrubbedIntegration` — a response type — free of a
/// request-side concern.
fn unwrap_services(
    services: super::gojson::GoMap<super::gojson::GoStruct<ServiceConfig>>,
) -> std::collections::BTreeMap<String, ServiceConfig> {
    services
        .0
        .into_iter()
        .map(|(name, service)| (name, service.0))
        .collect()
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
    let services = unwrap_services(req.services.unwrap_or_default());
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
        services: Some(services),
        integration_type: req.integration_type,
        updated_at: parse_written(&now)?,
    };
    // `integrationService.Create`'s own line. `name` is user-authored text the
    // access line does not carry, and that is deliberate — see
    // `writes::service_log_convention`.
    log::info!(
        "integration created id={:?} name={:?}",
        created.id,
        created.name
    );
    encode_created(&created)
}

/// `UpdateIntegrationRequest`.
///
/// Field for field identical to [`CreateIntegrationRequest`] today, and a
/// separate type all the same — Go keeps the two apart "to allow future
/// divergence (e.g. the integration type is immutable after creation)", and
/// collapsing them here would be the port deciding that for it.
#[derive(Default, Deserialize)]
#[serde(default)]
struct UpdateIntegrationRequest {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    name: String,
    #[serde(
        rename = "type",
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    integration_type: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    enabled: bool,
    /// Carried as raw bytes from decode to `UPDATE`, never through a
    /// `serde_json::Value` — which would sort keys and respell numbers, and
    /// Go stores this column verbatim. See `writes::decode_body`.
    #[serde(deserialize_with = "super::gojson::captured_raw")]
    credentials: Option<Box<RawValue>>,
    /// Same two rules as the create side's: `null` value is the zero struct
    /// (#295), and an array where a struct belongs is a 400 (#337).
    services: Option<super::gojson::GoMap<super::gojson::GoStruct<ServiceConfig>>>,
}

/// `handleUpdateIntegration` → `integrationService.Update`, then
/// `registry.Reload(id)`.
///
/// **There is no credential validation on this path.**
/// `validateIntegrationCredentials` is called by `Create` and by nothing else,
/// so a `PUT` carrying `{}`, garbage, or no `credentials` key at all is a 200 —
/// as are an empty `name` and an empty `type`. Adding the create-side checks
/// here would refuse requests Go applies.
///
/// Three things the store's upsert does that look like bugs and are the
/// behaviour to reproduce:
///
/// - **`credentials` is overwritten wholesale.** A `PUT` that omits the key
///   stores `""` — it wipes the secret rather than preserving it. The frontend
///   always sends the whole object; a hand-written request does not have to.
/// - **`services` is not defaulted.** `Create` fills a nil map with `make(...)`
///   before saving; `Update` does not, so an omitted `services` is the literal
///   `null` in the column *and* `null` in the response, where a create would
///   have said `{}`.
/// - **`created_at` is not in the upsert's `DO UPDATE SET` list at all**, which
///   is why nothing here writes it. `cfg.CreatedAt = existing.CreatedAt` exists
///   only to fill the response.
fn update(db_path: &Path, id: &str, body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<UpdateIntegrationRequest>(body)?;

    let conn = open_for_write(db_path)?;
    let Some(existing) = existing_for_update(&conn, id)? else {
        return Err(WriteError::NotFound {
            resource: "integration".to_string(),
            id: id.to_string(),
        });
    };
    decline_a_type_go_still_hosts(id, &existing.integration_type)?;
    // **Before the write, not after.** Its input comes out of the database, so
    // unlike the timestamps this process formats itself it can genuinely fail —
    // and a `Fallback` after `conn.execute` would answer 500 for a `PUT` that
    // already landed, inviting a retry that overwrites the credential and
    // reloads a second time. The reference implementation fails before saving
    // here too. See the invariant in `writes.rs`.
    let created_at = parse_stored(&existing.created_at)?;

    let now = super::gotime::now_go_text();
    let credentials = req
        .credentials
        .as_ref()
        .map(|c| c.get().to_string())
        .unwrap_or_default();
    // `json.Marshal(cfg.Services)`: a nil map is `null`, an empty one is `{}`.
    let services_json = super::gojson::to_vec_marshal(&req.services)
        .map_err(|e| WriteError::Fallback(format!("marshaling services: {e}")))?;
    let services_json = String::from_utf8(services_json)
        .map_err(|e| WriteError::Fallback(format!("services json is not utf-8: {e}")))?;

    // `auth` is rewritten **in SQL, from itself**, so the token is preserved
    // without ever being read into this process — the rule the whole module is
    // built on, kept on the one write that touches the column.
    //
    // The `CASE` is not decoration. `Save` writes `authJSON`, which is nil
    // unless `cfg.IsAuthenticated()`, and `cfg.Auth` here is `existing.Auth`:
    // the request body has no `auth` field, so `if !cfg.IsAuthenticated()` is
    // *always* true on this path and the branch always preserves. What the
    // round trip does change is the two non-token spellings — a column holding
    // `''` or the literal four bytes `null` fails `IsAuthenticated`, so the
    // update writes SQL `NULL` in its place.
    conn.execute(
        "UPDATE integrations SET
            name = ?1, type = ?2, enabled = ?3,
            credentials = ?4, services = ?5, updated_at = ?6,
            auth = CASE
                WHEN auth IS NOT NULL AND auth != '' AND auth != 'null' THEN auth
                ELSE NULL
            END
         WHERE id = ?7",
        rusqlite::params![
            &req.name,
            &req.integration_type,
            i64::from(req.enabled),
            &credentials,
            &services_json,
            &now,
            id,
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("saving integration: {e}")))?;
    drop(conn);

    // **After the write, and its failure is swallowed.** Go logs a reload
    // failure and answers 200 regardless: "row written, server dead" is the
    // accepted outcome. It must never become a `Fallback`: that would report
    // failure for a write that already landed.
    registry::reload_blocking(db_path, id);

    let updated = ScrubbedIntegration {
        authenticated: existing.authenticated,
        created_at,
        enabled: req.enabled,
        id: id.to_string(),
        name: req.name,
        services: req.services.map(unwrap_services),
        integration_type: req.integration_type,
        updated_at: parse_written(&now)?,
    };
    let body = super::gojson::to_vec(&updated)
        .map_err(|e| WriteError::Fallback(format!("encoding integration: {e}")))?;
    log::info!("integration updated id={id:?}");
    Ok(super::Answer::json(body))
}

/// `handleDeleteIntegration` → `integrationService.Delete`.
///
/// The ordering is the reverse of the update's and is Go's: the existence check,
/// then `registry.Stop(id)`, then the row delete. So a delete whose SQL fails
/// leaves the server stopped and the row present — which is exactly what Go
/// does, and is why stopping first is safe to reproduce rather than reorder.
fn delete(db_path: &Path, id: &str) -> Result<super::Answer, WriteError> {
    let conn = open_for_write(db_path)?;
    let Some(integration_type) = integration_type_of(&conn, id)? else {
        return Err(WriteError::NotFound {
            resource: "integration".to_string(),
            id: id.to_string(),
        });
    };
    // Before the `Stop` as well as before the row delete: a type the sidecar
    // hosts has to be stopped by the sidecar, which only happens if Go serves
    // the whole request.
    decline_a_type_go_still_hosts(id, &integration_type)?;

    registry::registry().stop(id);

    conn.execute("DELETE FROM integrations WHERE id = ?1", [id])
        .map_err(|e| WriteError::Fallback(format!("deleting integration: {e}")))?;
    log::info!("integration deleted id={id:?}");
    Ok(super::Answer::no_content())
}

/// What a `PUT` needs from the row it is replacing, and nothing else.
///
/// `created_at` because the response carries it and the column keeps it;
/// `authenticated` because the response reports it — computed in SQL, so this
/// stays a read that cannot hold a secret; `type` because it decides whether
/// this process may answer at all. Absent means 404.
struct ExistingIntegration {
    created_at: String,
    authenticated: bool,
    integration_type: String,
}

fn existing_for_update(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<ExistingIntegration>, WriteError> {
    conn.query_row(
        "SELECT created_at,
                (auth IS NOT NULL AND auth != '' AND auth != 'null') AS authenticated,
                type
         FROM integrations WHERE id = ?1",
        [id],
        |row| {
            let authenticated: i64 = row.get(1)?;
            Ok(ExistingIntegration {
                created_at: row.get(0)?,
                authenticated: authenticated != 0,
                integration_type: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(|e| WriteError::Fallback(format!("looking up integration: {e}")))
}

/// Refuse, **before touching the row**, a write whose lifecycle effect belongs
/// to the sidecar.
///
/// `AGENTO_INTEGRATIONS` names the types the shell hosts and Go keeps the rest
/// (#311), so for a slack or whatsapp row it is Go that must fire `Reload` /
/// `Stop` — and Go only does that from its own handler. Serving such a write
/// here would persist the row and leave the sidecar's server on the old
/// credential, which is precisely the stranded-listener bug this issue exists to
/// remove, relocated rather than fixed. WhatsApp is the sharpest case: its
/// "server" is a live whatsmeow connection registered in a package global, so a
/// missed reload strands a socket, not a port.
///
/// A `Fallback` is a 500, which is only honest while nothing has been written
/// yet — which is why both callers check straight after their existence read
/// and before any mutation. The invariant in `writes.rs`.
fn decline_a_type_go_still_hosts(id: &str, integration_type: &str) -> Result<(), WriteError> {
    if registry::hosts_type(integration_type) {
        return Ok(());
    }
    Err(WriteError::Fallback(format!(
        "integration {id:?} is of type {integration_type:?}, whose MCP server the sidecar hosts"
    )))
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
    log::info!(
        "trigger rule created id={:?} integration_id={:?}",
        rule.id,
        rule.integration_id
    );
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
    log::info!("trigger rule updated id={rule_id:?}");
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
    log::info!("trigger rule deleted id={rule_id:?}");
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

/// The type of an existing integration, or `None` for a 404. The `DELETE`'s
/// half of [`existing_for_update`]: it needs the existence check and the type,
/// and nothing else.
fn integration_type_of(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<String>, WriteError> {
    conn.query_row("SELECT type FROM integrations WHERE id = ?1", [id], |row| {
        row.get::<_, String>(0)
    })
    .optional()
    .map_err(|e| WriteError::Fallback(format!("looking up integration: {e}")))
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

/// A timestamp this process is reading back rather than writing — the
/// `created_at` a `PUT` preserves. Same parse as [`parse_written`], different
/// thing to say when it fails.
fn parse_stored(text: &str) -> Result<GoTime, WriteError> {
    GoTime::parse_go_string(text)
        .map_err(|e| WriteError::Fallback(format!("reading stored timestamp: {e}")))
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

        // …and the two that moved in #311, once the sidecar stopped hosting
        // the servers they reload and stop.
        assert!(claims(&Method::PUT, "/api/integrations/abc"));
        assert!(claims(&Method::DELETE, "/api/integrations/abc"));
        // Still only those three methods on that path; a PATCH is unrouted.
        assert!(!claims(&Method::PATCH, "/api/integrations/abc"));
        assert!(!claims(&Method::POST, "/api/integrations/abc"));
        // The collection has no PUT/DELETE.
        assert!(!claims(&Method::PUT, "/api/integrations"));
        assert!(!claims(&Method::DELETE, "/api/integrations"));
        // `available-tools` sits in the `{id}` position and must not be
        // swallowed by it — a DELETE there would otherwise try to delete an
        // integration whose id is `available-tools`.
        assert!(!claims(
            &Method::DELETE,
            "/api/integrations/available-tools"
        ));
        assert!(!claims(&Method::PUT, "/api/integrations/available-tools"));

        // A rule id is one segment, and the collection has no PUT/DELETE.
        assert!(!claims(&Method::PUT, "/api/integrations/abc/triggers"));
        assert!(!claims(&Method::POST, "/api/integrations/abc/triggers/r1"));
        assert!(!claims(&Method::PUT, "/api/integrations/abc/triggers/r1/x"));

        // #318: the whole auth surface is the shell's — the OAuth flow, and
        // since this issue's second half the token validation too.
        assert!(claims(&Method::GET, "/api/integrations/abc/auth/status"));
        assert!(claims(&Method::POST, "/api/integrations/abc/auth/start"));
        assert!(claims(&Method::POST, "/api/integrations/abc/auth/validate"));
        // …and only for their own methods. There is no GET on `validate`.
        assert!(!claims(&Method::POST, "/api/integrations/abc/auth/status"));
        assert!(!claims(&Method::GET, "/api/integrations/abc/auth/start"));
        assert!(!claims(&Method::GET, "/api/integrations/abc/auth/validate"));
        assert!(!claims(
            &Method::DELETE,
            "/api/integrations/abc/auth/validate"
        ));
        // An id is one segment on all three.
        assert!(!claims(&Method::GET, "/api/integrations//auth/status"));
        assert!(!claims(&Method::POST, "/api/integrations/a/b/auth/start"));
        assert!(!claims(&Method::POST, "/api/integrations//auth/validate"));
        assert!(!claims(
            &Method::POST,
            "/api/integrations/a/b/auth/validate"
        ));

        // Reads that stay with Go, each for its own reason — see the header.
        // #319: a plain read of three columns; see `claims`.
        assert!(claims(&Method::GET, "/api/integrations/abc/webhook/status"));
        assert!(!claims(
            &Method::POST,
            "/api/integrations/abc/webhook/status"
        ));
        // The three that call Telegram (#319).
        assert!(claims(
            &Method::POST,
            "/api/integrations/abc/webhook/register"
        ));
        assert!(claims(
            &Method::DELETE,
            "/api/integrations/abc/webhook/register"
        ));
        assert!(claims(
            &Method::POST,
            "/api/integrations/abc/webhook/regenerate-secret"
        ));
        // …each only for the methods chi mounts.
        assert!(!claims(
            &Method::GET,
            "/api/integrations/abc/webhook/register"
        ));
        assert!(!claims(
            &Method::DELETE,
            "/api/integrations/abc/webhook/regenerate-secret"
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
        // `GoList`) is what closes the short ones; `GoStruct` (#337) closes the
        // rest. `ServiceConfig` has one field without a default, so `[true]`
        // was already refused and `[true,null]` — its full length — was the
        // shape that got through and stored a row Go answers 400 to.
        for services in [
            r#"{"s":1}"#,
            r#"{"s":[]}"#,
            r#"{"s":[true]}"#,
            r#"{"s":[true,null]}"#,
            r#"{"s":[true,["send"]]}"#,
            r#"{"s":[true,null,"extra"]}"#,
        ] {
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

    // ─── The `{id}` writes (#311) ─────────────────────────────────────────────
    //
    // The literals below are Go's, captured from a live Go server built from
    // this checkout by
    // `desktop/src-tauri/tests/parity_writes.rs::the_integration_id_write_answers_match_go`,
    // which is where they are re-asked against a running sidecar rather than
    // asserted against a fixture. There is no vectors file for these: unlike
    // `local_tools_vectors.json` and its siblings, nothing here is derived from
    // a Go *function* a Go test could dump — the answers are a whole handler's,
    // so the live suite is the capture.

    /// A row the update path can be driven against, with a token and a
    /// credential distinctive enough that a leak into any assertion is obvious.
    fn seed_full_integration(file: &tempfile::NamedTempFile, id: &str, auth: Option<&str>) {
        Connection::open(file.path())
            .expect("open")
            .execute(
                "INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                           created_at, updated_at)
                 VALUES (?1, 'Original', 'github', 1, ?2, ?3,
                         '{\"repos\":{\"enabled\":true,\"tools\":[\"list_repos\"]}}',
                         '2026-01-01 00:00:00 +0000 UTC', '2026-01-02 00:00:00 +0000 UTC')",
                rusqlite::params![id, format!(r#"{{"pat":"{SECRET}"}}"#), auth],
            )
            .expect("seed");
    }

    #[test]
    fn updating_an_integration_answers_200_and_replaces_the_row() {
        let file = migrated();
        seed_full_integration(&file, "int-1", Some(r#"{"token":"KEEP-ME"}"#));

        let answer = update(
            file.path(),
            "int-1",
            br#"{"name":"Renamed","type":"github","enabled":false,
                 "credentials":{"zebra":"z", "pat":"new","rate":1.50},
                 "services":{"repos":{"enabled":true,"tools":["get_repo"]}}}"#,
        )
        .expect("update should succeed");

        assert_eq!(answer.status, axum::http::StatusCode::OK);
        let body = body_of(&answer);
        // Alphabetical, because Go builds the response from a map.
        assert!(
            body.starts_with(r#"{"authenticated":true,"created_at":"#),
            "{body}"
        );
        assert!(body.contains(r#""name":"Renamed""#), "{body}");
        assert!(body.contains(r#""enabled":false"#), "{body}");
        assert!(
            body.contains(r#""services":{"repos":{"enabled":true,"tools":["get_repo"]}}"#),
            "{body}"
        );
        // `created_at` is preserved from the stored row; `updated_at` is not.
        assert!(
            body.contains(r#""created_at":"2026-01-01T00:00:00Z""#),
            "{body}"
        );
        assert!(!body.contains("2026-01-02T00:00:00Z"), "{body}");
        // No secret, on either the way in or the way out.
        assert!(!body.contains("credentials"), "{body}");
        assert!(!body.contains("pat"), "{body}");
        assert!(!body.contains("KEEP-ME"), "{body}");

        // The credentials column is overwritten verbatim — key order, `1.50`
        // and the space after the first comma all survive.
        assert_eq!(
            stored(&file, "SELECT credentials FROM integrations"),
            r#"{"zebra":"z", "pat":"new","rate":1.50}"#
        );
        // …and the token is untouched, because the update rewrites `auth` from
        // itself in SQL rather than reading it.
        assert_eq!(
            stored(&file, "SELECT auth FROM integrations"),
            r#"{"token":"KEEP-ME"}"#
        );
        // `created_at` is not in the upsert's `DO UPDATE SET` list at all.
        assert_eq!(
            stored(&file, "SELECT created_at FROM integrations"),
            "2026-01-01 00:00:00 +0000 UTC"
        );
    }

    /// The store's upsert overwrites `credentials` wholesale, so a `PUT` that
    /// omits the key **wipes the secret**. Not helpfully preserved: the write
    /// has to be the one Go performs, or the two databases diverge on a column
    /// nothing else can reconcile.
    #[test]
    fn a_put_that_omits_credentials_wipes_them() {
        let file = migrated();
        seed_full_integration(&file, "int-1", Some(r#"{"token":"t"}"#));

        update(file.path(), "int-1", br#"{"name":"N","type":"github"}"#).expect("update");
        assert_eq!(stored(&file, "SELECT credentials FROM integrations"), "");
    }

    /// `Update` does **not** default a nil services map the way `Create` does,
    /// so an omitted `services` is the literal `null` in the column and in the
    /// response — where a create would have said `{}`.
    #[test]
    fn an_omitted_services_map_stays_null_on_an_update() {
        let file = migrated();
        seed_full_integration(&file, "int-1", None);

        let answer =
            update(file.path(), "int-1", br#"{"name":"N","type":"github"}"#).expect("update");
        assert!(
            body_of(&answer).contains(r#""services":null"#),
            "{}",
            body_of(&answer)
        );
        assert_eq!(stored(&file, "SELECT services FROM integrations"), "null");

        // An empty object is still an empty object, which is the distinction a
        // `null`-defaulting port would lose.
        let answer = update(
            file.path(),
            "int-1",
            br#"{"name":"N","type":"github","services":{}}"#,
        )
        .expect("update");
        assert!(
            body_of(&answer).contains(r#""services":{}"#),
            "{}",
            body_of(&answer)
        );
        assert_eq!(stored(&file, "SELECT services FROM integrations"), "{}");
    }

    /// The `auth` round trip's two non-token spellings. `Save` writes `authJSON`,
    /// which is nil unless `IsAuthenticated()` — so a column holding `''` or the
    /// literal four bytes `null` becomes SQL `NULL`, and `authenticated` is
    /// false either way.
    #[test]
    fn an_unauthenticated_row_stays_unauthenticated_and_its_auth_column_becomes_null() {
        for stored_auth in [None, Some("null"), Some("")] {
            let file = migrated();
            seed_full_integration(&file, "int-1", stored_auth);

            let answer =
                update(file.path(), "int-1", br#"{"name":"N","type":"github"}"#).expect("update");
            assert!(
                body_of(&answer).contains(r#""authenticated":false"#),
                "{stored_auth:?}: {}",
                body_of(&answer)
            );
            let after: Option<String> = Connection::open(file.path())
                .expect("open")
                .query_row("SELECT auth FROM integrations", [], |row| row.get(0))
                .expect("query");
            assert_eq!(after, None, "{stored_auth:?}: auth must be SQL NULL");
        }
    }

    /// **No credential validation on update.** `validateIntegrationCredentials`
    /// is `Create`'s and `Create`'s only, so every one of these is a 200 to Go —
    /// including an empty name and an empty type, which a create refuses with a
    /// 422.
    #[test]
    fn an_update_validates_nothing_a_create_would_have_refused() {
        for body in [
            &br#"{}"#[..],
            br#"{"name":"","type":""}"#,
            br#"{"name":"N","type":"github","credentials":{}}"#,
            br#"{"name":"N","type":"github","credentials":"not an object"}"#,
            br#"{"name":"N","type":"jira","credentials":{"site_url":"nonsense"}}"#,
            br#"{"name":"N","type":"github","credentials":null}"#,
        ] {
            let file = migrated();
            seed_full_integration(&file, "int-1", None);
            let answer = update(file.path(), "int-1", body)
                .unwrap_or_else(|e| panic!("{body:?} must be accepted, got {:?}", e.message()));
            assert_eq!(answer.status, axum::http::StatusCode::OK, "{body:?}");
        }
    }

    #[test]
    fn a_malformed_body_is_400_and_a_missing_row_is_404() {
        let file = migrated();
        seed_full_integration(&file, "int-1", None);

        // The decode comes first, so a malformed body aimed at a missing row is
        // a 400 rather than a 404 — the order Go's handler has.
        let err = update(file.path(), "nope", b"not json at all").expect_err("malformed");
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "invalid JSON body");

        let err = update(file.path(), "nope", br#"{"name":"N"}"#).expect_err("missing");
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(err.message(), r#"integration "nope" not found"#);

        let err = delete(file.path(), "nope").expect_err("missing");
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(err.message(), r#"integration "nope" not found"#);
    }

    #[test]
    fn deleting_an_integration_answers_204_with_no_body_and_removes_the_row() {
        let file = migrated();
        seed_full_integration(&file, "int-1", Some(r#"{"token":"t"}"#));

        let answer = delete(file.path(), "int-1").expect("delete");
        assert_eq!(answer.status, axum::http::StatusCode::NO_CONTENT);
        assert!(
            answer.body.is_none(),
            "a 204 carries no body and no Content-Type"
        );
        assert!(get(file.path(), "int-1").expect("get").is_none());
    }

    /// A row whose MCP server this build cannot host must be declined rather
    /// than written, because nothing here can fire the `Reload`/`Stop` it
    /// needs. WhatsApp is the case that makes this more than tidiness:
    /// its "server" is a live whatsmeow connection registered in a package
    /// global that the status, reconnect and QR endpoints read, so a missed
    /// reload strands a socket and a paired client that never connects.
    #[test]
    fn a_write_for_a_type_this_build_cannot_host_is_declined_without_touching_the_row() {
        // `whatsapp` alone since #313 landed google — and it is the case that
        // makes this more than tidiness, per the doc comment above.
        {
            let integration_type = "whatsapp";
            let file = migrated();
            Connection::open(file.path())
                .expect("open")
                .execute(
                    "INSERT INTO integrations (id, name, type, enabled, credentials, auth,
                                               services, created_at, updated_at)
                     VALUES ('int-1', 'Original', ?1, 1, ?2, '{\"token\":\"KEEP-ME\"}', '{}',
                             '2026-01-01 00:00:00 +0000 UTC', '2026-01-02 00:00:00 +0000 UTC')",
                    rusqlite::params![integration_type, format!(r#"{{"pat":"{SECRET}"}}"#)],
                )
                .expect("seed");

            for err in [
                update(
                    file.path(),
                    "int-1",
                    br#"{"name":"Renamed","type":"github","enabled":false}"#,
                )
                .expect_err("must decline"),
                delete(file.path(), "int-1").expect_err("must decline"),
            ] {
                assert!(
                    matches!(err, WriteError::Fallback(_)),
                    "{integration_type}: {err:?}"
                );
            }

            // **Nothing was written**, so the 500 is honest: a partial write
            // behind it would be reported as a failure that half-happened —
            // the invariant in `writes.rs`.
            assert_eq!(stored(&file, "SELECT name FROM integrations"), "Original");
            assert_eq!(
                stored(&file, "SELECT type FROM integrations"),
                integration_type
            );
            assert_eq!(
                stored(&file, "SELECT CAST(enabled AS TEXT) FROM integrations"),
                "1"
            );
            assert_eq!(
                stored(&file, "SELECT credentials FROM integrations"),
                format!(r#"{{"pat":"{SECRET}"}}"#)
            );
            assert_eq!(
                stored(&file, "SELECT updated_at FROM integrations"),
                "2026-01-02 00:00:00 +0000 UTC"
            );
        }

        // …and the one type this process does host is served natively.
        let file = migrated();
        seed_full_integration(&file, "int-1", Some(r#"{"token":"t"}"#));
        assert_eq!(
            update(file.path(), "int-1", br#"{"name":"N","type":"github"}"#)
                .expect("github is ours")
                .status,
            axum::http::StatusCode::OK
        );
        assert_eq!(
            delete(file.path(), "int-1").expect("github is ours").status,
            axum::http::StatusCode::NO_CONTENT
        );
    }

    /// `created_at` comes out of the database, so unlike the timestamps this
    /// process formats itself it can genuinely fail to parse — and its
    /// `Fallback` therefore has to happen **before** the `UPDATE`, or the 500
    /// would be reporting a write that had already landed.
    #[test]
    fn an_unparseable_created_at_fails_before_anything_is_written() {
        let file = migrated();
        Connection::open(file.path())
            .expect("open")
            .execute(
                "INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                           created_at, updated_at)
                 VALUES ('int-1', 'Original', 'github', 1, '{}', NULL, '{}',
                         'not a timestamp', '2026-01-02 00:00:00 +0000 UTC')",
                [],
            )
            .expect("seed");

        let err = update(
            file.path(),
            "int-1",
            br#"{"name":"Renamed","type":"github"}"#,
        )
        .expect_err("an unparseable stored timestamp fails");
        assert!(matches!(err, WriteError::Fallback(_)), "{err:?}");

        assert_eq!(stored(&file, "SELECT name FROM integrations"), "Original");
        assert_eq!(
            stored(&file, "SELECT updated_at FROM integrations"),
            "2026-01-02 00:00:00 +0000 UTC",
            "the row must be untouched: Go re-runs the whole PUT"
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

    /// #335: the integration and trigger-rule writes' own lines.
    ///
    /// `integration created … name=` is the one line here that carries
    /// user-authored text the access line does not, and it is deliberate — a
    /// line that cannot say which integration was created is most of what it is
    /// for. See `writes::service_log_convention`.
    #[test]
    fn the_integration_writes_log_their_entity_and_outcome() {
        crate::native::writes::testlog::install();
        let file = migrated();

        create(
            file.path(),
            br#"{"name":"Logged Bot","type":"telegram","credentials":{"bot_token":"t"}}"#,
        )
        .expect("create");
        let created = crate::native::writes::testlog::matching(r#"name="Logged Bot""#);
        assert_eq!(created.len(), 1, "{created:?}");
        assert!(
            created[0].starts_with("INFO integration created id=\""),
            "{}",
            created[0]
        );

        seed_full_integration(&file, "logged-int", None);
        update(
            file.path(),
            "logged-int",
            br#"{"name":"N","type":"github"}"#,
        )
        .expect("update");
        crate::native::writes::testlog::assert_info_once(r#"integration updated id="logged-int""#);

        delete(file.path(), "logged-int").expect("delete");
        crate::native::writes::testlog::assert_info_once(r#"integration deleted id="logged-int""#);
    }

    /// The trigger-rule half, whose create line carries both ids as Go's does.
    #[test]
    fn the_trigger_rule_writes_log_their_entity_and_outcome() {
        crate::native::writes::testlog::install();
        let file = migrated();
        seed_integration(&file, "rule-int");

        create_trigger_rule(file.path(), "rule-int", br#"{"agent_slug":"a"}"#).expect("create");
        let id = stored(&file, "SELECT id FROM trigger_rules");
        crate::native::writes::testlog::assert_info_once(&format!(
            r#"trigger rule created id="{id}" integration_id="rule-int""#
        ));

        update_trigger_rule(file.path(), "rule-int", &id, br#"{"agent_slug":"b"}"#)
            .expect("update");
        crate::native::writes::testlog::assert_info_once(&format!(
            r#"trigger rule updated id="{id}""#
        ));

        delete_trigger_rule(file.path(), "rule-int", &id).expect("delete");
        crate::native::writes::testlog::assert_info_once(&format!(
            r#"trigger rule deleted id="{id}""#
        ));
    }
}

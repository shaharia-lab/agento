//! The integration reads: `GET /api/integrations`, `GET /api/integrations/{id}`,
//! `GET /api/integrations/available-tools` and
//! `GET /api/integrations/{id}/triggers`.
//!
//! Mirrors `handleListIntegrations`, `handleGetIntegration`,
//! `handleAvailableTools` (`internal/api/integrations.go`) and
//! `handleListTriggerRules` (`internal/api/trigger_rules.go`) over
//! `SQLiteIntegrationStore` and `SQLiteTriggerStore`.
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

use std::collections::BTreeMap;
use std::path::Path;

use axum::http::Method;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::gotime::GoTime;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfig {
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub enabled: bool,
    /// A nil slice is `null` and an empty one is `[]`; the stored value decides.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
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
            for tool_name in service.tools.iter().flatten() {
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
        .query_map([integration_id], |row| {
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
        })
        .map_err(|e| format!("listing trigger rules: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("scanning trigger rule: {e}"))?);
    }
    Ok(out)
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
}

/// Match the four reads and nothing else.
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
    segment(rest).map(Route::Get)
}

fn segment(value: &str) -> Option<&str> {
    if value.is_empty() || value.contains('/') {
        return None;
    }
    Some(value)
}

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && route_of(path).is_some()
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let db = &ctx.db_path;
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

        None => return Err(format!("{} is not an integration read", req.path)),
    };
    Ok(super::Answer::json(body))
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
    fn only_the_four_reads_are_claimed() {
        assert!(claims(&Method::GET, "/api/integrations"));
        assert!(claims(&Method::GET, "/api/integrations/available-tools"));
        assert!(claims(&Method::GET, "/api/integrations/abc"));
        assert!(claims(&Method::GET, "/api/integrations/abc/triggers"));

        // Writes.
        assert!(!claims(&Method::POST, "/api/integrations"));
        assert!(!claims(&Method::PUT, "/api/integrations/abc"));
        assert!(!claims(&Method::DELETE, "/api/integrations/abc"));
        assert!(!claims(&Method::POST, "/api/integrations/abc/triggers"));

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
}

//! `GET /api/agents` and `GET /api/agents/{slug}`.
//!
//! Mirrors `SQLiteAgentStore.List`/`Get` (`internal/storage/sqlite_agent_store.go`),
//! `agentService.List`/`Get` and the two handlers in `internal/api/agents.go`.
//! The service layer only wraps the store in a span, so the shape the handler
//! writes is the store's row.
//!
//! Reads only. Create, update and delete stay with Go until the storage layer
//! moves, which is the right split for now: this is the same database file, and
//! a second writer would race the migrations and seeding the Go server performs
//! on every startup.
//!
//! Two Go-isms decide the bytes here and neither is visible in the struct:
//! **field order is declaration order**, and **a nil slice marshals as `null`**,
//! not `[]`. `capabilities.built_in` is exactly that case — an agent with no
//! built-in tools sends `null`, and a `Vec` defaulting to empty would send `[]`.

use std::collections::BTreeMap;
use std::path::Path;

use axum::http::Method;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::db;

/// One agent. Mirrors `config.AgentConfig`.
#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    pub name: String,
    pub slug: String,
    pub description: String,
    pub model: String,
    /// "adaptive", "disabled" or "enabled".
    pub thinking: String,
    /// "bypass" (default), "default", "plan" or "dontAsk".
    pub permission_mode: String,
    pub system_prompt: String,
    pub capabilities: Capabilities,
    /// Overrides which Claude config dir this agent's runs target — how a work
    /// agent and a personal agent stay live in one instance. Empty means the
    /// global default.
    pub claude_config_dir: String,
}

/// What tools an agent may use. Mirrors `config.AgentCapabilities`.
///
/// Every field is `Option`, because Go distinguishes a nil slice (`null`) from
/// an empty one (`[]`) on the wire and the stored JSON carries whichever the
/// writer produced. Round-tripping the distinction is the only way the bytes
/// match.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub built_in: Option<Vec<String>>,
    pub local: Option<Vec<String>>,
    pub mcp: Option<BTreeMap<String, McpCapability>>,
}

/// Which tools from one MCP server an agent may use.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCapability {
    pub tools: Option<Vec<String>>,
}

const COLUMNS: &str = "SELECT slug, name, description, model, thinking, permission_mode,
       system_prompt, capabilities, claude_config_dir
FROM agents";

/// Every agent, ordered by name as the store orders them.
pub fn list(db_path: &Path) -> Result<Vec<Agent>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{COLUMNS}\nORDER BY name ASC");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("listing agents: {e}"))?;
    let rows = stmt
        .query_map([], scan)
        .map_err(|e| format!("listing agents: {e}"))?;

    let mut agents = Vec::new();
    for row in rows {
        agents.push(row.map_err(|e| format!("listing agents: {e}"))?);
    }
    Ok(agents)
}

/// One agent by slug, or `None` when there is no such row — which the caller
/// turns into the 404 Go returns.
pub fn get(db_path: &Path, slug: &str) -> Result<Option<Agent>, String> {
    let conn = db::open_read_only(db_path)?;
    let sql = format!("{COLUMNS} WHERE slug = ?");
    conn.query_row(&sql, [slug], scan)
        .optional()
        .map_err(|e| format!("getting agent {slug:?}: {e}"))
}

fn scan(row: &rusqlite::Row<'_>) -> rusqlite::Result<Agent> {
    let capabilities: String = row.get(7)?;
    Ok(Agent {
        name: row.get(1)?,
        slug: row.get(0)?,
        description: row.get(2)?,
        model: row.get(3)?,
        thinking: row.get(4)?,
        permission_mode: row.get(5)?,
        system_prompt: row.get(6)?,
        // Go fails the whole request on unparsable capabilities rather than
        // serving an agent whose tool allowlist is unknown. So does this: the
        // error reaches the proxy, which falls back to Go.
        capabilities: serde_json::from_str(&capabilities).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(format!("parsing capabilities: {e}"))),
            )
        })?,
        claude_config_dir: row.get(8)?,
    })
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`. Covers both reads, because the
/// list and the per-agent read share this file and a registry entry is per
/// area, not per path.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "agents",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && (path == "/api/agents" || slug_of(path).is_some())
}

/// The slug in `/api/agents/{slug}`, or `None` for anything else.
///
/// One segment only: `/api/agents/{slug}/duplicate` is a different route with a
/// different method, and a prefix match would swallow it. An empty slug is not
/// a match either — chi routes `/api/agents/` to nothing, and so does this.
fn slug_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/agents/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(rest)
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let body = match slug_of(req.path) {
        None => super::gojson::to_vec(&list(&ctx.db_path)?)
            .map_err(|e| format!("encoding agents: {e}"))?,
        Some(slug) => match get(&ctx.db_path, slug)? {
            Some(agent) => {
                super::gojson::to_vec(&agent).map_err(|e| format!("encoding agent: {e}"))?
            }
            // Falling back lets Go answer the 404, rather than this having to
            // reproduce its body and status.
            None => return Err(format!("agent {slug:?} not found")),
        },
    };
    Ok(super::Answer { body, probe: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::gojson;

    const SCHEMA: &str = "
        CREATE TABLE agents (
            slug            TEXT PRIMARY KEY,
            name            TEXT NOT NULL,
            description     TEXT NOT NULL DEFAULT '',
            model           TEXT NOT NULL DEFAULT 'claude-sonnet-4-6',
            thinking        TEXT NOT NULL DEFAULT 'adaptive',
            permission_mode TEXT NOT NULL DEFAULT 'bypass',
            system_prompt   TEXT NOT NULL DEFAULT '',
            capabilities    TEXT NOT NULL DEFAULT '{}',
            claude_config_dir TEXT NOT NULL DEFAULT '',
            created_at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );";

    fn fixture() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute(
            "INSERT INTO agents (slug, name, description, capabilities, claude_config_dir)
             VALUES ('zeta', 'Zeta', 'last by name', '{\"built_in\":[\"Read\"],\"local\":null,\"mcp\":{\"github\":{\"tools\":[\"list_prs\"]}}}', '/home/u/.claude-work')",
            [],
        )
        .expect("insert");
        conn.execute(
            "INSERT INTO agents (slug, name, capabilities) VALUES ('alpha', 'Alpha', '{}')",
            [],
        )
        .expect("insert");
        file
    }

    #[test]
    fn agents_are_ordered_by_name() {
        let file = fixture();
        let agents = list(file.path()).expect("list");
        assert_eq!(
            agents.iter().map(|a| a.slug.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }

    #[test]
    fn a_missing_agent_is_none_not_an_error() {
        let file = fixture();
        assert!(get(file.path(), "nope").expect("get").is_none());
        assert!(get(file.path(), "zeta").expect("get").is_some());
    }

    /// The nil-versus-empty distinction Go puts on the wire. An agent whose
    /// capabilities were never filled in sends `null` for each list, and one
    /// with a built-in tool sends an array — mixing the two up would be a
    /// silent wire change for every agent in the list.
    #[test]
    fn nil_and_empty_capability_lists_stay_distinct() {
        let file = fixture();
        let agents = list(file.path()).expect("list");

        let alpha = gojson::to_vec(&agents[0]).expect("encode");
        let alpha = String::from_utf8(alpha).expect("utf-8");
        assert!(
            alpha.contains(r#""capabilities":{"built_in":null,"local":null,"mcp":null}"#),
            "{alpha}"
        );

        let zeta = gojson::to_vec(&agents[1]).expect("encode");
        let zeta = String::from_utf8(zeta).expect("utf-8");
        assert!(
            zeta.contains(r#""built_in":["Read"],"local":null"#),
            "{zeta}"
        );
        assert!(
            zeta.contains(r#""mcp":{"github":{"tools":["list_prs"]}}"#),
            "{zeta}"
        );
    }

    #[test]
    fn the_field_order_is_gos_declaration_order() {
        let file = fixture();
        let agents = list(file.path()).expect("list");
        let json = String::from_utf8(gojson::to_vec(&agents[0]).expect("encode")).expect("utf-8");
        assert!(
            json.starts_with(r#"{"name":"Alpha","slug":"alpha","description":""#),
            "{json}"
        );
        assert!(
            json.trim_end().ends_with(r#""claude_config_dir":""}"#),
            "{json}"
        );
    }

    #[test]
    fn unparsable_capabilities_fail_the_read_rather_than_guessing_an_allowlist() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute(
            "INSERT INTO agents (slug, name, capabilities) VALUES ('broken', 'Broken', 'not json')",
            [],
        )
        .expect("insert");

        assert!(list(file.path()).is_err());
    }
}

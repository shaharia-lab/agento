//! The agent CRUD: `GET`, `POST`, `PUT` and `DELETE` on `/api/agents`.
//!
//! Mirrors `SQLiteAgentStore` (`internal/storage/sqlite_agent_store.go`),
//! `agentService` and the handlers in `internal/api/agents.go`. The service
//! layer only wraps the store in a span, so the shape the handler writes is the
//! store's row.
//!
//! Two Go-isms decide the bytes here and neither is visible in the struct:
//! **field order is declaration order**, and **a nil slice marshals as `null`**,
//! not `[]`. `capabilities.built_in` is exactly that case — an agent with no
//! built-in tools sends `null`, and a `Vec` defaulting to empty would send `[]`.
//!
//! ## The writes (#274)
//!
//! Three things about them are easy to get subtly wrong, and all three are
//! observable:
//!
//! - **The validation order is not the same on create and update.** Create
//!   checks the name first and looks the agent up last; update looks it up
//!   *first*, so a `PUT` to a missing agent with an empty name is a 404, not a
//!   422. Reordering for tidiness changes which error the user sees.
//! - **`permission_mode` can never be set through this API.** `AgentRequest`
//!   (`internal/api/types.go`) has no such field, so it is always `""` — which
//!   the service's switch accepts and the store then writes, *overwriting a
//!   value that got there another way*. That is an upstream bug, recorded in
//!   `desktop/CLAUDE.md`; reproducing it is the parity bar, and "fixing" it here
//!   would make the two implementations disagree.
//! - **Deleting a missing agent is a 500, not a 404.** The store returns a plain
//!   error rather than a typed not-found, so the error mapping falls through to
//!   its default arm. Inherited behaviour, reproduced: the case is detected,
//!   nothing is written, and the answer is a 500. It is on the known-bugs list
//!   in `CLAUDE.md`.

use std::path::{Path, PathBuf};

use axum::http::{Method, StatusCode};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::db;
use super::gojson::{GoList, GoMap, GoStruct};
use super::writes::{decode_body, finish, WriteError};

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
///
/// [`GoList`] and [`GoMap`] carry one further rule: a `null` **inside** a list
/// or the map is the zero value (#295). `Option` alone covers
/// `"built_in": null`; it does nothing for `"built_in": [null]`, which Go reads
/// as `[""]` with no error and plain `Vec<String>` rejects — a 400 for a
/// request Go applies. They are types rather than `deserialize_with` functions
/// precisely so this struct needs no `#[serde(default)]`, which would also make
/// it accept `{"capabilities":[]}` — see [`GoList`]'s header.
///
/// [`GoStruct`] is the third rule (#337) and closes what those two left: serde
/// builds a struct from a **full-length** JSON array, positionally, so
/// `{"capabilities":[["Read"],null,null]}` was accepted here and 400 to Go. It
/// wraps the `mcp` *value* rather than this struct, because a field cannot
/// protect itself — `AgentRequest.capabilities` carries the wrapper for this
/// one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub built_in: Option<GoList<String>>,
    pub local: Option<GoList<String>>,
    pub mcp: Option<GoMap<GoStruct<McpCapability>>>,
}

/// Which tools from one MCP server an agent may use.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCapability {
    pub tools: Option<GoList<String>>,
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
        // An unparsable capabilities column fails the whole request rather
        // than serving an agent whose tool allowlist is unknown — a silently
        // empty allowlist is the dangerous answer here, not the loud one.
        //
        // Through [`GoStruct`] (#337) so a stored *array* fails here too. Go's
        // `json.Unmarshal` refuses one and this used to accept a full-length
        // one positionally — reading `[["Read"],null,null]` as a real allowlist
        // where Go fails the request. Neither implementation can write such a
        // column, which is precisely why it has to be a type rather than an
        // observation.
        capabilities: serde_json::from_str::<GoStruct<Capabilities>>(&capabilities)
            .map(|wrapped| wrapped.0)
            .map_err(|e| {
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
    match *method {
        // The collection: listed and created.
        Method::GET => path == "/api/agents" || slug_of(path).is_some(),
        Method::POST => path == "/api/agents",
        // One agent: replaced or removed. `/api/agents/{slug}/duplicate` is a
        // different route and `slug_of` rejects it, so it is unclaimed.
        Method::PUT | Method::DELETE => slug_of(path).is_some(),
        _ => false,
    }
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
    match *req.method {
        Method::GET => serve_read(ctx, req),
        Method::POST => finish(create(&ctx.db_path, req.body)),
        Method::PUT => match slug_of(req.path) {
            Some(slug) => finish(update(&ctx.db_path, slug, req.body)),
            None => Err("PUT /api/agents has no slug".to_string()),
        },
        Method::DELETE => match slug_of(req.path) {
            Some(slug) => finish(delete(&ctx.db_path, slug)),
            None => Err("DELETE /api/agents has no slug".to_string()),
        },
        _ => Err(format!("{} /api/agents is not ported", req.method)),
    }
}

fn serve_read(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let body = match slug_of(req.path) {
        None => super::gojson::to_vec(&list(&ctx.db_path)?)
            .map_err(|e| format!("encoding agents: {e}"))?,
        Some(slug) => match get(&ctx.db_path, slug)? {
            Some(agent) => {
                super::gojson::to_vec(&agent).map_err(|e| format!("encoding agent: {e}"))?
            }
            // `handleGetAgent`'s own 404, body verbatim.
            None => {
                return super::Answer::error(axum::http::StatusCode::NOT_FOUND, "agent not found")
            }
        },
    };
    Ok(super::Answer::json(body))
}

// ─── Writes ───────────────────────────────────────────────────────────────────

/// `AgentRequest` (`internal/api/types.go`).
///
/// Every field defaults, because Go's decoder leaves a missing key at its zero
/// value rather than failing — and because a stored JSON `null` for a scalar is
/// a zero value to Go and a type error to serde.
///
/// **There is deliberately no `permission_mode`.** Adding one here would accept
/// a field the Go API silently drops, so the two would diverge on exactly the
/// request a user would send to work around the upstream bug.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AgentRequest {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    name: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    slug: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    description: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    model: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    thinking: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    system_prompt: String,
    /// [`GoStruct`] so `{"capabilities":[["Read"],null,null]}` is the 400 Go
    /// answers rather than a created agent (#337). The container
    /// `#[serde(default)]` above is what makes the field itself optional; the
    /// wrapper only decides what a *present* value may be shaped like.
    capabilities: Option<GoStruct<Capabilities>>,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    claude_config_dir: String,
}

impl AgentRequest {
    /// The `config.AgentConfig` the handler builds, before the service applies
    /// its defaults. `permission_mode` is `""` because the request cannot carry
    /// one — see the module header.
    fn into_agent(self) -> Agent {
        Agent {
            name: self.name,
            slug: self.slug,
            description: self.description,
            model: self.model,
            thinking: self.thinking,
            permission_mode: String::new(),
            system_prompt: self.system_prompt,
            capabilities: self.capabilities.map(|c| c.0).unwrap_or_default(),
            claude_config_dir: self.claude_config_dir,
        }
    }
}

/// `agentService.Create`, in its order.
fn create(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let mut agent = decode_body::<AgentRequest>(body)?.into_agent();

    if agent.name.is_empty() {
        return Err(WriteError::validation("name", "name is required"));
    }
    if agent.slug.is_empty() {
        agent.slug = to_slug(&agent.name);
    }
    if !is_valid_slug(&agent.slug) {
        return Err(WriteError::validation(
            "slug",
            format!(
                "invalid slug {:?}: use lowercase letters, digits and hyphens",
                agent.slug
            ),
        ));
    }
    apply_defaults(&mut agent);
    normalize_config_dir(&mut agent)?;

    let mut conn = open_for_write(db_path)?;

    // The uniqueness check and the insert have to be one transaction, or two
    // concurrent creates both see "no such slug" and the second overwrites the
    // first through the upsert. Go is not exposed to that only because it holds
    // a single serialized connection; a separate process is.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin agent create: {e}")))?;

    let exists: bool = tx
        .query_row(
            "SELECT 1 FROM agents WHERE slug = ?1",
            [&agent.slug],
            |_| Ok(true),
        )
        .optional()
        .map_err(|e| WriteError::Fallback(format!("checking slug uniqueness: {e}")))?
        .unwrap_or(false);
    if exists {
        return Err(WriteError::Conflict {
            resource: "agent".to_string(),
            id: agent.slug.clone(),
        });
    }

    save(&tx, &agent)?;

    // Encode *before* committing. Everything after the commit must be
    // infallible: a failure there would answer 500 for an agent that was
    // actually created, so the caller retries and creates a second one. A
    // failing `commit` is the one safe exception — it rolls back, so there is
    // nothing to be wrong about.
    let answer = encode(StatusCode::CREATED, &agent)?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit agent create: {e}")))?;
    // `agentService.Create`'s own line, after the store call as Go's is — see
    // `writes::service_log_convention`.
    log::info!("agent created slug={:?}", agent.slug);
    Ok(answer)
}

/// `agentService.Update`. Note it looks the agent up **before** validating the
/// name, which is the opposite of create.
fn update(db_path: &Path, slug: &str, body: &[u8]) -> Result<super::Answer, WriteError> {
    let mut agent = decode_body::<AgentRequest>(body)?.into_agent();
    let mut conn = open_for_write(db_path)?;

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin agent update: {e}")))?;

    let exists: bool = tx
        .query_row("SELECT 1 FROM agents WHERE slug = ?1", [slug], |_| Ok(true))
        .optional()
        .map_err(|e| WriteError::Fallback(format!("looking up agent: {e}")))?
        .unwrap_or(false);
    if !exists {
        return Err(WriteError::NotFound {
            resource: "agent".to_string(),
            id: slug.to_string(),
        });
    }

    if agent.name.is_empty() {
        return Err(WriteError::validation("name", "name is required"));
    }
    // The slug is the stable identifier; a body that carries a different one is
    // ignored rather than rejected.
    agent.slug = slug.to_string();
    apply_defaults(&mut agent);
    normalize_config_dir(&mut agent)?;

    save(&tx, &agent)?;

    // Encoded before the commit — see `create`.
    let answer = encode(StatusCode::OK, &agent)?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit agent update: {e}")))?;
    log::info!("agent updated slug={slug:?}");
    Ok(answer)
}

/// `agentService.Delete`. A missing agent is a 500 — inherited, not a 404.
fn delete(db_path: &Path, slug: &str) -> Result<super::Answer, WriteError> {
    let conn = open_for_write(db_path)?;
    let affected = conn
        .execute("DELETE FROM agents WHERE slug = ?1", [slug])
        .map_err(|e| WriteError::Fallback(format!("deleting agent {slug:?}: {e}")))?;
    if affected == 0 {
        // Nothing was written, so the 500 is honest: it reports a delete that
        // did not happen rather than hiding one that did.
        return Err(WriteError::Fallback(format!("agent {slug:?} not found")));
    }
    log::info!("agent deleted slug={slug:?}");
    Ok(super::Answer::no_content())
}

/// The defaults the service fills in. Shared by create and update because they
/// apply the same three — but note update does *not* re-derive the slug.
fn apply_defaults(agent: &mut Agent) {
    if agent.model.is_empty() {
        agent.model = "claude-sonnet-4-6".to_string();
    }
    if agent.thinking.is_empty() {
        agent.thinking = "adaptive".to_string();
    }
    // `permission_mode` is always "" here, so the service's switch — which
    // accepts "", "bypass" and "default" — can never reject it. Modelled as a
    // fact rather than a branch, so it reads as deliberate.
}

/// `config.ValidateClaudeConfigDir` then `NormalizeClaudeConfigDir`.
///
/// Go validates the *normalized* path and then stores the normalized form, so
/// the order matters: `~/x` is expanded before the absolute-path check, or
/// every tilde path would be rejected.
fn normalize_config_dir(agent: &mut Agent) -> Result<(), WriteError> {
    if agent.claude_config_dir.is_empty() {
        return Ok(());
    }
    let normalized = normalize_claude_config_dir(&agent.claude_config_dir);
    if normalized.is_empty() {
        agent.claude_config_dir = normalized;
        return Ok(());
    }

    let path = PathBuf::from(&normalized);
    if !path.is_absolute() {
        return Err(WriteError::validation(
            "claude_config_dir",
            format!("claude config dir must be an absolute path, got {normalized:?}"),
        ));
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(WriteError::validation(
                "claude_config_dir",
                format!("claude config dir {normalized:?} is not a directory"),
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(WriteError::validation(
                "claude_config_dir",
                format!("claude config dir {normalized:?} does not exist"),
            ))
        }
        // The original wrapped a runtime error whose text is not reproducible,
        // so this answers the same status class with the reason in the log
        // rather than inventing a message the user would see.
        Err(e) => {
            return Err(WriteError::Fallback(format!(
                "claude config dir {normalized:?} is not readable: {e}"
            )))
        }
    }

    agent.claude_config_dir = normalized;
    Ok(())
}

/// `config.NormalizeClaudeConfigDir`: trim, expand a leading `~`, then
/// `filepath.Clean`.
fn normalize_claude_config_dir(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let expanded = if trimmed == "~" || trimmed.starts_with("~/") {
        match crate::paths::home() {
            Some(home) => super::gopath::join(&[
                &home.to_string_lossy(),
                trimmed.strip_prefix('~').unwrap_or(""),
            ]),
            // Go leaves the value alone when the home dir cannot be resolved,
            // and the absolute-path check then rejects it.
            None => trimmed.to_string(),
        }
    } else {
        trimmed.to_string()
    };
    super::gopath::clean(&expanded)
}

/// `SQLiteAgentStore.Save` — one upsert, `created_at` untouched on conflict.
fn save(tx: &rusqlite::Transaction, agent: &Agent) -> Result<(), WriteError> {
    // `validateAgentForSave` rejects an unknown thinking value with a plain
    // error, which is a 500. Detect it *before* writing, so the 500 reports a
    // save that did not happen.
    if !matches!(
        agent.thinking.as_str(),
        "" | "adaptive" | "disabled" | "enabled"
    ) {
        return Err(WriteError::Fallback(format!(
            "invalid thinking value {:?}: must be adaptive, disabled, or enabled",
            agent.thinking
        )));
    }

    let capabilities = super::gojson::to_vec_marshal(&agent.capabilities)
        .map_err(|e| WriteError::Fallback(format!("encoding capabilities: {e}")))?;
    let capabilities = String::from_utf8(capabilities)
        .map_err(|e| WriteError::Fallback(format!("capabilities are not UTF-8: {e}")))?;
    let now = super::gotime::now_go_text();

    tx.execute(
        "INSERT INTO agents (slug, name, description, model, thinking, permission_mode,
                             system_prompt, capabilities, claude_config_dir,
                             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(slug) DO UPDATE SET
             name = excluded.name,
             description = excluded.description,
             model = excluded.model,
             thinking = excluded.thinking,
             permission_mode = excluded.permission_mode,
             system_prompt = excluded.system_prompt,
             capabilities = excluded.capabilities,
             claude_config_dir = excluded.claude_config_dir,
             updated_at = excluded.updated_at",
        rusqlite::params![
            agent.slug,
            agent.name,
            agent.description,
            agent.model,
            agent.thinking,
            agent.permission_mode,
            agent.system_prompt,
            capabilities,
            agent.claude_config_dir,
            now,
            now,
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("saving agent {:?}: {e}", agent.slug)))?;
    Ok(())
}

/// `toSlug` (`internal/service/agent_service.go`), byte for byte.
///
/// Lowercases, keeps `[a-z0-9]`, collapses every other run into a single
/// hyphen, never leads with one, and trims trailing ones. It walks **bytes**,
/// not chars, so a multi-byte character becomes one hyphen rather than one per
/// byte — the `len(result) > 0` guard and the `prevHyphen` flag together are
/// what produce that, and a `chars()` port would differ on any non-ASCII name.
fn to_slug(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut result: Vec<u8> = Vec::new();
    let mut prev_hyphen = false;
    for &c in lower.as_bytes() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            result.push(c);
            prev_hyphen = false;
        } else if !prev_hyphen && !result.is_empty() {
            result.push(b'-');
            prev_hyphen = true;
        }
    }
    while result.last() == Some(&b'-') {
        result.pop();
    }
    String::from_utf8(result).unwrap_or_default()
}

/// `slugRE`: `^[a-z0-9]+(?:-[a-z0-9]+)*$`.
///
/// Written out rather than pulled in as a regex dependency: it is one
/// alternation, and the crate is not otherwise needed.
fn is_valid_slug(slug: &str) -> bool {
    if slug.is_empty() {
        return false;
    }
    let mut segments = slug.split('-');
    segments.all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

fn open_for_write(db_path: &Path) -> Result<rusqlite::Connection, WriteError> {
    let conn = db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    Ok(conn)
}

fn encode(status: StatusCode, agent: &Agent) -> Result<super::Answer, WriteError> {
    let body = super::gojson::to_vec(agent)
        .map_err(|e| WriteError::Fallback(format!("encoding agent: {e}")))?;
    Ok(super::Answer::json_status(status, body))
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

        // A stored `[]` is refused too, and that is not incidental: it is the
        // shape a `#[serde(default)]` on `Capabilities` would have turned into
        // an *empty* allowlist — an agent whose tools are unknown served as an
        // agent with none. Neither implementation can write such a column
        // today, which is exactly why the guard has to be a test rather than an
        // observation.
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute(
            "INSERT INTO agents (slug, name, capabilities) VALUES ('arr', 'Arr', '[]')",
            [],
        )
        .expect("insert");
        assert!(
            list(file.path()).is_err(),
            "a stored [] must not read as an empty allowlist"
        );

        // A *full-length* positional array used to be **accepted** here, by
        // serde's `visit_seq` — the last shape #295 left standing, closed by
        // `GoStruct` in #337. Inverted rather than deleted: this assertion is
        // the boundary between the two changes, and it is the read half of the
        // pair (`the_write_path_refuses_an_array_where_a_struct_belongs` is the
        // write half).
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute(
            r#"INSERT INTO agents (slug, name, capabilities) VALUES ('arr', 'Arr', '[["Read"],null,null]')"#,
            [],
        )
        .expect("insert");
        assert!(
            list(file.path()).is_err(),
            "a stored full-length array must not read as an allowlist (#337)"
        );
    }

    // ─── Writes ───────────────────────────────────────────────────────────────

    /// A database built by the **real** migrations rather than the hand-written
    /// `SCHEMA` above, because the write path checks the recorded schema version
    /// and because a fixture table is exactly where a column default drifts away
    /// from the one production has.
    fn migrated() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        super::super::migrate::apply(&mut conn).expect("migrate");
        file
    }

    fn stored(file: &tempfile::NamedTempFile, slug: &str) -> Option<Agent> {
        get(file.path(), slug).expect("get")
    }

    /// A `null` **inside** a capability list or the MCP map is the zero value,
    /// not a type error (#295). Go answers `[""]` and the zero `MCPCapability`
    /// with no error at all, so rejecting these was a **400** — `decode_body`'s
    /// `InvalidBody`, since the typed decode is what failed — for an agent Go
    /// creates.
    ///
    /// Driven through `create` and read back from the row, because the stored
    /// JSON is what a later read serves and because that is the path the
    /// `Value` shape-check sits in front of.
    #[test]
    fn a_null_inside_capabilities_is_a_zero_value_rather_than_a_400() {
        let file = migrated();
        let body = br#"{"name":"N","slug":"n","capabilities":{
            "built_in":[null,"Read"],
            "local":[null],
            "mcp":{"quiet":null,"github":{"tools":[null]}}
        }}"#;
        let answer = create(file.path(), body).expect("a body Go accepts");
        assert_eq!(answer.status, StatusCode::CREATED);

        let caps = stored(&file, "n").expect("stored").capabilities;
        assert_eq!(
            caps.built_in,
            Some(vec![String::new(), "Read".into()].into())
        );
        assert_eq!(caps.local, Some(vec![String::new()].into()));
        let mcp = caps.mcp.expect("mcp");
        // A null map value is the zero struct — every field at its own zero.
        assert!(mcp["quiet"].tools.is_none());
        assert_eq!(mcp["github"].tools, Some(vec![String::new()].into()));
    }

    /// The nil-versus-empty distinction is untouched by any of it, and a
    /// wrongly-typed element still fails — which is Go's answer too.
    #[test]
    fn nil_empty_and_wrongly_typed_capabilities_are_unmoved() {
        let file = migrated();
        create(
            file.path(),
            br#"{"name":"B","slug":"b","capabilities":{"built_in":null,"local":[]}}"#,
        )
        .expect("nil and empty");
        let caps = stored(&file, "b").expect("stored").capabilities;
        assert!(caps.built_in.is_none());
        assert_eq!(caps.local, Some(Vec::new().into()));
        assert!(caps.mcp.is_none());

        // And the shapes Go refuses are still refused here — including the
        // array a `#[serde(default)]` on `Capabilities` would have accepted.
        for body in [
            &br#"{"name":"C","slug":"c","capabilities":{"built_in":[1]}}"#[..],
            &br#"{"name":"C","slug":"c","capabilities":[]}"#[..],
            &br#"{"name":"C","slug":"c","capabilities":{"mcp":{"s":[]}}}"#[..],
        ] {
            let err = create(file.path(), body).unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{:?}", err);
        }
    }

    /// #337, on the routes it can actually create a row on.
    ///
    /// The accepted set was **not uniform**, which is why every length is
    /// listed rather than one representative: the derive's `visit_seq` errors
    /// only when the array runs out of elements for a field with no default, so
    /// `Capabilities` (three such fields) needed three and `McpCapability` (one)
    /// needed one. Everything shorter was already the 400 Go answers — pinned
    /// above — and everything at or past the length was a **created row Go
    /// refuses**, which is an over-accept and the one direction this port must
    /// not move in.
    #[test]
    fn the_write_path_refuses_an_array_where_a_struct_belongs() {
        let file = migrated();
        for body in [
            // `Capabilities` at exactly its length, and past it.
            &br#"{"name":"C","slug":"c","capabilities":[["Read"],null,null]}"#[..],
            &br#"{"name":"C","slug":"c","capabilities":[["Read"],null,null,"extra"]}"#[..],
            // `McpCapability` as a map value, at its length and past it.
            &br#"{"name":"C","slug":"c","capabilities":{"mcp":{"g":[null]}}}"#[..],
            &br#"{"name":"C","slug":"c","capabilities":{"mcp":{"g":[["x"],"extra"]}}}"#[..],
        ] {
            let err = create(file.path(), body).unwrap_err();
            assert_eq!(
                err.status(),
                StatusCode::BAD_REQUEST,
                "{}",
                String::from_utf8_lossy(body)
            );
            // …and nothing was written. An over-accept is only interesting
            // because of the row it leaves behind.
            assert!(stored(&file, "c").is_none());
        }

        // The update path decodes the same body, so it refuses the same shapes.
        create(file.path(), br#"{"name":"U","slug":"u"}"#).expect("create");
        let err = update(
            file.path(),
            "u",
            br#"{"name":"U","capabilities":[["Read"],null,null]}"#,
        )
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    /// The other half, and the one a blunt fix would have broken: a **genuine**
    /// array is still a genuine array. `built_in`, `local` and `mcp.*.tools` are
    /// lists in Go too, so refusing arrays wholesale would have turned every
    /// real agent into a 400.
    #[test]
    fn genuine_arrays_are_unaffected_by_the_struct_check() {
        let file = migrated();
        let answer = create(
            file.path(),
            br#"{"name":"G","slug":"g","capabilities":{
                "built_in":["Read","Bash"],
                "local":[],
                "mcp":{"github":{"tools":["list_prs"]}}
            }}"#,
        )
        .expect("a body Go accepts");
        assert_eq!(answer.status, StatusCode::CREATED);

        let caps = stored(&file, "g").expect("stored").capabilities;
        assert_eq!(
            caps.built_in,
            Some(vec!["Read".into(), "Bash".into()].into())
        );
        assert_eq!(caps.local, Some(Vec::new().into()));
        assert_eq!(
            caps.mcp.expect("mcp")["github"].tools,
            Some(vec!["list_prs".to_string()].into())
        );
    }

    #[test]
    fn creating_an_agent_answers_201_and_stores_it() {
        let file = migrated();
        let answer = create(
            file.path(),
            br#"{"name":"My Agent","description":"d","system_prompt":"p"}"#,
        )
        .expect("create");

        assert_eq!(answer.status, StatusCode::CREATED);
        let agent = stored(&file, "my-agent").expect("stored");
        assert_eq!(agent.name, "My Agent");
        // The service's defaults, not the column's.
        assert_eq!(agent.model, "claude-sonnet-4-6");
        assert_eq!(agent.thinking, "adaptive");
        // The one the API cannot set. The column default is 'bypass', but the
        // insert supplies "" explicitly, so "" is what lands.
        assert_eq!(agent.permission_mode, "");

        // The response body is the agent, not an envelope.
        let body = String::from_utf8(answer.body.expect("body")).unwrap();
        assert!(
            body.starts_with(r#"{"name":"My Agent","slug":"my-agent""#),
            "{body}"
        );
    }

    /// The upstream bug this port must not "fix": a `permission_mode` in the
    /// request is dropped, because `AgentRequest` has no such field.
    #[test]
    fn a_permission_mode_in_the_request_is_ignored_as_go_ignores_it() {
        let file = migrated();
        create(file.path(), br#"{"name":"Pm","permission_mode":"plan"}"#).expect("create");
        assert_eq!(stored(&file, "pm").expect("stored").permission_mode, "");
    }

    #[test]
    fn a_missing_name_is_422_and_writes_nothing() {
        let file = migrated();
        let err = create(file.path(), br#"{"description":"no name"}"#).unwrap_err();
        assert_eq!(err, WriteError::validation("name", "name is required"));
        assert_eq!(list(file.path()).expect("list").len(), 0);
    }

    #[test]
    fn a_bad_slug_is_422_with_gos_message() {
        let file = migrated();
        let err = create(file.path(), br#"{"name":"X","slug":"Not A Slug"}"#).unwrap_err();
        assert_eq!(
            err.message(),
            "validation error for \"slug\": invalid slug \"Not A Slug\": use lowercase letters, digits and hyphens"
        );
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn a_duplicate_slug_is_409_and_leaves_the_original_alone() {
        let file = migrated();
        create(file.path(), br#"{"name":"First","slug":"dup"}"#).expect("first");
        let err = create(file.path(), br#"{"name":"Second","slug":"dup"}"#).unwrap_err();

        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.message(), "agent with id \"dup\" already exists");
        // The upsert must not have run: a conflict that overwrote would be
        // silent data loss reported as an error.
        assert_eq!(stored(&file, "dup").expect("stored").name, "First");
    }

    #[test]
    fn updating_replaces_the_agent_and_answers_200() {
        let file = migrated();
        create(file.path(), br#"{"name":"Before","slug":"a1"}"#).expect("create");
        let answer = update(
            file.path(),
            "a1",
            br#"{"name":"After","description":"changed","thinking":"disabled"}"#,
        )
        .expect("update");

        assert_eq!(answer.status, StatusCode::OK);
        let agent = stored(&file, "a1").expect("stored");
        assert_eq!(agent.name, "After");
        assert_eq!(agent.description, "changed");
        assert_eq!(agent.thinking, "disabled");
    }

    /// The slug is the identifier: a body carrying a different one is ignored,
    /// not honoured and not rejected. Getting this wrong would silently create
    /// a second agent and orphan the first.
    #[test]
    fn updating_ignores_a_slug_in_the_body() {
        let file = migrated();
        create(file.path(), br#"{"name":"Orig","slug":"keep"}"#).expect("create");
        update(file.path(), "keep", br#"{"name":"Renamed","slug":"other"}"#).expect("update");

        assert!(stored(&file, "other").is_none(), "must not have moved");
        assert_eq!(stored(&file, "keep").expect("stored").name, "Renamed");
        assert_eq!(list(file.path()).expect("list").len(), 1);
    }

    /// Update looks the agent up **before** validating the name, so this is a
    /// 404 rather than the 422 create would give. The orders genuinely differ
    /// in Go and the difference is visible.
    #[test]
    fn updating_a_missing_agent_is_404_even_with_an_invalid_body() {
        let file = migrated();
        let err = update(file.path(), "ghost", br#"{"name":""}"#).unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), "agent \"ghost\" not found");
    }

    #[test]
    fn deleting_answers_204_with_no_body() {
        let file = migrated();
        create(file.path(), br#"{"name":"Bye","slug":"bye"}"#).expect("create");

        let answer = delete(file.path(), "bye").expect("delete");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert!(answer.body.is_none(), "204 carries no body");
        assert!(stored(&file, "bye").is_none());
    }

    /// A missing agent is a 500, inherited. The important half is that nothing
    /// is written first: the status has to report a delete that did not
    /// happen.
    #[test]
    fn deleting_a_missing_agent_forwards_rather_than_inventing_a_404() {
        let file = migrated();
        create(file.path(), br#"{"name":"Keep","slug":"keep"}"#).expect("create");

        let err = delete(file.path(), "ghost").unwrap_err();
        assert!(matches!(err, WriteError::Fallback(_)), "{err:?}");
        assert!(stored(&file, "keep").is_some(), "unrelated rows untouched");
    }

    /// A body that is not JSON is a 400 with Go's fixed message — not the
    /// decoder's error, and not a 422.
    #[test]
    fn a_malformed_body_is_400() {
        let file = migrated();
        for body in [&b""[..], b"{not json", b"[]"] {
            let err = create(file.path(), body).unwrap_err();
            assert_eq!(err, WriteError::InvalidBody, "body {body:?}");
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        }
    }

    /// The write path refuses a database whose schema it does not recognise,
    /// rather than writing through a shape it guessed at.
    #[test]
    fn a_write_against_an_unmigrated_database_forwards() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(SCHEMA).expect("schema");
        drop(conn);

        let err = create(file.path(), br#"{"name":"X"}"#).unwrap_err();
        assert!(matches!(err, WriteError::Fallback(_)), "{err:?}");
    }

    /// `toSlug` walks bytes, so one multi-byte character collapses to a single
    /// hyphen. A `chars()` port agrees on ASCII and differs here.
    #[test]
    fn to_slug_matches_gos_byte_walk() {
        assert_eq!(to_slug("My Agent"), "my-agent");
        assert_eq!(to_slug("  leading"), "leading");
        assert_eq!(to_slug("trailing  "), "trailing");
        assert_eq!(to_slug("Multiple   Spaces"), "multiple-spaces");
        // "ünïcode" as bytes: `ü` is two non-alphanumeric bytes with nothing
        // accumulated yet, so both are dropped rather than becoming a leading
        // hyphen; `ï` is two more, and the pair collapses to a single hyphen
        // because `prevHyphen` suppresses the second. A `chars()` port produces
        // "-n-code" here and agrees with this one on every ASCII name.
        assert_eq!(to_slug("Ünïcode"), "n-code");
        assert_eq!(to_slug("---"), "");
        assert_eq!(to_slug(""), "");
        assert_eq!(to_slug("a1-b2"), "a1-b2");
    }

    #[test]
    fn the_slug_pattern_is_gos_regex() {
        for good in ["a", "a1", "my-agent", "a-b-c", "9-9"] {
            assert!(is_valid_slug(good), "{good} should be valid");
        }
        for bad in ["", "-a", "a-", "a--b", "A", "a_b", "a b", "-", "ä"] {
            assert!(!is_valid_slug(bad), "{bad} should be invalid");
        }
    }

    #[test]
    fn the_write_routes_are_claimed_and_the_nested_ones_are_not() {
        assert!(claims(&Method::POST, "/api/agents"));
        assert!(claims(&Method::PUT, "/api/agents/my-agent"));
        assert!(claims(&Method::DELETE, "/api/agents/my-agent"));

        // Creating is on the collection, not on one agent.
        assert!(!claims(&Method::POST, "/api/agents/my-agent"));
        // Still unported, and still a different route.
        assert!(!claims(&Method::POST, "/api/agents/my-agent/duplicate"));
        assert!(!claims(&Method::PUT, "/api/agents"));
        assert!(!claims(&Method::DELETE, "/api/agents"));
        assert!(!claims(&Method::PATCH, "/api/agents/my-agent"));
    }

    /// #335: the access line says a `POST /api/agents` happened; only the
    /// handler knows which agent. A line with no test is a line that quietly
    /// stops being emitted, which for this half of #301 is the whole failure
    /// mode.
    #[test]
    fn the_agent_writes_log_their_entity_and_outcome() {
        crate::native::writes::testlog::install();
        let file = migrated();

        create(
            file.path(),
            br#"{"name":"Logged Agent","slug":"logged-agent"}"#,
        )
        .expect("create");
        crate::native::writes::testlog::assert_info_once(r#"agent created slug="logged-agent""#);

        update(file.path(), "logged-agent", br#"{"name":"Logged Agent 2"}"#).expect("update");
        crate::native::writes::testlog::assert_info_once(r#"agent updated slug="logged-agent""#);

        delete(file.path(), "logged-agent").expect("delete");
        crate::native::writes::testlog::assert_info_once(r#"agent deleted slug="logged-agent""#);
    }
}

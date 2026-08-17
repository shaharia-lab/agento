//! The integration MCP server lifecycle, ported from
//! `internal/integrations/registry.go` (#311).
//!
//! Go keeps one long-lived in-process MCP server per enabled, authenticated
//! integration: `Start` brings them all up at boot, `Reload` restarts one when
//! its row changes, `Stop` tears one down when it is deleted. This module is
//! that, and since #311 it is the **only** implementation of it — the sidecar
//! runs with `AGENTO_INTEGRATIONS=off` (`sidecar.rs`), which no-ops all three
//! on the Go side.
//!
//! ## Why the ownership had to flip before the writes could move
//!
//! `PUT /api/integrations/{id}` and `DELETE` are the two routes that drive this
//! lifecycle, and #277 left them with Go for the standard reason: a native write
//! would persist the row and leave the Go-hosted server on stale config. What
//! makes that sharper than the usual "two caches disagree" is the listener. Go's
//! `claude.StartInProcessMCPServer` binds an **unauthenticated** loopback port
//! and the server closes over the credential it was started with, so a sidecar
//! that never hears `Reload`/`Stop` keeps answering `tools/call` with a token
//! the user just revoked, for the rest of the process's life. That is a security
//! regression, not a staleness one, and it applies to all six integration types
//! rather than only the one Rust can host.
//!
//! So the fix is #289's, applied a second time: give the Go half an off switch,
//! and let one process own the state. `AGENTO_INTEGRATIONS` has exactly
//! `AGENTO_SCANNER`'s semantics — `off`/`0`/`false`/`disabled` off, unset on,
//! unrecognized on — and gates `Start`/`Reload`/`Stop` and nothing else.
//!
//! **It deliberately does not gate `StartFilteredServer`**, which is what every
//! agent run uses: `runner.go` builds a per-run server from a fresh row read and
//! never touches the hosted map. That is what keeps a chat, a scheduled task or
//! a Telegram trigger the sidecar still serves able to reach its integration
//! tools while only GitHub is ported here.
//!
//! ## What is hosted, and what happens to the rest
//!
//! Go registers a starter per *type*; only `github` has a Rust one (#312).
//! Every other type takes the path Go's own unregistered type takes — the error
//! `no starter registered for integration type "slack"`, **logged and never
//! surfaced**, because `Start` swallows it and `Update`'s caller swallows
//! `Reload`'s. A user with a Slack integration therefore sees exactly what a
//! user with a mistyped type saw before: the row saves, and nothing is hosted.
//! #313–#317 fill the table in.
//!
//! ## Reload is not restart-if-changed
//!
//! `Reload` stops and starts **unconditionally**, holding no lock across the
//! two, so there is a window with no server and the port changes every time.
//! That is Go's behaviour and it is reproduced rather than improved on: a
//! "nothing changed, skip it" check would be a different set of live ports after
//! the same sequence of requests. Shutdown is graceful on both sides — see
//! `claude/mcp.rs`, where the ordering that makes it so is one line's placement.
//!
//! ## Secrets
//!
//! This is the first place in the port that reads `integrations.credentials`.
//! `native/integrations.rs` never selects that column and collapses `auth` to a
//! boolean in SQL precisely so that a secret cannot exist in this process to be
//! echoed; that rule still holds for every response type. [`HostingRow`] is the
//! deliberate exception, and it is kept away from the wire structurally: it is
//! private to this module, it derives nothing (no `Serialize`, and no `Debug`
//! either — a `{row:?}` in a log line would be the leak), and the only thing
//! that ever leaves it is a `&str` handed to a tool constructor.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rusqlite::OptionalExtension;

use crate::claude::InProcessMcpServer;

use super::{decode_services, ServiceConfig};

/// One integration row, **credentials included**.
///
/// Read only by this module, and only to build a tool server. Deriving anything
/// on it is a mistake: `Serialize` would put a token on the wire and `Debug`
/// would put one in a log line, which is the same leak with a longer fuse. See
/// the module header.
struct HostingRow {
    id: String,
    integration_type: String,
    enabled: bool,
    /// `IsAuthenticated()`, computed in SQL so the token itself is never read.
    authenticated: bool,
    /// The raw `credentials` column. A secret.
    credentials: String,
    services: Option<BTreeMap<String, ServiceConfig>>,
}

impl HostingRow {
    /// `!cfg.Enabled || !cfg.IsAuthenticated()` — the skip both `Start` and
    /// `Reload` apply before they reach a starter.
    fn is_startable(&self) -> bool {
        self.enabled && self.authenticated
    }

    /// The services map a starter sees. A stored `null` is a nil Go map, which
    /// ranges zero times — an empty map is the same thing to every reader here.
    fn services(&self) -> BTreeMap<String, ServiceConfig> {
        self.services.clone().unwrap_or_default()
    }
}

/// The hosted servers, keyed by integration id — `IntegrationRegistry.servers`
/// and `.cancels` in one map, because in Rust the handle *is* the cancel: a
/// dropped [`InProcessMcpServer`] fires the shutdown oneshot.
pub struct Registry {
    servers: Mutex<HashMap<String, InProcessMcpServer>>,
}

/// The process-wide registry.
///
/// A module-level `OnceLock`, which is the shape the other long-lived native
/// state already uses (`native::scan::state`, `native::chat::live::registry`).
/// It has to outlive a request — a server started by a `PUT` is still hosted
/// when the next one arrives — and threading it through [`super::super::Ctx`]
/// would put a lifetime on a value that has exactly one instance.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry {
        servers: Mutex::new(HashMap::new()),
    })
}

impl Registry {
    /// `Stop`: idempotent, and an unknown id is a silent no-op with no error.
    ///
    /// Dropping the handle is what stops the listener, and it is done **inside**
    /// the lock: the drop only sends a oneshot, so there is nothing to await and
    /// nothing to deadlock on, and releasing first would leave a window in which
    /// the map says the server is gone while the port is still open.
    pub fn stop(&self, id: &str) {
        let removed = self
            .servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if removed.is_some() {
            log::info!("integration MCP server stopped: id={id:?}");
        }
    }

    fn put(&self, id: &str, server: InProcessMcpServer) {
        self.servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), server);
    }

    /// Whether an integration is hosted right now. Nothing on the wire reads
    /// this; it exists so the lifecycle can be asserted.
    pub fn is_hosted(&self, id: &str) -> bool {
        self.servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }
}

/// `IntegrationRegistry.Start`: host every enabled, authenticated integration.
///
/// **A failed start is logged and swallowed**, never propagated — Go's
/// "Continue with other integrations rather than failing all". Only a failure to
/// read the list at all is an error, and even that is only logged by the one
/// caller (boot), because there is nothing better to do with it there.
pub async fn start_all(db_path: &Path) -> Result<(), String> {
    let rows = list_for_hosting(db_path)?;
    for row in rows {
        if !row.is_startable() {
            continue;
        }
        if let Err(e) = start_one(&row).await {
            log::warn!(
                "failed to start integration server: id={:?} type={:?} error={e}",
                row.id,
                row.integration_type
            );
        }
    }
    Ok(())
}

/// `IntegrationRegistry.Reload`: stop, then start, unconditionally.
///
/// Every early return is `Ok(())` rather than an error, matching Go exactly: a
/// row that has been deleted has nothing to start, and one that is disabled or
/// unauthenticated is not a failure to report. Only a store read that fails or a
/// starter that fails is an `Err` — and both of this function's callers log it
/// rather than surfacing it.
pub async fn reload(db_path: &Path, id: &str) -> Result<(), String> {
    registry().stop(id);

    let Some(row) = get_for_hosting(db_path, id)? else {
        return Ok(()); // deleted — nothing to start
    };
    if !row.is_startable() {
        return Ok(()); // disabled or not authenticated
    }
    start_one(&row).await
}

/// `startOne`: resolve the type's starter, run it, record the handle.
async fn start_one(row: &HostingRow) -> Result<(), String> {
    let server = start_for_type(row).await?;
    let url = server.url().to_string();
    registry().put(&row.id, server);
    log::info!(
        "integration MCP server started: id={:?} type={:?} url={url}",
        row.id,
        row.integration_type
    );
    Ok(())
}

/// The starter table. Go builds a `map[string]ServerStarter` at wiring time;
/// there is one entry to look up here, so this is the lookup.
///
/// An unregistered type produces Go's own message, `%q`-quoted the way
/// `fmt.Errorf` quotes it. It is logged and never surfaced, which is what makes
/// five of the six types behave, from the user's side, exactly as they did
/// before this module existed.
async fn start_for_type(row: &HostingRow) -> Result<InProcessMcpServer, String> {
    match row.integration_type.as_str() {
        "github" => start_github(&row.id, &row.services(), &row.credentials).await,
        other => Err(format!(
            "no starter registered for integration type {other:?}"
        )),
    }
}

/// `github.Start`'s first two steps — the auth check and the credential parse —
/// followed by the third, which `native/integrations/github` already owns.
///
/// The auth check is Go's `if !cfg.IsAuthenticated()` inside `Start`, which is
/// redundant with the caller's own skip and is kept for the same reason Go keeps
/// it: `StartFilteredServer` reaches a starter by a different path.
async fn start_github(
    id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    credentials: &str,
) -> Result<InProcessMcpServer, String> {
    let token = github_token(id, credentials)?;
    super::github::start_github_mcp_server(id, services, &token)
        .await
        .map_err(|e| format!("starting in-process MCP server for {id:?}: {e}"))
}

/// `cfg.ParseCredentials(&creds)` for `config.GitHubCredentials`, reduced to the
/// one field `buildMCPServer` uses.
///
/// Note what is **not** checked: `auth_mode`. `github.Start` reads
/// `creds.PersonalAccessToken` whatever the mode says, so an `oauth` row hosts a
/// server with an empty token and every tool 401s — which is Go's behaviour, and
/// not something to improve on here.
fn github_token(id: &str, credentials: &str) -> Result<String, String> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct GitHubCredentials {
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        personal_access_token: String,
    }

    if credentials.is_empty() {
        // Go's own `fmt.Errorf("credentials are empty")`, wrapped as `Start`
        // wraps it.
        return Err(format!(
            "parsing github credentials for {id:?}: credentials are empty"
        ));
    }
    // Through `Option<T>`, because a literal `null` is a no-op to
    // `json.Unmarshal` and a type error to serde — the rule
    // `native/integration_credentials.rs` carries for the same columns.
    serde_json::from_str::<Option<GitHubCredentials>>(credentials)
        .map(Option::unwrap_or_default)
        .map(|creds| creds.personal_access_token)
        .map_err(|e| {
            // **The serde message is deliberately dropped.** It quotes the
            // offending value, so a malformed blob would put the PAT itself
            // into this log line. Line and column are enough to debug with and
            // carry nothing secret — the same trade
            // `native/integration_credentials.rs` makes on the request path.
            //
            // Go's own text (`encoding/json`'s, naming Go types) is not
            // reproducible either way, and unlike a validation error this one
            // never reaches a response: `Reload`'s failure is logged.
            format!(
                "parsing github credentials for {id:?}: does not decode at line {} column {}",
                e.line(),
                e.column()
            )
        })
}

// ─── The per-run server, which the hosting switch does not touch ──────────────

/// `AllowedToolNames`: `mcp__<integration id>__<tool>`.
///
/// The **bare integration id**, not `github::server_name`'s `github-<id>`. Those
/// are two different strings and both are Go's: `mcp.NewServer` is named
/// `github-<id>` (an implementation name the CLI never puts on a tool), while
/// `StartInProcessMCPServer(ctx, cfg.ID, …)` and the `mcp_servers` map key are
/// the id — and the map key is what the CLI prefixes tool names with. Every
/// agent's stored allowlist and every `tool_use` block already written carries
/// the id form.
pub fn allowed_tool_names<I, S>(integration_id: &str, tools: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    tools
        .into_iter()
        .map(|tool| format!("mcp__{integration_id}__{}", tool.as_ref()))
        .collect()
}

/// `StartFilteredServer`: a server for one run, hosting only the tools the agent
/// asked for.
///
/// Nothing is recorded — that is what makes it per-run, and it is why the
/// hosting switch does not reach it. The caller owns the handle and dropping it
/// stops the listener, which is how a turn's tools die with the turn.
///
/// The three refusals are Go's, in Go's order and with Go's wording, because
/// `resolveServerConfig` discards the error and falls through to `nil` — so a
/// refusal here means the agent runs without that server, exactly as it does on
/// the Go side.
pub async fn start_filtered_server(
    db_path: &Path,
    id: &str,
    tools: &[String],
) -> Result<InProcessMcpServer, String> {
    let Some(row) = get_for_hosting(db_path, id)? else {
        return Err(format!("integration {id:?} not found"));
    };
    if !row.is_startable() {
        return Err(format!(
            "integration {id:?} is not enabled or not authenticated"
        ));
    }
    let filtered = filter_config_tools(&row.services(), tools);
    match row.integration_type.as_str() {
        "github" => start_github(&row.id, &filtered, &row.credentials).await,
        other => Err(format!(
            "no starter registered for integration type {other:?}"
        )),
    }
}

/// Whether a run naming this integration can be served natively at all.
///
/// Separate from [`start_filtered_server`] because the caller has to decide
/// *before* it starts anything: an agent that names a type Rust cannot host must
/// forward the whole turn to Go rather than run with some of its tools missing.
pub fn can_host(db_path: &Path, id: &str) -> Result<bool, String> {
    Ok(get_for_hosting(db_path, id)?
        .is_some_and(|row| matches!(row.integration_type.as_str(), "github")))
}

/// `filterConfigTools`: keep only the requested tools, of only the enabled
/// services.
///
/// Two halves that both look like oversights and are not: an **empty** request
/// list returns the services untouched (including disabled ones, which the
/// starters skip on their own), and a service left with no kept tools is dropped
/// entirely rather than kept empty — which matters, because an empty `tools`
/// list is what `buildAllowedSet` reads as "host everything".
fn filter_config_tools(
    services: &BTreeMap<String, ServiceConfig>,
    tools: &[String],
) -> BTreeMap<String, ServiceConfig> {
    if tools.is_empty() {
        return services.clone();
    }
    let want: std::collections::HashSet<&str> = tools.iter().map(String::as_str).collect();
    let mut out = BTreeMap::new();
    for (name, service) in services {
        if !service.enabled {
            continue;
        }
        let kept: Vec<String> = service
            .tools
            .iter()
            .flat_map(|list| list.iter())
            .filter(|tool| want.contains(tool.as_str()))
            .cloned()
            .collect();
        if kept.is_empty() {
            continue;
        }
        out.insert(
            name.clone(),
            ServiceConfig {
                enabled: true,
                tools: Some(crate::native::gojson::GoList(kept)),
            },
        );
    }
    out
}

// ─── Reads ────────────────────────────────────────────────────────────────────

/// The **secrets** projection. Every column of it stays inside this module.
///
/// Deliberately not built on `INTEGRATION_COLUMNS`, which is the projection the
/// response types are scanned from: sharing one would put `credentials` one
/// `SELECT` away from every read in `native/integrations.rs`, and the whole
/// point of that constant is that the column is not in it.
const HOSTING_COLUMNS: &str = "SELECT id, type, enabled,
            (auth IS NOT NULL AND auth != '' AND auth != 'null') AS authenticated,
            credentials, services
     FROM integrations";

fn scan_hosting_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HostingRow> {
    let enabled: i64 = row.get(2)?;
    let authenticated: i64 = row.get(3)?;
    let services: String = row.get(5)?;
    Ok(HostingRow {
        id: row.get(0)?,
        integration_type: row.get(1)?,
        enabled: enabled != 0,
        authenticated: authenticated != 0,
        credentials: row.get(4)?,
        services: decode_services(&services),
    })
}

fn list_for_hosting(db_path: &Path) -> Result<Vec<HostingRow>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    // `ORDER BY name ASC` is the store's, and `Start` ranges the list in order.
    let sql = format!("{HOSTING_COLUMNS}\n     ORDER BY name ASC");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("listing integrations: {e}"))?;
    let rows = stmt
        .query_map([], scan_hosting_row)
        .map_err(|e| format!("listing integrations: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("listing integrations: {e}"))?);
    }
    Ok(out)
}

fn get_for_hosting(db_path: &Path, id: &str) -> Result<Option<HostingRow>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    let sql = format!("{HOSTING_COLUMNS}\n     WHERE id = ?1");
    conn.query_row(&sql, [id], scan_hosting_row)
        .optional()
        .map_err(|e| format!("loading integration {id:?}: {e}"))
}

// ─── Running an async reload from a synchronous handler ───────────────────────

/// Run `future` to completion on the ambient tokio runtime, from a thread that
/// is not itself async.
///
/// Both halves are the point. The seam's [`super::super::Endpoint::serve`] is a
/// **sync** `fn` the proxy calls on `spawn_blocking`, and a reload is async —
/// so something has to bridge. Go's bridge is
/// `registry.Reload(context.WithoutCancel(ctx), id)`: synchronous with respect
/// to the response, but detached from the request's cancellation, so a client
/// that hangs up mid-`PUT` does not abandon a half-restarted server.
///
/// Spawning onto the runtime and blocking on a plain `std` channel is exactly
/// that pair. `Handle::block_on` would tie the work to this thread instead, and
/// nothing else here needs a second async entry point.
///
/// With no runtime at all — which in practice means a unit test that called the
/// handler directly rather than through the proxy — the work is skipped and
/// logged. It cannot happen in the app: the proxy is the only caller and it is
/// axum.
fn block_on_detached<F>(what: &str, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        log::warn!("{what}: no tokio runtime on this thread; skipping");
        return;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    handle.spawn(async move {
        future.await;
        let _ = tx.send(());
    });
    if rx.recv().is_err() {
        log::warn!("{what}: the task ended without reporting");
    }
}

/// The reload `PUT /api/integrations/{id}` performs, with Go's swallowing.
///
/// **Never returns anything.** The row is already written by the time this runs,
/// and Go's handler logs a reload failure and answers 200 regardless — "row
/// written, server dead" is its accepted outcome. Turning it into a
/// `WriteError::Fallback` would be much worse than that: the seam forwards a
/// fallback to Go, which would re-apply the write.
pub fn reload_blocking(db_path: &Path, id: &str) {
    let db_path = db_path.to_path_buf();
    let owned_id = id.to_string();
    block_on_detached(&format!("reloading integration {id:?}"), async move {
        if let Err(e) = reload(&db_path, &owned_id).await {
            log::warn!(
                "failed to reload integration server after update: id={owned_id:?} error={e}"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// A distinctive token, so a leak into any string this module produces is
    /// unmistakable.
    const PAT: &str = "ghp_SUPER_SECRET_PAT";

    fn db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        file
    }

    fn insert(
        file: &tempfile::NamedTempFile,
        id: &str,
        integration_type: &str,
        enabled: bool,
        auth: Option<&str>,
        credentials: &str,
        services: &str,
    ) {
        Connection::open(file.path())
            .expect("open")
            .execute(
                "INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                           created_at, updated_at)
                 VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6,
                         '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
                rusqlite::params![
                    id,
                    integration_type,
                    i64::from(enabled),
                    credentials,
                    auth,
                    services
                ],
            )
            .expect("insert");
    }

    const GITHUB_SERVICES: &str = r#"{"repos":{"enabled":true,"tools":["list_repos","get_repo"]}}"#;

    fn github_credentials() -> String {
        format!(r#"{{"auth_mode":"pat","personal_access_token":"{PAT}"}}"#)
    }

    /// The whole lifecycle over the one type Rust hosts.
    #[tokio::test]
    async fn a_github_integration_starts_reloads_and_stops() {
        let file = db();
        insert(
            &file,
            "gh-1",
            "github",
            true,
            Some(r#"{"validated":true}"#),
            &github_credentials(),
            GITHUB_SERVICES,
        );

        start_all(file.path()).await.expect("start_all");
        assert!(registry().is_hosted("gh-1"));

        // Reload is unconditional: the old listener goes and a new one binds, so
        // the port changes even though nothing about the row did.
        reload(file.path(), "gh-1").await.expect("reload");
        assert!(registry().is_hosted("gh-1"));

        registry().stop("gh-1");
        assert!(!registry().is_hosted("gh-1"));
        // Idempotent, and an unknown id is a silent no-op.
        registry().stop("gh-1");
        registry().stop("never-existed");
    }

    /// Both flags gate hosting, exactly as they gate `available-tools`.
    #[tokio::test]
    async fn a_disabled_or_unauthenticated_integration_is_not_hosted() {
        let file = db();
        insert(
            &file,
            "gh-off",
            "github",
            false,
            Some(r#"{"ok":true}"#),
            &github_credentials(),
            GITHUB_SERVICES,
        );
        insert(
            &file,
            "gh-anon",
            "github",
            true,
            None,
            &github_credentials(),
            GITHUB_SERVICES,
        );
        // The literal four bytes `null` are not authentication either.
        insert(
            &file,
            "gh-null",
            "github",
            true,
            Some("null"),
            &github_credentials(),
            GITHUB_SERVICES,
        );

        start_all(file.path()).await.expect("start_all");
        for id in ["gh-off", "gh-anon", "gh-null"] {
            assert!(!registry().is_hosted(id), "{id} must not be hosted");
            // …and reloading one is `Ok(())`, not an error.
            reload(file.path(), id)
                .await
                .expect("reload is not a failure");
            assert!(!registry().is_hosted(id), "{id} must not be hosted");
        }
    }

    /// A type with no starter is Go's unregistered-type path: an error that the
    /// callers swallow, and a row that is simply not hosted.
    #[tokio::test]
    async fn an_unported_type_fails_the_way_gos_unregistered_type_does() {
        let file = db();
        insert(
            &file,
            "sl-1",
            "slack",
            true,
            Some(r#"{"validated":true}"#),
            r#"{"bot_token":"xoxb-secret"}"#,
            r#"{"chat":{"enabled":true,"tools":["post"]}}"#,
        );

        // `start_all` swallows it: one bad integration must not stop the others.
        start_all(file.path()).await.expect("start_all swallows it");
        assert!(!registry().is_hosted("sl-1"));

        // `reload` returns it, and its callers are the ones that swallow.
        let err = reload(file.path(), "sl-1").await.expect_err("no starter");
        assert_eq!(err, r#"no starter registered for integration type "slack""#);
    }

    /// A row that has been deleted is `Ok(())`, not a failure — `DELETE` stops
    /// before it deletes, but an `Update` racing a delete lands here.
    #[tokio::test]
    async fn reloading_a_missing_integration_is_not_an_error() {
        let file = db();
        reload(file.path(), "ghost")
            .await
            .expect("no row, no error");
        assert!(!registry().is_hosted("ghost"));
    }

    /// One failure must not stop the others — Go's "Continue with other
    /// integrations rather than failing all".
    #[tokio::test]
    async fn one_failed_start_does_not_abort_the_rest() {
        let file = db();
        insert(
            &file,
            "aaa-slack",
            "slack",
            true,
            Some(r#"{"ok":true}"#),
            "{}",
            "{}",
        );
        insert(
            &file,
            "zzz-github",
            "github",
            true,
            Some(r#"{"ok":true}"#),
            &github_credentials(),
            GITHUB_SERVICES,
        );

        start_all(file.path()).await.expect("start_all");
        assert!(!registry().is_hosted("aaa-slack"));
        assert!(
            registry().is_hosted("zzz-github"),
            "the integration after the failing one must still be hosted"
        );
        registry().stop("zzz-github");
    }

    /// The credential parse: Go's two failure shapes, and neither may echo the
    /// token.
    #[test]
    fn a_credential_failure_never_carries_the_credential() {
        assert_eq!(
            github_token("gh-1", "").unwrap_err(),
            r#"parsing github credentials for "gh-1": credentials are empty"#
        );

        let err = github_token("gh-1", &format!(r#"{{"personal_access_token":{PAT:?},"#))
            .expect_err("truncated json");
        assert!(
            !err.contains(PAT),
            "the token must not reach the log line: {err}"
        );
        assert!(err.contains("does not decode at line"), "{err}");

        // A literal `null` is a zero value to Go, not a type error.
        assert_eq!(github_token("gh-1", "null").expect("null decodes"), "");
        assert_eq!(
            github_token("gh-1", &github_credentials()).expect("valid"),
            PAT
        );
    }

    /// `filterConfigTools`, both halves.
    #[test]
    fn filtering_keeps_only_the_named_tools_of_enabled_services() {
        let services: BTreeMap<String, ServiceConfig> = serde_json::from_str(
            r#"{"repos":{"enabled":true,"tools":["list_repos","get_repo"]},
                    "issues":{"enabled":true,"tools":["list_issues"]},
                    "actions":{"enabled":false,"tools":["list_workflows"]}}"#,
        )
        .expect("services");

        // An empty request list is a no-op — including on the disabled service,
        // which the starter skips on its own.
        assert_eq!(filter_config_tools(&services, &[]), services);

        let filtered = filter_config_tools(&services, &["get_repo".to_string()]);
        assert_eq!(
            filtered.keys().collect::<Vec<_>>(),
            vec!["repos"],
            "a service left with no kept tools is dropped, not kept empty"
        );
        assert_eq!(
            filtered["repos"].tools.as_ref().expect("tools").0,
            vec!["get_repo".to_string()]
        );
        assert!(filtered["repos"].enabled);

        // A disabled service contributes nothing even when it names the tool.
        let filtered = filter_config_tools(&services, &["list_workflows".to_string()]);
        assert!(filtered.is_empty());
    }

    /// The qualified name is built from the **bare id**, which is what every
    /// stored allowlist already contains. Spelled out rather than derived, so a
    /// rename cannot pass through it.
    #[test]
    fn the_qualified_name_uses_the_integration_id_not_the_server_name() {
        assert_eq!(
            allowed_tool_names("abc123", ["list_repos", "get_repo"]),
            vec![
                "mcp__abc123__list_repos".to_string(),
                "mcp__abc123__get_repo".to_string()
            ]
        );
        // …and the MCP implementation name is a *different* string, which must
        // not leak into a tool name.
        assert_eq!(super::super::github::server_name("abc123"), "github-abc123");
        assert!(allowed_tool_names("abc123", ["x"])[0].starts_with("mcp__abc123__"));
    }

    /// The per-run server is not recorded — that is what makes it per-run.
    #[tokio::test]
    async fn a_filtered_server_is_owned_by_its_caller_and_never_hosted() {
        let file = db();
        insert(
            &file,
            "gh-run",
            "github",
            true,
            Some(r#"{"ok":true}"#),
            &github_credentials(),
            GITHUB_SERVICES,
        );

        let server = start_filtered_server(file.path(), "gh-run", &["get_repo".to_string()])
            .await
            .expect("filtered server");
        assert!(server.url().starts_with("http://127.0.0.1:"));
        // The registry is process-wide and the tests share it, so this asserts
        // on *this* id rather than on a count another test could move.
        assert!(!registry().is_hosted("gh-run"));

        assert!(can_host(file.path(), "gh-run").expect("can_host"));
        assert!(!can_host(file.path(), "nope").expect("can_host"));
    }

    /// Go's three refusals, in Go's wording. `resolveServerConfig` discards
    /// them, so what they buy is a run that goes to Go instead of one that runs
    /// with tools missing.
    #[tokio::test]
    async fn a_filtered_server_refuses_the_way_go_refuses() {
        let file = db();
        insert(&file, "gh-off", "github", false, Some("{}"), "{}", "{}");
        insert(&file, "sl-1", "slack", true, Some("{}"), "{}", "{}");

        // `.err()` rather than `unwrap_err()`: the `Ok` side is an
        // `InProcessMcpServer`, which deliberately has no `Debug` — printing
        // one would print the bearer token its config carries.
        let refusal = |id: &'static str| {
            let path = file.path().to_path_buf();
            async move {
                start_filtered_server(&path, id, &[])
                    .await
                    .err()
                    .unwrap_or_else(|| panic!("{id} must be refused"))
            }
        };

        assert_eq!(refusal("ghost").await, r#"integration "ghost" not found"#);
        assert_eq!(
            refusal("gh-off").await,
            r#"integration "gh-off" is not enabled or not authenticated"#
        );
        assert_eq!(
            refusal("sl-1").await,
            r#"no starter registered for integration type "slack""#
        );
        assert!(!can_host(file.path(), "sl-1").expect("can_host"));
    }
}

//! The integration MCP server lifecycle, ported from
//! `internal/integrations/registry.go` (#311).
//!
//! Go keeps one long-lived in-process MCP server per enabled, authenticated
//! integration: `Start` brings them all up at boot, `Reload` restarts one when
//! its row changes, `Stop` tears one down when it is deleted. This module is
//! that, for the types listed in [`HOSTED_TYPES`] — today `github` (#312),
//! `confluence` (#317), `jira` (#316) and `slack` (#315).
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
//! regression, not a staleness one.
//!
//! So the fix is #289's, applied a second time — but **per type, not per
//! process**, which is the one way it differs from the scan.
//!
//! ## Why the switch carries a list
//!
//! It is tempting to read a starter as a pure MCP-server constructor, in which
//! case switching Go's hosting off costs a bound port and nothing else. That is
//! true of five of the six. It is **not** true of `whatsapp`:
//! `internal/integrations/whatsapp/server.go` opens a real whatsmeow WebSocket,
//! registers the live client in a package global and only then returns a server
//! config, and `whatsapp/status.go`'s `ConnectionStatus` reads that global. So
//! `GET /api/integrations/{id}/whatsapp/status`, the reconnect endpoint and QR
//! pairing all work only in the process that started the integration. Turning Go
//! hosting off wholesale does not cost WhatsApp a port; it costs WhatsApp.
//!
//! Hence `AGENTO_INTEGRATIONS=off:<types>`: `off`/`0`/`false`/`disabled`
//! optionally followed by `:` and the comma-separated types the *shell* hosts.
//! Unset is on, unrecognized is on, and an empty list is on — the same
//! fail-toward-hosting rule `AGENTO_SCANNER` has. On the Go side it gates
//! `Start` and `Reload` per row; `Stop` is ungated there, because stopping can
//! only ever remove a server that process started.
//!
//! **The list is built here, by [`hosting_env_value`], from the same
//! [`HOSTED_TYPES`] that [`hosts_type`] and the starter dispatch read.** That is
//! the point of carrying it in the environment rather than hardcoding it on both
//! sides: the shell is the process that knows what it hosts, so #313 and #314 each
//! add one string to one list and the two halves cannot drift. The failure mode
//! being designed against is a Rust slack starter landing while Go is never told
//! to stop hosting slack — two processes on one integration — and its mirror is
//! the WhatsApp bug above.
//!
//! ## What happens to a type neither side hosts
//!
//! Nothing changes for it: Go still hosts every type not in [`HOSTED_TYPES`],
//! exactly as it did before this module existed. What *this* module does with
//! one is take the path Go's own unregistered type takes — the error `no starter
//! registered for integration type "slack"`, **logged and never surfaced** —
//! but it never gets the chance, because the native `PUT`/`DELETE` decline a row
//! whose type is not [`hosts_type`] and forward the whole request to Go, which
//! then fires its own `Reload`/`Stop`. That refusal is a *pre-write* one; see
//! `native/integrations.rs` and the invariant in `writes.rs`.
//!
//! ## Reload is not restart-if-changed
//!
//! `Reload` stops and starts **unconditionally**, so there is a window with no
//! server and the port changes every time. That is Go's behaviour and it is
//! reproduced rather than improved on: a "nothing changed, skip it" check would
//! be a different set of live ports after the same sequence of requests. What is
//! *not* reproduced is Go's orphan: no lock is held across the stop, the async
//! row read and the start, so a `DELETE` landing in that window would leave a
//! bound port holding a credential for a row that no longer exists. Go's
//! equivalent orphan is a map entry nobody reads; this one is the thing the
//! whole issue exists to prevent, so [`Registry::stop`] bumps a per-id
//! generation and a start only records its handle if the generation it observed
//! before reading the row still stands. Both concurrent-`reload` handles are
//! still safe on their own — `HashMap::insert` drops the displaced one and
//! `Drop` fires the shutdown oneshot.
//!
//! Shutdown is graceful on both sides — see `claude/mcp.rs`, where the ordering
//! that makes it so is one line's placement.
//!
//! ## What still reaches Go's `Reload` and not this one
//!
//! `Reload` has seven callers in Go and only two of them (`Update`, and
//! `Delete` via `Stop`) are ported. Five run inside the sidecar. `completeOAuth`
//! and the telegram validator is fine **because the gate is per type**: they can only reach types Go still hosts, and
//! `startProviderCallback` supports exactly `google` and `slack`, so an OAuth
//! completion never concerns a hosted one. The other three —
//! `validateGitHubPATAuth`, and since #316 and #317 `validateJiraTokenAuth` and
//! `validateConfluenceAuth` — write a credential for a type
//! *this* process hosts, from a handler that cannot tell it. Without a hook such
//! an integration would first be hosted at the next boot's [`start_all`], so the
//! seam fires [`reload_after_auth`] after forwarding a 2xx for that one route
//! (`native::after_forward`). That hook needs no per-type list of its own: it
//! runs for every id on the route and [`reload_after_auth`] reads the row's type
//! through [`can_host`], so #313 and #314 are covered the moment they add their
//! string to [`HOSTED_TYPES`]. The reload is idempotent and the response has
//! already been produced, so doing it on the forward path costs nothing but a
//! restart of a server that was about to be restarted anyway.
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
    /// `IsAuthenticated()`, computed in SQL so the predicate does not depend on
    /// parsing [`Self::auth`].
    authenticated: bool,
    /// The raw `credentials` column. A secret.
    credentials: String,
    /// The raw `auth` column. **Also a secret**, and the newer of the two.
    ///
    /// Until #315 this projection selected `auth` only as the boolean above, and
    /// `native/integrations.rs` still never selects it at all — the rule that a
    /// stored token cannot exist in this process to be echoed. Slack is the
    /// exception that forced it: `resolveToken` reads
    /// `cfg.ParseOAuthToken()` — the `auth` column parsed as an `oauth2.Token` —
    /// whenever `credentials.auth_mode` is `oauth`, so the value is genuinely
    /// needed to build the server. What has *not* changed is where it may go:
    /// this struct still derives neither `Serialize` nor `Debug`, it is private
    /// to this module, and only a `&str` ever leaves it.
    auth: String,
    services: Option<BTreeMap<String, ServiceConfig>>,
}

impl HostingRow {
    /// `!cfg.Enabled || !cfg.IsAuthenticated()` — the skip both `Start` and
    /// `Reload` apply before they reach a starter.
    fn is_startable(&self) -> bool {
        self.enabled && self.authenticated
    }

    /// A fingerprint of the two secret columns, for [`reload_if_secrets_changed`].
    ///
    /// A hash, so the registry's record of "what is this server running on" is
    /// not a second place the token itself lives. `DefaultHasher` because the
    /// question is only ever "did this change", within one process, against a
    /// value this process wrote — there is nothing here for a collision to buy
    /// an attacker that reading the database would not.
    fn secrets_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.credentials.hash(&mut hasher);
        self.auth.hash(&mut hasher);
        hasher.finish()
    }

    /// The services map a starter sees. A stored `null` is a nil Go map, which
    /// ranges zero times — an empty map is the same thing to every reader here.
    fn services(&self) -> BTreeMap<String, ServiceConfig> {
        self.services.clone().unwrap_or_default()
    }
}

/// The integration types **this** process hosts.
///
/// One list, read by three things that must agree: [`hosts_type`] (which the
/// native `PUT`/`DELETE` consult before they touch a row), the starter dispatch
/// in [`start_for_type`], and [`hosting_env_value`], which tells the Go sidecar
/// which types to stop hosting. #313 and #314 each add one string here.
pub const HOSTED_TYPES: &[&str] = &["github", "confluence", "jira", "slack"];

/// Whether this process hosts an integration of the given type.
pub fn hosts_type(integration_type: &str) -> bool {
    HOSTED_TYPES.contains(&integration_type)
}

/// The value `sidecar.rs` puts in `AGENTO_INTEGRATIONS`.
///
/// Derived from [`HOSTED_TYPES`] rather than written out, so the sidecar cannot
/// be told to stop hosting a type this build does not start. An empty list would
/// render as `off:`, which the Go parser reads as "host everything" — the safe
/// direction, and the reason the list is joined rather than the switch being a
/// bare `off`.
pub fn hosting_env_value() -> String {
    format!("off:{}", HOSTED_TYPES.join(","))
}

/// The hosted servers, keyed by integration id — `IntegrationRegistry.servers`
/// and `.cancels` in one map, because in Rust the handle *is* the cancel: a
/// dropped [`InProcessMcpServer`] fires the shutdown oneshot.
pub struct Registry {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    servers: HashMap<String, InProcessMcpServer>,
    /// Bumped by every [`Registry::stop`], and never reset.
    ///
    /// This is what closes the window `reload` opens by design: a start records
    /// the generation it saw *before* the row read that justified it, and
    /// [`Registry::put_if_current`] refuses a handle whose generation has moved
    /// since. A `DELETE` in that window therefore drops the new server instead
    /// of leaving a bound port holding a credential for a deleted row.
    generations: HashMap<String, u64>,
    /// A fingerprint of the secrets each hosted server was **started with**.
    ///
    /// Only [`reload_if_secrets_changed`] reads it, and only to answer "is what
    /// is running still what the row says". It is a hash rather than the values,
    /// so this map is not a second place a token lives — see that function for
    /// why the question needs asking at all.
    secrets: HashMap<String, u64>,
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
        state: Mutex::new(State::default()),
    })
}

impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `Stop`: idempotent, and an unknown id is a silent no-op with no error.
    ///
    /// Dropping the handle is what stops the listener, and it is done **inside**
    /// the lock: the drop only sends a oneshot, so there is nothing to await and
    /// nothing to deadlock on, and releasing first would leave a window in which
    /// the map says the server is gone while the port is still open.
    ///
    /// The generation bump is what makes a concurrent start notice. It happens
    /// whether or not anything was removed, because a `DELETE` racing a `reload`
    /// that has already stopped the old server is exactly the case with nothing
    /// to remove.
    pub fn stop(&self, id: &str) {
        let mut state = self.lock();
        *state.generations.entry(id.to_string()).or_default() += 1;
        state.secrets.remove(id);
        let removed = state.servers.remove(id);
        drop(state);
        if removed.is_some() {
            log::info!("integration MCP server stopped: id={id:?}");
        }
    }

    /// The generation to quote to [`Registry::put_if_current`] later. Read
    /// **before** the row read that decides whether to start.
    fn generation(&self, id: &str) -> u64 {
        self.lock().generations.get(id).copied().unwrap_or_default()
    }

    /// The whole generation map, for a caller that does not know the ids yet —
    /// `start_all`, which has to fix its reference point before it lists the
    /// table or a delete landing between the list and the start would slip
    /// through. An id absent from the snapshot is generation 0, which any later
    /// `stop` moves off.
    fn generations(&self) -> HashMap<String, u64> {
        self.lock().generations.clone()
    }

    /// Record a started server, unless it has been stopped out from under us.
    ///
    /// Returns whether the handle was kept. A refused handle is dropped here,
    /// which fires its shutdown oneshot — so the listener the caller started
    /// goes away rather than outliving the row it was built from.
    fn put_if_current(
        &self,
        id: &str,
        generation: u64,
        server: InProcessMcpServer,
        secrets: u64,
    ) -> bool {
        let mut state = self.lock();
        if state.generations.get(id).copied().unwrap_or_default() != generation {
            return false;
        }
        state.servers.insert(id.to_string(), server);
        state.secrets.insert(id.to_string(), secrets);
        true
    }

    /// The fingerprint the server hosted under `id` was started with, or `None`
    /// when nothing is hosted for it.
    fn secrets(&self, id: &str) -> Option<u64> {
        self.lock().secrets.get(id).copied()
    }

    /// Whether an integration is hosted right now. Nothing on the wire reads
    /// this; it exists so the lifecycle can be asserted.
    pub fn is_hosted(&self, id: &str) -> bool {
        self.lock().servers.contains_key(id)
    }

    /// The loopback URL a hosted server is listening on.
    ///
    /// Test-only, and it is how "did this reload actually restart anything"
    /// is observed: `reload` binds a fresh port every time, so an unchanged URL
    /// is proof that nothing was torn down. Not exposed outside tests because
    /// nothing else needs it — `InProcessMcpServer` deliberately has no `Debug`,
    /// and the URL is the one part of it that is safe to look at.
    #[cfg(test)]
    fn url(&self, id: &str) -> Option<String> {
        self.lock()
            .servers
            .get(id)
            .map(|server| server.url().to_string())
    }
}

/// `IntegrationRegistry.Start`: host every enabled, authenticated integration.
///
/// **A failed start is logged and swallowed**, never propagated — Go's
/// "Continue with other integrations rather than failing all". Only a failure to
/// read the list at all is an error, and even that is only logged by the one
/// caller (boot), because there is nothing better to do with it there.
pub async fn start_all(db_path: &Path) -> Result<(), String> {
    // Fixed **before** the list read, because `start_all` is spawned rather
    // than awaited at boot (`lib.rs`) and the proxy is already answering. A
    // `DELETE` any time after this point moves the id's generation off what is
    // recorded here, so the start that follows is refused instead of orphaning
    // a listener for a row that has just gone.
    let generations = registry().generations();
    let rows = list_for_hosting(db_path)?;
    for row in rows {
        if !row.is_startable() {
            continue;
        }
        let generation = generations.get(&row.id).copied().unwrap_or_default();
        if let Err(e) = start_one(&row, generation).await {
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
    // After the stop, so this *is* the generation that stop just wrote. Any
    // later `stop` — a concurrent `DELETE`, or a second `reload` — moves it,
    // and the start below is then refused rather than resurrecting a row that
    // has been deleted since it was read.
    let generation = registry().generation(id);

    let Some(row) = get_for_hosting(db_path, id)? else {
        return Ok(()); // deleted — nothing to start
    };
    if !row.is_startable() {
        return Ok(()); // disabled or not authenticated
    }
    start_one(&row, generation).await
}

/// `startOne`: resolve the type's starter, run it, record the handle.
///
/// `generation` is the value [`Registry::stop`] had written when the caller
/// decided to start: a mismatch means the integration was stopped or deleted
/// while the server was being built, and the handle is dropped rather than
/// recorded — which stops the listener it just bound.
async fn start_one(row: &HostingRow, generation: u64) -> Result<(), String> {
    let server = start_for_type(row).await?;
    let url = server.url().to_string();
    if !registry().put_if_current(&row.id, generation, server, row.secrets_fingerprint()) {
        log::info!(
            "integration MCP server discarded before it was recorded, \
             the integration was stopped while it started: id={:?} type={:?}",
            row.id,
            row.integration_type
        );
        return Ok(());
    }
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
/// Its arms must cover [`HOSTED_TYPES`] exactly — `a_hosted_type_always_has_a_
/// starter` pins that, since the two are what the Go sidecar's own gate is
/// derived from and a type claimed but not started would be hosted by nobody.
///
/// An unregistered type produces Go's own message, `%q`-quoted the way
/// `fmt.Errorf` quotes it. In this build nothing reaches it through the hosted
/// path — the writes decline an unhosted type before they mutate — but it is
/// still what a row whose type changed under a stale caller would produce, and
/// it is the message `start_filtered_server` genuinely returns.
async fn start_for_type(row: &HostingRow) -> Result<InProcessMcpServer, String> {
    match row.integration_type.as_str() {
        "github" => start_github(&row.id, &row.services(), &row.credentials).await,
        "confluence" => start_confluence(&row.id, &row.services(), &row.credentials).await,
        "jira" => start_jira(&row.id, &row.services(), &row.credentials).await,
        "slack" => start_slack(&row.id, &row.services(), &row.credentials, &row.auth).await,
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

/// `confluence.Start`'s first three steps — the auth check, the credential parse
/// and the site-URL normalisation — followed by the fourth, which
/// `native/integrations/confluence` owns.
///
/// The auth check is Go's `if !cfg.IsAuthenticated()` inside `Start`, which is
/// redundant with the caller's own skip and is kept for the same reason Go keeps
/// it: `StartFilteredServer` reaches a starter by a different path.
///
/// Note the order: the site URL is validated **before** the server is built and
/// therefore before the token is captured into any closure, so a plaintext site
/// URL never gets as far as a client that could send a `Basic` header over it.
async fn start_confluence(
    id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    credentials: &str,
) -> Result<InProcessMcpServer, String> {
    let creds = atlassian_credentials("confluence", id, credentials)?;
    let site_url = super::confluence::validate_site_url(&creds.site_url)
        .map_err(|e| format!("invalid site URL for {id:?}: {e}"))?;
    super::confluence::start_confluence_mcp_server(
        id,
        services,
        &site_url,
        &creds.email,
        &creds.api_token,
    )
    .await
    .map_err(|e| format!("starting in-process MCP server for {id:?}: {e}"))
}

/// `jira.Start`'s first two steps — the auth check and the credential parse —
/// followed by the third, which `native/integrations/jira` owns.
///
/// **There is no third check.** `confluence.Start` normalises the site URL and
/// fails on a bad one; `jira.Start` does not look at it, so this starter cannot
/// fail on it either and `jira::client::Client` carries the decision per call
/// instead. That asymmetry is #277's, and reproducing it is what keeps the
/// advertised tool set identical to Go's — see `jira::client`'s header.
async fn start_jira(
    id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    credentials: &str,
) -> Result<InProcessMcpServer, String> {
    let creds = atlassian_credentials("jira", id, credentials)?;
    super::jira::start_jira_mcp_server(
        id,
        services,
        &creds.site_url,
        &creds.email,
        &creds.api_token,
    )
    .await
    .map_err(|e| format!("starting in-process MCP server for {id:?}: {e}"))
}

/// `slack.Start`'s first two steps — the auth check and `resolveToken` —
/// followed by the third, which `native/integrations/slack` owns.
///
/// The token is the reason this starter takes `auth` where the others take only
/// `credentials`: see [`resolve_slack_token`].
async fn start_slack(
    id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    credentials: &str,
    auth: &str,
) -> Result<InProcessMcpServer, String> {
    let token = resolve_slack_token(id, credentials, auth)?;
    super::slack::start_slack_mcp_server(id, services, &token)
        .await
        .map_err(|e| format!("starting in-process MCP server for {id:?}: {e}"))
}

/// `resolveToken` (`slack/server.go`), wrapped as `Start` wraps it.
///
/// Three arms, and the third is the one a port drops:
///
/// - `bot_token` — the credentials blob, refusing an empty one.
/// - `oauth` — `cfg.ParseOAuthToken()`, which is the **`auth` column** decoded
///   as an `oauth2.Token`. This is the only place in the port that reads that
///   column as a value; see [`HostingRow::auth`].
/// - anything else, **including the empty string** — falls back to the bot token
///   if it is non-empty, and only then fails. So a row whose `auth_mode` was
///   never set still works, which is what makes this a fallback rather than a
///   default.
///
/// Every message is Go's, and none of them interpolates a token. The one
/// deliberate divergence is the `oauth` arm's decode failure: Go's carries
/// `encoding/json`'s wording, and this carries line and column for
/// [`github_token`]'s reason — the serde message quotes the offending value,
/// which here *is* the access token.
pub(super) fn resolve_slack_token(
    id: &str,
    credentials: &str,
    auth: &str,
) -> Result<String, String> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct SlackCredentials {
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        auth_mode: String,
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        bot_token: String,
    }

    let wrap = |message: String| format!("resolving slack token for {id:?}: {message}");

    if credentials.is_empty() {
        return Err(wrap(
            "parsing slack credentials: credentials are empty".to_string(),
        ));
    }
    let creds = serde_json::from_str::<Option<SlackCredentials>>(credentials)
        .map(Option::unwrap_or_default)
        .map_err(|e| {
            wrap(format!(
                "parsing slack credentials: does not decode at line {} column {}",
                e.line(),
                e.column()
            ))
        })?;

    match creds.auth_mode.as_str() {
        "bot_token" => {
            if creds.bot_token.is_empty() {
                return Err(wrap("bot_token is empty".to_string()));
            }
            Ok(creds.bot_token)
        }
        "oauth" => {
            // `oauth2.Token`, of which only `access_token` is read. A Go
            // `json.Unmarshal` into that struct would also reject a malformed
            // `expiry` (it is a `time.Time`), which this does not — an
            // unreachable difference, since the column is written by
            // `SetOAuthToken`, and a log line either way.
            #[derive(Default, serde::Deserialize)]
            #[serde(default)]
            struct OAuthToken {
                #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
                access_token: String,
            }
            let token = serde_json::from_str::<Option<OAuthToken>>(auth)
                .map(Option::unwrap_or_default)
                .map_err(|e| {
                    wrap(format!(
                        "parsing oauth token: does not decode at line {} column {}",
                        e.line(),
                        e.column()
                    ))
                })?;
            Ok(token.access_token)
        }
        other => {
            if !creds.bot_token.is_empty() {
                return Ok(creds.bot_token);
            }
            Err(wrap(format!(
                "unsupported auth_mode {other:?} and no bot_token available"
            )))
        }
    }
}

/// `config.AtlassianCredentials` — the struct Confluence and Jira share.
///
/// Neither derives `Debug` nor `Serialize`, for [`HostingRow`]'s reason: a
/// `{creds:?}` in a log line is the same leak with a longer fuse.
///
/// `kind` is the word Go's wrapper uses (`parsing confluence credentials for %q`
/// against `parsing jira credentials for %q`) — the struct is shared and the
/// sentence is not, so #316 passes `"jira"` to the same function.
struct AtlassianCredentials {
    site_url: String,
    email: String,
    api_token: String,
}

fn atlassian_credentials(
    kind: &str,
    id: &str,
    credentials: &str,
) -> Result<AtlassianCredentials, String> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct Raw {
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        site_url: String,
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        email: String,
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        api_token: String,
    }

    if credentials.is_empty() {
        // Go's own `fmt.Errorf("credentials are empty")`, wrapped as `Start`
        // wraps it.
        return Err(format!(
            "parsing {kind} credentials for {id:?}: credentials are empty"
        ));
    }
    // Through `Option<T>`, because a literal `null` is a no-op to
    // `json.Unmarshal` and a type error to serde — the rule
    // `native/integration_credentials.rs` carries for the same columns.
    serde_json::from_str::<Option<Raw>>(credentials)
        .map(Option::unwrap_or_default)
        .map(|raw| AtlassianCredentials {
            site_url: raw.site_url,
            email: raw.email,
            api_token: raw.api_token,
        })
        .map_err(|e| {
            // **The serde message is deliberately dropped**, for
            // [`github_token`]'s reason: it quotes the offending value, so a
            // malformed blob would put the API token itself into this log line.
            format!(
                "parsing {kind} credentials for {id:?}: does not decode at line {} column {}",
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
        "confluence" => start_confluence(&row.id, &filtered, &row.credentials).await,
        "jira" => start_jira(&row.id, &filtered, &row.credentials).await,
        "slack" => start_slack(&row.id, &filtered, &row.credentials, &row.auth).await,
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
///
/// [`hosts_type`] is the predicate; this only adds the row read. The two writes
/// in `native/integrations.rs` have already read the row and so call
/// [`hosts_type`] directly rather than reading it a second time through the
/// credential-carrying projection.
/// The row's `type`, and **nothing else** — no `credentials`, no `auth`.
///
/// The point is what it does not select. [`HOSTING_COLUMNS`] is the one
/// projection in the port that reads the secret columns, and its charter is to
/// read them *to build a tool server*. Two callers only want to know whether the
/// type is one this process hosts, and both are on hot paths for rows it does
/// not: [`can_host`] runs per chat turn, and [`reload_if_secrets_changed`] runs
/// per poll of an OAuth dialog — which the Google, WhatsApp and detail pages all
/// poll. Answering that question through the secret-carrying projection would
/// pull an unrelated integration's token into memory to decide it belongs to
/// somebody else.
fn type_of(db_path: &Path, id: &str) -> Result<Option<String>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    conn.query_row("SELECT type FROM integrations WHERE id = ?1", [id], |row| {
        row.get::<_, String>(0)
    })
    .optional()
    .map_err(|e| format!("looking up integration {id:?}: {e}"))
}

pub fn can_host(db_path: &Path, id: &str) -> Result<bool, String> {
    // [`type_of`] rather than the hosting projection: this answers a question
    // about the `type` column and has no business reading the other two.
    Ok(type_of(db_path, id)?.is_some_and(|integration_type| hosts_type(&integration_type)))
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
            credentials, services, COALESCE(auth, '')
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
        auth: row.get(6)?,
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
/// The reload the seam owes a request Go answered: `POST
/// /api/integrations/{id}/auth/validate`, whose Go-side `Reload` is a no-op for
/// a type this process hosts.
///
/// Async and awaited by its caller's spawned task rather than blocking, because
/// the proxy is already on the runtime there — [`reload_blocking`] exists for
/// the *sync* handler path and would only tie up a worker here. A type this
/// process does not host is skipped silently: Go's own `Reload` handled it, and
/// running one here would log an unregistered-type error on every Slack
/// credential save.
pub async fn reload_after_auth(db_path: &Path, id: &str) {
    match can_host(db_path, id) {
        Ok(false) => return,
        Ok(true) => {}
        Err(e) => {
            log::warn!("reloading integration {id:?} after auth: {e}");
            return;
        }
    }
    if let Err(e) = reload(db_path, id).await {
        log::warn!("failed to reload integration server after auth: id={id:?} error={e}");
    }
}

/// Reload only if what the row says no longer matches what is hosted.
///
/// # The hole this fills, which #315 opened
///
/// Go's `handleOAuthToken` writes the token to the `auth` column and then calls
/// `registry.Reload` — and `startProviderCallback` supports exactly `google` and
/// `slack`. While neither was hosted here that was harmless, and
/// `registry.rs`'s own header said so. **#315 made `slack` a hosted type**, so
/// that `Reload` now reaches nothing and a Slack integration authenticated by
/// OAuth would first be served at the next boot's [`start_all`].
///
/// [`reload_after_auth`] cannot cover it: that hook hangs off a *forwarded
/// request*, and an OAuth token does not arrive on one. It is delivered by the
/// browser to a callback server the **sidecar** opens on its own port, which
/// this process never sees. The one part of the flow that does come through the
/// proxy is the UI polling `GET /api/integrations/{id}/auth/status` while it
/// waits, so that is where this hangs — which makes it **best-effort**: a user
/// who closes the dialog before it completes is served at the next boot, as they
/// were before. #318 owns the flow itself and is where that stops being true.
///
/// # Why conditional, where [`reload_after_auth`] is not
///
/// A poll is not an event. `reload` is unconditional by design — stop, then
/// start, with a new port each time — so firing it per poll would restart the
/// server every second the dialog is open and drop any in-flight `tools/call`
/// with it. So this compares the row's current secrets against the ones the
/// running server was started with and does nothing when they match. That covers
/// every transition without churn: unauthenticated → authenticated (nothing
/// hosted, now startable), a re-authentication that replaces the token
/// (fingerprint moved), and a de-authentication (startable → not, which stops
/// it).
pub async fn reload_if_secrets_changed(db_path: &Path, id: &str) {
    // The type first, through a projection that reads no secret: every page with
    // an auth dialog polls this route, and most of those rows are types this
    // process does not host.
    match type_of(db_path, id) {
        Ok(Some(integration_type)) if hosts_type(&integration_type) => {}
        // Not ours, or deleted between the poll and this read. `stop` is the
        // `DELETE` path's job and has already run; nothing to do here.
        Ok(_) => return,
        Err(e) => {
            log::warn!("reloading integration {id:?} after an auth-status poll: {e}");
            return;
        }
    }

    let row = match get_for_hosting(db_path, id) {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(e) => {
            log::warn!("reloading integration {id:?} after an auth-status poll: {e}");
            return;
        }
    };

    let want = row.is_startable().then(|| row.secrets_fingerprint());
    if want == registry().secrets(id) {
        return;
    }

    log::info!("integration {id:?} changed while its auth status was polled; reloading");
    if let Err(e) = reload(db_path, id).await {
        log::warn!("failed to reload integration server after auth: id={id:?} error={e}");
    }
}

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

    /// The poll-driven reload: it fires on a change and, crucially, **not**
    /// otherwise.
    ///
    /// #315 opened the hole this fills — Go reloads after an OAuth token lands
    /// and `slack` is now hosted here, but the token arrives on the sidecar's own
    /// callback port, so the UI's polling of `auth/status` is the only part of
    /// the flow this process sees. A poll is not an event, so the anti-churn half
    /// is as load-bearing as the firing half: `reload` rebinds the port and drops
    /// in-flight calls, and the dialog polls every second.
    #[tokio::test]
    async fn a_polled_auth_status_reloads_on_a_change_and_not_on_a_poll() {
        let file = db();
        // Unauthenticated, so not startable. Slack would be the true subject, but
        // its starter needs a live token to be interesting; the condition under
        // test is type-blind and github is the type these tests already seed.
        insert(
            &file,
            "gh-oauth",
            "github",
            true,
            None,
            &github_credentials(),
            "{}",
        );

        // Nothing hosted and nothing startable: the poll must be a no-op rather
        // than an attempt.
        reload_if_secrets_changed(file.path(), "gh-oauth").await;
        assert!(!registry().is_hosted("gh-oauth"));

        // The token lands — which is what Go's `handleOAuthToken` does, and what
        // it then tells nobody about.
        Connection::open(file.path())
            .expect("open")
            .execute(
                "UPDATE integrations SET auth = ?1 WHERE id = 'gh-oauth'",
                rusqlite::params![r#"{"login":"octocat"}"#],
            )
            .expect("authenticate");

        reload_if_secrets_changed(file.path(), "gh-oauth").await;
        assert!(
            registry().is_hosted("gh-oauth"),
            "the poll after the token landed must host it"
        );
        let first = registry()
            .url("gh-oauth")
            .expect("a hosted server has a url");

        // …and every later poll must leave it exactly where it is. A changed
        // port here would mean the dialog restarts the server once a second.
        for _ in 0..3 {
            reload_if_secrets_changed(file.path(), "gh-oauth").await;
            assert_eq!(
                registry().url("gh-oauth").as_deref(),
                Some(first.as_str()),
                "a poll that changed nothing must not rebind the port"
            );
        }

        // A token *replaced* is a change, and must be picked up.
        Connection::open(file.path())
            .expect("open")
            .execute(
                "UPDATE integrations SET credentials = ?1 WHERE id = 'gh-oauth'",
                rusqlite::params![r#"{"auth_mode":"pat","personal_access_token":"ghp-second"}"#],
            )
            .expect("re-authenticate");
        reload_if_secrets_changed(file.path(), "gh-oauth").await;
        assert!(registry().is_hosted("gh-oauth"));
        assert_ne!(
            registry().url("gh-oauth").as_deref(),
            Some(first.as_str()),
            "a replaced credential must restart the server"
        );

        // …and a de-authentication stops it.
        Connection::open(file.path())
            .expect("open")
            .execute(
                "UPDATE integrations SET auth = NULL WHERE id = 'gh-oauth'",
                [],
            )
            .expect("de-authenticate");
        reload_if_secrets_changed(file.path(), "gh-oauth").await;
        assert!(!registry().is_hosted("gh-oauth"));

        registry().stop("gh-oauth");
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

    /// The value the sidecar is started with is **derived** from the starter
    /// table, which is the whole reason it travels in the environment rather
    /// than being spelled out on both sides. Adding a type to `HOSTED_TYPES`
    /// without a starter, or shipping a starter Go is never told about, are the
    /// two ways to put a credential on two open ports at once.
    #[test]
    fn the_sidecar_is_told_exactly_what_this_process_hosts() {
        assert_eq!(hosting_env_value(), "off:github,confluence,jira,slack");
        assert_eq!(
            hosting_env_value(),
            format!("off:{}", HOSTED_TYPES.join(",")),
            "the switch must be built from the starter table, never written out"
        );
        assert!(hosts_type("github"));
        assert!(hosts_type("confluence"));
        assert!(hosts_type("jira"));
        assert!(hosts_type("slack"));
        // The two types #313 and #314 still owe. Go must keep hosting every one of
        // them, and `whatsapp` is the reason this is a list at all: its starter
        // opens a live whatsmeow connection that its status, reconnect and QR
        // endpoints read out of a package global.
        for unported in ["telegram", "google", "whatsapp"] {
            assert!(!hosts_type(unported), "{unported} is not hosted here");
            assert!(
                !hosting_env_value().contains(unported),
                "{unported} must not be switched off in the sidecar — nobody would host it"
            );
        }
    }

    /// A type claimed by `HOSTED_TYPES` but missing from the starter dispatch
    /// would be hosted by nobody: the sidecar is told to drop it and this
    /// process cannot start it.
    #[tokio::test]
    async fn a_hosted_type_always_has_a_starter() {
        let file = db();
        for (i, integration_type) in HOSTED_TYPES.iter().enumerate() {
            let id = format!("probe-{i}");
            // Deliberately empty credentials: whatever a real starter does, it
            // must be reached at all, and the unregistered-type message is the
            // one answer that proves it was not.
            insert(&file, &id, integration_type, true, Some("{}"), "", "{}");
            let err = reload(file.path(), &id).await.err().unwrap_or_default();
            assert!(
                !err.contains("no starter registered"),
                "{integration_type} is in HOSTED_TYPES with no starter: {err}"
            );
        }
    }

    /// The race `reload` opens by design, and the guard that closes it: a
    /// `DELETE` between the row read and the handle being recorded must not
    /// leave a bound port holding the credential of a row that is gone.
    #[tokio::test]
    async fn a_stop_between_the_read_and_the_put_discards_the_server() {
        let file = db();
        insert(
            &file,
            "gh-race",
            "github",
            true,
            Some(r#"{"ok":true}"#),
            &github_credentials(),
            GITHUB_SERVICES,
        );

        // What `reload` observes before it reads the row.
        let generation = registry().generation("gh-race");
        // …and the concurrent `DELETE`, which lands while the server is being
        // built. Nothing is hosted yet, so `stop` removes nothing — the bump
        // happens anyway, which is what makes this case visible at all.
        registry().stop("gh-race");

        let server = start_filtered_server(file.path(), "gh-race", &[])
            .await
            .expect("a server to race with");
        assert!(
            !registry().put_if_current("gh-race", generation, server, 0),
            "a handle whose generation has moved must be refused"
        );
        assert!(!registry().is_hosted("gh-race"));

        // …and the same handle is kept when nothing intervened.
        let generation = registry().generation("gh-race");
        let server = start_filtered_server(file.path(), "gh-race", &[])
            .await
            .expect("server");
        assert!(registry().put_if_current("gh-race", generation, server, 0));
        assert!(registry().is_hosted("gh-race"));
        registry().stop("gh-race");
    }

    /// A type with no starter is Go's unregistered-type path: an error that the
    /// callers swallow, and a row that is simply not hosted.
    #[tokio::test]
    async fn an_unported_type_fails_the_way_gos_unregistered_type_does() {
        let file = db();
        insert(
            &file,
            "tg-1",
            "telegram",
            true,
            Some(r#"{"validated":true}"#),
            r#"{"bot_token":"tg-secret"}"#,
            r#"{"chat":{"enabled":true,"tools":["post"]}}"#,
        );

        // `start_all` swallows it: one bad integration must not stop the others.
        start_all(file.path()).await.expect("start_all swallows it");
        assert!(!registry().is_hosted("tg-1"));

        // `reload` returns it, and its callers are the ones that swallow.
        let err = reload(file.path(), "tg-1").await.expect_err("no starter");
        assert_eq!(
            err,
            r#"no starter registered for integration type "telegram""#
        );
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
        insert(&file, "tg-1", "telegram", true, Some("{}"), "{}", "{}");

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
            refusal("tg-1").await,
            r#"no starter registered for integration type "telegram""#
        );
        assert!(!can_host(file.path(), "tg-1").expect("can_host"));
    }
}

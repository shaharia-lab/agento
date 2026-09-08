//! The integration MCP server lifecycle, ported from
//! `internal/integrations/registry.go` (#311).
//!
//! Go keeps one long-lived in-process MCP server per enabled, authenticated
//! integration: `Start` brings them all up at boot, `Reload` restarts one when
//! its row changes, `Stop` tears one down when it is deleted. This module is
//! that, for the types listed in [`HOSTED_TYPES`] — since #313, **all six**:
//! `github` (#312), `confluence` (#317), `jira` (#316), `slack` (#315),
//! `telegram` (#314) and `google` (#313). `whatsapp` is not among them and is
//! not waiting to be: see below.
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
//! true of all six of the ported types. It is **not** true of `whatsapp`:
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
//! **The list was carried to the sidecar by `hosting_env_value`, from the same
//! [`HOSTED_TYPES`] that [`hosts_type`] and the starter dispatch read** — one
//! list, so the two halves could not drift. #278 removed the sidecar, and with
//! it the environment plumbing; [`HOSTED_TYPES`] remains the single list the
//! two in-process consumers read.
//!
//! ## What happens to a type neither side hosts
//!
//! Nothing changes for it: Go still hosts every type not in [`HOSTED_TYPES`],
//! exactly as it did before this module existed. What *this* module does with
//! one is take the path Go's own unregistered type takes — the error `no starter
//! registered for integration type "slack"`, **logged and never surfaced** —
//! but it never gets the chance, because the `PUT`/`DELETE` decline a row whose
//! type is not [`hosts_type`] rather than writing it. That refusal is a
//! *pre-write* one; see `native/integrations.rs` and the invariant in
//! `writes.rs`.
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
//! `Delete` via `Stop`) are ported. Five run inside the sidecar, and as of #314
//! **none of them is safe by the per-type gate any more** — and as of #313 every
//! one of the six is hosted here, so a `Reload` for any of them reaches nothing
//! in the sidecar. There is no longer a type for which those callers happen to
//! be harmless.
//!
//! Four are the token validators — `validateGitHubPATAuth` and, since #314–#317,
//! `validateTelegramTokenAuth`, `validateSlackTokenAuth`, `validateJiraTokenAuth`
//! and `validateConfluenceAuth`. Each writes a credential for a type *this*
//! process hosts, from a handler that cannot tell it. Without a hook such an
//! integration would first be hosted at the next boot's [`start_all`], so the
//! seam fires [`reload_after_auth`] on a 2xx for that one route. That hook needs
//! no per-type list of its own: it runs for every id on the route and
//! [`reload_after_auth`] reads the row's type through [`can_host`]. The reload
//! is idempotent and the response has already been produced, so firing it here
//! costs nothing but a restart of a server that was about to be restarted
//! anyway.
//!
//! The fifth is `completeOAuth`, and until #318 it needed a **different** hook
//! because its trigger never crossed the proxy: the token was delivered by the
//! browser to a callback server the *sidecar* opened on its own port, so the
//! only part of the flow this process could see was the UI polling
//! `GET /api/integrations/{id}/auth/status`. That poll drove a
//! reload-only-if-changed, which was an inference standing in for an event.
//!
//! #318 moved the flow here: `oauth::flow` binds the callback server, writes the
//! token and calls [`reload_after_auth`] directly, exactly as `handleOAuthToken`
//! does. So the inference and the fingerprint it compared against are gone, and
//! `completeOAuth` now uses the same hook as everything else.
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

use super::slack::socket::SocketWorker;
use super::{decode_services, ServiceConfig};

/// One integration row, **credentials included**.
///
/// Read only by this module, and only to build a tool server. Deriving anything
/// on it is a mistake: `Serialize` would put a token on the wire and `Debug`
/// would put one in a log line, which is the same leak with a longer fuse. See
/// the module header.
pub(crate) struct HostingRow {
    pub(crate) id: String,
    pub(crate) integration_type: String,
    enabled: bool,
    /// `IsAuthenticated()`, computed in SQL so the predicate does not depend on
    /// parsing [`Self::auth`].
    authenticated: bool,
    /// The raw `credentials` column. A secret.
    pub(crate) credentials: String,
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
    /// Migration 39's switch, as `PUT /api/integrations/{id}/inbound` leaves it
    /// (#566).
    ///
    /// Read here and nowhere else in the process: the inbound worker (#567)
    /// starts and stops off this projection, because the reload that write ends
    /// with is the only thing that tells it the switch moved. Not a secret, and
    /// on this struct anyway — a second projection over the same row would be a
    /// second `SELECT` naming `credentials`, which the module header exists to
    /// prevent.
    ///
    /// Read by [`start_socket_worker`] since #567, which is what took the
    /// standing `allow(dead_code)` off it.
    pub(crate) inbound_enabled: bool,
}

impl HostingRow {
    /// `!cfg.Enabled || !cfg.IsAuthenticated()` — the skip both `Start` and
    /// `Reload` apply before they reach a starter.
    fn is_startable(&self) -> bool {
        self.enabled && self.authenticated
    }

    /// `cfg.IsAuthenticated()` on its own — whether the `auth` column holds
    /// anything.
    ///
    /// The boolean, never the value: [`Self::auth`] stays private to this
    /// module, and the one caller outside it ([`super::token_validate`]) only
    /// needs to know whether there is an authorisation to clear (#521).
    pub(crate) fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Slack's app-level token (`xapp-…`), or `None` when the blob holds none
    /// (#566).
    ///
    /// An accessor rather than a field, because [`Self::credentials`] is
    /// already selected and already a secret: adding `app_token` as its own
    /// column would widen the leak surface the module header argues against for
    /// nothing. What leaves is a `&str`, the same thing every tool constructor
    /// gets — never the blob, and never a `Debug` of this struct, which does
    /// not exist.
    ///
    /// The rule is the one `has_app_token_sql` decides in SQLite and
    /// `stores_an_app_token` decides in this process: a **text** value,
    /// non-empty once the four ASCII whitespace bytes are trimmed.
    /// `an_app_token_is_read_exactly_when_the_scrubbed_read_reports_one` holds
    /// this third spelling to the other two — the wire says a token is stored
    /// and the worker then finds none is the one disagreement that matters.
    /// It is deliberately *not* the `xapp-` prefix check `validate_slack`
    /// makes: that guards the write, and a row stored before it existed must
    /// still be readable here.
    ///
    /// An owned `String` rather than a `&str` into [`Self::credentials`]: a
    /// JSON string is escaped in the blob, so the decoded token is not always a
    /// slice of it, and every other credential accessor in this module hands
    /// back an owned value for the same reason.
    /// #567's socket worker is the caller, through
    /// [`start_socket_worker`], and this accessor is what it reads rather than
    /// reaching into the blob itself.
    pub(crate) fn app_token(&self) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(&self.credentials)
            .ok()?
            .get("app_token")?
            .as_str()
            .map(|token| token.trim_matches([' ', '\t', '\n', '\r']).to_string())
            .filter(|token| !token.is_empty())
    }

    /// The services map a starter sees. A stored `null` is a nil Go map, which
    /// ranges zero times — an empty map is the same thing to every reader here.
    fn services(&self) -> BTreeMap<String, ServiceConfig> {
        self.services.clone().unwrap_or_default()
    }
}

/// The integration types **this** process hosts.
///
/// One list, read by the two things that must agree: [`hosts_type`] (which the
/// native `PUT`/`DELETE` consult before they touch a row) and the starter
/// dispatch in [`start_for_type`]. (Until #278 it also fed the sidecar's
/// `AGENTO_INTEGRATIONS` switch; that plumbing died with the sidecar.)
pub const HOSTED_TYPES: &[&str] = &[
    "github",
    "confluence",
    "jira",
    "slack",
    "telegram",
    "google",
];

/// Whether this process hosts an integration of the given type.
pub fn hosts_type(integration_type: &str) -> bool {
    HOSTED_TYPES.contains(&integration_type)
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
    /// The Slack Socket Mode workers, keyed the same way (#567).
    ///
    /// A second map rather than a second field on the value, because the two
    /// handles are independent: every hosted type has a server, and only a
    /// Slack row with `inbound_enabled` and an `xapp-` token has a worker. They
    /// share one generation, so [`Registry::stop`] and
    /// [`Registry::put_if_current`] move both at once and a stop racing a
    /// reload can no more leave a socket holding a credential than it can leave
    /// a bound port.
    sockets: HashMap<String, SocketWorker>,
    /// Which worker's status writes still count, per integration (#567).
    ///
    /// Separate from [`Self::generations`] and not merged into it: the
    /// generation is bumped by every `stop`, including one for a row that has no
    /// socket at all, and it is read *before* a start to be quoted back
    /// afterwards. This is the opposite shape — taken at the moment a handle is
    /// accepted or retired, and never read speculatively. See
    /// `slack::socket::status_writer` for why the grant has to happen exactly
    /// here.
    socket_epochs: HashMap<String, u64>,
    /// Bumped by every [`Registry::stop`], and never reset.
    ///
    /// This is what closes the window `reload` opens by design: a start records
    /// the generation it saw *before* the row read that justified it, and
    /// [`Registry::put_if_current`] refuses a handle whose generation has moved
    /// since. A `DELETE` in that window therefore drops the new server instead
    /// of leaving a bound port holding a credential for a deleted row.
    generations: HashMap<String, u64>,
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

/// What [`Registry::put_if_current`] did with a start.
enum Recorded {
    /// The integration was stopped while the server was being built; both
    /// handles were dropped rather than recorded.
    Refused,
    /// Recorded, with a socket worker that is now the current one.
    WithSocket,
    /// Recorded, with no socket worker — so any previous one has been retired,
    /// and `epoch` is what the caller quotes to `slack::socket::clear_status`.
    WithoutSocket { epoch: u64 },
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
        let removed = state.servers.remove(id);
        // Removed under the same lock, for the reason above — after this the
        // map cannot hand the worker to anyone. *Dropping* it is what closes
        // the socket, and that happens a line later, outside the guard: the
        // drop only sends a oneshot, so there is nothing to await and nothing
        // to gain from holding the lock across it. The window it leaves is one
        // in which the socket is closing and the map already says so, which is
        // the direction that is safe.
        // Retired under the same lock. A stop is the end of that worker's right
        // to report, whether or not a replacement follows.
        Self::take_socket_epoch(&mut state, id);
        let removed_socket = state.sockets.remove(id);
        drop(state);
        drop(removed_socket);
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
    ///
    /// The socket worker is recorded in the **same** critical section as the
    /// server, so the pair a `start_one` built is kept or dropped together. Two
    /// calls would leave a `stop` landing between them able to keep one half.
    fn put_if_current(
        &self,
        id: &str,
        generation: u64,
        server: InProcessMcpServer,
        socket: Option<SocketWorker>,
    ) -> Recorded {
        let mut state = self.lock();
        if state.generations.get(id).copied().unwrap_or_default() != generation {
            // The refused socket is dropped with `socket` at the end of this
            // arm, still holding `NOT_ACCEPTED`, so it never writes a status.
            return Recorded::Refused;
        }
        state.servers.insert(id.to_string(), server);
        match socket {
            Some(socket) => {
                // **Granted here and nowhere else.** This is the one critical
                // section that decides which worker won, so taking the epoch in
                // it is what makes epoch order and acceptance order the same
                // order — see `slack::socket::status_writer`.
                let epoch = Self::take_socket_epoch(&mut state, id);
                socket.accept(epoch);
                // Displaced under the lock, dropped outside it — the discipline
                // `stop` and the arm below already follow. Two concurrent
                // reloads were always safe here because `insert` returns the
                // old handle, but dropping it *inside* the guard is a shutdown
                // fired while holding a lock the shutdown path may want.
                let displaced = state.sockets.insert(id.to_string(), socket);
                drop(state);
                drop(displaced);
                Recorded::WithSocket
            }
            // A Slack row whose inbound switch was turned off still reloads, and
            // the reload must retire the previous worker rather than leave it
            // running against a row that no longer wants it. Removed under the
            // lock and dropped outside it, exactly as [`Registry::stop`] does
            // and for the same reason.
            None => {
                let epoch = Self::take_socket_epoch(&mut state, id);
                let stale = state.sockets.remove(id);
                drop(state);
                drop(stale);
                Recorded::WithoutSocket { epoch }
            }
        }
    }

    /// Take the next status epoch for `id`, retiring whatever held the last one.
    /// Callers must already hold the state lock.
    fn take_socket_epoch(state: &mut State, id: &str) -> u64 {
        let epoch = state.socket_epochs.entry(id.to_string()).or_default();
        *epoch += 1;
        *epoch
    }

    /// Retire whatever socket worker is recorded for `id`, **unless the caller's
    /// start has been superseded**, answering the epoch that is now current.
    ///
    /// `generation` is the value the caller read before the row read that
    /// justified its start — the same value [`Registry::put_if_current`] checks,
    /// and it has to be checked here for the same reason. A start that fails is
    /// still a start that may have lost: a user saving a bad Slack blob and then
    /// a good one gives two overlapping reloads, and the first one's *failure*
    /// arrives after the second one's worker has been accepted. Retiring
    /// unconditionally there would drop a live worker and hand its caller an
    /// epoch with which to blank the row — leaving Slack inbound silently dead
    /// with an empty status, which is the outage the column exists to prevent.
    ///
    /// `None` means the caller lost and must touch nothing. `Some(epoch)` is
    /// quoted to `slack::socket::clear_status`, which re-checks it once more
    /// across its own await.
    pub fn retire_socket_if_current(&self, id: &str, generation: u64) -> Option<u64> {
        let mut state = self.lock();
        if state.generations.get(id).copied().unwrap_or_default() != generation {
            return None;
        }
        let epoch = Self::take_socket_epoch(&mut state, id);
        let stale = state.sockets.remove(id);
        drop(state);
        drop(stale);
        Some(epoch)
    }

    /// [`Registry::retire_socket_if_current`] with no generation to check.
    ///
    /// **No production caller, and there should not be one**: every start in
    /// this module has a generation, and skipping it is finding #3 of #567's
    /// review. It exists for `tests/slack_socket.rs`, which drives workers with
    /// no registry lifecycle at all and needs a current epoch to grant.
    pub fn retire_socket(&self, id: &str) -> u64 {
        let mut state = self.lock();
        let epoch = Self::take_socket_epoch(&mut state, id);
        let stale = state.sockets.remove(id);
        drop(state);
        drop(stale);
        epoch
    }

    /// Whether `epoch` is the one currently granted for `id`.
    ///
    /// [`slack::socket::NOT_ACCEPTED`](super::slack::socket::NOT_ACCEPTED) never
    /// matches, because epochs are handed out from one upwards — so a worker
    /// that was built and then refused writes nothing, ever.
    pub fn socket_epoch_is_current(&self, id: &str, epoch: u64) -> bool {
        epoch != super::slack::socket::NOT_ACCEPTED
            && self
                .lock()
                .socket_epochs
                .get(id)
                .copied()
                .unwrap_or_default()
                == epoch
    }

    /// Whether an integration is hosted right now. Nothing on the wire reads
    /// this; it exists so the lifecycle can be asserted.
    pub fn is_hosted(&self, id: &str) -> bool {
        self.lock().servers.contains_key(id)
    }

    /// Whether a Slack Socket Mode worker is running for this integration.
    /// Nothing on the wire reads this either — `inbound_status` is what the UI
    /// sees, and it is the worker's to write. This exists so the lifecycle can
    /// be asserted (#567).
    pub fn is_socket_running(&self, id: &str) -> bool {
        self.lock().sockets.contains_key(id)
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
    // **Before any worker exists**, so nothing races it. A fresh process hosts
    // no sockets, so every `inbound_status` in the database is about a
    // connection that no longer exists — including the `connected` a crash left
    // behind, which nothing else would ever correct. Each worker writes
    // `connecting` a moment later; a row that gets no worker is left saying
    // nothing, which is what the column's default means. See
    // `slack::socket::clear_all_inbound_status_blocking`.
    // Through `db::blocking` like every other database touch in this module:
    // `start_all` is spawned onto the runtime at boot, and this is a *write*
    // against a five-second `busy_timeout` at the one moment the scanner's batch
    // writer is most likely to hold the lock. The read below is a WAL read and
    // never waits on a writer, which is why it is not the same question.
    let clear_db = db_path.to_path_buf();
    crate::native::db::blocking("slack inbound clear at boot", move || {
        super::slack::socket::clear_all_inbound_status_blocking(&clear_db);
    })
    .await;
    let rows = list_for_hosting(db_path)?;
    for row in rows {
        if !row.is_startable() {
            continue;
        }
        let generation = generations.get(&row.id).copied().unwrap_or_default();
        if let Err(e) = start_one(db_path, &row, generation).await {
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
        return Ok(()); // deleted — the row is gone, and its status with it
    };
    if !row.is_startable() {
        // Disabled or not authenticated, so no worker will run — and for the
        // same reason as in `start_one`, the status it left behind is the
        // registry's to clear.
        if row.integration_type == "slack" {
            if let Some(epoch) = registry().retire_socket_if_current(&row.id, generation) {
                super::slack::socket::clear_status(db_path, &row.id, epoch).await;
            }
        }
        return Ok(());
    }
    start_one(db_path, &row, generation).await
}

/// `startOne`: resolve the type's starter, run it, record the handle.
///
/// `generation` is the value [`Registry::stop`] had written when the caller
/// decided to start: a mismatch means the integration was stopped or deleted
/// while the server was being built, and the handle is dropped rather than
/// recorded — which stops the listener it just bound.
async fn start_one(db_path: &Path, row: &HostingRow, generation: u64) -> Result<(), String> {
    // **The error arm clears too, and that is not symmetry for its own sake.**
    // `reload` has already stopped the previous worker by the time this runs, so
    // a start that fails here — `resolve_slack_token` refusing a blob whose
    // usable token has gone, an in-process server failing to bind — leaves a row
    // with no worker. Returning the `Err` without clearing would leave the dead
    // worker's `connected` standing until the next boot, which is the same lie
    // every other no-worker path in this function is careful to avoid.
    let server = match start_for_type(row).await {
        Ok(server) => server,
        Err(e) => {
            if row.integration_type == "slack" {
                // Only if this start is still the current one — a failure that
                // arrives after a concurrent reload's worker was accepted must
                // not drop it. See `retire_socket_if_current`.
                if let Some(epoch) = registry().retire_socket_if_current(&row.id, generation) {
                    super::slack::socket::clear_status(db_path, &row.id, epoch).await;
                }
            }
            return Err(e);
        }
    };
    let url = server.url().to_string();
    let socket = start_socket_worker(db_path, row);
    let hosted_socket = socket.is_some();
    match registry().put_if_current(&row.id, generation, server, socket) {
        Recorded::Refused => {
            log::info!(
                "integration MCP server discarded before it was recorded, \
                 the integration was stopped while it started: id={:?} type={:?}",
                row.id,
                row.integration_type
            );
            return Ok(());
        }
        // A Slack row that is not getting a worker must not keep the previous
        // worker's status. **This is the registry's to do and not the
        // worker's**: a `reload` is a stop followed immediately by a start, so a
        // retiring worker clearing its own row would race the replacement's
        // `connecting` — and a compare-and-swap cannot break the tie, because
        // both write the identical `"connected"`. Here the clear is ordered
        // after the decision, and quotes the epoch that decision took.
        Recorded::WithoutSocket { epoch } if row.integration_type == "slack" => {
            super::slack::socket::clear_status(db_path, &row.id, epoch).await;
        }
        Recorded::WithoutSocket { .. } | Recorded::WithSocket => {}
    }
    log::info!(
        "integration MCP server started: id={:?} type={:?} url={url}",
        row.id,
        row.integration_type
    );
    // A separate statement rather than a field on the line above, and that is
    // not style. CodeQL reads `url` there as tainted by a stored credential —
    // an alert that has been open against `main` for as long as the line has
    // existed — and *editing* the line re-reports it as introduced by whatever
    // change touched it. #567 has no business either fixing or inheriting that,
    // so the line is left byte-for-byte as it was.
    if hosted_socket {
        log::info!("slack socket mode worker hosted: id={:?}", row.id);
    }
    Ok(())
}

/// The Slack Socket Mode worker for this row, when the row asks for one (#567).
///
/// Three conditions, and each is a different absence: the type must be `slack`,
/// migration 39's switch must be on, and the credentials blob must actually hold
/// an `xapp-` token. The last is not redundant with the 422 on `PUT
/// /api/integrations/{id}/inbound` — a later `PUT /api/integrations/{id}` can
/// replace the blob with one that has no `app_token`, which is exactly why that
/// write clears `inbound_enabled`; this is the second line, because a row stored
/// before that clearing existed still has to start cleanly rather than open a
/// socket with an empty bearer.
///
/// **Started here rather than in `start_for_type`**, which returns one
/// `InProcessMcpServer` and is the starter table for the six hosted types. The
/// worker is not a seventh type — it is a second handle on one of them.
fn start_socket_worker(db_path: &Path, row: &HostingRow) -> Option<SocketWorker> {
    if row.integration_type != "slack" || !row.inbound_enabled {
        return None;
    }
    let Some(app_token) = row.app_token() else {
        log::warn!(
            "slack inbound is enabled but no app token is stored, \
             not starting the socket worker: id={:?}",
            row.id
        );
        return None;
    };
    Some(super::slack::socket::start(
        db_path,
        &row.id,
        &app_token,
        super::slack::socket::SocketOptions::default(),
    ))
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
        "telegram" => start_telegram(&row.id, &row.services(), &row.credentials).await,
        "google" => start_google(&row.id, &row.services(), &row.credentials, &row.auth).await,
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
    serde_json::from_str::<Option<crate::native::gojson::GoStruct<GitHubCredentials>>>(credentials)
        .map(|wrapped| wrapped.map_or_else(GitHubCredentials::default, |wrapped| wrapped.0))
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
    let creds = serde_json::from_str::<Option<crate::native::gojson::GoStruct<SlackCredentials>>>(
        credentials,
    )
    .map(|wrapped| wrapped.map_or_else(SlackCredentials::default, |wrapped| wrapped.0))
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
            let token =
                serde_json::from_str::<Option<crate::native::gojson::GoStruct<OAuthToken>>>(auth)
                    .map(|wrapped| wrapped.map_or_else(OAuthToken::default, |wrapped| wrapped.0))
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

/// `telegram.Start`'s first two steps — the auth check and the credential parse
/// — followed by the third, which `native/integrations/telegram` owns.
///
/// `config.TelegramCredentials` is one field, so this needs no shared struct the
/// way the two Atlassian ones do.
async fn start_telegram(
    id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    credentials: &str,
) -> Result<InProcessMcpServer, String> {
    let token = telegram_bot_token(id, credentials)?;
    super::telegram::start_telegram_mcp_server(id, services, &token)
        .await
        .map_err(|e| format!("starting in-process MCP server for {id:?}: {e}"))
}

/// `cfg.ParseCredentials(&creds)` for `config.TelegramCredentials`, wrapped as
/// `telegram.Start` wraps it.
///
/// `pub(super)` so the sentences can be asserted directly: they are hand-written
/// rather than vector-pinned, and one of them would carry the bot token if the
/// serde message were kept.
///
/// Note what is **not** checked: an empty bot token. `telegram.Start` reads
/// `creds.BotToken` and hosts whatever it finds, so an empty one produces a
/// server whose every call reaches `/bot/<method>` and 404s — Go's behaviour, and
/// not something to improve on here.
pub(super) fn telegram_bot_token(id: &str, credentials: &str) -> Result<String, String> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct TelegramCredentials {
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        bot_token: String,
    }

    if credentials.is_empty() {
        return Err(format!(
            "parsing telegram credentials for {id:?}: credentials are empty"
        ));
    }
    // `Option<GoStruct<T>>`: a literal `null` is a no-op to `json.Unmarshal` and
    // a type error to serde, and a JSON **array** is a type error to Go and a
    // positional struct to serde. Both directions matter here more than anywhere
    // — a bogus token becomes a URL path segment.
    serde_json::from_str::<Option<crate::native::gojson::GoStruct<TelegramCredentials>>>(
        credentials,
    )
    .map(|wrapped| wrapped.map_or_else(TelegramCredentials::default, |wrapped| wrapped.0))
    .map(|creds| creds.bot_token)
    .map_err(|e| {
        // The serde message is dropped for [`github_token`]'s reason: it quotes
        // the offending value, which here is the bot token.
        format!(
            "parsing telegram credentials for {id:?}: does not decode at line {} column {}",
            e.line(),
            e.column()
        )
    })
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
    serde_json::from_str::<Option<crate::native::gojson::GoStruct<Raw>>>(credentials)
        .map(|wrapped| wrapped.map_or_else(Raw::default, |wrapped| wrapped.0))
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
    let filtered = filter_config_tools(
        &row.services(),
        tools,
        service_tool_table(&row.integration_type),
    );
    match row.integration_type.as_str() {
        "github" => start_github(&row.id, &filtered, &row.credentials).await,
        "confluence" => start_confluence(&row.id, &filtered, &row.credentials).await,
        "jira" => start_jira(&row.id, &filtered, &row.credentials).await,
        "slack" => start_slack(&row.id, &filtered, &row.credentials, &row.auth).await,
        "telegram" => start_telegram(&row.id, &filtered, &row.credentials).await,
        "google" => start_google(&row.id, &filtered, &row.credentials, &row.auth).await,
        other => Err(format!(
            "no starter registered for integration type {other:?}"
        )),
    }
}

/// `google.Start`'s first two steps — the auth check and the two parses — then
/// the third, which `native/integrations/google` owns.
///
/// Google is the only one of the six whose starter needs **both** secret
/// columns and needs them for different things: `credentials` carries the OAuth2
/// client pair, and `auth` carries the token itself. Slack reads `auth` too, but
/// only for an access token; here the whole `oauth2.Token` matters, because
/// `expiry` is what decides whether the first tool call refreshes.
///
/// Every sentence is Go's and is pinned by the `starting` section of
/// `desktop/parity/google_vectors.json`.
async fn start_google(
    id: &str,
    services: &BTreeMap<String, ServiceConfig>,
    credentials: &str,
    auth: &str,
) -> Result<InProcessMcpServer, String> {
    let (client_id, client_secret, token) = google_start_inputs(id, credentials, auth)?;

    super::google::start_google_mcp_server(
        id,
        services,
        std::sync::Arc::new(super::google::client::TokenSource::new(
            client_id,
            client_secret,
            token,
        )),
    )
    .await
    .map_err(|e| format!("starting in-process MCP server for {id:?}: {e}"))
}

/// Everything `google.Start` decides **before** it builds a server: the auth
/// check, then the credentials parse, then the token parse — in that order,
/// which is `Start` calling `IsAuthenticated` and then `buildHTTPClient` calling
/// its two parses.
///
/// Split out of [`start_google`] so the order is testable without binding a
/// port. The order is half of what the vectors pin — two of them have more than
/// one thing wrong — and a test that rebuilt this chain itself would agree with
/// whatever it had written rather than with the starter.
pub(super) fn google_start_inputs(
    id: &str,
    credentials: &str,
    auth: &str,
) -> Result<(String, String, super::google::client::Token), String> {
    // `Start`'s own `if !cfg.IsAuthenticated()`, kept for the reason the others
    // keep theirs: `StartFilteredServer` reaches a starter by a different path.
    // It runs first, so an empty auth beats a broken credentials blob.
    if auth.is_empty() || auth == "null" {
        return Err(format!("integration {id:?} has no auth token"));
    }
    let (client_id, client_secret) = google_credentials(id, credentials)?;
    let token = google_oauth_token(id, auth)?;
    Ok((client_id, client_secret, token))
}

/// `cfg.ParseCredentials(&creds)` for `config.GoogleCredentials`, wrapped as
/// `buildHTTPClient` wraps it.
///
/// Note what is **not** checked: an empty client id or secret. Go hosts a row
/// with both empty and every refresh then fails at Google — measured, and not
/// something to improve on here.
pub(super) fn google_credentials(id: &str, credentials: &str) -> Result<(String, String), String> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct GoogleCredentials {
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        client_id: String,
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        client_secret: String,
    }

    if credentials.is_empty() {
        return Err(format!(
            "parsing google credentials for {id:?}: credentials are empty"
        ));
    }
    // The three rules: a `null` is a no-op, an array is not a struct, and every
    // field takes a JSON null as its zero value.
    serde_json::from_str::<Option<crate::native::gojson::GoStruct<GoogleCredentials>>>(credentials)
        .map(|wrapped| wrapped.map_or_else(GoogleCredentials::default, |wrapped| wrapped.0))
        .map(|creds| (creds.client_id, creds.client_secret))
        .map_err(|e| {
            // The serde message is dropped for `github_token`'s reason: it
            // quotes the offending value, which here is the client secret.
            format!(
                "parsing google credentials for {id:?}: does not decode at line {} column {}",
                e.line(),
                e.column()
            )
        })
}

/// `cfg.ParseOAuthToken()` for Google, wrapped as `buildHTTPClient` wraps it.
///
/// The interesting field is `expiry`, a Go `time.Time`, so **the whole token is
/// refused when it does not parse** — an integration with a corrupt expiry is not
/// hosted at all rather than hosted with a token that never refreshes. Slack's
/// equivalent deliberately skips this check because it reads only
/// `access_token`; here the value decides the refresh.
pub(super) fn google_oauth_token(
    id: &str,
    auth: &str,
) -> Result<super::google::client::Token, String> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct OAuthToken {
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        access_token: String,
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        refresh_token: String,
        /// Absent, `null` and a valid RFC3339 string are all legal; anything
        /// else fails the document. `Option<String>` rather than a time type
        /// because the *shape* check and the *value* check produce different Go
        /// sentences, and only a raw string can tell them apart.
        expiry: Option<serde_json::Value>,
        /// Declared but unread, and that is the point: Go decodes into the whole
        /// `oauth2.Token`, so `{"token_type": 5}` fails the **document**. A
        /// three-field struct here silently hosted a row Go refuses.
        ///
        /// Its value is deliberately not used for the `Authorization` scheme —
        /// see `google::client`'s header on the hardcoded `Bearer`.
        #[allow(dead_code)]
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        token_type: String,
        /// Same: declared for the type check. The transport reads `expiry`.
        #[allow(dead_code)]
        #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
        expires_in: i64,
    }

    let wrap = |message: String| format!("parsing auth token for {id:?}: {message}");

    let parsed = serde_json::from_str::<Option<crate::native::gojson::GoStruct<OAuthToken>>>(auth)
        .map(|wrapped| wrapped.map_or_else(OAuthToken::default, |wrapped| wrapped.0))
        .map_err(|e| {
            // Go's carries `encoding/json`'s wording; this drops it because the
            // serde message quotes the offending value, which here is the
            // access token.
            wrap(format!(
                "parsing oauth token: does not decode at line {} column {}",
                e.line(),
                e.column()
            ))
        })?;

    let expiry = match parsed.expiry {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(raw)) => {
            let parsed = crate::native::gotime::parse_rfc3339(&raw)
                .ok_or_else(|| wrap(format!("parsing oauth token: parsing time {raw:?}")))?;
            // **Go's zero `time.Time` is a valid parse of a token that never
            // expires**, not a failure and not an instant in year 1.
            // `omitempty` does not suppress a struct, so `SetOAuthToken` emits
            // `"expiry":"0001-01-01T00:00:00Z"` rather than omitting the key,
            // and `Token.Valid()` then reuses that token forever. Read as an
            // ordinary instant it is permanently expired — the inverse.
            (parsed.naive_utc() != crate::native::gotime::ZERO)
                .then(|| std::time::SystemTime::from(parsed.to_utc()))
        }
        // `Time.UnmarshalJSON` checks the JSON shape before the layout.
        Some(_) => {
            return Err(wrap(
                "parsing oauth token: Time.UnmarshalJSON: input is not a JSON string".to_string(),
            ))
        }
    };

    Ok(super::google::client::Token {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expiry,
    })
}

/// The row's `type`, and **nothing else** — no `credentials`, no `auth`.
///
/// The point is what it does not select. [`HOSTING_COLUMNS`] is the one
/// projection in the port that reads the secret columns, and its charter is to
/// read them *to build a tool server*. Two callers only want to know whether the
/// type is one this process hosts, and both are on hot paths for rows it does
/// not: [`can_host`] runs per chat turn, and the reload paths run
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

/// Whether a run naming this integration can be served natively at all.
///
/// Separate from [`start_filtered_server`] because the caller has to decide
/// *before* it starts anything: an agent that names a type this build cannot
/// host must have its whole turn refused rather than run with some of its tools
/// silently missing.
///
/// [`hosts_type`] is the predicate and [`type_of`] is the whole of the read —
/// one column, chosen because this question has no business touching the other
/// two. The two writes in `native/integrations.rs` reach [`hosts_type`] by yet
/// another route: they have already read the row for their own purposes, so they
/// call it directly rather than reading anything a second time.
pub fn can_host(db_path: &Path, id: &str) -> Result<bool, String> {
    Ok(hostable_type(db_path, id)?.is_some())
}

/// [`can_host`], answering **which** type it is rather than only whether it is
/// one — `Some` exactly when `can_host` is true.
///
/// The type is the only readable name this process has for an integration: the
/// id is a v4 UUID (`native/integrations.rs`), the string a user never sees, and
/// #556 puts a sentence about a broken server in front of them. It is one read
/// either way, so the caller that needs the word takes this and the two that do
/// not keep the boolean.
pub fn hostable_type(db_path: &Path, id: &str) -> Result<Option<String>, String> {
    Ok(type_of(db_path, id)?.filter(|integration_type| hosts_type(integration_type)))
}

/// `filterConfigTools`: keep only the requested tools, of only the enabled
/// services.
///
/// Two halves that both look like oversights and are not: an **empty** request
/// list returns the services untouched (including disabled ones, which the
/// starters skip on their own), and a service left with no kept tools is dropped
/// entirely rather than kept empty — which matters, because an empty `tools`
/// list is what `buildAllowedSet` reads as "host everything".
///
/// The third half is the one that **was** an oversight (#501). A service stored
/// as `{"enabled": true}` — no `tools` key at all, which is what
/// `POST /api/integrations` accepts and what rows written before the per-service
/// lists existed carry — has nothing to intersect the request against, so it
/// came out empty and was dropped by the rule above. `build_allowed_set` then
/// saw an **empty map**, every `service_enabled` answered false, and the
/// integration hosted **zero tools** — with `--allowedTools` still naming them,
/// so the only symptom was a model that did not know about its own tools.
///
/// A service naming no tools of its own means "every tool of **this** service",
/// so it is given the request narrowed to its own tool set — `table`, which is
/// the integration's `SERVICE_TOOLS`.
///
/// **Handing it the whole request instead is unsound, and that was the first
/// attempt at this fix.** The tempting argument is that a name from another
/// group is rejected by the service gate anyway, so the widening is harmless.
/// It is not, because `build_allowed_set` is a union over *every* enabled
/// service and `push` is `service_enabled(group) && allowed.contains(name)`:
/// the union is integration-wide, so a name injected by a listless service
/// satisfies the `allowed` half for a **sibling** service — and that sibling's
/// gate passes too, since it is enabled. So
/// `{"gmail":{"enabled":true},"drive":{"enabled":true,"tools":["list_files"]}}`
/// with a request naming `create_file` would host `create_file`, a write tool
/// the user's own Drive list deliberately excludes. Narrowing per service is
/// what makes the widening argument true rather than nearly true.
///
/// An unknown integration type has an empty table, so a listless service under
/// one contributes nothing — the same answer as before #501, and the starter
/// refuses that row a few lines later regardless.
fn filter_config_tools(
    services: &BTreeMap<String, ServiceConfig>,
    tools: &[String],
    table: &[(&str, &[&str])],
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
        let listed: &[String] = service.tools.as_ref().map_or(&[], |list| list.as_slice());
        let kept: Vec<String> = if listed.is_empty() {
            let own: &[&str] = table
                .iter()
                .find(|(group, _)| group == name)
                .map_or(&[], |(_, tools)| *tools);
            // Request order, since the service supplied none of its own.
            tools
                .iter()
                .filter(|tool| own.contains(&tool.as_str()))
                .cloned()
                .collect()
        } else {
            listed
                .iter()
                .filter(|tool| want.contains(tool.as_str()))
                .cloned()
                .collect()
        };
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

/// Which tools each of an integration type's service groups registers.
///
/// The one place the six `SERVICE_TOOLS` tables are dispatched on, so
/// [`filter_config_tools`] stays type-agnostic. A type this build does not host
/// has no table; [`start_filtered_server`]'s own `match` is what answers it.
fn service_tool_table(
    integration_type: &str,
) -> &'static [(&'static str, &'static [&'static str])] {
    match integration_type {
        "github" => super::github::SERVICE_TOOLS,
        "confluence" => super::confluence::SERVICE_TOOLS,
        "jira" => super::jira::SERVICE_TOOLS,
        "slack" => super::slack::SERVICE_TOOLS,
        "telegram" => super::telegram::SERVICE_TOOLS,
        "google" => super::google::SERVICE_TOOLS,
        _ => &[],
    }
}

// ─── Reads ────────────────────────────────────────────────────────────────────

/// The **secrets** projection. Every column of it stays inside this module.
///
/// Deliberately not built on `INTEGRATION_COLUMNS`, which is the projection the
/// response types are scanned from: sharing one would put `credentials` one
/// `SELECT` away from every read in `native/integrations.rs`, and the whole
/// point of that projection is that the column's *value* is not in it. It names
/// `credentials` once, inside `auth_mode_sql`, which can return nothing but a
/// known discriminator — the exception that is enforced in SQL rather than
/// promised in a comment. This one selects the column itself.
const HOSTING_COLUMNS: &str = "SELECT id, type, enabled,
            (auth IS NOT NULL AND auth != '' AND auth != 'null') AS authenticated,
            credentials, services, COALESCE(auth, ''), inbound_enabled
     FROM integrations";

fn scan_hosting_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HostingRow> {
    let enabled: i64 = row.get(2)?;
    let authenticated: i64 = row.get(3)?;
    let services: String = row.get(5)?;
    let inbound_enabled: i64 = row.get(7)?;
    Ok(HostingRow {
        id: row.get(0)?,
        integration_type: row.get(1)?,
        enabled: enabled != 0,
        authenticated: authenticated != 0,
        credentials: row.get(4)?,
        services: decode_services(&services),
        auth: row.get(6)?,
        inbound_enabled: inbound_enabled != 0,
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

pub(crate) fn get_for_hosting(db_path: &Path, id: &str) -> Result<Option<HostingRow>, String> {
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
/// written, server dead" is the accepted outcome. Turning it into a
/// `WriteError::Fallback` would be much worse: a 500 reporting failure for a
/// write that landed, inviting a retry that applies it again.
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

    const SLACK_SERVICES: &str =
        r#"{"messaging":{"enabled":true,"tools":["send_message","list_channels"]}}"#;
    const SLACK_APP_TOKEN: &str = "xapp-1-A000-1111-secret";

    fn slack_credentials(app_token: Option<&str>) -> String {
        match app_token {
            Some(token) => format!(
                r#"{{"auth_mode":"bot_token","bot_token":"xoxb-test","app_token":"{token}"}}"#
            ),
            None => r#"{"auth_mode":"bot_token","bot_token":"xoxb-test"}"#.to_string(),
        }
    }

    fn set_inbound(file: &tempfile::NamedTempFile, id: &str, enabled: bool) {
        Connection::open(file.path())
            .expect("open")
            .execute(
                "UPDATE integrations SET inbound_enabled = ?1 WHERE id = ?2",
                rusqlite::params![i64::from(enabled), id],
            )
            .expect("set the switch");
    }

    /// A Slack row carries **two** handles or one, and the switch plus the
    /// stored token is what decides which — over the same `start_all` / `reload`
    /// / `stop` path the MCP server already travels (#567).
    ///
    /// The three negative cases are the point. A socket started for a row with
    /// the switch off would ignore `PUT /api/integrations/{id}/inbound`
    /// entirely; one started for a row with no `app_token` would open a
    /// connection with an empty bearer and report `error` forever; and a socket
    /// left running by a reload that turned the switch off is a socket holding a
    /// credential for a state the user has just left — the same failure the
    /// generation counter exists to prevent for listeners.
    #[tokio::test]
    async fn a_slack_socket_worker_follows_the_switch_and_the_stored_token() {
        // Pointed at a local fake, never at slack.com: the worker's first act is
        // an `apps.connections.open`, and a test that reached the real one would
        // pass offline for the wrong reason.
        let _guard = super::super::slack::client::api_base_lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let app = axum::Router::new().fallback(|| async {
                // What Slack answers for a token it will not accept.
                (
                    axum::http::StatusCode::OK,
                    r#"{"ok":false,"error":"invalid_auth"}"#,
                )
            });
            let _ = axum::serve(listener, app).await;
        });
        super::super::slack::client::set_api_base(Some(format!("http://{addr}")));

        let file = db();
        // Inbound on, token stored: both handles.
        insert(
            &file,
            "sl-on",
            "slack",
            true,
            Some(r#"{"validated":true}"#),
            &slack_credentials(Some(SLACK_APP_TOKEN)),
            SLACK_SERVICES,
        );
        // Inbound off: server only.
        insert(
            &file,
            "sl-off",
            "slack",
            true,
            Some(r#"{"validated":true}"#),
            &slack_credentials(Some(SLACK_APP_TOKEN)),
            SLACK_SERVICES,
        );
        // Inbound on but nothing to authenticate with: server only.
        insert(
            &file,
            "sl-tokenless",
            "slack",
            true,
            Some(r#"{"validated":true}"#),
            &slack_credentials(None),
            SLACK_SERVICES,
        );
        set_inbound(&file, "sl-on", true);
        set_inbound(&file, "sl-tokenless", true);

        start_all(file.path()).await.expect("start_all");
        for id in ["sl-on", "sl-off", "sl-tokenless"] {
            assert!(registry().is_hosted(id), "{id} must be hosted");
        }
        assert!(registry().is_socket_running("sl-on"));
        assert!(!registry().is_socket_running("sl-off"));
        assert!(!registry().is_socket_running("sl-tokenless"));

        // A status left behind by a worker that is no longer running is the
        // registry's to clear, because it is the only thing that can tell a stop
        // from a replacement — see `slack::socket::clear_status_blocking`.
        let status_of = |id: &str| -> (String, String) {
            Connection::open(file.path())
                .expect("open")
                .query_row(
                    "SELECT inbound_status, inbound_error FROM integrations WHERE id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("read the inbound state")
        };
        assert_eq!(
            status_of("sl-off"),
            (String::new(), String::new()),
            "a row with the switch off must not carry an inbound status"
        );

        // Turning the switch off and reloading retires the worker without
        // touching the hosted server, and takes the status with it.
        crate::native::integrations::slack::socket::write_status_blocking(
            file.path(),
            "sl-on",
            crate::native::integrations::slack::socket::STATUS_CONNECTED,
            "",
        );
        set_inbound(&file, "sl-on", false);
        reload(file.path(), "sl-on").await.expect("reload");
        assert!(registry().is_hosted("sl-on"));
        assert!(
            !registry().is_socket_running("sl-on"),
            "a reload with the switch off must retire the worker"
        );
        assert_eq!(
            status_of("sl-on"),
            (String::new(), String::new()),
            "and must not leave `connected` in a row whose worker is gone"
        );

        // The same for a row that stops being startable at all: disabling the
        // integration is a second way to end up with no worker.
        set_inbound(&file, "sl-off", true);
        reload(file.path(), "sl-off").await.expect("reload");
        assert!(registry().is_socket_running("sl-off"));
        Connection::open(file.path())
            .expect("open")
            .execute(
                "UPDATE integrations SET enabled = 0 WHERE id = 'sl-off'",
                [],
            )
            .expect("disable");
        crate::native::integrations::slack::socket::write_status_blocking(
            file.path(),
            "sl-off",
            crate::native::integrations::slack::socket::STATUS_CONNECTED,
            "",
        );
        reload(file.path(), "sl-off").await.expect("reload");
        assert_eq!(
            status_of("sl-off"),
            (String::new(), String::new()),
            "a disabled integration must not keep reporting `connected`"
        );

        // And back on again.
        set_inbound(&file, "sl-on", true);
        reload(file.path(), "sl-on").await.expect("reload");
        assert!(registry().is_socket_running("sl-on"));

        for id in ["sl-on", "sl-off", "sl-tokenless"] {
            registry().stop(id);
            assert!(!registry().is_hosted(id));
            assert!(!registry().is_socket_running(id));
        }
        super::super::slack::client::set_api_base(None);
    }

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

    /// The Google starter end to end, which nothing else covers.
    ///
    /// `a_hosted_type_always_has_a_starter` inserts empty credentials, so for
    /// google it only ever reaches the `credentials are empty` arm — it proves
    /// the dispatch arm exists and nothing more. `tests_vectors` covers
    /// `start_google_mcp_server` with a hand-built `TokenSource`. What neither
    /// touches is the glue this PR added: the precheck, the two parses being
    /// wired to the columns they belong to, and the order of
    /// `TokenSource::new`'s arguments — a swap there would send the client id as
    /// the secret and fail only at refresh time, months later, on somebody's
    /// laptop.
    #[tokio::test]
    async fn a_google_integration_starts_reloads_and_stops() {
        let file = db();
        insert(
            &file,
            "gg-1",
            "google",
            true,
            // An hour out, so nothing tries to refresh while the test runs.
            Some(
                r#"{"access_token":"ya29.test","refresh_token":"1//test","expiry":"2099-01-01T00:00:00Z"}"#,
            ),
            r#"{"client_id":"cid.apps.googleusercontent.com","client_secret":"GOCSPX-test"}"#,
            r#"{"gmail":{"enabled":true,"tools":["send_email"]}}"#,
        );

        start_all(file.path()).await.expect("start_all");
        assert!(registry().is_hosted("gg-1"), "a valid google row must host");

        reload(file.path(), "gg-1").await.expect("reload");
        assert!(registry().is_hosted("gg-1"));

        registry().stop("gg-1");
        assert!(!registry().is_hosted("gg-1"));
    }

    /// The columns land in the right parameters — the failure that would
    /// otherwise surface only at refresh time.
    #[test]
    fn an_app_token_is_read_exactly_when_the_scrubbed_read_reports_one() {
        let file = db();
        // The blobs the two spellings could disagree about: an absent key, a
        // JSON null, an empty and a whitespace-only string, a non-string value,
        // and one that has to be trimmed before it counts.
        let shapes = [
            (
                "none",
                r#"{"auth_mode":"bot_token","bot_token":"xoxb-1"}"#,
                None,
            ),
            ("null", r#"{"app_token":null}"#, None),
            ("empty", r#"{"app_token":""}"#, None),
            ("blank", r#"{"app_token":"  \t "}"#, None),
            ("number", r#"{"app_token":7}"#, None),
            ("broken", "not json at all", None),
            ("plain", r#"{"app_token":"xapp-1-abc"}"#, Some("xapp-1-abc")),
            (
                "padded",
                r#"{"app_token":" xapp-1-abc\n"}"#,
                Some("xapp-1-abc"),
            ),
            // Escaped in the blob, so the decoded token is not a slice of it —
            // which is why the accessor hands back an owned `String`.
            (
                "escaped",
                r#"{"app_token":"xapp-\u0031-abc"}"#,
                Some("xapp-1-abc"),
            ),
        ];
        for (id, credentials, want) in shapes {
            insert(&file, id, "slack", true, None, credentials, "{}");
            let row = get_for_hosting(file.path(), id)
                .expect("read")
                .expect("a row");
            assert_eq!(row.app_token().as_deref(), want, "{id}: {credentials}");
            // The third spelling of one rule: the wire says whether a token is
            // stored, and the worker then has to find one. They must not
            // disagree — a switch that enables and a worker that cannot
            // connect is the failure this pins.
            assert_eq!(
                row.app_token().is_some(),
                crate::native::integrations::stores_an_app_token(credentials),
                "{id}: the accessor and the scrubbed read disagree"
            );
        }
    }

    #[test]
    fn the_credential_columns_are_not_swapped() {
        let (client_id, client_secret, token) = google_start_inputs(
            "gg-1",
            r#"{"client_id":"CID","client_secret":"SECRET"}"#,
            r#"{"access_token":"ACCESS","refresh_token":"REFRESH","expiry":"2099-01-01T00:00:00Z"}"#,
        )
        .expect("a valid row");
        assert_eq!(client_id, "CID");
        assert_eq!(client_secret, "SECRET");
        assert_eq!(token.access_token, "ACCESS");
        assert_eq!(token.refresh_token, "REFRESH");
        assert!(token.expiry.is_some());

        // Go's zero time is the sentinel its own writer emits, and it means
        // "never expires" — not "expired in the year 1".
        let (_, _, zero) = google_start_inputs(
            "gg-1",
            r#"{"client_id":"CID","client_secret":"SECRET"}"#,
            r#"{"access_token":"ACCESS","expiry":"0001-01-01T00:00:00Z"}"#,
        )
        .expect("a zero expiry is a valid token");
        assert!(
            zero.expiry.is_none(),
            "Go's zero time must reach the token source as `never expires`"
        );
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

    /// The six ported types, and only them: `whatsapp` was dropped with #273
    /// and has no starter here — since #278 there is no other process hosting
    /// it either, so its endpoints are gone rather than delegated.
    #[test]
    fn the_hosted_types_are_the_six_ported_integrations() {
        assert_eq!(
            HOSTED_TYPES,
            &[
                "github",
                "confluence",
                "jira",
                "slack",
                "telegram",
                "google"
            ]
        );
        for t in HOSTED_TYPES {
            assert!(hosts_type(t));
        }
        assert!(!hosts_type("whatsapp"), "whatsapp is dropped, not hosted");
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

    /// The same coverage question for the *second* dispatch on `HOSTED_TYPES`,
    /// and it is the one that fails silently.
    ///
    /// `service_tool_table`'s `_ => &[]` arm is the right answer for a type this
    /// build does not host — the starter refuses that row anyway. For a type
    /// that **is** hosted it is #501 reintroduced with nothing to report it:
    /// every service storing no tool list of its own goes back to being dropped,
    /// so the integration hosts zero tools while `--allowedTools` names them.
    /// A missing `match` arm is exactly how that lands.
    #[test]
    fn a_hosted_type_always_has_a_service_tool_table() {
        for integration_type in HOSTED_TYPES {
            let table = service_tool_table(integration_type);
            assert!(
                !table.is_empty(),
                "{integration_type} is in HOSTED_TYPES with no SERVICE_TOOLS table"
            );
            assert!(
                table.iter().all(|(_, tools)| !tools.is_empty()),
                "{integration_type} has a service group naming no tools"
            );
        }
    }

    /// A start that *fails* still clears the status of the worker it replaced.
    ///
    /// `reload` stops the previous worker before it tries to start the next one,
    /// so a start that then fails leaves a row with no worker — and returning
    /// the `Err` without clearing would leave the dead worker's `connected`
    /// standing until the next boot. That path is the one every other no-worker
    /// branch of `start_one` is careful about, and it is the one reached by an
    /// `Err`, so it is easy to write and easy to forget.
    ///
    /// The row here is *startable* — enabled and authenticated — but its blob
    /// carries no usable token, which is what `resolve_slack_token` refuses.
    #[tokio::test]
    async fn a_start_that_fails_still_clears_the_status_it_replaced() {
        let file = db();
        insert(
            &file,
            "sl-broken",
            "slack",
            true,
            Some(r#"{"validated":true}"#),
            // `auth_mode` says bot token and there is none: startable by the
            // registry's test, refused by the Slack starter.
            r#"{"auth_mode":"bot_token","bot_token":"","app_token":"xapp-1-A000-1111-secret"}"#,
            SLACK_SERVICES,
        );
        set_inbound(&file, "sl-broken", true);
        crate::native::integrations::slack::socket::write_status_blocking(
            file.path(),
            "sl-broken",
            crate::native::integrations::slack::socket::STATUS_CONNECTED,
            "",
        );

        reload(file.path(), "sl-broken")
            .await
            .expect_err("the start must fail");

        let (status, error): (String, String) = Connection::open(file.path())
            .expect("open")
            .query_row(
                "SELECT inbound_status, inbound_error FROM integrations WHERE id = 'sl-broken'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read the inbound state");
        assert_eq!(
            (status, error),
            (String::new(), String::new()),
            "a failed start must not leave the previous worker's `connected` standing"
        );
        assert!(!registry().is_socket_running("sl-broken"));
        registry().stop("sl-broken");
    }

    /// A socket worker the registry **refused** never takes the epoch from the
    /// one it accepted.
    ///
    /// This is the failure that made the epoch the registry's rather than the
    /// worker's. A worker is built before the decision — `start_for_type` is
    /// `async` and can be slow — so with the epoch claimed at spawn, two
    /// overlapping `reload`s whose starts finish out of order would have the
    /// *refused* worker holding the later epoch. `status_writer` would then
    /// discard every status the accepted worker ever posted, freezing the row on
    /// whatever it last held — a `connected` with nothing connected, which is
    /// the exact outage the column exists to make visible. Granting the epoch
    /// inside `put_if_current` is what makes epoch order and acceptance order
    /// the same order.
    #[tokio::test]
    async fn a_refused_socket_never_takes_the_epoch_from_the_accepted_one() {
        let file = db();
        insert(
            &file,
            "gh-epoch",
            "github",
            true,
            Some(r#"{"ok":true}"#),
            &github_credentials(),
            GITHUB_SERVICES,
        );

        let worker = |id: &str| {
            crate::native::integrations::slack::socket::start(
                file.path(),
                id,
                SLACK_APP_TOKEN,
                // Pointed at a port nothing is listening on: this test is about
                // the epoch, and the worker must not reach the network for it.
                crate::native::integrations::slack::socket::SocketOptions {
                    api_base: "http://127.0.0.1:1".to_string(),
                    ..Default::default()
                },
            )
        };

        // The accepted start: it reads the generation, then records.
        let accepted_generation = registry().generation("gh-epoch");
        let accepted = worker("gh-epoch");
        let server = start_filtered_server(file.path(), "gh-epoch", &[])
            .await
            .expect("a server for the accepted start");
        assert!(matches!(
            registry().put_if_current("gh-epoch", accepted_generation, server, Some(accepted)),
            Recorded::WithSocket
        ));
        assert!(registry().is_socket_running("gh-epoch"));

        // The slower start, which read an older generation and is refused. It
        // was *built* after the accepted one, so a spawn-time claim would have
        // given it the newer epoch.
        let stale_generation = accepted_generation.wrapping_sub(1);
        let refused = worker("gh-epoch");
        let server = start_filtered_server(file.path(), "gh-epoch", &[])
            .await
            .expect("a server for the refused start");
        assert!(matches!(
            registry().put_if_current("gh-epoch", stale_generation, server, Some(refused)),
            Recorded::Refused
        ));

        // The accepted worker is still the one that may report. Asserting it
        // through `socket_epoch_is_current` rather than the map, because being
        // *recorded* was never the thing at risk — being *heard* was.
        assert!(
            registry().is_socket_running("gh-epoch"),
            "the refused start must not have displaced the accepted worker"
        );
        let epoch_now = {
            let state = registry().lock();
            state.socket_epochs.get("gh-epoch").copied().unwrap_or(0)
        };
        assert_eq!(
            epoch_now, 1,
            "exactly one epoch was granted, to the accepted worker"
        );
        assert!(registry().socket_epoch_is_current("gh-epoch", 1));
        assert!(
            !registry().socket_epoch_is_current(
                "gh-epoch",
                crate::native::integrations::slack::socket::NOT_ACCEPTED
            ),
            "a worker that was never accepted matches nothing"
        );

        // And a retire moves it on, which is what a clear quotes.
        let retired = registry().retire_socket("gh-epoch");
        assert_eq!(retired, 2);
        assert!(!registry().socket_epoch_is_current("gh-epoch", 1));
        assert!(registry().socket_epoch_is_current("gh-epoch", 2));
        assert!(!registry().is_socket_running("gh-epoch"));
        registry().stop("gh-epoch");
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
            matches!(
                registry().put_if_current("gh-race", generation, server, None),
                Recorded::Refused
            ),
            "a handle whose generation has moved must be refused"
        );
        assert!(!registry().is_hosted("gh-race"));

        // …and the same handle is kept when nothing intervened.
        let generation = registry().generation("gh-race");
        let server = start_filtered_server(file.path(), "gh-race", &[])
            .await
            .expect("server");
        assert!(!matches!(
            registry().put_if_current("gh-race", generation, server, None),
            Recorded::Refused
        ));
        assert!(registry().is_hosted("gh-race"));
        registry().stop("gh-race");
    }

    /// A type with no starter is Go's unregistered-type path: an error that the
    /// callers swallow, and a row that is simply not hosted.
    #[tokio::test]
    async fn an_unported_type_fails_the_way_gos_unregistered_type_does() {
        let file = db();
        // `whatsapp` is the stand-in now that #313 has landed google. It is not
        // a placeholder waiting for a port: its starter opens a live whatsmeow
        // connection registered in a package global, so it stays Go's.
        insert(
            &file,
            "wa-1",
            "whatsapp",
            true,
            Some(r#"{"validated":true}"#),
            r#"{"session":"wa-secret"}"#,
            r#"{"messaging":{"enabled":true,"tools":["send"]}}"#,
        );

        // `start_all` swallows it: one bad integration must not stop the others.
        start_all(file.path()).await.expect("start_all swallows it");
        assert!(!registry().is_hosted("wa-1"));

        // `reload` returns it, and its callers are the ones that swallow.
        let err = reload(file.path(), "wa-1").await.expect_err("no starter");
        assert_eq!(
            err,
            r#"no starter registered for integration type "whatsapp""#
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

    /// `google.Start`'s accept/reject boundary, replayed from the `starting`
    /// section of `desktop/parity/google_vectors.json`.
    ///
    /// The other five integrations' credential parses are asserted by hand.
    /// Google's is measured, for one reason: its `auth` column decodes as an
    /// `oauth2.Token` whose `expiry` is a Go `time.Time`, so whether a stored
    /// value is accepted is `time.Parse`'s decision and not something to guess
    /// at. Five vectors are exactly where `chrono` and Go disagree, in both
    /// directions.
    ///
    /// It calls [`google_start_inputs`] — the function [`start_google`] itself
    /// calls — rather than re-chaining the three checks, because the **order** is
    /// half of what this pins: two vectors have more than one thing wrong, and a
    /// test that rebuilt the chain would agree with itself whatever the starter
    /// did.
    ///
    /// Sentences are compared exactly wherever Go's is reproducible. The two
    /// classes that are not carry `rust_error` in the vector: `time.Parse`'s
    /// layout-diffing wording, and `encoding/json`'s — the second dropped on
    /// purpose, since serde's quotes the value, which here is a credential.
    #[test]
    fn google_start_accepts_exactly_what_go_accepts() {
        #[derive(serde::Deserialize)]
        struct StartVector {
            case: String,
            credentials: String,
            auth: String,
            error: String,
            #[serde(default)]
            rust_error: String,
        }
        #[derive(serde::Deserialize)]
        struct Vectors {
            starting: Vec<StartVector>,
        }

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/google_vectors.json");
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
        let vectors: Vectors = serde_json::from_str(&raw).expect("parsing the google vectors");
        assert!(vectors.starting.len() >= 30, "vectors look partial");

        // Collected rather than asserted one at a time: a change to a shared
        // sentence moves many cases at once, and seeing only the first is how a
        // regeneration turns into several rounds.
        let mut mismatches: Vec<String> = Vec::new();
        for case in &vectors.starting {
            let got = google_start_inputs("goog-parity", &case.credentials, &case.auth);

            assert_eq!(
                got.is_err(),
                !case.error.is_empty(),
                "{}: Go said {:?}, this said {:?}",
                case.case,
                case.error,
                got.as_ref().err()
            );

            match got {
                Ok((client_id, client_secret, _)) => {
                    // A swapped pair would only surface at refresh time, so the
                    // happy path asserts which column landed where.
                    assert!(
                        client_secret.is_empty() || client_secret.starts_with("GOCSPX"),
                        "{}: the client secret is not the secret column",
                        case.case
                    );
                    assert!(
                        !client_id.starts_with("GOCSPX"),
                        "{}: the client id carries the secret",
                        case.case
                    );
                }
                Err(message) => {
                    // Whatever it says, it must never name a credential.
                    assert!(
                        !message.contains("GOCSPX") && !message.contains("1//"),
                        "{}: a credential leaked into a refusal: {message}",
                        case.case
                    );
                    let want = if case.rust_error.is_empty() {
                        &case.error
                    } else {
                        &case.rust_error
                    };
                    if &message != want {
                        mismatches.push(format!(
                            "  {}\n    go/expected: {want:?}\n    rust:        {message:?}",
                            case.case
                        ));
                    }
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} refusal sentence(s) differ from the vectors:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
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

        // …and a JSON **array** is a type error to Go, where serde would build
        // the struct from it positionally. Measured against `json.Unmarshal`:
        // `["tok"]` is `cannot unmarshal array into Go value of type
        // config.GitHubCredentials`, so hosting a server for it would run one Go
        // refuses to start.
        assert!(github_token("gh-1", &format!(r#"[{PAT:?}]"#)).is_err());
    }

    /// The same three shapes for Telegram, which is the one integration where a
    /// bogus token becomes a **URL path segment** rather than a header value.
    ///
    /// Hand-written rather than vector-pinned — `telegram.Start`'s wrapper is not
    /// reachable from a `tools/call` — so the sentences are asserted here.
    #[test]
    fn a_telegram_credential_failure_never_carries_the_token() {
        const BOT: &str = "123456:AAF-secret-bot-token";

        assert_eq!(
            telegram_bot_token("tg-1", "").unwrap_err(),
            r#"parsing telegram credentials for "tg-1": credentials are empty"#
        );

        let err = telegram_bot_token("tg-1", &format!(r#"{{"bot_token":{BOT:?},"#))
            .expect_err("truncated json");
        assert!(
            !err.contains(BOT),
            "the bot token must not reach the log line: {err}"
        );
        assert!(err.contains("does not decode at line"), "{err}");

        // A literal `null` is a zero value; a JSON array is a type error.
        assert_eq!(
            telegram_bot_token("tg-1", "null").expect("null decodes"),
            ""
        );
        assert!(telegram_bot_token("tg-1", &format!(r#"[{BOT:?}]"#)).is_err());

        assert_eq!(
            telegram_bot_token("tg-1", &format!(r#"{{"bot_token":{BOT:?}}}"#)).expect("valid"),
            BOT
        );

        // An **empty** bot token is deliberately not refused: `telegram.Start`
        // hosts whatever it reads, so every call 404s at `/bot/<method>`.
        assert_eq!(
            telegram_bot_token("tg-1", r#"{"bot_token":""}"#).expect("empty is hosted"),
            ""
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

        let github = service_tool_table("github");

        // An empty request list is a no-op — including on the disabled service,
        // which the starter skips on its own.
        assert_eq!(filter_config_tools(&services, &[], github), services);

        let filtered = filter_config_tools(&services, &["get_repo".to_string()], github);
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
        let filtered = filter_config_tools(&services, &["list_workflows".to_string()], github);
        assert!(filtered.is_empty());
    }

    /// The third half (#501): a service that names **no** tools of its own is
    /// "every tool of this service", not "no tools".
    ///
    /// Both spellings reach it — `{"enabled":true}` with no key at all, which is
    /// what `POST /api/integrations` accepts and what pre-list rows carry, and
    /// `{"enabled":true,"tools":[]}`, which the Integrations UI could store
    /// until this issue. Before the fix both were dropped, `build_allowed_set`
    /// saw an empty map, and the integration hosted nothing at all while
    /// `--allowedTools` still named the tools.
    #[test]
    fn a_service_naming_no_tools_contributes_its_own_tools() {
        let google = service_tool_table("google");
        for spelling in [
            r#"{"gmail":{"enabled":true},"drive":{"enabled":true,"tools":["list_files"]}}"#,
            r#"{"gmail":{"enabled":true,"tools":[]},"drive":{"enabled":true,"tools":["list_files"]}}"#,
        ] {
            let services: BTreeMap<String, ServiceConfig> =
                serde_json::from_str(spelling).expect("services");
            let filtered = filter_config_tools(&services, &["read_email".to_string()], google);

            assert_eq!(
                filtered.keys().collect::<Vec<_>>(),
                vec!["gmail"],
                "the listless service survives and the listed one that names \
                 nothing requested is still dropped: {spelling}"
            );
            assert_eq!(
                filtered["gmail"].tools.as_ref().expect("tools").0,
                vec!["read_email".to_string()],
                "it is given the request narrowed to its own tools: {spelling}"
            );

            // **The sibling case, and the reason this is per service rather
            // than the whole request.** `build_allowed_set` unions every
            // enabled service, so a name the listless `gmail` entry carried
            // would satisfy `allowed` for `drive` — whose own list names only
            // `list_files`. `create_file` must not survive into either entry.
            let filtered = filter_config_tools(
                &services,
                &[
                    "read_email".to_string(),
                    "list_files".to_string(),
                    "create_file".to_string(),
                ],
                google,
            );
            assert_eq!(
                filtered["gmail"].tools.as_ref().expect("tools").0,
                vec!["read_email".to_string()],
                "a listless service takes no name belonging to a sibling: {spelling}"
            );
            assert_eq!(
                filtered["drive"].tools.as_ref().expect("tools").0,
                vec!["list_files".to_string()],
                "the sibling's own list still bounds it: {spelling}"
            );
        }
    }

    /// The same thing one level out: what the *server* ends up hosting.
    ///
    /// `filter_config_tools` producing a sensible map is only half the claim —
    /// the tools are chosen by each integration's `push`, which gates on the
    /// service **and** on an integration-wide union. This is where the
    /// interaction between those two is asserted rather than argued, because
    /// arguing it is exactly what went wrong: the first version of this fix gave
    /// a listless service the caller's *whole* request, and the union carried
    /// the surplus names into every other enabled service.
    #[test]
    fn a_service_with_no_tool_list_hosts_exactly_what_was_asked_for() {
        let google = service_tool_table("google");
        let tokens = std::sync::Arc::new(super::super::google::client::TokenSource::new(
            "CID",
            "CSECRET",
            super::super::google::client::Token {
                access_token: "ACCESS".to_string(),
                refresh_token: "REFRESH".to_string(),
                expiry: None,
            },
        ));
        let hosted = |services: &BTreeMap<String, ServiceConfig>| -> Vec<String> {
            super::super::google::google_tools(services, tokens.clone())
                .iter()
                .map(|tool| tool.name().to_string())
                .collect()
        };

        let services: BTreeMap<String, ServiceConfig> =
            serde_json::from_str(r#"{"gmail":{"enabled":true}}"#).expect("services");

        // The reported symptom: two Gmail tools requested, zero hosted.
        let want = ["read_email".to_string(), "search_email".to_string()];
        assert_eq!(
            hosted(&filter_config_tools(&services, &want, google)),
            want,
            "an enabled service with no stored tool list hosts the requested tools"
        );

        // A requested name belonging to a group that is not enabled at all is
        // rejected by the service gate.
        assert_eq!(
            hosted(&filter_config_tools(
                &services,
                &["read_email".to_string(), "list_files".to_string()],
                google
            )),
            ["read_email".to_string()],
            "a name from a service that is not enabled reaches nothing"
        );

        // …and the case the service gate does **not** cover, which is why the
        // narrowing above exists: `drive` is enabled, with its own list naming
        // only `list_files`. Handing `gmail` the whole request would put
        // `create_file` in the union, and `drive`'s gate would pass it.
        let mixed: BTreeMap<String, ServiceConfig> = serde_json::from_str(
            r#"{"gmail":{"enabled":true},"drive":{"enabled":true,"tools":["list_files"]}}"#,
        )
        .expect("services");
        assert_eq!(
            hosted(&filter_config_tools(
                &mixed,
                &[
                    "read_email".to_string(),
                    "list_files".to_string(),
                    "create_file".to_string(),
                ],
                google
            )),
            ["read_email".to_string(), "list_files".to_string()],
            "a write tool the user's own Drive list excludes is not hosted"
        );
    }

    /// #501's second criterion, settled and pinned: an **empty union stays
    /// "host everything"**.
    ///
    /// The alternative — an explicitly empty list meaning "no tools" — would
    /// diverge from behaviour that is ported deliberately, identical in all six
    /// integrations and asserted by each of their own suites. So the semantics
    /// are kept and the *inverting shape* is closed one level up instead:
    /// `IntegrationsView.tsx` turns a service **off** when its last tool is
    /// unchecked, so "Only the tools you leave on are exposed to agents" is true
    /// of everything the app can store.
    ///
    /// The gap that remains is the API's, and it is deliberate: `POST
    /// /api/integrations` still accepts `{"enabled":true,"tools":[]}` and that
    /// row hosts the service's whole tool set. Refusing it would be a wire
    /// change on a byte-exact endpoint.
    #[test]
    fn an_enabled_service_with_an_empty_list_still_hosts_everything() {
        let tokens = std::sync::Arc::new(super::super::google::client::TokenSource::new(
            "CID",
            "CSECRET",
            super::super::google::client::Token {
                access_token: "ACCESS".to_string(),
                refresh_token: "REFRESH".to_string(),
                expiry: None,
            },
        ));
        for spelling in [
            r#"{"gmail":{"enabled":true}}"#,
            r#"{"gmail":{"enabled":true,"tools":[]}}"#,
        ] {
            let services: BTreeMap<String, ServiceConfig> =
                serde_json::from_str(spelling).expect("services");
            // No agent-side request, so nothing narrows it — `filter_config_tools`
            // returns the map untouched and the union is empty.
            let hosted: Vec<String> = super::super::google::google_tools(
                &filter_config_tools(&services, &[], service_tool_table("google")),
                tokens.clone(),
            )
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
            assert_eq!(
                hosted,
                ["send_email", "read_email", "search_email"],
                "an empty union is 'host everything', bounded by the service gate: {spelling}"
            );
        }

        // **And "empty" is a property of the whole union, not of one group.**
        // The same listless `gmail` beside a service that *does* name a tool
        // hosts **nothing**: `build_allowed_set` unions every enabled service,
        // so the union is `{list_files}` and no Gmail name is in it. Reading
        // "this group stored no list" as "this group hosts everything" is the
        // mistake, and `IntegrationsView.tsx` has to make the same distinction
        // to render an untouched row honestly — which is why it is pinned here
        // rather than left to the UI to be right about on its own.
        let mixed: BTreeMap<String, ServiceConfig> = serde_json::from_str(
            r#"{"gmail":{"enabled":true},"drive":{"enabled":true,"tools":["list_files"]}}"#,
        )
        .expect("services");
        let hosted: Vec<String> = super::super::google::google_tools(
            &filter_config_tools(&mixed, &[], service_tool_table("google")),
            tokens.clone(),
        )
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
        assert_eq!(
            hosted,
            ["list_files"],
            "a sibling's stored list makes the union non-empty, so the listless \
             service matches nothing"
        );
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
        insert(&file, "wa-1", "whatsapp", true, Some("{}"), "{}", "{}");

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
            refusal("wa-1").await,
            r#"no starter registered for integration type "whatsapp""#
        );
        assert!(!can_host(file.path(), "wa-1").expect("can_host"));
    }
}

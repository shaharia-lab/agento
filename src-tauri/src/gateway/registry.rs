//! The gateway listener's lifecycle (#424).
//!
//! Shaped after [`crate::native::integrations::registry`], which settled this
//! problem for the MCP servers: a process-wide `OnceLock`, *the handle is the
//! cancel* (dropping it fires the shutdown), and a generation counter so a
//! `stop` racing a `reload` cannot leave a bound port holding a credential.
//! Two differences, both from there being one listener rather than a map:
//! the handle is an `Option` rather than a `HashMap`, and the generation is a
//! single `u64` rather than a per-id one.
//!
//! # Bind failure is a value, not a log line
//!
//! [`Status`] is **stored**, not derived from whether a handle exists, and that
//! is the whole reason it is an enum with a `BindFailed` variant rather than a
//! `bool`. The collision this exists for is routine rather than exotic: a
//! developer's `~/.agento-desktop-dev` instance and their installed
//! `~/.agento` one read *different* databases but share the machine's ports, so
//! the second to start finds the configured port taken. Without a stored
//! status, #426's `GET /api/gateway/status` would answer "not running" and the
//! UI would offer a Start button that does nothing, forever, with the reason
//! only in a log file nobody opens.
//!
//! A bind failure therefore records the address and the OS error, logs once at
//! `warn`, and does **not** retry on another port — a gateway on a port the
//! user did not configure is one every tool they set up is pointed away from.
//!
//! # Shutdown is graceful, and the precedent is `claude/mcp.rs`, not `proxy.rs`
//!
//! `proxy.rs` never stops: it is spawned once and the process exit is its
//! shutdown, so it has no `with_graceful_shutdown` to copy. This listener is
//! torn down on every settings write, which makes the window a request can be
//! in flight across an ordinary occurrence — a tool mid-completion when the
//! user changes the port. `claude/mcp.rs` solved exactly this for #311 and the
//! shape is taken from there: a oneshot fired by `Drop`, awaited as
//! `axum::serve`'s graceful-shutdown future, so in-flight requests drain.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use super::{config, dispatch::Dispatcher, server};

/// What the listener is doing, for #426's status route and the UI behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Not running — either disabled in settings, or stopped.
    Stopped,
    Running {
        port: u16,
    },
    /// The port was taken, or the socket could not be opened. Carries what to
    /// tell the user, because "gateway is off" would be a lie about a gateway
    /// they turned on.
    BindFailed {
        port: u16,
        error: String,
    },
    /// The gateway could not be built at all — a provider row this build cannot
    /// turn into an adapter.
    ///
    /// **Separate from [`Status::BindFailed`] because the two send the user to
    /// different places**, and it is reachable from an ordinary typo: a
    /// `base_url` ending in `/chat/completions` is refused by ferrox's OpenAI
    /// adapter, so folding it into `BindFailed` would have the UI say "port
    /// 8880 is already in use" about a port nothing ever tried to bind. There
    /// is no port here on purpose — none was reached.
    StartFailed {
        error: String,
    },
}

/// A live listener. Dropping it shuts the listener down gracefully.
struct Handle {
    /// `Option` only so `Drop` can take it; it is always `Some` while alive.
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Default)]
struct State {
    handle: Option<Handle>,
    /// Bumped by every [`stop`], never reset — the same protocol
    /// `integrations::registry` uses, and for the same race.
    generation: u64,
    status: Option<Status>,
}

pub struct Registry {
    state: Mutex<State>,
}

/// The process-wide registry.
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

    /// What to report to #426. `Stopped` before anything has ever run.
    pub fn status(&self) -> Status {
        self.lock().status.clone().unwrap_or(Status::Stopped)
    }

    /// The generation to quote to [`Registry::put_if_current`] later.
    ///
    /// Read **before** the settings read that decides whether to start, so a
    /// `stop` landing during that read is visible when the handle comes back.
    fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Stop the listener, if any, and mark it stopped.
    ///
    /// Idempotent. Dropping the handle inside the lock fires its oneshot;
    /// nothing is awaited here, so a request still draining upstream keeps
    /// draining while this returns.
    pub fn stop(&self) {
        let mut state = self.lock();
        state.generation += 1;
        let removed = state.handle.take();
        state.status = Some(Status::Stopped);
        drop(state);
        if removed.is_some() {
            log::info!("llm gateway stopped");
        }
    }

    /// Record a started listener, unless it was stopped while it was starting.
    ///
    /// A refused handle is dropped here, which fires its shutdown — so the
    /// socket the caller just bound goes away rather than outliving the
    /// settings that justified it, still holding every configured provider key.
    fn put_if_current(&self, generation: u64, handle: Handle, status: Status) -> bool {
        let mut state = self.lock();
        if state.generation != generation {
            return false;
        }
        state.handle = Some(handle);
        state.status = Some(status);
        true
    }

    /// Record a terminal status with no handle — a bind failure, or "disabled".
    ///
    /// Generation-checked for the same reason a handle is: a `stop` that landed
    /// mid-start has already written `Stopped`, and overwriting it with
    /// `BindFailed` would report a listener nobody asked for any more.
    fn record_if_current(&self, generation: u64, status: Status) -> bool {
        let mut state = self.lock();
        if state.generation != generation {
            return false;
        }
        state.status = Some(status);
        true
    }
}

/// Start the listener if `gateway_settings.enabled`, recording what happened.
///
/// Never returns `Err` for a configuration the user chose — a disabled gateway
/// and a port already in use are both *answers*, recorded in [`Status`]. The
/// `Err` arm is for a database this process cannot read, which is not a gateway
/// problem and is logged by the caller.
///
/// # Ordering
///
/// Must run strictly after [`crate::native::security::keys::install`] and
/// [`crate::native::security::tokens::load_revoked`]. Both are process-wide
/// statics the auth middleware reads per request, and a listener bound before
/// either would answer with no key installed (a 401 for every client, which
/// merely looks broken) or with an empty revoked set (a **revoked token
/// accepted**, for the length of that window, which does not).
/// `lib.rs` places the call, and `a_gateway_start_requires_an_installed_keypair`
/// is what fails if it is ever moved above them.
pub async fn start_if_enabled(db_path: &Path) -> Result<(), String> {
    // Before the settings read, exactly as `integrations::registry::start_all`
    // snapshots generations before it lists the table.
    let generation = registry().generation();

    let path = db_path.to_path_buf();
    let settings =
        crate::native::db::blocking("gateway settings", move || config::load_settings(&path))
            .await
            .ok_or_else(|| "reading gateway settings: the database task failed".to_string())??;

    if !settings.enabled {
        registry().record_if_current(generation, Status::Stopped);
        return Ok(());
    }

    let dispatcher = match Dispatcher::build(db_path).await {
        Ok(d) => std::sync::Arc::new(d),
        Err(e) => {
            // Reported rather than returned, for the same reason a taken port
            // is: the user configured something this build cannot serve, and
            // that is an answer they need to see rather than a boot task that
            // failed quietly. It is `StartFailed` and not `BindFailed` because
            // no port was reached — see the variant's doc.
            log::warn!("llm gateway not started: {e}");
            registry().record_if_current(generation, Status::StartFailed { error: e });
            return Ok(());
        }
    };

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], settings.port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            let error = format!("binding the llm gateway on {addr}: {e}");
            // Once, at warn, and no retry on another port. A dev instance and
            // an installed one configured for the same port collide here, and
            // that collision has to be legible rather than silently routed
            // around to a port no tool is pointed at.
            log::warn!("{error}");
            registry().record_if_current(
                generation,
                Status::BindFailed {
                    port: settings.port,
                    error,
                },
            );
            return Ok(());
        }
    };

    let bound = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(settings.port);

    let app = server::router(server::GatewayState {
        db_path: db_path.to_path_buf(),
        dispatcher,
    });

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
        if let Err(e) = served {
            log::error!("llm gateway stopped: {e}");
        }
    });

    let handle = Handle {
        shutdown: Some(shutdown_tx),
    };
    if !registry().put_if_current(generation, handle, Status::Running { port: bound }) {
        log::info!(
            "llm gateway discarded before it was recorded, \
             it was stopped while it started: port={bound}"
        );
        return Ok(());
    }

    log::info!("llm gateway listening on http://127.0.0.1:{bound}");
    Ok(())
}

/// Stop and start again, unconditionally.
///
/// No diff against what is running, for the reason
/// `integrations::registry::reload` gives: the thing most likely to have
/// changed is a provider's API key, comparing configuration to decide whether
/// to restart means holding that key to compare it, and a stale listener is a
/// *security* problem rather than a staleness one — it goes on spending
/// credits with a key the user just rotated away.
pub async fn reload(db_path: &Path) -> Result<(), String> {
    registry().stop();
    start_if_enabled(db_path).await
}

/// Stop the listener. Public for #426's disable path.
pub fn stop() {
    registry().stop();
}

/// What #426's `GET /api/gateway/status` answers with.
pub fn status() -> Status {
    registry().status()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status a fresh process reports, and the one every later assertion
    /// is relative to.
    #[test]
    fn a_registry_that_never_started_is_stopped() {
        let registry = Registry {
            state: Mutex::new(State::default()),
        };
        assert_eq!(registry.status(), Status::Stopped);
    }

    /// The race `put_if_current` exists for: a `stop` between the generation
    /// read and the handle coming back must discard the handle.
    ///
    /// Discarding it is what drops it, and dropping it is what closes the
    /// socket — so this is the assertion standing between a `DELETE`-shaped
    /// race and a bound port holding provider credentials for the life of the
    /// process.
    #[test]
    fn a_handle_whose_generation_moved_is_refused_and_dropped() {
        let registry = Registry {
            state: Mutex::new(State::default()),
        };
        let generation = registry.generation();

        // The stop lands while the listener is still starting.
        registry.stop();

        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        let handle = Handle { shutdown: Some(tx) };
        assert!(
            !registry.put_if_current(generation, handle, Status::Running { port: 8880 }),
            "a handle from a superseded generation must be refused"
        );
        assert_eq!(
            registry.status(),
            Status::Stopped,
            "the refused handle must not overwrite the stop's status"
        );
        assert!(
            rx.try_recv().is_ok(),
            "refusing the handle must drop it, which is what fires its shutdown"
        );
    }

    #[test]
    fn a_handle_from_the_current_generation_is_kept() {
        let registry = Registry {
            state: Mutex::new(State::default()),
        };
        let generation = registry.generation();
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        assert!(registry.put_if_current(
            generation,
            Handle { shutdown: Some(tx) },
            Status::Running { port: 8880 }
        ));
        assert_eq!(registry.status(), Status::Running { port: 8880 });
        assert!(
            rx.try_recv().is_err(),
            "a kept handle is still alive, so its shutdown must not have fired"
        );

        // ...and stopping it fires the shutdown and reports stopped.
        registry.stop();
        assert_eq!(registry.status(), Status::Stopped);
        assert!(
            rx.try_recv().is_ok(),
            "stop must fire the handle's shutdown"
        );
    }

    /// A bind failure is a stored value that survives being read, not a
    /// transient the next status call loses.
    #[test]
    fn a_bind_failure_is_recorded_and_stays_readable() {
        let registry = Registry {
            state: Mutex::new(State::default()),
        };
        let generation = registry.generation();
        assert!(registry.record_if_current(
            generation,
            Status::BindFailed {
                port: 8880,
                error: "address already in use".to_string(),
            }
        ));
        for _ in 0..3 {
            assert_eq!(
                registry.status(),
                Status::BindFailed {
                    port: 8880,
                    error: "address already in use".to_string(),
                },
                "#426 reads this more than once; it must not be consumed"
            );
        }
    }

    /// The half a `bool` status could not express: a stop that lands mid-start
    /// must not be overwritten by the failure of the start it cancelled.
    #[test]
    fn a_stop_mid_start_outranks_a_late_bind_failure() {
        let registry = Registry {
            state: Mutex::new(State::default()),
        };
        let generation = registry.generation();
        registry.stop();
        assert!(!registry.record_if_current(
            generation,
            Status::BindFailed {
                port: 8880,
                error: "address already in use".to_string(),
            }
        ));
        assert_eq!(registry.status(), Status::Stopped);
    }
}

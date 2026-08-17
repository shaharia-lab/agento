//! Supervises the bundled `agento` Go server.
//!
//! Phase 1 of the migration runs the original Go binary as a Tauri sidecar, so
//! the desktop app has exactly the backend behaviour the web app has — same
//! code, so there is nothing to keep in sync. Later phases move endpoints into
//! `proxy.rs`, which decides per-route whether to answer natively or forward
//! here.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// Handle to the running Go process, so it can be killed on window close.
/// Tauri kills sidecars on a clean exit, but not when the app is force-quit,
/// and an orphaned server would hold the SQLite lock against the next launch.
pub struct Sidecar {
    child: Mutex<Option<CommandChild>>,
}

impl Sidecar {
    pub fn shutdown(&self) {
        if let Some(child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
        }
    }
}

/// Ask the OS for a free TCP port by binding to :0 and immediately releasing.
///
/// This races in principle — another process could take the port between the
/// probe and the server's own bind. In practice the window is microseconds and
/// the alternative (a fixed port) fails far more often, because a second Agento
/// instance or a leftover process would collide every time.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("no free TCP port available")
}

/// Spawn the Go server and block until it answers /health.
pub async fn spawn(app: &AppHandle, port: u16) -> Result<Sidecar, String> {
    // Only the debug build reassigns this, to point at the dev data directory.
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut sidecar = app
        .shell()
        .sidecar("agento-server")
        .map_err(|e| format!("sidecar binary not found: {e}"))?
        .args(["web", "--port", &port.to_string(), "--no-browser"])
        // Bind loopback only. The Go server defaults to this too, but the
        // desktop app must never widen it, whatever the user's environment says.
        .env("AGENTO_BIND", "127.0.0.1")
        // The shell owns the Claude session scan (#289). Two writers on one
        // SQLite file is the hazard this port has been avoiding since #274, and
        // the scan is a writer — so the child must not run one. `EnsureScan` is
        // the single place a scan starts on the Go side, and this switches it
        // off there, which covers both the boot scan and every read path's
        // `ensureFresh`.
        //
        // This is not a tuning knob: with it unset the two processes would both
        // scan, and with the shell's scanner removed nothing would.
        .env("AGENTO_SCANNER", "off")
        // The shell owns the integration MCP servers **of the types it hosts**
        // (#311), for a sharper reason than the scan's. Go's
        // `StartInProcessMCPServer` binds an *unauthenticated* loopback listener
        // that closes over the credential it was started with; with
        // `PUT`/`DELETE /api/integrations/{id}` served natively the child never
        // hears `Reload`/`Stop`, so a token the user just revoked would keep
        // answering `tools/call` on an open port for the rest of the sidecar's
        // life.
        //
        // The value is a **list**, built from `registry::HOSTED_TYPES` rather
        // than written out here, and that is load-bearing in both directions. A
        // type the shell hosts and Go does not switch off is two processes on
        // one integration. A type Go switches off and the shell does not host is
        // hosted by nobody — which for `whatsapp` is not a spare port but the
        // feature: its starter opens a live whatsmeow WebSocket and registers
        // the client in a package global that the status, reconnect and QR
        // pairing endpoints read. Deriving the list from the starter table means
        // #313–#316 each add one string in one place.
        //
        // It does **not** switch off `StartFilteredServer`, which is what an
        // agent run uses: that reads the integration row afresh per run and
        // records nothing, so a chat or scheduled task the sidecar still serves
        // keeps reaching its integration tools.
        .env(
            "AGENTO_INTEGRATIONS",
            crate::native::integrations::registry::hosting_env_value(),
        )
        .env("PORT", port.to_string());

    // Development runs against its own data directory.
    //
    // Two Agento processes sharing ~/.agento share one SQLite file *and* one
    // scheduler, so a scheduled task would fire twice and the Telegram webhook
    // would be re-registered out from under whichever instance registered it
    // last. Release builds keep the default path — there the desktop app is
    // the user's Agento, not a second copy of it.
    //
    // Either way the directory comes from `paths::data_dir`, which the ported
    // endpoints in `native/` also use to open this same database: in debug it
    // is handed to the child explicitly, and in release it mirrors the
    // resolution the child makes from its own inherited environment. One
    // answer, so the two halves of the app cannot end up on different files.
    #[cfg(debug_assertions)]
    {
        if let Some(dir) = crate::paths::data_dir() {
            sidecar = sidecar.env("AGENTO_DATA_DIR", dir.to_string_lossy().to_string());
        }
    }

    let (mut rx, child) = sidecar
        .spawn()
        .map_err(|e| format!("failed to start agento server: {e}"))?;

    // Drain the sidecar's output into the Rust log. Without a reader the pipe
    // fills and the Go process blocks on its own stdout once it has logged
    // enough — a hang that looks like a backend freeze.
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    log::debug!("[agento] {}", String::from_utf8_lossy(&line).trim_end());
                }
                CommandEvent::Stderr(line) => {
                    log::warn!("[agento] {}", String::from_utf8_lossy(&line).trim_end());
                }
                CommandEvent::Terminated(payload) => {
                    log::error!("[agento] server exited: {:?}", payload.code);
                    break;
                }
                _ => {}
            }
        }
    });

    wait_until_healthy(port).await?;

    Ok(Sidecar {
        child: Mutex::new(Some(child)),
    })
}

/// Poll /health until the server responds or the budget runs out.
///
/// First launch is the slow case: the Go server runs 27 SQLite migrations and
/// seeds the pricing catalog before it listens, so the budget has to cover a
/// cold start on a slow disk rather than a warm one.
async fn wait_until_healthy(port: u16) -> Result<(), String> {
    const ATTEMPTS: u32 = 150;
    const INTERVAL: Duration = Duration::from_millis(200);

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/health");

    for attempt in 0..ATTEMPTS {
        if let Ok(resp) = client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            if resp.status().is_success() {
                log::info!("agento server healthy on port {port} after {attempt} attempts");
                return Ok(());
            }
        }
        tokio::time::sleep(INTERVAL).await;
    }

    Err(format!(
        "agento server did not become healthy on port {port} within {}s",
        ATTEMPTS * INTERVAL.as_millis() as u32 / 1000
    ))
}

/// Kill the sidecar held in Tauri's state, if any.
pub fn shutdown(app: &AppHandle) {
    if let Some(sidecar) = app.try_state::<Sidecar>() {
        sidecar.shutdown();
    }
}

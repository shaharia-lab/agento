// The Claude Agent SDK, ported from Go. Public because it is a library in its
// own right — the agent runtime, the integrations' MCP servers and the chat SSE
// all build on it — and because its tests drive it against a scripted CLI from
// outside the crate.
pub mod claude;
mod menu;
// `native` and `paths` are public so `tests/live_parity.rs` can diff a ported
// endpoint against the running Go server without going through a window.
pub mod native;
pub mod paths;
mod proxy;
mod sidecar;

// The seam's two fallbacks are destructors and caught panics: `proxy.rs` turns a
// panicking native handler into an `Err` that forwards to the Go sidecar, and
// `native/scan.rs` clears its in-progress flag from a `Drop` guard. `panic =
// "abort"` runs neither, so setting it would silently delete both — and no test
// could catch it, because the test profile always unwinds. Fail the build
// instead. See `[profile.release]` in Cargo.toml.
#[cfg(panic = "abort")]
compile_error!(
    "agento's desktop shell requires panic=\"unwind\": aborting disables the \
     native-handler fallback in proxy.rs and the scan guard in native/scan.rs"
);

use serde::Serialize;
use tauri::{Manager, WindowEvent};

/// Fixed proxy port in development so `vite.config.ts` can target it without a
/// handshake. Release builds take a free port instead, so two installs (or a
/// leftover process) can never collide.
#[cfg(debug_assertions)]
const DEV_PROXY_PORT: u16 = 8991;

/// Platform facts the frontend needs before its first paint — chiefly where the
/// window controls live, which decides the titlebar layout.
#[derive(Serialize)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub version: String,
    /// True when the OS draws its own window controls on the left (macOS).
    pub controls_on_left: bool,
    /// Origin the frontend should send API requests to. Empty means "same
    /// origin as this page", which is the case once the proxy serves the UI.
    pub api_base: String,
    /// Resolved path to the Claude Code CLI, or null when it is not installed.
    pub claude_cli: Option<String>,
    /// Whether this install can replace itself, which decides between offering
    /// an in-app update and merely announcing one.
    pub can_self_update: bool,
    /// How this copy was installed, for the update wording ("appimage",
    /// "package", "dmg", "installer").
    pub install_kind: String,
}

#[tauri::command]
fn host_info(state: tauri::State<'_, AppPorts>) -> HostInfo {
    HostInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        controls_on_left: cfg!(target_os = "macos"),
        api_base: format!("http://127.0.0.1:{}", state.proxy),
        claude_cli: find_claude_cli(),
        can_self_update: install_kind() != "package",
        install_kind: install_kind().to_string(),
    }
}

/// How this copy was installed.
///
/// This has to be decided at runtime, not build time: one `tauri build` on
/// Linux emits the .deb, .rpm and .AppImage from the *same* executable, so a
/// compile-time flag could not tell them apart. The AppImage runtime is the
/// only one that identifies itself, via the APPIMAGE variable it exports.
///
/// It matters because dpkg and rpm own the files they installed. An updater
/// that overwrote them would leave the package database describing a version
/// that is no longer on disk, so those installs are notify-only and update
/// through apt/dnf like everything else on the system.
fn install_kind() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("APPIMAGE").is_some() {
            "appimage"
        } else {
            "package"
        }
    }
    #[cfg(target_os = "macos")]
    {
        "dmg"
    }
    #[cfg(target_os = "windows")]
    {
        "installer"
    }
}

/// Locate the `claude` binary the way the backend will.
///
/// Everything the app ships is self-contained except this: the Go server runs
/// agents by spawning the Claude Code CLI as a subprocess, and that CLI is a
/// separate ~280 MB install we do not redistribute. Detecting it up front turns
/// "every chat fails with exec: not found" into one honest message.
pub(crate) fn find_claude_cli() -> Option<String> {
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };

    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }

    // A GUI app launched from a desktop environment often inherits a minimal
    // PATH that omits the per-user install locations, so check those directly
    // before reporting the CLI missing.
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let home = std::path::PathBuf::from(home);
    for rel in [
        ".local/bin",
        ".npm-global/bin",
        ".bun/bin",
        ".volta/bin",
        "bin",
        "AppData/Roaming/npm",
    ] {
        let candidate = home.join(rel).join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }

    None
}

/// Ports resolved at startup, exposed to the frontend and to diagnostics.
pub struct AppPorts {
    pub proxy: u16,
    pub upstream: u16,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // OAuth consent must open in the user's real browser: inside the app's
        // own webview there is no address bar to check who is asking, and the
        // provider's session would live in a container we throw away.
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Relaunching after an update is the only reason this is here.
        .plugin(tauri_plugin_process::init())
        // The default targets are stdout *and* the app log dir, and the second
        // one is load-bearing since #301: `proxy.rs` writes an access line per
        // /api request, and a packaged .app/.AppImage has no console to read it
        // on. Calling `targets(...)` here would replace both — narrow it only by
        // adding, never by replacing.
        //
        // `Info` is also what decides what that log contains: `proxy.rs` logs
        // failures at warn, writes at info and successful reads at debug, so
        // the file holds the state-changing requests and everything that went
        // wrong, without the UI's polling.
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                // Size and rotation are load-bearing for the same reason the
                // targets are, and their defaults were not survivable once this
                // file became the access log. `DEFAULT_MAX_FILE_SIZE` is 40_000
                // bytes and `DEFAULT_ROTATION_STRATEGY` is `KeepOne` — which
                // does not mean "keep one archive": `rotate()` is
                // `fs::remove_file(&self.path)`, so there is no archive at all.
                // At roughly 90 bytes an access line that is ~440 requests of
                // history and then nothing, reached inside a single ordinary
                // session and fastest in `diff` mode, where every compared
                // request also logs `identical`. It was harmless while the file
                // held a handful of startup lines; since #301 it is the record
                // a user is asked to send when they hit a bug an hour in.
                //
                // 5 MiB × `KeepSome(3)` is three dated archives beside the live
                // file, so ~20 MiB and days of history rather than minutes.
                .max_file_size(5 * 1024 * 1024)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(3))
                .build(),
        )
        .invoke_handler(tauri::generate_handler![host_info])
        .setup(|app| {
            let handle = app.handle().clone();

            let menu = menu::build(&handle)?;
            app.set_menu(menu)?;

            // Bring up the backend before showing the window. Everything the UI
            // renders comes from the Go server, so a window that appears first
            // would just be an empty shell throwing fetch errors.
            tauri::async_runtime::block_on(async {
                let upstream = sidecar::free_port();

                let sc = sidecar::spawn(&handle, upstream)
                    .await
                    .map_err(|e| format!("starting backend: {e}"))?;

                #[cfg(debug_assertions)]
                let proxy_port = DEV_PROXY_PORT;
                #[cfg(not(debug_assertions))]
                let proxy_port = 0; // let the OS choose

                let proxy = proxy::serve(upstream, proxy_port)
                    .await
                    .map_err(|e| format!("starting proxy: {e}"))?;

                handle.manage(sc);
                handle.manage(AppPorts { proxy, upstream });

                // The scan is ours now (#289): the sidecar is started with
                // AGENTO_SCANNER=off, so nothing else will do this. Replaces the
                // `sessionCache.StartBackgroundScan()` the Go server used to run
                // on boot, and like it, it does not block startup — the window
                // opens while the corpus is still being read, and the sessions
                // list reports progress from `GET /api/claude-sessions/status`.
                if let Some(db) = crate::paths::database_path() {
                    crate::native::scan::ensure_scan(db);
                }

                // Release builds load the UI from the proxy, which makes the
                // page same-origin with the API. Debug builds load Vite, which
                // proxies /api to the same place.
                #[cfg(not(debug_assertions))]
                if let Some(window) = handle.get_webview_window("main") {
                    let url = format!("http://127.0.0.1:{proxy}");
                    let parsed = url
                        .parse()
                        .map_err(|e| format!("invalid proxy url {url}: {e}"))?;
                    window
                        .navigate(parsed)
                        .map_err(|e| format!("navigating to {url}: {e}"))?;
                }

                Ok::<(), String>(())
            })?;

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Kill the Go server explicitly. Tauri reaps sidecars on a clean
            // exit but not on a force-quit, and a survivor holds the SQLite
            // lock against the next launch.
            if matches!(event, WindowEvent::Destroyed) {
                sidecar::shutdown(window.app_handle());
            }
        })
        .on_menu_event(menu::on_event)
        .run(tauri::generate_context!())
        .expect("error while running Agento");
}

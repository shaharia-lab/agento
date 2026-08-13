mod menu;
// `native` and `paths` are public so `tests/live_parity.rs` can diff a ported
// endpoint against the running Go server without going through a window.
pub mod native;
pub mod paths;
mod proxy;
mod sidecar;

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
fn find_claude_cli() -> Option<String> {
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
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
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

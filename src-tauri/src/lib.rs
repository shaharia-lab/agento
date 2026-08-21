// The Claude Agent SDK, ported from Go. Public because it is a library in its
// own right — the agent runtime, the integrations' MCP servers and the chat SSE
// all build on it — and because its tests drive it against a scripted CLI from
// outside the crate.
pub mod claude;
mod guards;
// Reading back the log file the plugin below writes, for Settings → Logs.
// Commands rather than `/api` routes: see the module header.
mod logs;
// The menubar is macOS-only: the app menu (About/Hide/Quit ⌘Q) is what macOS
// users expect, while an in-window GTK/win32 menubar is not the convention
// for this class of app — the titlebar and the ⌘K palette carry the same
// actions there.
#[cfg(target_os = "macos")]
mod macos_window;
#[cfg(target_os = "macos")]
mod menu;
// `native` and `paths` are public so `tests/live_parity.rs` can diff a ported
// endpoint against the running Go server without going through a window.
pub mod native;
pub mod paths;
mod proxy;

// Two recovery paths are destructors and caught panics: `proxy.rs` turns a
// panicking native handler into a clean JSON 500 (via `spawn_blocking`'s join
// error), and `native/scan.rs` clears its in-progress flag from a `Drop`
// guard. `panic = "abort"` runs neither, so setting it would silently delete
// both — and no test could catch it, because the test profile always unwinds.
// Fail the build instead. See `[profile.release]` in Cargo.toml.
#[cfg(panic = "abort")]
compile_error!(
    "agento's desktop shell requires panic=\"unwind\": aborting disables the \
     panicking-handler 500 in proxy.rs and the scan guard in native/scan.rs"
);

use serde::Serialize;
use tauri::Manager;

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
/// Everything the app ships is self-contained except this: agents run by
/// spawning the Claude Code CLI as a subprocess (`src/claude/`), and that CLI
/// is a separate ~280 MB install we do not redistribute. Detecting it up front
/// turns "every chat fails with exec: not found" into one honest message.
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
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        // Registered first, as its docs require. A second launch would be a
        // second scheduler and a second writer on one ~/.agento/agento.db —
        // exactly the collision the dev data-dir split exists to avoid — so
        // it focuses the existing window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // OAuth consent must open in the user's real browser: inside the app's
        // own webview there is no address bar to check who is asking, and the
        // provider's session would live in a container we throw away.
        .plugin(tauri_plugin_opener::init())
        // Native open-folder dialogs for every working-directory field; the
        // HTML fallback browser depends on /api/fs, which is Unix-only.
        .plugin(tauri_plugin_dialog::init())
        // Remember size/position/maximized across launches; without it every
        // launch is a centered 1280×820, which no native app does.
        .plugin(tauri_plugin_window_state::Builder::new().build())
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
        .invoke_handler(tauri::generate_handler![
            host_info,
            logs::log_files,
            logs::read_log,
            logs::export_logs
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            #[cfg(target_os = "macos")]
            app.set_menu(menu::build(&handle)?)?;

            // Bring up the backend before showing the window. Everything the UI
            // renders comes from this process now (#278), so the database has
            // to exist and be migrated before the first request arrives.
            tauri::async_runtime::block_on(async {
                // What Go's `NewSQLiteDB` did at every startup, in the same
                // order: create the data dir and the database, apply the
                // migrations, seed the pricing catalog. Blocking is right —
                // a window shown before the schema exists would just be an
                // empty shell throwing fetch errors — and on anything but a
                // first run all three are no-ops measured in milliseconds.
                //
                // Failures here are fatal on purpose. A missing HOME or an
                // unappliable migration is not something any later request can
                // recover from, and Go's server refused to start on exactly
                // the same conditions. The one exception is the pricing seed:
                // Go logged and carried on with cost computation degraded, so
                // this does too.
                let db = crate::paths::database_path()
                    .ok_or_else(|| "no home directory to resolve the data dir".to_string())?;
                {
                    let mut conn = crate::native::db::ensure_database(&db)
                        .map_err(|e| format!("opening database: {e}"))?;
                    crate::native::migrate::apply(&mut conn)
                        .map_err(|e| format!("migrating database: {e}"))?;
                    match crate::native::pricing_seed::seed(&conn) {
                        Ok(written) if written > 0 => {
                            log::info!("pricing catalog seeded rows_written={written}");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            log::warn!("pricing seed failed; cost computation degraded: {e}");
                        }
                    }
                }

                #[cfg(debug_assertions)]
                let proxy_port = DEV_PROXY_PORT;
                #[cfg(not(debug_assertions))]
                let proxy_port = 0; // let the OS choose

                let proxy = proxy::serve(proxy_port)
                    .await
                    .map_err(|e| format!("starting api server: {e}"))?;

                handle.manage(AppPorts { proxy });

                // The scan (#289): replaces the `sessionCache.StartBackgroundScan()`
                // the Go server used to run on boot, and like it, it does not
                // block startup — the window opens while the corpus is still
                // being read, and the sessions list reports progress from
                // `GET /api/claude-sessions/status`.
                {
                    // The integration MCP servers (#311): replaces the
                    // `reg.Start(ctx)` that `buildIntegrationRegistry` used to
                    // run at boot. Spawned rather than awaited — a GitHub row
                    // binds a loopback listener and the window should not wait
                    // on it — and a failure to read the list is logged, since
                    // there is nothing better to do with it here.
                    let integrations_db = db.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) =
                            crate::native::integrations::registry::start_all(&integrations_db).await
                        {
                            log::warn!("some integrations failed to start: {e}");
                        }
                    });

                    // The task scheduler (#275): replaces the
                    // `initTaskScheduler` the Go server used to run at boot.
                    // Unlike the two above it is not spawned — `start` only
                    // lists the active tasks and installs a timer per row, and
                    // the timers are themselves tasks — but like them a failure
                    // to read the list is logged rather than fatal.
                    crate::native::schedule::runtime::start(db.clone());

                    crate::native::scan::ensure_scan(db);
                }

                // Release builds load the UI from the proxy, which makes the
                // page same-origin with the API. Debug builds load Vite, which
                // proxies /api to the same place.
                //
                // **This navigation is why `capabilities/default.json` carries a
                // `remote` block**, and the coupling is invisible from either
                // file. Tauri's ACL asks `Webview::is_local_url` which side of
                // the local/remote split a request came from, and that compares
                // the page's URL against `tauri://localhost` (release) or the
                // configured `devUrl` (debug) — so a release window pointed at
                // `http://127.0.0.1:<port>` is **remote**, while the identical
                // dev window on `http://localhost:1420` is local. A capability
                // with no `remote.urls` is local-only, so every plugin and
                // `core:` command was denied in release builds and allowed in
                // dev: no window dragging (which on macOS is the *only* way to
                // move the window, since `titleBarStyle: Overlay` leaves no OS
                // titlebar to grab), no folder picker, no external links, no
                // updater, no theme sync and no macOS menu events. Commands
                // registered through `invoke_handler` — `host_info` — bypass the
                // ACL entirely, which is why the app otherwise looked healthy.
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

                #[cfg(target_os = "macos")]
                if let Ok(ptr) = window.ns_window() {
                    // SAFETY: a live window's NSWindow pointer, on the main
                    // thread (setup runs inside the event loop).
                    unsafe { macos_window::position_traffic_lights(ptr) };
                }
            }

            Ok(())
        });

    #[cfg(target_os = "macos")]
    let builder = builder
        .on_menu_event(menu::on_event)
        // AppKit re-lays-out the titlebar (and resets the button positions)
        // on resize, fullscreen transitions, theme changes and focus — the
        // same class of resets tauri-plugin-decorum re-applies on.
        .on_window_event(|window, event| {
            if matches!(
                event,
                tauri::WindowEvent::Resized(_)
                    | tauri::WindowEvent::ThemeChanged(_)
                    | tauri::WindowEvent::Focused(_)
            ) {
                if let Ok(ptr) = window.ns_window() {
                    // SAFETY: a live window's NSWindow pointer, on the main
                    // thread (window events dispatch from the event loop).
                    unsafe { macos_window::position_traffic_lights(ptr) };
                }
            }
        });

    builder
        .run(tauri::generate_context!())
        .expect("error while running Agento");
}

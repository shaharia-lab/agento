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
    /// A bearer token for `/api`, signed by the install's Ed25519 key (#400,
    /// #405).
    ///
    /// Tauri IPC is the whole delivery mechanism: it is the one channel a local
    /// process cannot reach, which is what makes the token worth having.
    ///
    /// **Minted fresh on every invocation**, which is what makes `api.ts`'s
    /// 401-retry able to recover from anything. #400's token was one value for
    /// the life of the process, so re-invoking this command would have handed
    /// back the same dead string; a signed token has two ways to stop working —
    /// its `exp` passing, and the keypair being regenerated from the Security
    /// tab — and re-minting answers both. Signing is a few microseconds and this
    /// is called once per page load, so there is nothing to cache.
    ///
    /// Empty when no signing key is installed, which `setup` treats as fatal —
    /// so in practice the page either gets a working credential or never loads.
    pub api_token: String,
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
        // Minted by `native::security`, which owns the key, rather than carried
        // on `AppPorts`: one source of truth for what the guard verifies
        // against. A failure is logged and answered as an empty token — the
        // page then gets one honest 401 from `api.ts` rather than a command
        // that rejects and leaves every view with its own error.
        api_token: native::security::mint_session_token().unwrap_or_else(|e| {
            log::error!("minting the webview session token: {e}");
            String::new()
        }),
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

/// Where a debug build leaves this launch's `/api` token (#400).
///
/// **Debug only, and the whole function compiles out of a shipped binary.** The
/// token is otherwise memory-only, and in release it must stay that way.
///
/// It exists because two developer workflows are load-bearing and neither can
/// reach a token held only in this process's memory:
///
/// - `curl -H "Authorization: Bearer $(cat ~/.agento-desktop-dev/api-token)" …`,
///   which is how a backend hop is bisected (`.claude/skills/local-verify/`).
/// - Chrome on `localhost:1420`, where `vite.config.ts`'s proxy reads this file
///   and adds the header server-side. A plain browser tab has no Tauri IPC, so
///   the page itself can never hold a token.
///
/// The alternative was exempting debug builds from the guard entirely, which was
/// rejected: the dev port is *fixed and well-known* (8991), which makes dev the
/// easier target, and a guard never exercised where we develop is a guard whose
/// regressions ship.
///
/// Rewritten on every launch. Since #405 the token is a JWT rather than an
/// opaque string, which changes the *shape* of a stale file's failure and not
/// the rule: the signing key now survives a restart, so a token left over from a
/// previous launch keeps working until its `exp` or the next regenerate — where
/// #400's stopped working the moment the app did. That is a convenience for the
/// `curl` workflow and nothing more; it is still a cache of a live value, never
/// the source of one, and a regenerate is what makes it stale for real.
#[cfg(debug_assertions)]
fn write_dev_token_file(token: &str) {
    let Some(dir) = paths::data_dir() else {
        log::warn!("dev api token: no data dir to write it to");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("dev api token: creating {}: {e}", dir.display());
        return;
    }
    let path = dir.join("api-token");
    if let Err(e) = std::fs::write(&path, token) {
        log::warn!("dev api token: writing {}: {e}", path.display());
        return;
    }
    // 0600 before anyone can read it. On a multi-user machine the default umask
    // would leave it world-readable, which would hand the token to exactly the
    // account this guard exists to keep out.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            log::warn!("dev api token: chmod {}: {e}", path.display());
        }
    }
    log::info!("dev api token written to {}", path.display());
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

                // The `/api` signing key (#405, replacing #400's per-launch
                // string), loaded **before** the listener is spawned so no
                // request can arrive while there is nothing to verify against —
                // which the guard would fail closed on, but only by luck of
                // ordering rather than by design.
                //
                // Create-if-absent: the first run writes a keypair, every run
                // after it reuses the same one, and only the Security tab's
                // regenerate ever replaces it. That is what makes a token the
                // user issued survive a restart.
                //
                // **Fatal on failure, deliberately.** A corrupt or unreadable
                // private key must not fall back to generating a fresh one
                // (which would silently invalidate every issued token on a
                // transient permission problem) and must not fall back to
                // serving unauthenticated (the hole #400 closed). The data dir
                // is the same directory the database lives in, so a debug build
                // gets its own keypair for free and a development launch can
                // never mint a token the release install would honour.
                let data_dir = crate::paths::data_dir()
                    .ok_or_else(|| "no home directory to resolve the data dir".to_string())?;
                let keypair = crate::native::security::keys::load_or_create(&data_dir)
                    .map_err(|e| format!("loading the api signing key: {e}"))?;
                crate::native::security::keys::install(keypair);

                // Which tokens have been revoked, read once into memory because
                // the guard is on the request path and cannot await a query.
                // See `native::security::tokens` for why that set is
                // authoritative rather than a cache.
                {
                    let conn = crate::native::db::open_read_only(&db)
                        .map_err(|e| format!("opening database: {e}"))?;
                    crate::native::security::tokens::load_revoked(&conn)
                        .map_err(|e| format!("loading revoked api tokens: {e}"))?;
                }

                #[cfg(debug_assertions)]
                match crate::native::security::mint_session_token() {
                    Ok(token) => write_dev_token_file(&token),
                    Err(e) => log::warn!("dev api token: {e}"),
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

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;

    /// The dev hatch is only worth having if it is actually written, actually
    /// holds the live token, and is actually unreadable by other accounts.
    ///
    /// This is a unit test rather than a manual `npm run app` check on purpose:
    /// the mode is the part that fails silently. A default umask would leave the
    /// file world-readable, handing this launch's token to precisely the other
    /// local account #400 exists to keep out — and nothing about the app's
    /// behaviour would look different.
    #[test]
    fn the_dev_token_file_is_written_private_to_this_user() {
        let _lock = crate::paths::tests::env_lock();
        let home = tempfile::tempdir().expect("tempdir");
        let _home = crate::paths::tests::EnvVar::set("HOME", home.path());

        write_dev_token_file("a-token");

        let path = crate::paths::data_dir()
            .expect("data dir")
            .join("api-token");
        assert_eq!(
            std::fs::read_to_string(&path).expect("token file"),
            "a-token"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the token must not be group/world readable"
            );
        }
    }

    /// Every launch mints a new token, so the file is a cache of a live value
    /// and must be replaced rather than appended to or left stale.
    #[test]
    fn a_second_launch_overwrites_the_previous_launchs_token() {
        let _lock = crate::paths::tests::env_lock();
        let home = tempfile::tempdir().expect("tempdir");
        let _home = crate::paths::tests::EnvVar::set("HOME", home.path());

        write_dev_token_file("first-launch");
        write_dev_token_file("second-launch");

        let path = crate::paths::data_dir()
            .expect("data dir")
            .join("api-token");
        assert_eq!(
            std::fs::read_to_string(&path).expect("token file"),
            "second-launch"
        );
    }
}

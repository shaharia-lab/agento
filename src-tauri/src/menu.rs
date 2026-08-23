//! Native application menu — macOS only.
//!
//! On macOS the menubar is the app's front door: the first submenu becomes
//! the application menu, and users expect About / Services / Hide / Quit ⌘Q
//! there and Settings… beside them. On Linux and Windows an in-window
//! menubar is not the convention for this class of app (the custom titlebar
//! and the ⌘K palette carry the same actions), so `lib.rs` installs this
//! menu behind `#[cfg(target_os = "macos")]` and this module compiles only
//! there.
//!
//! Keyboard shortcuts live in the webview's own keydown handler (`App.tsx`);
//! the menu deliberately declares no accelerators for the custom items,
//! because WKWebView sees key equivalents before the menu does and the two
//! paths would double-fire. The predefined items (Quit, Hide, clipboard)
//! carry their system accelerators, which the webview does not intercept.

use tauri::menu::{Menu, MenuBuilder, MenuEvent, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Emitted to the webview whenever a menu item fires. The payload is the item id.
pub const MENU_EVENT: &str = "menu://action";

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    // The label is ignored on macOS — the system shows the process name.
    let app_menu = SubmenuBuilder::new(app, "Agento")
        .about(None)
        .separator()
        .text("settings", "Settings…")
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    let file = SubmenuBuilder::new(app, "File")
        .text("new_chat", "New Chat")
        .text("new_agent", "New Agent")
        .text("new_task", "New Scheduled Task")
        .separator()
        .close_window()
        .build()?;

    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let view = SubmenuBuilder::new(app, "View")
        .text("toggle_sidebar", "Toggle Sidebar")
        .text("toggle_inspector", "Toggle Inspector")
        .separator()
        .text("palette", "Command Palette…")
        .separator()
        .text("theme_light", "Appearance: Light")
        .text("theme_dark", "Appearance: Dark")
        .text("theme_system", "Appearance: Match System")
        .separator()
        .fullscreen()
        .build()?;

    let go = SubmenuBuilder::new(app, "Go")
        .text("go_back", "Back")
        .text("go_forward", "Forward")
        .separator()
        .text("go:chats", "Chats")
        .text("go:agents", "Agents")
        .text("go:integrations", "Integrations")
        .text("go:tasks", "Scheduled Tasks")
        .text("go:jobs", "Job History")
        .text("go:sessions", "Claude Sessions")
        .text("go:tokens", "Token Usage")
        .build()?;

    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()?;

    let help = SubmenuBuilder::new(app, "Help")
        .text("docs", "Agento Documentation")
        .text("github", "Star on GitHub")
        .separator()
        .text("go:about", "About Agento")
        .build()?;

    MenuBuilder::new(app)
        .items(&[&app_menu, &file, &edit, &view, &go, &window, &help])
        .build()
}

/// Forward every menu selection to the webview, which owns the app state.
pub fn on_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    let id = event.id().0.as_str();

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit(MENU_EVENT, id);
    }
}

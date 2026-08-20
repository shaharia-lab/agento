//! macOS traffic-light positioning — the part `tauri.conf.json` cannot do.
//!
//! `trafficLightPosition` in the window config looks like the answer and is
//! not: tao only *stores* the value in its view state, and the code that
//! moves the buttons runs in the tao content view's `drawRect:`
//! (`tao/src/platform_impl/macos/view.rs`). In a Tauri window the WKWebView
//! covers that view completely, so AppKit rarely — sometimes never — repaints
//! it, and the buttons stay at AppKit's default corner position, visibly
//! misaligned with the app's 38px titlebar strip. This is the flakiness
//! `tauri-plugin-decorum` exists to work around.
//!
//! So the shell positions the buttons directly, immediately, with the same
//! AppKit calls tao's `inset_traffic_lights` makes (transcribed from tao
//! 0.35.3, which compiles this exact API surface against the objc2 versions
//! in our lockfile), and re-applies on the window events that make AppKit
//! re-layout the titlebar and reset them: resize (including fullscreen
//! transitions), theme change, and focus.
//!
//! Geometry: the titlebar container is resized to exactly the strip's height
//! and the buttons are centered in it, so the lights share a centerline with
//! the strip's own icons by construction rather than by a tuned offset.

use objc2::msg_send;
use objc2_app_kit::{NSView, NSWindow, NSWindowButton};
use objc2_foundation::NSRect;

/// Height of the app's titlebar strip. Must match `--titlebar-h` in
/// `src/styles/tokens.css`.
const TITLEBAR_HEIGHT: f64 = 38.0;

/// Left inset of the close button. AppKit's default (~7px) crowds the corner
/// once the strip is this tall; custom-header apps sit around this value.
const PAD_X: f64 = 16.0;

/// Center the traffic lights in the titlebar strip.
///
/// # Safety
///
/// `ns_window_ptr` must be a valid pointer to an `NSWindow` (the value
/// `tauri::Window::ns_window()` returns for a live window), and the call
/// must happen on the main thread — both call sites run inside Tauri's
/// event-loop callbacks, which do.
pub unsafe fn position_traffic_lights(ns_window_ptr: *mut std::ffi::c_void) {
    let ns_window: &NSWindow = &*(ns_window_ptr as *const NSWindow);

    let Some(close) = ns_window.standardWindowButton(NSWindowButton::CloseButton) else {
        return;
    };
    let Some(miniaturize) = ns_window.standardWindowButton(NSWindowButton::MiniaturizeButton)
    else {
        return;
    };
    let Some(zoom) = ns_window.standardWindowButton(NSWindowButton::ZoomButton) else {
        return;
    };
    let Some(container) = close.superview().and_then(|v| v.superview()) else {
        return;
    };

    let close_rect = NSView::frame(&close);
    let button_h = close_rect.size.height;

    // Anchor the titlebar container to the top of the window at the strip's
    // height (Cocoa's origin is bottom-left).
    let mut title_bar_rect = NSView::frame(&container);
    title_bar_rect.size.height = TITLEBAR_HEIGHT;
    title_bar_rect.origin.y = ns_window.frame().size.height - TITLEBAR_HEIGHT;
    let _: () = msg_send![&container, setFrame: title_bar_rect];

    // Keep AppKit's own spacing between the buttons; only the group moves.
    // Reading it from the current frames is idempotent — a second apply sees
    // the same delta because both buttons shifted equally.
    let space_between = NSView::frame(&miniaturize).origin.x - close_rect.origin.x;

    for (i, button) in [close, miniaturize, zoom].into_iter().enumerate() {
        let mut rect: NSRect = NSView::frame(&button);
        rect.origin.x = PAD_X + (i as f64) * space_between;
        rect.origin.y = (TITLEBAR_HEIGHT - button_h) / 2.0;
        button.setFrameOrigin(rect.origin);
    }
}

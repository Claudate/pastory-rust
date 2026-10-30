//! Bringing this accessory process to the front — one implementation, used by
//! everything that opens a window the user has to be able to type into.
//!
//! The app is `accessory`: no Dock icon, no window the system counts as the
//! app's own. That makes the obvious calls silently useless. `makeKeyWindow`
//! only makes the *window* key while the *process* stays in the background,
//! and `NSApplication.activate()` / `activateIgnoringOtherApps:` go through
//! the cooperative activation rules, which decline because an accessory
//! process owns no window they recognise — the keystrokes keep going to
//! whatever app was in front. `NSRunningApplication.activate(options:)`
//! (macOS 14+) is the call that actually moves the process forward; it was
//! verified against real window-server events from `panel.rs`, where without
//! it the panel's own ⎋ never reached the app at all.
//!
//! Keep this the only place that asks. A second copy of the wrong call is how
//! this class of bug comes back.

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};

/// Bring this process to the front. Safe to call from the main thread while a
/// window is already up; it is the *process* that moves, not a window.
///
/// `ActivateIgnoringOtherApps` is formally deprecated in favour of cooperative
/// activation, but that path declines for an accessory process, so the flag is
/// the only one that works here.
pub fn activate_app() {
    let app = NSRunningApplication::currentApplication();
    #[allow(deprecated)]
    let _ = app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
}

/// Whether this process is the active one right now — what `activate_app` is
/// for, and what the self-tests assert instead of trusting the call.
pub fn is_active() -> bool {
    NSRunningApplication::currentApplication().isActive()
}

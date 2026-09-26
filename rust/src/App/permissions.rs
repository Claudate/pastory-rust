//! Port of `App/Permissions.swift` (M0 slice: preflight + the sendPaste
//! spike; M3 slice: the screen-recording request flow).
//!
//! TCC/AX state plus the synthetic-⌘V writer. The paste is posted at session
//! level, never HID level: HID-level events feed the system's
//! physical-keyboard state, and interleaving with real key presses once left
//! Command latched down machine-wide (2026-09-17, cleared only by a reboot).

use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_graphics as cg;

/// `CGPreflightScreenCaptureAccess()`.
pub fn has_screen_recording() -> bool {
    // SAFETY: plain C query with no preconditions.
    cg::CGPreflightScreenCaptureAccess()
}

/// The system's own dialog is shown at most once per launch; after that it
/// is our alert, which can relaunch (`askedSystemThisLaunch`).
static ASKED_SYSTEM_THIS_LAUNCH: AtomicBool = AtomicBool::new(false);

/// `CGRequestScreenCaptureAccess()`. The system's own dialog is shown at
/// most once per launch; after that the alert in `ensure_screen_recording`.
pub fn request_screen_recording() -> bool {
    if has_screen_recording() {
        return true;
    }
    if ASKED_SYSTEM_THIS_LAUNCH.swap(true, Ordering::SeqCst) {
        return false;
    }
    // SAFETY: plain C request.
    cg::CGRequestScreenCaptureAccess()
}

/// ⌘V into whatever is in front right now — the M0 spike proves the
/// session-tap post works from Rust (red line 3: never HID).
pub fn send_paste() {
    send_paste_inner(12);
}

fn send_paste_inner(retries: u32) {
    use cg::{
        CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    };

    let held = CGEventFlags::MaskCommand
        | CGEventFlags::MaskShift
        | CGEventFlags::MaskAlternate
        | CGEventFlags::MaskControl;
    // Wait for the user's own modifiers to lift first, so the synthetic ⌘
    // never overlaps a real one (generated binding is a safe call).
    if CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState) & held
        != cg::CGEventFlags(0)
    {
        if retries > 0 {
            crate::app::delegate::dispatch_main_after(
                0.05,
                Box::new(move || send_paste_inner(retries - 1)),
            );
        }
        return;
    }
    let src = match CGEventSource::new(CGEventSourceStateID::CombinedSessionState) {
        Some(src) => src,
        None => return,
    };
    // While the synthetic keystroke is in flight, hold back real keyboard
    // events so nothing interleaves with it.
    cg::CGEventSource::set_local_events_filter_during_suppression_state(
        Some(&src),
        cg::CGEventFilterMask::PermitLocalMouseEvents
            | cg::CGEventFilterMask::PermitSystemDefinedEvents,
        cg::CGEventSuppressionState::EventSuppressionStateSuppressionInterval,
    );
    let v: cg::CGKeyCode = 9; // kVK_ANSI_V (CGKeyCode = u16)
    let down = CGEvent::new_keyboard_event(Some(&src), v, true).unwrap();
    let up = CGEvent::new_keyboard_event(Some(&src), v, false).unwrap();
    CGEvent::set_flags(Some(&down), CGEventFlags::MaskCommand);
    CGEvent::set_flags(Some(&up), CGEventFlags::MaskCommand);
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&down));
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&up));
}

/// `openSettings` — System Settings deep link (used from M3 on).
pub fn open_settings_pane_url(pane: &str) -> String {
    format!("x-apple.systempreferences:com.apple.preference.security?{}", pane)
}

/// `AXIsProcessTrusted` — Accessibility permission, needed to press ⌘V for
/// the user (paste-into-previous-app).
pub fn has_accessibility() -> bool {
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    // SAFETY: plain C query with no preconditions.
    unsafe { AXIsProcessTrusted() }
}

/// `AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: true})` —
/// asks once; the toggle lands in System Settings.
pub fn request_accessibility() {
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: *const core::ffi::c_void) -> bool;
    }
    let key = CFString::from_static_str("AXTrustedCheckOptionPrompt");
    let value = CFBoolean::new(true);
    let dict = CFDictionary::from_slices(&[&*key], &[&*value]);
    // SAFETY: a valid CFDictionary of (CFString, CFBoolean), passed as the
    // options dictionary.
    unsafe {
        let raw = std::ptr::from_ref(AsRef::<CFDictionary>::as_ref(&dict));
        let _ = AXIsProcessTrustedWithOptions(raw as *const core::ffi::c_void);
    }
}

/// `Permissions.openSettings` — System Settings deep link.
pub fn open_settings(pane: &str) {
    let url = objc2_foundation::NSURL::URLWithString(&objc2_foundation::NSString::from_str(
        &open_settings_pane_url(pane),
    ));
    if let Some(url) = url {
        objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

/// `Permissions.relaunch` — start a fresh copy of ourselves, then quit.
/// If the new copy did not start, stay alive (Swift comment kept).
pub fn relaunch() {
    let bundle = objc2_foundation::NSBundle::mainBundle();
    let url = bundle.bundleURL();
    let cfg = objc2_app_kit::NSWorkspaceOpenConfiguration::configuration();
    cfg.setCreatesNewApplicationInstance(true);
    let block = block2::RcBlock::new(
        |_app: *mut objc2_app_kit::NSRunningApplication, error: *mut objc2_foundation::NSError| {
            // The completion runs on the main queue; nil error = the new
            // copy started, so this one can go (Swift: terminate on success).
            if error.is_null() {
                if let Some(mtm) = objc2::MainThreadMarker::new() {
                    objc2_app_kit::NSApplication::sharedApplication(mtm).terminate(None);
                }
            }
        },
    );
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .openApplicationAtURL_configuration_completionHandler(&url, &cfg, Some(&*block));
}

/// Gate before every screenshot (`Permissions.ensureScreenRecording`). A
/// grant made while we are running only takes effect after a relaunch, so
/// the alert offers exactly that instead of sending people back to Settings
/// again and again. Main thread only (NSAlert is modal).
pub fn ensure_screen_recording() -> bool {
    if has_screen_recording() {
        return true;
    }
    let _ = request_screen_recording();
    if has_screen_recording() {
        return true;
    }
    use crate::app::localization::l;
    let alert = {
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        objc2_app_kit::NSAlert::new(mtm)
    };
    alert.setMessageText(&objc2_foundation::NSString::from_str(&l(
        "Pastory 还没有屏幕录制权限",
    )));
    alert.setInformativeText(&objc2_foundation::NSString::from_str(&l(
        "在「系统设置 › 隐私与安全性 › 屏幕录制」里打开 Pastory。已经打开了的话，权限要重新启动后才生效。",
    )));
    alert.addButtonWithTitle(&objc2_foundation::NSString::from_str(&l(
        "我已打开，重新启动 Pastory",
    )));
    alert.addButtonWithTitle(&objc2_foundation::NSString::from_str(&l("打开系统设置")));
    alert.addButtonWithTitle(&objc2_foundation::NSString::from_str(&l("取消")));
    let mtm = objc2::MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    unsafe {
        // SAFETY: modern activation first; the legacy call is the fallback
        // (same pair as ShelfPanel.with_dialog).
        let _: () = objc2::msg_send![&app, activate];
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    match alert.runModal() {
        r if r == objc2_app_kit::NSAlertFirstButtonReturn => relaunch(),
        r if r == objc2_app_kit::NSAlertSecondButtonReturn => {
            open_settings("Privacy_ScreenCapture")
        }
        _ => {}
    }
    false
}

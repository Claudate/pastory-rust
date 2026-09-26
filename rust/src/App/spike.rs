//! M0 spike: objc2-app-kit's minimal NSPanel — borderless, non-activating,
//! shielding level. The shape every later overlay (frame selection M3,
//! recording HUD M5) reuses. Triggered by `--selftest panel`, which shows a
//! 2-second panel then exits.

use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSPanel,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGShieldingWindowLevel;
use objc2_foundation::{NSDate, NSRunLoop};

/// Show a small borderless panel for `secs`, pump the run loop, done.
/// Returns true when the panel came up and ordered front.
pub fn show_brief_panel(mtm: objc2::MainThreadMarker, secs: f64) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Borderless + nonactivating panel at the shielding level (M3 frame
    // selection sits above every floating toolbar with the same recipe).
    let rect = CGRect::new(CGPoint::new(100.0, 100.0), CGSize::new(280.0, 120.0));
    // SAFETY: plain AppKit construction; the mask bits are the overlay recipe.
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: setters without generated safe wrappers.
    unsafe {
        panel.setLevel(CGShieldingWindowLevel() as isize);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setReleasedWhenClosed(false);
    }
    panel.makeKeyAndOrderFront(None);

    // Pump the run loop so the panel is actually on screen, then leave.
    let run_loop = NSRunLoop::mainRunLoop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
    while std::time::Instant::now() < deadline {
        let limit = NSDate::dateWithTimeIntervalSinceNow(0.05);
        // SAFETY: NSDefaultRunLoopMode is a valid mode token.
        let mode: &'static objc2_foundation::NSRunLoopMode =
            unsafe { objc2_foundation::NSDefaultRunLoopMode };
        let _ = run_loop.runMode_beforeDate(mode, &limit);
    }
    panel.orderOut(None);
    true
}

//! Port of the AppKit half of `Shelf/ShelfPanel.swift` — the controller, the
//! panel subclass, and Quick Look. `QLPreviewPanel` is hand-declared (no
//! binding crate): one extern class plus msg_send wrappers for the ~10
//! selectors the shelf touches; the panel overrides carry the data source
//! and delegate, exactly like the Swift subclass.
//!
//! Everything here is main-thread-only; the one-time controller lives behind
//! a raw static pointer just like the app delegate does.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSEvent, NSPanel, NSRunningApplication, NSScreen, NSView, NSWindowDelegate, NSWindowStyleMask,
    NSBackingStoreType, NSWindowCollectionBehavior, NSWorkspace,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSNotification, NSString};

use crate::app::permissions;
use crate::app::preferences::Preferences;
use crate::app::localization::l;
use crate::clipboard::item::{ClipItem, ClipKind};
use crate::shelf::model::ShelfModel;

/// Slide distance for the open/close animations (`Self.slide`).
const SLIDE: f64 = 28.0;
/// `.statusBar` window level.
const STATUS_BAR_LEVEL: isize = 25;

// MARK: Controller singleton

pub struct ShelfPanelController {
    model: RefCell<ShelfModel>,
    panel: RefCell<Option<Retained<ShelfPanel>>>,
    panel_delegate: RefCell<Option<Retained<ShelfPanelWinDelegate>>>,
    /// The app that was in front when the shelf opened; a single-click copy
    /// hands the keyboard back to it so ⌘V lands there.
    previous_app: RefCell<Option<Retained<NSRunningApplication>>>,
    /// Swallow one resign-key while focus is handed back deliberately.
    keep_open_on_resign: Cell<bool>,
    /// True while a system dialog (save/alert) is up.
    hold_open: Cell<bool>,
    outside_monitor: RefCell<Option<usize>>,
    watching_switches: Cell<bool>,
    poll_timer: RefCell<Option<usize>>,
    /// The live panel carries the material; a snapshot cannot composite one.
    /// See `build_view` in `shelf::view` where it is read.
    material: Cell<bool>,
    /// Set by `show()`: the shelf wants the keyboard and has not got it yet.
    ///
    /// Activation is asynchronous, so the `makeKeyWindow()` that runs in
    /// `show()` itself finds nothing to take — the process is not yet
    /// frontmost in the window server's eyes. This flag lets the poll tock
    /// keep trying until the app really is active, which is the only moment a
    /// key window can be taken at all.
    want_key: Cell<bool>,
}

static CONTROLLER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// The shared controller (Swift `ShelfPanelController.shared`).
pub fn controller() -> &'static ShelfPanelController {
    let ptr = *CONTROLLER.get_or_init(|| {
        let c = ShelfPanelController {
            model: RefCell::new(ShelfModel::shared()),
            panel: RefCell::new(None),
            panel_delegate: RefCell::new(None),
            previous_app: RefCell::new(None),
            keep_open_on_resign: Cell::new(false),
            hold_open: Cell::new(false),
            outside_monitor: RefCell::new(None),
            watching_switches: Cell::new(false),
            poll_timer: RefCell::new(None),
            material: Cell::new(false),
            want_key: Cell::new(false),
        };
        Box::into_raw(Box::new(c)) as usize
    });
    // SAFETY: the controller was boxed above for the process lifetime.
    unsafe { &*(ptr as *const ShelfPanelController) }
}

/// Borrow the shelf's model (Swift `@MainActor` access).
pub(crate) fn with_model<R>(f: impl FnOnce(&ShelfModel) -> R) -> R {
    f(&controller().model.borrow())
}

/// Mutate the model. Never call back into `with_model` from inside.
pub(crate) fn with_model_mut<R>(f: impl FnOnce(&mut ShelfModel) -> R) -> R {
    f(&mut controller().model.borrow_mut())
}

/// `ClipStore.thumbnailMissing` for card drawing (through the model's store).
pub(crate) fn thumbnail_missing(id: &str) -> bool {
    with_model(|m| m.with_store(|s| s.thumbnail_missing(id)))
}

// MARK: Panel lifecycle

fn ensure_panel() -> Retained<ShelfPanel> {
    let c = controller();
    if let Some(p) = c.panel.borrow().as_ref() {
        return p.clone();
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    let p = make_panel(mtm);
    *c.panel.borrow_mut() = Some(p.clone());
    p
}

/// Whether a view tree built now should carry the panel material. True for
/// the live panel, false while a render self-test snapshots offscreen.
pub(crate) fn is_material_enabled() -> bool {
    controller().material.get()
}

/// The live panel's content view, for self-test diagnostics.
pub(crate) fn content_view() -> Option<Retained<NSView>> {
    controller().panel.borrow().as_ref().and_then(|p| p.contentView())
}

/// Self-test hook: whether the *drawer's* window is on screen, whatever form
/// is current. `is_visible` answers for the running form, so a test asserting
/// "the drawer went away when the form switched" cannot use it — by then the
/// switch has changed what the question means.
pub(crate) fn debug_panel_window_visible() -> bool {
    controller()
        .panel
        .borrow()
        .as_ref()
        .map(|p| p.isVisible())
        .unwrap_or(false)
}

/// Self-test hook: the live `ShelfPanel` itself. The keyboard table in
/// `handle` only runs from this window's `sendEvent:`, so a test that wants
/// the real key route (rather than `handle` called directly) needs the
/// window, not just its content view.
pub(crate) fn selftest_window() -> Option<Retained<ShelfPanel>> {
    controller().panel.borrow().clone()
}

/// `makePanel` — borderless, non-activating, status-bar level, clear.
fn make_panel(mtm: MainThreadMarker) -> Retained<ShelfPanel> {
    let p = ShelfPanel::new(
        mtm,
        CGRect::new(CGPoint::ZERO, CGSize::new(800.0, 400.0)),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
    );
    p.setLevel(STATUS_BAR_LEVEL);
    p.setOpaque(false);
    p.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
    p.setHasShadow(false);
    unsafe {
        p.setReleasedWhenClosed(false);
    }
    p.setHidesOnDeactivate(false);
    p.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary,
    );
    let delegate = ShelfPanelWinDelegate::new(mtm);
    let proto: &ProtocolObject<dyn NSWindowDelegate> = ProtocolObject::from_ref(&*delegate);
    p.setDelegate(Some(proto));
    *controller().panel_delegate.borrow_mut() = Some(delegate);
    p
}

/// (Re)build the view tree for `size` (Swift's hosting view is rebuilt every
/// render command; for the live panel a rebuild matches a fresh open).
fn attach(mtm: MainThreadMarker, size: CGSize) -> Retained<ShelfRootView> {
    let root = crate::shelf::view::build(mtm, size);
    if let Some(p) = controller().panel.borrow().as_ref() {
        p.setContentView(Some(&root));
    }
    root
}

use crate::shelf::view::ShelfRootView;

/// Point the model at the repaint. Without it a search that lands shows up on
/// the next 0.25 s poll tick; with it the shelf updates on the next pass of the
/// main queue. Correctness comes from `reload` draining completions either way.
///
/// The closure captures nothing on purpose: `set_notify` asks for `Send + Sync`
/// because the search worker holds it, but it is only ever called on the main
/// queue (the worker dispatches it there). Closing over the view's
/// thread-local state would make it non-`Send`.
///
/// Both forms install it: the hook repaints whichever tree is up, so a
/// background search landing redraws the rope as well as the drawer.
pub(crate) fn install_notify_hook() {
    with_model_mut(|m| {
        m.set_notify(Some(std::sync::Arc::new(|| crate::shelf::view::refresh())))
    });
}

/// Remember the app in front right now, so a click-copy in either form can
/// hand the keyboard (and a ⌘V) back to it. `show()` and the line strip's
/// reveal both call this, which is why it lives here and not inside a form.
pub(crate) fn remember_previous_app() {
    let ws = NSWorkspace::sharedWorkspace();
    if let Some(front) = ws.frontmostApplication() {
        if !front.isEqual(Some(&NSRunningApplication::currentApplication())) {
            *controller().previous_app.borrow_mut() = Some(front);
        }
    }
}

/// Build the shelf for launch; the first ⇧⌘V does not pay for it (`prewarm`).
///
/// Which form gets built is this process's to decide (`layout_skin`), so the
/// shell keeps calling one entry point whatever the form is.
pub fn prewarm() {
    watch_app_switches();
    if crate::app::layout_skin::is_line() {
        crate::shelf::line_panel::prewarm();
        with_model_mut(|m| m.prewarm_search());
        return;
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    let p = ensure_panel();
    p.setFrame_display(CGRect::new(CGPoint::ZERO, CGSize::new(1200.0, 480.0)), false);
    controller().material.set(true);
    crate::app::theme::set_panel_material_alpha(1.0);
    let root = attach(mtm, CGSize::new(1200.0, 480.0));
    root.layoutSubtreeIfNeeded();
    install_notify_hook();
    with_model_mut(|m| m.prewarm_search());
}

/// Swap the shelf's whole form (`PanelCommand::Relayout`).
///
/// Deliberately *not* `retheme`: a form swap must move and resize the window
/// — the drawer rises from the bottom edge, the strip hangs from the top —
/// whereas a skin swap keeps the frame so the shelf does not jump. The two
/// forms own separate windows, so a swap also has to put the other one away,
/// or a strip would sit on screen behind the drawer.
pub fn relayout() {
    if crate::app::layout_skin::is_line() {
        // Panel → line: take the drawer down before the strip goes up. It has
        // to go through `hide_panel_window` and not `hide`: the form is
        // already 晾衣绳 here, so `hide` would answer for the strip — retract
        // one that is not up yet — and leave the drawer (with the settings
        // page on it) sitting at the bottom of the screen, which is what the
        // user saw when they picked 晾衣绳 in 设置.
        retheme();
        hide_panel_window();
        crate::shelf::line_panel::relayout();
        // Then show the form that was picked. The strip parks itself above the
        // screen and only comes out when the pointer reaches the menu bar, so
        // switching from the settings page would otherwise leave the user
        // looking at nothing. `show` grants the no-retract grace, so the
        // pointer — left wherever the click that switched put it — cannot take
        // the strip straight back down.
        crate::shelf::line_panel::show();
    } else {
        // Line → panel: the strip must not linger above the drawer, and its
        // clock has to stop — a panel-form process has nothing to watch for.
        crate::shelf::line_panel::hide();
        crate::shelf::line_panel::suspend();
        retheme();
    }
}

/// Rebuild the view tree after a skin change. Every view caches the face and
/// the radii it was built with, so a repaint is not enough — this is the one
/// thing a skin switch has to do besides storing the choice. The panel keeps
/// its frame, so the shelf does not move.
pub fn retheme() {
    let Some(p) = controller().panel.borrow().clone() else { return };
    let mtm = MainThreadMarker::new().expect("main thread");
    let size = p.frame().size;
    let root = attach(mtm, size);
    root.setFrame(CGRect::new(CGPoint::ZERO, size));
    root.layoutSubtreeIfNeeded();
    if p.isVisible() {
        p.displayIfNeeded();
    }
}

/// The shelf's window number while visible (`ShelfPanelController.windowID`)
/// — the one own window the picker may name as a capture candidate.
pub fn window_id() -> Option<u32> {
    let p = controller().panel.borrow().clone()?;
    if !p.isVisible() {
        return None;
    }
    Some(p.windowNumber() as u32)
}

/// `ShelfPanelController.holdOpen` — outside clicks don't hide the shelf
/// while a capture is on (the shelf may be what you want to capture).
pub fn set_hold_open(v: bool) {
    controller().hold_open.set(v);
}

/// The screen rect of the shelf while visible (`frameOnScreen`). Both forms
/// answer through this — the desktop-note drop and the outside click test
/// read it, and neither knows which shape the shelf has.
pub fn frame_on_screen() -> Option<CGRect> {
    if crate::app::layout_skin::is_line() {
        return crate::shelf::line_panel::frame_on_screen();
    }
    let p = controller().panel.borrow().clone()?;
    if !p.isVisible() {
        return None;
    }
    Some(p.frame())
}

pub fn is_visible() -> bool {
    if crate::app::layout_skin::is_line() {
        return crate::shelf::line_panel::is_visible();
    }
    controller()
        .panel
        .borrow()
        .as_ref()
        .map(|p| p.isVisible())
        .unwrap_or(false)
}

/// Leave the panel visible but no longer the key window — the state the shelf
/// sits in after a copy hands the keyboard back to the previous app, and the
/// one every self-test above accidentally skipped by going straight from
/// `show()` (key) to the click. `keep_open_on_resign` stops the resign from
/// triggering the hide.
pub fn selftest_drop_key() {
    let c = controller();
    c.keep_open_on_resign.set(true);
    if let Some(p) = c.panel.borrow().as_ref() {
        unsafe {
            // SAFETY: `resignKeyWindow` has no generated binding.
            let _: () = msg_send![&**p, resignKeyWindow];
        }
    }
}

pub fn refocus() {
    if let Some(p) = controller().panel.borrow().as_ref() {
        if p.isVisible() {
            p.makeKeyWindow();
        }
    }
}

/// True while a system dialog (open / save / alert) owns the screen.
///
/// The poll timer below keeps firing inside that dialog's nested event loop,
/// so the view refresh has to stand down: repainting mid-dialog can hand the
/// keyboard back to the shelf, and — when the dialog's caller still holds the
/// model borrow — borrowing again aborts the process. See `view::refresh`.
pub fn dialog_open() -> bool {
    controller().hold_open.get()
}

/// Run a system dialog (open / save / alert) from the shelf: the shelf
/// normally floats above everything, which would bury the dialog and leave
/// the user stuck. Lower it for the duration, keep it open, then restore.
pub fn with_dialog<T>(body: impl FnOnce() -> T) -> T {
    let c = controller();
    c.hold_open.set(true);
    let panel = c.panel.borrow().clone();
    if let Some(p) = &panel {
        p.setLevel(0);
        unsafe {
            let sender: Option<&AnyObject> = None;
            let _: () = msg_send![&**p, orderBack: sender];
        }
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    activate_app(mtm);
    let result = body();
    if let Some(p) = &panel {
        p.setLevel(STATUS_BAR_LEVEL);
    }
    c.hold_open.set(false);
    refocus();
    result
}

pub fn toggle() {
    if crate::app::layout_skin::is_line() {
        crate::shelf::line_panel::toggle();
        return;
    }
    if is_visible() {
        hide();
    } else {
        show();
    }
}

/// Open the settings page. The page lives in the panel form, so from the 晾衣绳
/// this switches the shelf to the panel (see `line_panel::switch_to_settings`)
/// rather than silently doing nothing — the strip never owns the keyboard, so a
/// settings window has nowhere else to be. In the panel form it is the original
/// path.
pub fn show_settings() {
    if crate::app::layout_skin::is_line() {
        crate::shelf::line_panel::switch_to_settings();
        return;
    }
    if !is_visible() {
        show();
    }
    with_model_mut(|m| m.show_settings = true);
    crate::shelf::view::refresh();
}

/// Open (if needed) with the search box focused (`showSearch`).
pub fn show_search() {
    if crate::app::layout_skin::is_line() {
        // The strip has no search box — it never owns the keyboard. A summon
        // just brings the strip up; its right-click menu carries the way back
        // to the panel, where search and the settings live.
        crate::shelf::line_panel::show();
        return;
    }
    if !is_visible() {
        show();
    }
    with_model_mut(|m| {
        m.show_settings = false;
        m.focus_search += 1;
    });
    crate::shelf::view::refresh();
}

pub fn show() {
    // The form is the shelf's, not this module's: the strip never takes the
    // keyboard, so its whole reveal path is the other module's.
    if crate::app::layout_skin::is_line() {
        crate::shelf::line_panel::show();
        return;
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    let p = ensure_panel();
    remember_previous_app();
    controller().keep_open_on_resign.set(false);
    // Decode the first row of thumbnails while the shelf slides in.
    crate::clipboard::store::with(|s| {
        for it in s
            .items
            .clone()
            .into_iter()
            .take(10)
            .filter(|it| it.kind == ClipKind::Image || it.kind == ClipKind::Video)
        {
            s.warm_thumbnail(&it);
        }
    });
    install_outside_monitor();
    let mouse = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    let mut frame = screens
        .iter()
        .find(|s| contains_point(&s.frame(), mouse))
        .map(|s| s.frame());
    if frame.is_none() {
        frame = NSScreen::mainScreen(mtm).map(|s| s.frame());
    }
    if frame.is_none() {
        frame = screens.firstObject().map(|s| s.frame());
    }
    let screen = frame.expect("at least one screen");
    // 48% minus about a centimetre; cards follow the panel.
    let height = (screen.size.height * 0.48).round().max(0.0);
    let height = 384.0_f64.max(height - 36.0);
    let target = CGRect::new(screen.origin, CGSize::new(screen.size.width, height));
    with_model_mut(|m| m.reset());
    // The window itself never leaves this screen; the slide happens to the
    // content inside the window, together with a fade.
    controller().material.set(true);
    crate::app::theme::set_panel_material_alpha(1.0);
    let root = attach(mtm, target.size);
    p.setFrame_display(target, false);
    p.setAlphaValue(0.0);
    root.setFrame(CGRect::new(CGPoint::new(0.0, -SLIDE), target.size));
    // Activate the app before the panel comes forward.
    //
    // The panel is a `NonactivatingPanel`: `makeKeyWindow` makes the *window*
    // key, but the app itself stays in the background. macOS then routes no
    // keyboard to it at all — the panel looks key, yet a click starts no
    // field editor and every keystroke goes to whatever app is active. That
    // is the report exactly: the search box stayed grey, a click put no
    // caret in it, and typing did nothing. `withDialog` already activates
    // for the same reason; opening the shelf needs it too.
    activate_app(mtm);
    controller().want_key.set(true);
    p.orderFrontRegardless();
    p.makeKeyWindow();
    p.makeFirstResponder(None);
    install_poll_timer();
    animate(0.22, "easeOut", move || {
        set_alpha_animated(&p, 1.0);
        set_frame_animated(&root, CGRect::new(CGPoint::ZERO, target.size));
    });
}

/// Bring this accessory app to the front so the window server delivers keys
/// to it. A `NonactivatingPanel` alone only makes the window key, which is not
/// enough: an inactive app receives no keyboard events at all.
///
/// The shared implementation lives in `app::activation`; new callers use that,
/// and this copy is kept because the shelf's own show path predates it.
///
/// `NSApplication.activate()` does nothing here — an accessory process has no
/// Dock icon and no window that counts as the app's own, so the cooperative
/// activation rules leave it in the background and every keystroke still goes
/// to the previously active app. `NSRunningApplication.activate(options:)`
/// (macOS 14+) is the call that actually brings the process forward, verified
/// against real window-server events: with it the panel's own ⎋ closes the
/// shelf, without it nothing reaches the app at all.
fn activate_app(_mtm: MainThreadMarker) {
    let app = NSRunningApplication::currentApplication();
    // `ActivateIgnoringOtherApps` is deprecated in favour of the cooperative
    // activation rules, but an accessory process owns no window that the
    // system counts as its own, so the cooperative path declines and this flag
    // is the only one that brings the process forward.
    #[allow(deprecated)]
    let _ = app.activateWithOptions(
        objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
    );
}

pub fn hide() {
    if crate::app::layout_skin::is_line() {
        crate::shelf::line_panel::hide();
        return;
    }
    hide_panel_window();
}

/// Take the drawer's window down — fade it out, then order it out — and stop
/// the two things only a visible drawer needs (the repaint poll, the
/// outside-click monitor).
///
/// Split out of `hide` for the form switch: `hide` dispatches on the *current*
/// form, and by the time a switch has to put the drawer away `layout_skin`
/// already answers 晾衣绳. Going through `hide` there would retract a strip
/// that is not up yet and leave the drawer on screen (`panel::relayout`).
fn hide_panel_window() {
    stop_poll_timer();
    with_model_mut(|m| m.cancel_pending_hand_back());
    remove_outside_monitor();
    controller().keep_open_on_resign.set(false);
    let Some(p) = controller().panel.borrow().clone() else { return };
    if !p.isVisible() {
        return;
    }
    let Some(content) = p.contentView() else { return };
    let size = p.frame().size;
    let p2 = p.clone();
    let content2 = content.clone();
    let completion = block2::RcBlock::new(move || {
        let sender: Option<&AnyObject> = None;
        p2.orderOut(sender);
        content2.setFrame(CGRect::new(CGPoint::ZERO, size));
        p2.setAlphaValue(1.0);
    });
    let changes = block2::RcBlock::new(move |_: std::ptr::NonNull<objc2_app_kit::NSAnimationContext>| {
        set_timing(0.18, "easeIn");
        set_alpha_animated(&p, 0.0);
        set_frame_animated(&content, CGRect::new(CGPoint::new(0.0, -SLIDE), size));
    });
    objc2_app_kit::NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&completion));
}

/// The app-switch watcher: with the shelf floating without the keyboard,
/// switching to yet another app closes it (`watchAppSwitches`).
fn watch_app_switches() {
    let c = controller();
    if c.watching_switches.replace(true) {
        return;
    }
    unsafe {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let block = block2::RcBlock::new(|n: std::ptr::NonNull<NSNotification>| {
            let n = n.as_ref();
            let Some(app) = activated_app(n) else { return };
            let is_current = app.isEqual(Some(&NSRunningApplication::currentApplication()));
            if is_current {
                // The activation `show()` asks for lands asynchronously.
                // `activateWithOptions` returns — and `isActive()` already
                // answers true — while the window server is still showing the
                // previous app, so the `makeKeyWindow()` that runs in the same
                // turn finds no key to take. This notification is the moment
                // the process is genuinely frontmost: take the keyboard here,
                // because nothing after `show()` ever will.
                let c = controller();
                let p = c.panel.borrow().clone();
                if let Some(p) = p {
                    if p.isVisible() && !p.isKeyWindow() && !c.hold_open.get() {
                        p.makeKeyWindow();
                    }
                }
                return;
            }
            let is_previous = controller()
                .previous_app
                .borrow()
                .as_ref()
                .map(|p| app.isEqual(Some(&**p)))
                .unwrap_or(false);
            if is_previous {
                return;
            }
            let c = controller();
            let p = c.panel.borrow().clone();
            if let Some(p) = p {
                if p.isVisible() && !p.isKeyWindow() && !c.hold_open.get() {
                    hide();
                }
            }
        });
        let obs = center.addObserverForName_object_queue_usingBlock(
            Some(objc2_app_kit::NSWorkspaceDidActivateApplicationNotification),
            None,
            None,
            &block,
        );
        std::mem::forget(obs);
    }
}

/// `NSWorkspaceApplicationUserInfoKey` as NSRunningApplication.
fn activated_app(n: &NSNotification) -> Option<Retained<NSRunningApplication>> {
    let info = n.userInfo()?;
    let key = unsafe { objc2_app_kit::NSWorkspaceApplicationKey };
    let obj = info.objectForKey(key)?;
    // SAFETY: the documented value class for this key.
    Some(unsafe { Retained::cast_unchecked::<NSRunningApplication>(obj) })
}

/// A click anywhere else closes the shelf, even when it no longer holds the
/// keyboard (after a copy).
fn install_outside_monitor() {
    let c = controller();
    if c.outside_monitor.borrow().is_some() {
        return;
    }
    let block = block2::RcBlock::new(|_e: std::ptr::NonNull<NSEvent>| {
        let c = controller();
        let Some(p) = c.panel.borrow().clone() else { return };
        if !p.isVisible() || c.hold_open.get() {
            return;
        }
        if !contains_point(&p.frame(), NSEvent::mouseLocation()) {
            hide();
        }
    });
    let mask = objc2_app_kit::NSEventMask::LeftMouseDown | objc2_app_kit::NSEventMask::RightMouseDown;
    if let Some(m) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block) {
        let raw = Retained::into_raw(m) as usize;
        *c.outside_monitor.borrow_mut() = Some(raw);
    }
}

fn remove_outside_monitor() {
    let c = controller();
    if let Some(raw) = c.outside_monitor.borrow_mut().take() {
        // SAFETY: retained at install; released exactly once here after
        // removal (NSWindow.removeMonitor does not consume the retain).
        let m = unsafe { Retained::from_raw(raw as *mut AnyObject) }.expect("monitor");
        unsafe { NSEvent::removeMonitor(&m) };
    }
}

/// Thumbnail decode + store writes invalidate the cards; SwiftUI observes,
/// AppKit polls lightly while the shelf is up.
///
/// Visible to the self-tests so they can reproduce the modal re-entrancy the
/// pin-delete confirm used to hit (`--selftest deletepin`).
pub(crate) fn install_poll_timer() {
    let c = controller();
    if c.poll_timer.borrow().is_some() {
        return;
    }
    unsafe {
        let block = block2::RcBlock::new(|_: std::ptr::NonNull<objc2_foundation::NSTimer>| {
            crate::shelf::view::refresh();
            // Take the keyboard once the app is genuinely frontmost.
            //
            // `show()` asks to activate and asks to be key in the same turn,
            // but the activation lands later: `isActive()` reports true while
            // the window server still shows the previous app, so that earlier
            // `makeKeyWindow()` is a no-op. Until the process really is in
            // front, a keystroke is delivered to nobody and a click starts no
            // field editor — the shelf looks key and behaves dead. This is the
            // retry that closes the gap.
            let c = controller();
            if c.want_key.get() {
                let p = c.panel.borrow().clone();
                if let Some(p) = p {
                    if !p.isVisible() {
                        c.want_key.set(false);
                    } else if !p.isKeyWindow() {
                        if NSRunningApplication::currentApplication().isActive() {
                            p.makeKeyWindow();
                        }
                    } else {
                        c.want_key.set(false);
                    }
                }
            }
        });
        let t = objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(0.25, true, &block);
        t.setTolerance(0.1);
        let mode: &'static objc2_foundation::NSRunLoopMode = objc2_foundation::NSRunLoopCommonModes;
        objc2_foundation::NSRunLoop::mainRunLoop().addTimer_forMode(&t, mode);
        *c.poll_timer.borrow_mut() = Some(Retained::into_raw(t) as usize);
    }
}

/// The shelf is going away: stop the repaint poll. It is installed on every
/// show, and without this it would keep reloading a hidden tree four times a
/// second for the life of the process.
fn stop_poll_timer() {
    let c = controller();
    let Some(raw) = c.poll_timer.borrow_mut().take() else { return };
    // SAFETY: retained at install; released exactly once here.
    if let Some(t) =
        unsafe { Retained::from_raw(raw as *mut objc2_foundation::NSTimer) }
    {
        t.invalidate();
    }
}

thread_local! {
    /// `--selftest deletepin` answers the pin-delete confirm here instead of a
    /// real alert: (verdict, seconds of nested event loop to run first).
    static CONFIRM_STUB: RefCell<Option<(bool, f64)>> = const { RefCell::new(None) };
}

/// Arm the confirm stub for `--selftest deletepin` (see `confirm_delete`).
pub(crate) fn stub_confirm_delete(verdict: bool, pump_secs: f64) {
    CONFIRM_STUB.with(|s| *s.borrow_mut() = Some((verdict, pump_secs)));
}

/// Keep a nested event loop running for `secs` — the shape of a modal dialog.
/// A real `NSAlert.runModal` parks here too, so the poll timer (common modes)
/// fires inside it exactly as it does in the product.
fn pump_run_loop(secs: f64) {
    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
    while std::time::Instant::now() < deadline {
        let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
        // SAFETY: NSDefaultRunLoopMode is a valid mode token for the main loop.
        let mode: &'static objc2_foundation::NSRunLoopMode =
            unsafe { objc2_foundation::NSDefaultRunLoopMode };
        run_loop.runMode_beforeDate(mode, &limit);
    }
}

fn contains_point(r: &CGRect, p: CGPoint) -> bool {
    p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y
}

// MARK: Animations (NSAnimationContext 0.22/0.18 with CAMedia curves)

fn set_alpha_animated(window: &ShelfPanel, alpha: f64) {
    unsafe {
        let proxy: Retained<AnyObject> = msg_send![&*window, animator];
        let _: () = msg_send![&*proxy, setAlphaValue: alpha];
    }
}

fn set_frame_animated(view: &NSView, frame: CGRect) {
    unsafe {
        let proxy: Retained<AnyObject> = msg_send![&*view, animator];
        let _: () = msg_send![&*proxy, setFrame: frame];
    }
}

/// `ctx.duration = secs; ctx.timingFunction = CAMediaTimingFunction(name:)`.
fn set_timing(secs: f64, curve: &str) {
    let ctx = objc2_app_kit::NSAnimationContext::currentContext();
    ctx.setDuration(secs);
    unsafe {
        let Some(cls) = AnyClass::get(c"CAMediaTimingFunction") else { return };
        let f: Retained<AnyObject> =
            msg_send![cls, functionWithName: &*NSString::from_str(curve)];
        let _: () = msg_send![&*ctx, setTimingFunction: &*f];
    }
}

/// NSAnimationContext group with a CAMedia timing curve.
fn animate(secs: f64, curve: &'static str, body: impl FnOnce()) {
    let slot = std::cell::Cell::new(Some(body));
    let block = block2::RcBlock::new(
        move |_: std::ptr::NonNull<objc2_app_kit::NSAnimationContext>| {
            set_timing(secs, curve);
            if let Some(b) = slot.take() {
                b();
            }
        },
    );
    objc2_app_kit::NSAnimationContext::runAnimationGroup(&block);
}

// MARK: Paste into the app you came from

/// Double-click / ⏎: once the shelf is gone and the previous app has the
/// keyboard again, press ⌘V for the user. Needs Accessibility; without it
/// (or with the setting off) this is a plain copy-and-close.
pub fn paste_into_previous_app() {
    if !Preferences::shared().paste_on_double_click() {
        return;
    }
    let Some(app) = controller().previous_app.borrow().clone() else { return };
    if app.isTerminated() {
        return;
    }
    if !permissions::has_accessibility() {
        permissions::request_accessibility();
        return;
    }
    app.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::ActivateAllWindows);
    // The hide animation takes 0.18 s; give activation a moment, then confirm
    // the target is actually in front.
    attempt_paste(app, 0);
}

fn attempt_paste(app: Retained<NSRunningApplication>, tries: u32) {
    // The dispatch closure must be Send; the app object crosses as a raw
    // retain (released inside, on the main queue).
    let raw = Retained::into_raw(app) as usize;
    let delay = if tries == 0 { 0.25 } else { 0.06 };
    crate::app::delegate::dispatch_main_after(
        delay,
        Box::new(move || {
            // SAFETY: retained once per attempt above/below; consumed here.
            let app = unsafe { Retained::from_raw(raw as *mut NSRunningApplication) }.expect("app alive");
            let front = NSWorkspace::sharedWorkspace().frontmostApplication();
            let is_front = front
                .as_ref()
                .map(|f| f.processIdentifier() == app.processIdentifier())
                .unwrap_or(false);
            if is_front {
                permissions::send_paste();
            } else if tries < 8 {
                attempt_paste(app, tries + 1);
            }
        }),
    );
}

/// After copying with a single click: the shelf stays, the keyboard goes
/// back to the app you were in (`handBackFocus`).
pub fn hand_back_focus() {
    let c = controller();
    let Some(p) = c.panel.borrow().clone() else { return };
    let Some(app) = c.previous_app.borrow().clone() else { return };
    if !p.isVisible() || app.isTerminated() {
        return;
    }
    if with_model(|m| m.renaming_id.is_some()) {
        return;
    }
    // Only a real resign should be swallowed.
    c.keep_open_on_resign.set(p.isKeyWindow());
    app.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::ActivateAllWindows);
}

/// `pendingHandBack` generation check, then the hand-back (model-scheduled).
pub(crate) fn hand_back_focus_if_current(generation: u64) {
    let current = with_model(|m| m.pending_hand_back_generation());
    if current == generation {
        hand_back_focus();
    }
}

/// The pinned-delete confirm (double-button NSAlert through `withDialog`).
fn confirm_delete() -> bool {
    // Self-test path: no real alert, but the same nested event loop. It
    // deliberately skips `with_dialog` so the test exercises the borrow
    // discipline rather than the dialog guard.
    if let Some((verdict, secs)) = CONFIRM_STUB.with(|s| s.borrow_mut().take()) {
        pump_run_loop(secs);
        return verdict;
    }
    with_dialog(|| {
        let mtm = MainThreadMarker::new().expect("main thread");
        let alert = objc2_app_kit::NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&l("这条是 Pin 住的，确定删除？")));
        alert.setInformativeText(&NSString::from_str(&l(
            "Pin 住的内容不会被自动清理，只有这样手动删除才会消失，而且不能恢复。",
        )));
        alert.addButtonWithTitle(&NSString::from_str(&l("删除")));
        alert.addButtonWithTitle(&NSString::from_str(&l("取消")));
        alert.runModal() == objc2_app_kit::NSAlertFirstButtonReturn
    })
}

/// The pinned-delete confirm + delete flow (`model.delete`).
///
/// The confirm runs *before* the model borrow, never inside it: `NSAlert`
/// parks in a nested event loop, the 0.25 s poll timer fires in that loop and
/// reloads the cards, and reloading takes the model borrow again. Holding it
/// across the alert therefore aborted the process the moment a user deleted a
/// pinned card.
pub(crate) fn delete_item(item: &ClipItem) {
    if item.pinned && !confirm_delete() {
        return;
    }
    with_model_mut(|m| m.delete(item));
    // The removed card may have been the highlighted one (`selectAvailableItem`).
    with_model_mut(|m| m.select_available_item());
}

/// ⌫ on the selected card (the NSAlert rides along only for pinned cards).
fn delete_selected() {
    let Some(item) = with_model_mut(|m| m.selected_item()) else { return };
    if item.pinned && !confirm_delete() {
        return;
    }
    with_model_mut(|m| m.delete_selected());
    finish_pending_close();
}

/// Drain a copy-and-close the model only queued (`model::copy_and_close`).
/// Call it once the model borrow is released, never from inside it: `hide()`
/// takes that borrow itself.
pub(crate) fn finish_pending_close() {
    let Some(paste) = with_model_mut(|m| m.take_pending_close()) else { return };
    hide();
    if paste {
        paste_into_previous_app();
    }
}

// MARK: keyboard table (`ShelfPanelController.handle`)

/// Only while visible. Returns true when the key was consumed.
pub(crate) fn handle(event: &NSEvent) -> bool {
    const VK_ESCAPE: u16 = 53;
    const VK_RETURN: u16 = 36;
    const VK_KEYPAD_ENTER: u16 = 76;
    const VK_LEFT: u16 = 123;
    const VK_RIGHT: u16 = 124;
    const VK_UP: u16 = 126;
    const VK_DOWN: u16 = 125;
    const VK_P: u16 = 35;
    const VK_S: u16 = 1;
    const VK_DELETE: u16 = 51;
    const VK_F: u16 = 3;
    const VK_SPACE: u16 = 49;

    // A title box is open: everything goes to it, ⎋ closes it.
    if with_model(|m| m.renaming_id.is_some()) {
        if event.keyCode() == VK_ESCAPE {
            crate::shelf::view::end_rename();
            return true;
        }
        return false;
    }
    let c = controller();
    let fr = c.panel.borrow().as_ref().and_then(|p| p.firstResponder());
    let (typing, composing) = responder_typing(fr.as_deref());
    let flags = event.modifierFlags();
    let cmd = flags.contains(objc2_app_kit::NSEventModifierFlags::Command);
    let ctrl = flags.contains(objc2_app_kit::NSEventModifierFlags::Control);
    let opt = flags.contains(objc2_app_kit::NSEventModifierFlags::Option);
    let code = event.keyCode();
    // Settings / contact panes: the shortcut recorders get every key first
    // (their capture suspends the hotkeys); otherwise only ⎋ (back) and ⌘F
    // (to the shelf's search) mean anything (contract §5.14).
    if with_model(|m| m.show_settings || m.show_contact) {
        if let Some(v) = crate::shelf::view::settings_view() {
            if crate::shelf::settings_pane::recorder_key_down(&v, event) {
                return true;
            }
        }
        if code == VK_ESCAPE {
            // Back to the shelf. Only a page opened from the strip closes the
            // shelf here (see `line_panel::leave_settings`); from the panel ⎋
            // has always meant "leave the page".
            crate::shelf::line_panel::leave_settings();
            return true;
        }
        if code == VK_F && cmd {
            // Turning to the shelf's search means staying in the panel — only
            // it has a search box — so the strip is not owed a return here.
            crate::shelf::line_panel::forget_return_to_line();
            with_model_mut(|m| {
                m.show_settings = false;
                m.show_contact = false;
                m.focus_search += 1;
            });
            crate::shelf::view::refresh();
            return true;
        }
        return false;
    }
    // Welcome card's two shortcut recorders get every key while capturing.
    if typed_welcome(event) {
        return true;
    }
    // A full-size picture is up: ⎋ closes it and nothing else moves. Done
    // before the table below so the shelf's own ⎋ (hide) cannot also fire.
    if event.keyCode() == VK_ESCAPE && crate::shelf::image_viewer::close() {
        return true;
    }
    let consumed = match code {
        VK_ESCAPE => {
            if typing && with_model(|m| !m.query().is_empty()) {
                with_model_mut(|m| m.set_query(String::new()));
                crate::shelf::view::refresh();
                true
            } else {
                hide();
                true
            }
        }
        VK_RETURN | VK_KEYPAD_ENTER => {
            if composing {
                return false;
            }
            with_model_mut(|m| m.copy_selected());
            // The model only queued the close; run it now that the borrow is
            // gone (see `model::copy_and_close`).
            finish_pending_close();
            true
        }
        VK_LEFT if !typing => {
            with_model_mut(|m| m.move_selection(-1));
            true
        }
        VK_RIGHT if !typing => {
            with_model_mut(|m| m.move_selection(1));
            true
        }
        VK_UP if !composing => {
            with_model_mut(|m| m.move_selection(-1));
            true
        }
        VK_DOWN if !composing => {
            with_model_mut(|m| m.move_selection(1));
            true
        }
        VK_P if cmd => {
            with_model_mut(|m| m.pin_selected());
            true
        }
        VK_S if cmd => {
            // The save panel is a modal dialog: take the item first, then run
            // it outside the borrow (same reason as the pin-delete confirm).
            let item = with_model_mut(|m| m.selected_item());
            if let Some(it) = item {
                crate::shelf::exporter::export(&it);
            }
            true
        }
        VK_DELETE if !typing => {
            delete_selected();
            true
        }
        VK_F if cmd => {
            with_model_mut(|m| m.focus_search += 1);
            true
        }
        VK_SPACE if !typing => {
            toggle_quick_look();
            true
        }
        _ => {
            // Just start typing: letters go straight into the search box.
            // No early `return true` here: every other consumed key falls
            // through to the refresh below, and this one has to as well. The
            // refresh is what runs `focus_search_field` — bumping
            // `focus_search` alone only sets a flag, and the focus jump (plus
            // the `pending_query` flush it schedules) would wait for the
            // 0.25 s poll tick. A character typed inside that window reaches
            // the box first and is then re-appended behind the pending one,
            // so fast typing came out transposed.
            if !typing && !cmd && !ctrl && !opt {
                if let Some(chars) = event.characters().map(|s| s.to_string()) {
                    if !chars.is_empty() && chars.chars().all(searchable_scalar) {
                        with_model_mut(|m| {
                            m.pending_query = chars;
                            m.focus_search += 1;
                        });
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        }
    };
    if consumed {
        crate::shelf::view::refresh();
    }
    consumed
}

/// Route a key to the welcome card's live recorder (it captures inside the
/// shelf's own keyboard table; the key never reaches the search box).
pub(crate) fn typed_welcome(event: &NSEvent) -> bool {
    let Some(v) = crate::shelf::view::welcome_view() else { return false };
    crate::shelf::welcome_card::recorder_key(&v, event)
}

/// Function / navigation keys arrive as private-use scalars (U+F700…) and
/// must never land in the search box.
fn searchable_scalar(c: char) -> bool {
    if c.is_control() {
        return false;
    }
    let v = c as u32;
    !(0xE000..=0xF8FF).contains(&v) && !(0xF0000..=0xFFFFD).contains(&v) && !(0x100000..=0x10FFFD).contains(&v)
}

/// Swift's "typing": any text input owns the keyboard (field editor,
/// SwiftUI text view, NSTextField). `composing` = IME candidate window open.
fn responder_typing(fr: Option<&objc2_app_kit::NSResponder>) -> (bool, bool) {
    let Some(fr) = fr else { return (false, false) };
    let typing = fr.class().name().to_bytes().windows(4).any(|w| w == b"Text") || {
        let text_cls = AnyClass::get(c"NSText");
        let field_cls = AnyClass::get(c"NSTextField");
        match (text_cls, field_cls) {
            (Some(t), Some(f)) => fr.isKindOfClass(t) || fr.isKindOfClass(f),
            _ => false,
        }
    };
    let composing = typing
        && fr.respondsToSelector(sel!(hasMarkedText))
        && unsafe { msg_send![fr, hasMarkedText] };
    (typing, composing)
}

// MARK: Quick Look (space) — hand-declared QLPreviewPanel

objc2::extern_class!(
    /// Hand declaration: Quartz's Quick Look panel (no binding crate in the
    /// dependency set). Layout-compatible with NSPanel.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[derive(Debug)]
    struct QLPreviewPanel;
);

/// QLPreviewPanel lives in Quartz.framework, which nothing links by default;
/// load it once so the class lookup succeeds (Swift links it transitively).
fn ensure_quartz_loaded() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let path = NSString::from_str("/System/Library/Frameworks/Quartz.framework");
        if let Some(bundle) = objc2_foundation::NSBundle::bundleWithPath(&path) {
            unsafe { let _ = bundle.load(); }
        }
    });
}

fn ql_panel() -> Option<Retained<QLPreviewPanel>> {
    ensure_quartz_loaded();
    unsafe { msg_send![objc2::class!(QLPreviewPanel), sharedPreviewPanel] }
}

fn ql_panel_exists() -> bool {
    ensure_quartz_loaded();
    unsafe { msg_send![objc2::class!(QLPreviewPanel), sharedPreviewPanelExists] }
}

fn ql_is_visible(ql: &QLPreviewPanel) -> bool {
    unsafe { msg_send![ql, isVisible] }
}

fn ql_reload(ql: &QLPreviewPanel) {
    unsafe {
        let _: () = msg_send![ql, reloadData];
    }
}

/// File to preview for the selected card: the payload itself, or the first
/// file of a files item (`quickLookURL`).
///
/// This takes the model mutably — resolving the selection walks the list
/// cache, which writes — so *no caller may hold a model borrow when it runs*.
/// That is the whole contract `toggle_quick_look` had been breaking: the card
/// actions used to call it from inside a `with_model_mut`, which re-enters the
/// `RefCell` and aborts the process.
pub(crate) fn quick_look_url() -> Option<std::path::PathBuf> {
    with_model_mut(|m| {
        let item = m.selected_item()?;
        if item.kind == ClipKind::Files {
            return m.with_store(|s| s.file_urls(&item).into_iter().next());
        }
        Some(m.with_store(|s| s.payload_url(&item)))
    })
}

pub(crate) fn toggle_quick_look() {
    if let Some(ql) = ql_panel() {
        if ql_is_visible(&ql) {
            unsafe {
                let sender: Option<&AnyObject> = None;
                let _: () = msg_send![&*ql, orderOut: sender];
            }
            // The foreground is ours again — the strip may come back.
            crate::shelf::line_panel::external_window_closed();
            return;
        }
    }
    if quick_look_url().is_none() {
        return;
    }
    controller().hold_open.set(true);
    // Quick Look owns the foreground now: the strip must not climb over it.
    crate::shelf::line_panel::external_window_opened();
    if let Some(ql) = ql_panel() {
        unsafe {
            let sender: Option<&AnyObject> = None;
            let _: () = msg_send![&*ql, makeKeyAndOrderFront: sender];
        }
    }
}

/// Test hook: is the Quick Look panel up right now? `--selftest previewvideo`
/// asks after the same call the live card makes, so a panel that never opened
/// reads as a failure rather than as an untested path.
pub(crate) fn debug_quick_look_visible() -> bool {
    ql_panel().is_some_and(|ql| ql_is_visible(&ql))
}

/// Selection moved with the preview up: reload (`quickLookSelectionChanged`).
/// Deferred: QL pulls the data itself, and a mid-mutation reload could
/// re-enter the model.
pub(crate) fn quick_look_selection_changed() {
    if !ql_panel_exists() {
        return;
    }
    crate::app::delegate::dispatch_main_after(0.0, Box::new(|| {
        if let Some(ql) = ql_panel() {
            if ql_is_visible(&ql) {
                ql_reload(&ql);
            }
        }
    }));
}

// MARK: ShelfPanel (+ QL data source / delegate) + window delegate

pub struct ShelfPanelIvars;

define_class!(
    // SAFETY:
    // - NSPanel subclass, main-thread-only like every window.
    // - The QL data source methods answer from the shared controller.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ShelfPanelIvars]
    pub struct ShelfPanel;

    unsafe impl NSObjectProtocol for ShelfPanel {}

    impl ShelfPanel {
        // The ObjC selectors are `canBecomeKeyWindow` / `canBecomeMainWindow`
        // (the Swift property `canBecomeKey` is only a Swift name). Registering
        // the Swift spelling overrides nothing, and the stock `NSPanel` answers
        // NO for a `nonactivatingPanel` — which made every `makeKeyWindow()` a
        // silent no-op, so the panel never took the keyboard and no click or
        // keystroke was ever delivered to it.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> objc2::runtime::Bool {
            false.into()
        }

        // Quick Look asks the key window's responder chain who wants the panel.
        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_preview_panel_control(&self, _panel: *mut AnyObject) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin_preview_panel_control(&self, panel: *mut AnyObject) {
            unsafe {
                let me: Option<&AnyObject> = Some(&*(self as *const Self as *const AnyObject));
                let _: () = msg_send![panel, setDataSource: me];
                let _: () = msg_send![panel, setDelegate: me];
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end_preview_panel_control(&self, panel: *mut AnyObject) {
            unsafe {
                let none: Option<&AnyObject> = None;
                let _: () = msg_send![panel, setDataSource: none];
                let _: () = msg_send![panel, setDelegate: none];
            }
            let c = controller();
            c.hold_open.set(false);
            // Quick Look gave the foreground back (its own ⎋, another app, or
            // the shelf taking control again).
            crate::shelf::line_panel::external_window_closed();
            refocus();
        }

        #[unsafe(method(numberOfPreviewItemsInPanel:))]
        fn number_of_preview_items(&self, _panel: *mut AnyObject) -> isize {
            if quick_look_url().is_some() { 1 } else { 0 }
        }

        #[unsafe(method(previewPanel:previewItemAtIndex:))]
        fn preview_item_at(&self, _panel: *mut AnyObject, _index: isize) -> *mut AnyObject {
            let Some(path) = quick_look_url() else { return std::ptr::null_mut() };
            let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(
                &path.to_string_lossy(),
            ));
            // QLPreviewItem is a protocol NSURL conforms to; the return is
            // autoreleased like any borrowed object.
            Retained::autorelease_ptr(url) as *mut AnyObject
        }

        /// Arrow keys keep working while the preview is up.
        #[unsafe(method(previewPanel:handleEvent:))]
        fn preview_panel_handle_event(&self, _panel: *mut AnyObject, event: *mut NSEvent) -> objc2::runtime::Bool {
            let e = unsafe { &*event };
            if e.r#type() != objc2_app_kit::NSEventType::KeyDown {
                return false.into();
            }
            handle(e).into()
        }

        /// Shortcuts are handled before the responder chain so the search
        /// field cannot swallow ⏎ / ⎋.
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            let kind = event.r#type();
            if kind == objc2_app_kit::NSEventType::KeyDown && handle(event) {
                return;
            }
            // Take the keyboard before AppKit runs the click.
            //
            // The shelf is a borderless `NonactivatingPanel`, and after a copy
            // it hands the keyboard back to the previous app
            // (`hand_back_focus`). From then on the panel is visible but not
            // key, and the window server gives a first click in such a window
            // only what it needs to swallow it: `acceptsFirstMouse:` lets the
            // view see the event, but no field editor is started and no
            // keystroke follows. That is exactly the report — the search box
            // looked grey, a click put no caret in it, and typing went to the
            // app behind. Becoming key here, before the click is dispatched,
            // is what makes the one click that lands on a text box the click
            // that focuses it.
            if kind == objc2_app_kit::NSEventType::LeftMouseDown && !self.isKeyWindow() {
                self.makeKeyWindow();
            }
            unsafe {
                let _: () = msg_send![super(self), sendEvent: event];
            }
        }
    }
);

impl ShelfPanel {
    fn new(mtm: MainThreadMarker, content_rect: CGRect, style: NSWindowStyleMask) -> Retained<Self> {
        let this = mtm.alloc::<ShelfPanel>().set_ivars(ShelfPanelIvars);
        unsafe {
            msg_send![
                super(this),
                initWithContentRect: content_rect,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        }
    }
}

/// `windowDidResignKey` → hide, unless a dialog holds it open.
pub struct ShelfPanelWinDelegateIvars;

define_class!(
    // SAFETY: NSObject window delegate; main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ShelfPanelWinDelegateIvars]
    struct ShelfPanelWinDelegate;

    unsafe impl NSObjectProtocol for ShelfPanelWinDelegate {}

    unsafe impl NSWindowDelegate for ShelfPanelWinDelegate {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            let c = controller();
            if c.keep_open_on_resign.replace(false) {
                return;
            }
            if !c.hold_open.get() {
                hide();
            }
        }
    }
);

impl ShelfPanelWinDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<ShelfPanelWinDelegate>().set_ivars(ShelfPanelWinDelegateIvars);
        unsafe { msg_send![super(this), init] }
    }
}

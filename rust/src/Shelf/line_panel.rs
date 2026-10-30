//! The 晾衣绳 window: a full-width strip along the top of the screen that
//! hangs the clips on a rope. The second of the app's two shelf forms — see
//! `app::layout_skin` for why the form is a switch and not a skin.
//!
//! The interaction contract is the opposite of the 面板's. This window never
//! takes focus (`canBecomeKeyWindow` is NO, the app is never activated), so it
//! cannot own the keyboard — no search box, no ⏎, no ⌘F. In exchange it does
//! what the panel cannot: every click that is not on a photo passes straight
//! through to whatever is behind (the window is `ignoresMouseEvents`), and it
//! appears by itself when the pointer reaches the menu bar and leaves when it
//! goes away. That is why `layout_skin` keeps the two forms apart rather than
//! folding one into the other.
//!
//! The click-through is Tendedero's mechanism, verified against its
//! `AppDelegate`: the window ignores mouse events, and a 30 Hz timer — the
//! same one that drives the swing — converts the pointer into the strip's own
//! coordinates and flips `ignoresMouseEvents` to NO exactly while the pointer
//! is inside a photo's rect (inflated 4pt). Off a photo it flips back, so the
//! desktop underneath stays clickable. `line_hit_rects` is that rect list.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSDraggingItem, NSEvent, NSMenu, NSMenuItem, NSPanel, NSPasteboardWriting,
    NSScreen,
    NSScrollView, NSView, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSNotification, NSString};

use crate::app::localization::l;
use crate::app::{layout_skin, motion, theme};
use crate::clipboard::item::{ClipItem, ClipKind};
use crate::shelf::line_card::{self, LineCardData};
use crate::shelf::line_view::{self, LineView};

/// `.floating` window level — below the panel form's `.statusBar` (25) so the
/// two never fight for the top.
const FLOATING_LEVEL: isize = 3;
/// The strip's fade in / out (Tendedero 0.25 s in, 0.5 s out).
const REVEAL_SECS: f64 = 0.25;
const RETRACT_SECS: f64 = 0.5;
/// The frame clock (Tendedero polls at 1/30 s).
const TICK_SECS: f64 = 1.0 / 30.0;
/// How close to the menu bar's bottom edge the pointer must come before the
/// strip reveals itself (Tendedero's hot zone). The menu bar *is* the target,
/// so only a couple of points of overshoot — enough that touching its lower
/// edge is easy, not so much that the strip pops out while the pointer is
/// still inside a window below it.
const HOT_ZONE_BAND: f64 = 2.0;
/// While parked, the clock only looks at the pointer every Nth frame: a
/// retracted strip must stay cheap, and the reveal does not need 30 Hz.
const IDLE_DIVISOR: u32 = 4;
/// A short no-retract window after a manual `toggle()`, so the pointer has
/// time to reach the strip before the clock would take it away again.
const MANUAL_GRACE_SECS: f64 = 1.5;

struct LinePanelController {
    panel: RefCell<Option<Retained<LinePanel>>>,
    delegate: RefCell<Option<Retained<LinePanelWinDelegate>>>,
    view: RefCell<Option<Retained<LineView>>>,
    cards: RefCell<Vec<LineCardData>>,
    /// Whether the strip is on screen (as opposed to parked above it).
    revealed: Cell<bool>,
    /// Wall-clock seconds at the last reveal; the swing is `now - started`.
    started: Cell<f64>,
    /// The card under the pointer (index), or -1.
    hot: Cell<i64>,
    tick: RefCell<Option<usize>>,
    tick_count: Cell<u32>,
    /// Whether the automatic reveal/retract runs — the self-tests turn it off
    /// while they drive the strip by hand.
    auto: Cell<bool>,
    /// A short no-retract window after a manual reveal (see
    /// `MANUAL_GRACE_SECS`).
    grace: Cell<f64>,
    /// An editor / the picture viewer / Quick Look owns the foreground. While
    /// it does, the clock must neither reveal the strip behind it nor retract
    /// it — a window the user is typing into would otherwise get a strip
    /// hanging over the menu bar, and the strip they are about to come back
    /// to would disappear first.
    external_hold: Cell<bool>,
    /// A drag out of the strip is in flight. While it is, the clock must not
    /// touch the window at all: the click-through flip would set
    /// `ignoresMouseEvents` back to YES mid-gesture (killing the drag session)
    /// and the presence check would retract the strip out from under the
    /// pointer (the source window leaving cancels the drag).
    dragging: Cell<bool>,
    /// The pointer button is down on the strip — a pan of the row or a drag-out
    /// in flight. Like `dragging`, the clock must leave the window alone while
    /// it is true: a click-through re-flip would drop the mouse-up that ends
    /// the gesture, and a retract would slide the strip out from under the
    /// pointer mid-pull. Cleared by the view's own mouse-up, or by the clock's
    /// end-poll when that event is missed.
    interacting: Cell<bool>,
    /// Whether the photos sway on the rope (设置 › 外观 › 晾衣绳 › 摆动). Read by
    /// the view every frame, so it is cached here rather than hitting
    /// Preferences 30 times a second.
    swing: Cell<bool>,
    /// The row's horizontal origin to put back into the next clip view, in
    /// points. Set by `rebuild` (which destroys the old clip view) and consumed
    /// by `reload_cards` (the first moment the new document view has a width to
    /// clamp against). 0 means nothing to restore.
    restore_origin: Cell<f64>,
}

static CONTROLLER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn controller() -> &'static LinePanelController {
    let ptr = *CONTROLLER.get_or_init(|| {
        let c = LinePanelController {
            panel: RefCell::new(None),
            delegate: RefCell::new(None),
            view: RefCell::new(None),
            cards: RefCell::new(Vec::new()),
            revealed: Cell::new(false),
            started: Cell::new(0.0),
            hot: Cell::new(-1),
            tick: RefCell::new(None),
            tick_count: Cell::new(0),
            auto: Cell::new(true),
            grace: Cell::new(0.0),
            external_hold: Cell::new(false),
            dragging: Cell::new(false),
            interacting: Cell::new(false),
            swing: Cell::new(true),
            restore_origin: Cell::new(0.0),
        };
        Box::into_raw(Box::new(c)) as usize
    });
    // SAFETY: boxed above for the process lifetime.
    unsafe { &*(ptr as *const LinePanelController) }
}

fn now_secs() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Seconds since the last reveal — the swing's clock, read by the view.
pub fn swing_time() -> f64 {
    let c = controller();
    if c.started.get() <= 0.0 {
        return 0.0;
    }
    (now_secs() - c.started.get()).max(0.0)
}

/// The clips on the rope (a clone; the view draws from this).
pub fn current_cards() -> Vec<LineCardData> {
    controller().cards.borrow().clone()
}

// MARK: Geometry (pure — the self-tests assert on these)

/// The strip's height: the rope's run, the clip, the frame and a margin.
pub fn strip_height() -> f64 {
    line_view::content_height().ceil()
}

fn point_in(r: &CGRect, p: CGPoint) -> bool {
    p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y
}

fn screen_containing(mtm: MainThreadMarker, p: CGPoint) -> Option<Retained<NSScreen>> {
    let screens = NSScreen::screens(mtm);
    if let Some(s) = screens.iter().find(|s| point_in(&s.frame(), p)) {
        return Some(s);
    }
    NSScreen::mainScreen(mtm).or_else(|| screens.firstObject())
}

fn screen_for_pointer(mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
    screen_containing(mtm, NSEvent::mouseLocation())
}

/// Whether the pointer is up at the menu bar on `screen` — the hot zone that
/// brings the strip out by itself.
fn at_menu_bar(screen: &NSScreen, mouse: CGPoint) -> bool {
    let vis = screen.visibleFrame();
    let f = screen.frame();
    mouse.x >= f.min().x && mouse.x <= f.max().x && mouse.y >= vis.max().y - HOT_ZONE_BAND
}

/// The strip's shown frame on `screen`: its top edge flush with the bottom of
/// the menu bar, full width.
pub fn strip_frame(screen: &NSScreen) -> CGRect {
    let vis = screen.visibleFrame();
    let f = screen.frame();
    CGRect::new(
        CGPoint::new(f.min().x, vis.max().y - strip_height()),
        CGSize::new(f.size.width, strip_height()),
    )
}

/// The parked frame: slid fully above the top of the screen so nothing shows.
fn parked_frame(screen: &NSScreen) -> CGRect {
    let f = screen.frame();
    CGRect::new(
        CGPoint::new(f.min().x, f.max().y + 8.0),
        CGSize::new(f.size.width, strip_height()),
    )
}

/// The screen rect of the shelf while shown — the line form's answer to
/// `panel::frame_on_screen`, used by the desktop-note drop and the outside
/// click test.
pub fn frame_on_screen() -> Option<CGRect> {
    if !is_visible() {
        return None;
    }
    let mtm = MainThreadMarker::new()?;
    let screen = screen_for_pointer(mtm)?;
    Some(strip_frame(&screen))
}

// MARK: Lifecycle

fn ensure_panel() -> Retained<LinePanel> {
    let c = controller();
    if let Some(p) = c.panel.borrow().as_ref() {
        return p.clone();
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    let p = LinePanel::new(
        mtm,
        CGRect::new(CGPoint::ZERO, CGSize::new(800.0, strip_height())),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
    );
    p.setLevel(FLOATING_LEVEL);
    p.setOpaque(false);
    p.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
    p.setHasShadow(false);
    p.setHidesOnDeactivate(false);
    p.setIgnoresMouseEvents(true);
    p.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    let delegate = LinePanelWinDelegate::new(mtm);
    let proto: &ProtocolObject<dyn NSWindowDelegate> = ProtocolObject::from_ref(&*delegate);
    p.setDelegate(Some(proto));
    *c.delegate.borrow_mut() = Some(delegate);
    *c.panel.borrow_mut() = Some(p.clone());
    p
}

/// Build the strip's tree (material + rope view) at `size`.
fn build_tree(mtm: MainThreadMarker, size: CGSize) -> (Retained<NSView>, Retained<LineView>) {
    let host = NSView::initWithFrame(NSView::alloc(mtm), CGRect::new(CGPoint::ZERO, size));
    if crate::app::theme_skin::material_code() != 0 {
        use objc2_app_kit::{
            NSAutoresizingMaskOptions, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
            NSVisualEffectState, NSVisualEffectView,
        };
        let v = NSVisualEffectView::initWithFrame(
            NSVisualEffectView::alloc(mtm),
            CGRect::new(CGPoint::ZERO, size),
        );
        let code = crate::app::theme_skin::material_code();
        v.setMaterial(if code == 0 {
            NSVisualEffectMaterial::UnderWindowBackground
        } else {
            NSVisualEffectMaterial(code as isize)
        });
        v.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        v.setState(NSVisualEffectState::Active);
        v.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        host.addSubview(&v);
    }
    // Only the top corners round, the way the panel's silhouette does.
    theme::clip_to_radius(&host, line_view::CORNER_R);
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        CGRect::new(CGPoint::ZERO, size),
    );
    scroll.setHasHorizontalScroller(false);
    scroll.setHasVerticalScroller(false);
    scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
    scroll.setDrawsBackground(false);
    scroll.contentView().setDrawsBackground(false);
    let view = LineView::new(mtm, CGRect::new(CGPoint::ZERO, size));
    scroll.setDocumentView(Some(&view));
    host.addSubview(&scroll);
    (host, view)
}

/// Rebuild the tree at the current screen's size and set the frame.
///
/// The scroll position is carried across: this runs on a form switch, a metric
/// change and a retheme, and the tree it replaces is a new view in a new clip
/// view whose origin starts at 0. Without the carry the row would jump back to
/// the first photo every time the user nudged the card size, which is the
/// 「改完卡片大小，绳子回到第一张」 defect.
fn rebuild(shown: bool) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let p = ensure_panel();
    let screen = screen_for_pointer(mtm);
    let size = screen
        .as_ref()
        .map(|s| CGSize::new(s.frame().size.width, strip_height()))
        .unwrap_or(CGSize::new(1200.0, strip_height()));
    // Read before the old tree goes: the clip origin lives on the scroll view,
    // which `take()` below drops. The restore itself waits for `reload_cards`,
    // because a clamp to the row's ends needs the document view's new width and
    // that is only set there.
    controller().restore_origin.set(
        controller()
            .view
            .borrow()
            .as_ref()
            .map(|v| v.clip_origin_pub())
            .unwrap_or(0.0),
    );
    controller().view.borrow_mut().take();
    let (host, view) = build_tree(mtm, size);
    p.setContentView(Some(&host));
    *controller().view.borrow_mut() = Some(view);
    if let Some(screen) = &screen {
        let frame = if shown { strip_frame(screen) } else { parked_frame(screen) };
        p.setFrame_display(frame, false);
    }
}

/// Rebuild after a form switch — the line form's half of `panel::relayout`.
/// Unlike `retheme` (a skin), a form switch *does* move the window.
pub fn relayout() {
    let was_shown = is_visible();
    // The display choices (card size / gap) are cached per process; a settings
    // change calls `reset` before landing here, so re-resolve now that the
    // tree and every geometry read are about to be rebuilt.
    line_card::metrics::reset();
    controller().auto.set(auto_reveal_enabled());
    controller().swing.set(swing_enabled());
    rebuild(was_shown);
    reload_cards();
    start_clock();
}

/// Re-resolve the display metrics and rebuild a live strip so a settings change
/// is visible at once. Called by the settings pane while the 晾衣绳 is running.
/// Unlike `relayout` it does not re-check the auto-reveal / swing prefs (the
/// pane stores those through their own setters) and does not restart anything.
pub fn refresh_metrics() {
    let was_shown = is_visible();
    line_card::metrics::reset();
    rebuild(was_shown);
    reload_cards();
    if was_shown {
        // Keep the flip honest: the cards moved, so the pointer's target may
        // have changed under it.
        hover_check();
    }
}

/// Whether the strip reveals itself when the pointer reaches the menu bar.
/// Off means it only appears on the hotkey or the right-click menu's manual
/// toggle — the setting exists because an always-on hot zone annoys some users.
pub fn auto_reveal_enabled() -> bool {
    use crate::app::preferences::{ns_string_id, Preferences};
    const KEY: &str = "lineAutoReveal";
    Preferences::shared()
        .defaults()
        .objectForKey(ns_string_id(KEY))
        .map(|o| o.downcast_ref::<objc2_foundation::NSNumber>().map(|n| n.boolValue()).unwrap_or(true))
        .unwrap_or(true)
}

/// Whether the photos sway on the rope. Off freezes the row — some people
/// find a moving strip distracting, and a still row is cheaper to look at.
pub fn swing_enabled() -> bool {
    use crate::app::preferences::{ns_string_id, Preferences};
    const KEY: &str = "lineSwing";
    Preferences::shared()
        .defaults()
        .objectForKey(ns_string_id(KEY))
        .map(|o| o.downcast_ref::<objc2_foundation::NSNumber>().map(|n| n.boolValue()).unwrap_or(true))
        .unwrap_or(true)
}

/// Persist the swing choice and apply it to a live strip.
pub fn store_swing(on: bool) {
    use crate::app::preferences::{ns_string_id, Preferences};
    const KEY: &str = "lineSwing";
    unsafe {
        Preferences::shared().defaults().setObject_forKey(
            Some(&*objc2_foundation::NSNumber::numberWithBool(on)),
            ns_string_id(KEY),
        );
    }
    controller().swing.set(on);
    repaint();
}

/// The current swing choice, as the view reads it each frame.
pub fn swing_active() -> bool {
    controller().swing.get()
}

/// Persist the auto-reveal choice and apply it to a live strip.
pub fn store_auto_reveal(on: bool) {
    use crate::app::preferences::{ns_string_id, Preferences};
    const KEY: &str = "lineAutoReveal";
    unsafe {
        Preferences::shared().defaults().setObject_forKey(
            Some(&*objc2_foundation::NSNumber::numberWithBool(on)),
            ns_string_id(KEY),
        );
    }
    controller().auto.set(on);
}

/// Release the form: stop the clock for good. Called when the shelf switches
/// back to the panel, so a panel-form process does not keep waking up 30 times
/// a second to watch for a strip that is not there.
pub fn suspend() {
    stop_clock();
}

// MARK: The external-window gate

/// A window of ours that owns the foreground has opened (an editor, the
/// picture viewer, Quick Look). The clock stops revealing and retracting the
/// strip while it holds: the strip belongs *behind* that window, and the panel
/// forms' own `hide()` has just put it away, so a reveal would be the strip
/// climbing back over the window the user is working in.
///
/// Idempotent, and cheap enough to call from every open path — the flag is a
/// counter-free latch, so nested openers (an editor opening the OCR panel) do
/// not need a count. Ordered as: the holder opens, the last one to close clears.
pub fn external_window_opened() {
    controller().external_hold.set(true);
}

/// That window closed. Opens the gate and grants a full grace, so a pointer
/// still resting on the menu bar — where it was when the user clicked, and
/// where the strip's hot zone is — does not drag the strip out before they
/// have moved it away.
pub fn external_window_closed() {
    let c = controller();
    c.external_hold.set(false);
    c.grace.set(now_secs() + MANUAL_GRACE_SECS);
}

/// Self-test only: is the external-window gate shut?
pub fn debug_external_hold() -> bool {
    controller().external_hold.get()
}

/// Build the strip and its tree at launch so the first reveal is instant.
pub fn prewarm() {
    let _ = ensure_panel();
    rebuild(false);
    // Honour the stored 自动呼出 / 摆动 choices before the clock starts.
    controller().auto.set(auto_reveal_enabled());
    controller().swing.set(swing_enabled());
    start_clock();
    crate::shelf::panel::install_notify_hook();
    eprintln!("[pastory] line strip prewarmed");
}

pub fn is_visible() -> bool {
    controller().revealed.get()
}

pub fn toggle() {
    if is_visible() {
        hide();
    } else {
        show();
        // A manual reveal happens with the pointer wherever the user left it,
        // usually nowhere near the strip — give it a moment to get there
        // before the clock would retract it.
        controller().grace.set(now_secs() + MANUAL_GRACE_SECS);
    }
}

/// Reveal the strip (idempotent) and start the frame clock.
pub fn show() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let p = ensure_panel();
    let Some(screen) = screen_for_pointer(mtm) else { return };
    if controller().view.borrow().is_none() {
        rebuild(true);
    }
    // The strip never activates the app; it just has to know who was in front
    // so a double click can hand the ⌘V back there.
    crate::shelf::panel::remember_previous_app();
    reload_cards();
    if is_visible() {
        p.displayIfNeeded();
        return;
    }
    controller().revealed.set(true);
    controller().started.set(now_secs());
    // A reveal that was asked for (the hotkey, the menu bar item, an editor
    // closing) happens with the pointer wherever the user left it, usually
    // nowhere near the strip — without this the clock would take the strip
    // back on its first pass, before the user could reach it. The automatic
    // reveal does not come through here (see `presence_check`), and any
    // pointer already on the strip or the menu bar clears the grace again.
    controller().grace.set(now_secs() + MANUAL_GRACE_SECS);
    p.setFrame_display(strip_frame(&screen), false);
    p.setAlphaValue(0.0);
    p.orderFrontRegardless();
    animate_alpha_reveal(p.clone());
    start_clock();
    eprintln!("[pastory] line strip revealed");
}

/// Retract the strip. The clock keeps running: it is what brings the strip
/// back when the pointer returns to the menu bar.
pub fn hide() {
    let c = controller();
    c.grace.set(0.0);
    if !c.revealed.get() {
        return;
    }
    let Some(p) = c.panel.borrow().clone() else { return };
    c.revealed.set(false);
    c.hot.set(-1);
    p.setIgnoresMouseEvents(true);
    if let Some(v) = c.view.borrow().as_ref() {
        v.set_hot(-1);
    }
    let p2 = p.clone();
    let completion = block2::RcBlock::new(move || {
        let sender: Option<&AnyObject> = None;
        p2.orderOut(sender);
        p2.setAlphaValue(1.0);
    });
    let p3 = p.clone();
    let changes =
        block2::RcBlock::new(move |_: std::ptr::NonNull<objc2_app_kit::NSAnimationContext>| {
            set_timing(RETRACT_SECS, "easeIn");
            set_alpha_animated(&p3, 0.0);
        });
    objc2_app_kit::NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&completion));
}

// MARK: The frame clock (swing + hover), one 30 Hz timer

fn start_clock() {
    let c = controller();
    if c.tick.borrow().is_some() {
        return;
    }
    if c.started.get() <= 0.0 {
        c.started.set(now_secs());
    }
    unsafe {
        let block = block2::RcBlock::new(|_: std::ptr::NonNull<objc2_foundation::NSTimer>| {
            tick();
        });
        let t =
            objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(TICK_SECS, true, &block);
        t.setTolerance(0.01);
        let mode: &'static objc2_foundation::NSRunLoopMode = objc2_foundation::NSRunLoopCommonModes;
        objc2_foundation::NSRunLoop::mainRunLoop().addTimer_forMode(&t, mode);
        *c.tick.borrow_mut() = Some(Retained::into_raw(t) as usize);
    }
}

fn stop_clock() {
    let c = controller();
    let Some(raw) = c.tick.borrow_mut().take() else { return };
    if let Some(t) = unsafe { Retained::from_raw(raw as *mut objc2_foundation::NSTimer) } {
        t.invalidate();
    }
}

fn tick() {
    let c = controller();
    c.tick_count.set(c.tick_count.get().wrapping_add(1));
    let mouse = NSEvent::mouseLocation();
    if !c.revealed.get() {
        // Parked: just watch for the pointer at the menu bar, and at a lower
        // rate than the swing needs.
        if c.tick_count.get() % IDLE_DIVISOR == 0 {
            presence_check(mouse);
        }
        return;
    }
    // Every ~0.5 s, re-pull the clips so a copy elsewhere shows up on the rope.
    if c.tick_count.get() % 15 == 0 {
        reload_cards();
    }
    // A drag session (or a pan) ends on its own run-loop mode
    // (`NSEventTrackingRunLoopMode`), so the timer's common-mode tick can miss
    // the mouse-up that ends it. Poll the view's own flags here so the guards
    // below are released as soon as the gesture is over, without waiting for an
    // event the clock will never see.
    if c.dragging.get() && !view_is_dragging() {
        drag_ended();
    }
    if c.interacting.get() && !view_is_interacting() {
        interaction_ended();
    }
    hover_check();
    presence_check(mouse);
    repaint();
}

/// The automatic reveal and retract, as a function of the pointer: parked →
/// out once the pointer reaches the menu bar; shown → away once the pointer is
/// neither on the strip nor up at the menu bar. Split out from the timer so
/// `--selftest linehover` can drive it with an explicit point, without moving
/// the real mouse.
fn presence_check(mouse: CGPoint) {
    let c = controller();
    if !c.auto.get() {
        return;
    }
    // A window of ours owns the foreground (an editor, the viewer, Quick
    // Look): the strip stays out of the way, and does not retract either —
    // the user is coming back to it. `external_window_closed` opens the gate
    // again, with a full grace so a pointer left up at the menu bar does not
    // pull the strip back the instant the window goes.
    if c.external_hold.get() {
        return;
    }
    // Never retract mid-gesture: the pointer is off the strip the whole time a
    // photo is being carried somewhere or the row is being pulled, and the
    // source window going away would cancel the very session the user is in the
    // middle of.
    if c.dragging.get() || c.interacting.get() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(screen) = screen_containing(mtm, mouse) else { return };
    if !c.revealed.get() {
        if at_menu_bar(&screen, mouse) {
            show();
        }
        return;
    }
    let Some(p) = c.panel.borrow().clone() else { return };
    if point_in(&p.frame(), mouse) || at_menu_bar(&screen, mouse) {
        // On the rope or still on the menu bar: hold, and cancel any leftover
        // manual grace — the pointer has arrived.
        c.grace.set(0.0);
        return;
    }
    if now_secs() < c.grace.get() {
        return;
    }
    hide();
}

/// The click-through flip (Tendedero's mechanism): convert the pointer into
/// the strip's own coordinates and, if it is on a photo, stop ignoring mouse
/// events so the click reaches the photo; otherwise ignore them again.
fn hover_check() {
    hover_check_at(NSEvent::mouseLocation());
}

/// `hover_check` with an explicit screen-space pointer, so a self-test can
/// drive the flip without moving the real mouse (and without the clock's own
/// races). Honours the drag gate exactly as the live clock does.
fn hover_check_at(mouse: CGPoint) {
    let c = controller();
    let Some(p) = c.panel.borrow().clone() else { return };
    if !c.revealed.get() {
        return;
    }
    // While a gesture is in flight the window must keep taking mouse events,
    // whatever the pointer does — flipping `ignoresMouseEvents` back on here
    // would cut the session short. The flip is re-evaluated the moment the
    // gesture ends (see `drag_ended` / `interaction_ended`).
    if c.dragging.get() || c.interacting.get() {
        return;
    }
    let Some(view) = c.view.borrow().clone() else { return };
    if !point_in(&p.frame(), mouse) {
        // Off the strip entirely: nothing to hit, and the desktop must stay
        // clickable.
        if c.hot.get() != -1 {
            c.hot.set(-1);
            view.set_hot(-1);
            p.setIgnoresMouseEvents(true);
        }
        view.set_gear_hot(false);
        return;
    }
    let local = view_point(&p, &view, mouse);
    // The strip's own geometry is fixed, so the card list is the only thing the
    // hit test needs from the model — read it once here rather than inside the
    // test (a second `borrow` in the same scope would panic).
    let cards = c.cards.borrow();
    let hit = line_view::hit_index(&cards, view.bounds().size.width, local);
    // The empty strip's settings corner is the one non-photo spot that must
    // take a click; everything else between and around the clips falls through.
    let on_gear = line_view::gear_visible(cards.len())
        && point_in(&line_view::gear_rect(view.bounds()), local);
    if hit != c.hot.get() {
        c.hot.set(hit);
        view.set_hot(hit);
    }
    // The flip is decided on "should this pass take a click at all", not on the
    // card index: the pointer crossing out of the gear zone and onto the bare
    // strip changes that answer while the index stays -1 either way, and
    // comparing indices alone would leave the desktop behind the strip
    // unclickable.
    let clickable = hit >= 0 || on_gear;
    if p.ignoresMouseEvents() == clickable {
        p.setIgnoresMouseEvents(!clickable);
    }
    view.set_gear_hot(on_gear);
}

/// A screen point in the rope view's own (flipped) coordinates.
fn view_point(window: &LinePanel, view: &LineView, screen: CGPoint) -> CGPoint {
    let in_window = window.convertPointFromScreen(screen);
    unsafe {
        msg_send![
            view,
            convertPoint: in_window,
            fromView: std::ptr::null::<AnyObject>()
        ]
    }
}

fn repaint() {
    if let Some(v) = controller().view.borrow().as_ref() {
        v.setNeedsDisplay(true);
    }
}

// MARK: Hit rects (the self-test's window onto the flip)

/// The on-screen rects of every photo, in draw order — the list Tendedero's
/// `hitRects` exposes, used by `--selftest linehit` to assert the flip points
/// against the real geometry.
pub fn line_hit_rects() -> Vec<CGRect> {
    let c = controller();
    let (Some(p), Some(view)) = (c.panel.borrow().clone(), c.view.borrow().clone()) else {
        return Vec::new();
    };
    // Laid out over the *document view's* width — the same width the drawing
    // used — and converted view → window → screen, so a scrolled row reports
    // where it really is rather than being re-derived from the window.
    line_view::layout(
        c.cards.borrow().len(),
        view.bounds().size.width,
        line_card::spacing(),
    )
    .iter()
    .map(|slot| {
        let r = line_card::frame_rect(slot.attach);
        let in_window = view.convertRect_toView(r, None);
        p.convertRectToScreen(in_window)
    })
    .collect()
}

// MARK: Cards

fn reload_cards() {
    let cards = crate::shelf::panel::with_model_mut(|m| {
        let first = m.with_store(|s| s.items.first().map(|it| it.id.clone()));
        // The panel's own rule (`view.rs` reads `selected_id()` the same way), so
        // 「编辑」 on the rope lights the card it acted on rather than always the
        // first one. Taken before the loop because `selected_id()` borrows the
        // model and `thumbnail` below wants it too.
        let selected = m.selected_id().map(|s| s.to_string());
        let items = m.items();
        items
            .into_iter()
            .map(|item| {
                let thumb = if item.kind == ClipKind::Image || item.kind == ClipKind::Video {
                    m.thumbnail(&item)
                } else {
                    None
                };
                LineCardData {
                    seed: line_card::seed_for(&item.id),
                    selected: selected.as_deref() == Some(item.id.as_str()),
                    on_clipboard: first.as_deref() == Some(item.id.as_str()),
                    thumb,
                    item,
                }
            })
            .collect::<Vec<_>>()
    });
    *controller().cards.borrow_mut() = cards;
    if controller().view.borrow().is_some() {
        // The doc view is as wide as the strip when the row fits, so a short
        // row centres across the whole screen; it grows past that (and then
        // scrolls) only when the row overflows. `line_hit_rects` and the drag
        // frame read this same width back, so what is drawn and what is
        // clickable cannot drift apart.
        sync_doc_width();
        let Some(v) = controller().view.borrow().clone() else { return };
        // The document view now has its real width, so this is the first moment
        // a saved scroll position can be clamped against the row's ends and
        // actually stick. `rebuild` put it here; consuming it here (rather than
        // inside `rebuild`) keeps the restore correct for both of its callers,
        // which both call this right after.
        let origin = controller().restore_origin.replace(0.0);
        if origin > 0.0 {
            v.scroll_to(origin);
        }
    }
}

/// Give the document view the width its current cards need. That width comes
/// from the card list, not the model, so it has to be re-derived wherever the
/// list changes by other means than `reload_cards` — otherwise the view keeps
/// a stale frame and a row that should scroll reports nowhere to go.
fn sync_doc_width() {
    let Some(v) = controller().view.borrow().clone() else { return };
    let clip_w = controller()
        .panel
        .borrow()
        .as_ref()
        .map(|p| p.frame().size.width)
        .unwrap_or(0.0);
    let w = line_view::row_width(controller().cards.borrow().len(), clip_w).max(1.0);
    v.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(w, strip_height())));
}

/// Called when the model reports a change (a background search landed).
pub fn notify_cards_changed() {
    reload_cards();
    repaint();
}

// MARK: Actions (dispatched by the view)

fn item_at(index: i64) -> Option<ClipItem> {
    if index < 0 {
        return None;
    }
    controller()
        .cards
        .borrow()
        .get(index as usize)
        .map(|c| c.item.clone())
}

/// Single click: copy and stay (the 已复制 tag appears on the rope).
pub fn click_copy(index: i64) {
    let Some(item) = item_at(index) else { return };
    crate::shelf::item_actions::copy(&item);
    reload_after_action();
}

/// Double click: copy, retract, and — with Accessibility and the setting on —
/// paste into the app you were in. Reuses the panel's own queue/drain pair so
/// the paste path has exactly one implementation.
pub fn double_click_paste(index: i64) {
    let Some(item) = item_at(index) else { return };
    crate::shelf::item_actions::copy_and_close(&item, true);
}

/// The menu's single dispatch: every row carries the card index in its `tag`
/// and an action id string in its `representedObject`, so a row added to the
/// menu needs no new selector on the view. Each action is a call into
/// `item_actions`, the layer both forms share, so the 晾衣绳 and the 面板 cannot
/// drift apart (which is how the strip ended up with copy-and-nothing-else).
pub fn perform_action(index: i64, action: &str, view: &NSView) {
    use crate::shelf::item_actions as act;
    // The two form/navigation rows act on the shelf, not on the clip.
    match action {
        "settings" => {
            switch_to_settings();
            return;
        }
        "to_panel" => {
            switch_to_panel();
            return;
        }
        _ => {}
    }
    let Some(item) = item_at(index) else { return };
    match action {
        "copy" => act::copy(&item),
        "copy_close" => {
            act::copy_and_close(&item, true);
            return;
        }
        "rename" => {
            rename_modal(&item, view);
        }
        "remove_title" => act::set_title(&item, None),
        "edit" | "preview" => act::edit(&item),
        "pin" => act::toggle_pin(&item),
        "stick" => act::stick(&item),
        "unstick" => act::unstick(&item),
        "save" => act::save(&item),
        "reveal" => act::show_in_finder(&item),
        "open" => act::open(&item),
        "copy_ocr" => {
            act::copy_ocr(&item);
            return;
        }
        _ => return,
    }
    reload_after_action();
}

fn reload_after_action() {
    reload_cards();
    repaint();
}

/// The right-click menu at the pointer. Mirrors the panel form's card menu
/// (copy · rename · edit · preview · pin · desktop · save · reveal · open ·
/// OCR · delete) plus the strip's own two rows, so the two forms offer the same
/// product. Every row dispatches through [`perform_action`].
pub fn show_menu(index: i64, event: &NSEvent, view: &NSView) {
    if index < 0 {
        return;
    }
    let Some(item) = item_at(index) else { return };
    let mtm = MainThreadMarker::new().expect("main thread");
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    let me: &AnyObject = unsafe { &*(view as *const NSView as *const AnyObject) };
    let add = |title: &str, action_id: &str| {
        let mi = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(sel!(lineAction:)),
                &NSString::from_str(""),
            )
        };
        mi.setTag(index as isize);
        unsafe {
            let _: () = msg_send![&*mi, setRepresentedObject: Some(&*NSString::from_str(action_id))];
            let _: () = msg_send![&*mi, setTarget: Some(me)];
        }
        menu.addItem(&mi);
    };
    use crate::shelf::item_actions as act;
    add(&l("复制"), "copy");
    add(&l("复制并关闭"), "copy_close");
    if act::has_title(&item) {
        add(&l("重命名…"), "rename");
        add(&l("去掉标题"), "remove_title");
    } else {
        add(&l("命名…"), "rename");
    }
    if act::can_edit(&item) {
        add(&l("编辑"), "edit");
    }
    add(&l("预览"), "preview");
    let pin_title = if item.pinned { l("取消 Pin") } else { "Pin".to_string() };
    add(&pin_title, "pin");
    if crate::shelf::desktop_notes::is_on_desktop(&item.id) {
        add(&l("在桌面上显示"), "stick");
        add(&l("从桌面收起"), "unstick");
    } else {
        add(&l("贴到桌面"), "stick");
    }
    add(&l("保存到本地…"), "save");
    if act::is_file_backed(&item) {
        add(&l("在 Finder 中显示"), "reveal");
    }
    if item.kind == ClipKind::Video {
        add(&l("打开"), "open");
    }
    if act::has_ocr(&item) {
        add(&l("复制识别出的文字"), "copy_ocr");
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    add(&l("删除"), "delete");
    // The shelf's own two rows, shared with the empty strip's menu.
    add_shelf_rows(&menu, &add);
    NSMenu::popUpContextMenu_withEvent_forView(&menu, event, view);
}

/// The two rows that act on the shelf rather than on a clip — 设置… and the way
/// back to the drawer. A separator goes in front of them, so they read as a
/// group of their own at the bottom of the menu.
fn add_shelf_rows(menu: &NSMenu, add: &dyn Fn(&str, &str)) {
    menu.addItem(&NSMenuItem::separatorItem(
        MainThreadMarker::new().expect("main thread"),
    ));
    add(&l("设置…"), "settings");
    add(&l("切回面板形态"), "to_panel");
}

/// The empty strip's right-click menu: 设置… and 切回面板形态.
///
/// With no clips there is nothing to copy, pin or edit, but the two rows that
/// act on the *shelf* still have to be reachable — otherwise a fresh install
/// (nothing copied yet) has no way into the settings page or back to the
/// drawer. [`perform_action`] needs no card for these two rows, which is why
/// they can be offered here the same way a card's menu offers them.
pub fn show_shelf_menu(event: &NSEvent, view: &NSView) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    let me: &AnyObject = unsafe { &*(view as *const NSView as *const AnyObject) };
    let add = |title: &str, action_id: &str| {
        let mi = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(sel!(lineAction:)),
                &NSString::from_str(""),
            )
        };
        mi.setTag(-1);
        unsafe {
            let _: () = msg_send![&*mi, setRepresentedObject: Some(&*NSString::from_str(action_id))];
            let _: () = msg_send![&*mi, setTarget: Some(me)];
        }
        menu.addItem(&mi);
    };
    add(&l("设置…"), "settings");
    add(&l("切回面板形态"), "to_panel");
    NSMenu::popUpContextMenu_withEvent_forView(&menu, event, view);
}

/// The strip's title editor. The panel form edits the title inline on the card;
/// the strip has no room for that, so it asks with a small modal and writes
/// through the shared action layer. `None` from the modal (cancel) writes
/// nothing; an empty field clears the title.
fn rename_modal(item: &ClipItem, _view: &NSView) {
    let current = crate::shelf::item_actions::title(item).unwrap_or_default();
    let Some(new_title) = prompt_for_title(&current) else { return };
    let trimmed = new_title.trim();
    let value = if trimmed.is_empty() { None } else { Some(trimmed) };
    crate::shelf::item_actions::set_title(item, value);
}

/// A one-field modal. Returns the typed title, or `None` on cancel.
fn prompt_for_title(current: &str) -> Option<String> {
    crate::shelf::panel::with_dialog(|| {
        let mtm = MainThreadMarker::new().expect("main thread");
        let alert = objc2_app_kit::NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&l("编辑标题…")));
        alert.addButtonWithTitle(&NSString::from_str(&l("确定")));
        alert.addButtonWithTitle(&NSString::from_str(&l("取消")));
        let field = objc2_app_kit::NSTextField::initWithFrame(
            objc2_app_kit::NSTextField::alloc(mtm),
            CGRect::new(CGPoint::ZERO, CGSize::new(240.0, 24.0)),
        );
        field.setStringValue(&NSString::from_str(current));
        alert.setAccessoryView(Some(&field));
        if alert.runModal() == objc2_app_kit::NSAlertFirstButtonReturn {
            Some(field.stringValue().to_string())
        } else {
            None
        }
    })
}

/// Right-click → 切回面板形态: store the choice and rebuild into the panel.
pub fn switch_to_panel() {
    hide();
    layout_skin::select(layout_skin::DEFAULT_ID);
    layout_skin::store(layout_skin::DEFAULT_ID);
    crate::shelf::panel::relayout();
    crate::shelf::panel::show();
}

/// Whether the settings page was opened *from* the strip.
///
/// The page lives in the panel form — it needs the panel's keyboard channel
/// (⎋ / ⌘F), and the strip by design owns no keyboard — so opening it has to
/// switch forms. But that switch is a consequence of asking for the page, not
/// a choice the user made about their shelf: recording it here is what lets
/// leaving the page hand the strip back, and what keeps the `layout`
/// preference untouched (see `switch_to_settings`).
///
/// Consumed by `swap`, so the first close spends it — a page closed through a
/// path this module does not know about cannot leave the form switched for the
/// rest of the session.
static RETURN_TO_LINE: AtomicBool = AtomicBool::new(false);

/// Right-click → 设置…: the settings page lives in the panel form, so switch to
/// it and open the page. The strip cannot host the page (no keyboard), which is
/// why this goes through the form switch rather than opening a window here.
///
pub fn switch_to_settings() {
    hide();
    RETURN_TO_LINE.store(true, Ordering::Relaxed);
    // `select`, never `store`: merely opening the settings must not rewrite
    // the user's layout preference, or the strip is lost the moment they look
    // at a setting and never comes back. Leaving the page restores it.
    layout_skin::select(layout_skin::DEFAULT_ID);
    crate::shelf::panel::relayout();
    // Raise the drawer, then open straight onto the settings page — the same
    // order `switch_to_panel` uses, plus the page.
    crate::shelf::panel::show();
    crate::shelf::panel::with_model_mut(|m| m.show_settings = true);
    crate::shelf::view::refresh();
}

/// The settings page is going away: give the strip back if that is where it
/// came from. True when it did (the caller has nothing left to do).
///
/// Shared by both exits — the pane's own 关闭 button, which takes the whole
/// shelf down, and ⎋, which only leaves the page. They differ in what happens
/// when the page was *not* opened from the strip; the restore itself is the
/// same either way.
fn restore_form_after_settings() -> bool {
    if !RETURN_TO_LINE.swap(false, Ordering::Relaxed) {
        return false;
    }
    // Still the panel form here, so this takes the drawer down.
    crate::shelf::panel::hide();
    layout_skin::select("line");
    // The line branch rebuilds the strip and restarts its clock.
    crate::shelf::panel::relayout();
    crate::shelf::panel::with_model_mut(|m| m.show_settings = false);
    crate::shelf::view::refresh();
    // `show` grants the no-retract grace, so the pointer — left wherever the
    // click that opened the page put it — cannot take the strip straight down.
    crate::shelf::line_panel::show();
    true
}

/// The settings pane's own way out (its 关闭 button): leave the page and, from
/// the strip, put the strip back. From the panel this is the original
/// behaviour, taking the whole shelf down.
pub fn close_settings() {
    if restore_form_after_settings() {
        return;
    }
    crate::shelf::panel::hide();
}

/// ⎋ in the settings page: leave the page. Unlike `close_settings` the panel
/// form stays up — ⎋ means "back to the shelf", not "put the shelf away" —
/// but a page entered from the strip still hands the strip back.
pub fn leave_settings() {
    if restore_form_after_settings() {
        return;
    }
    crate::shelf::panel::with_model_mut(|m| m.show_settings = false);
    crate::shelf::view::refresh();
}

/// Drop the "came from the strip" record without acting on it. Two callers:
/// ⌘F in the page (turning to search means staying in the panel — only the
/// panel has a search box), and the form picker, where the user has just made
/// an explicit choice about their shelf and that choice must not be overridden
/// by a second, automatic switch when the page closes.
pub fn forget_return_to_line() {
    RETURN_TO_LINE.store(false, Ordering::Relaxed);
}

// MARK: Drag out

/// Start a drag carrying the item's payload: an `NSURL` for a file payload
/// (Finder, Mail, any drop target) and an `NSString` for text. The card's own
/// frame is the dragging frame.
///
/// Returns whether a session was actually begun. Only then does the view clear
/// `dragged` to swallow the pending click — a press that produced no payload
/// (no writer, no slot) must still fall through to the normal copy on release.
pub fn begin_drag(view: &LineView, event: &NSEvent, index: i64) -> bool {
    let Some(item) = item_at(index) else { return false };
    let Some(writer) = pasteboard_writer(&item) else { return false };
    let c = controller();
    let Some(p) = c.panel.borrow().clone() else { return false };
    let f = p.frame();
    // The row is laid out over the view's own width — the same one it was
    // drawn at — so the drag frame lands on the card the pointer pressed.
    let width = view.bounds().size.width;
    let _ = f;
    let Some(slot) = line_view::layout(c.cards.borrow().len(), width, line_card::spacing())
        .into_iter()
        .find(|s| s.index == index as usize)
    else {
        return false;
    };
    let frame = line_card::frame_rect(slot.attach);
    let drag_item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), &writer);
    let image = drag_image(view, frame);
    match image {
        Some(img) => unsafe {
            let obj: &AnyObject = &*(Retained::as_ptr(&img) as *const AnyObject);
            drag_item.setDraggingFrame_contents(frame, Some(obj));
        },
        None => drag_item.setDraggingFrame(frame),
    }
    let items = NSArray::from_retained_slice(&[drag_item]);
    let source: &ProtocolObject<dyn objc2_app_kit::NSDraggingSource> =
        ProtocolObject::from_ref(view);
    let _ = view.beginDraggingSessionWithItems_event_source(&items, event, source);
    // From here the clock must leave the window alone (see `drag_started`).
    drag_started();
    true
}

/// A drag session carrying a photo has begun (`LineView::mouseDragged`). From
/// here until [`drag_ended`] the clock leaves the window alone: no click-through
/// re-flip, no retract.
pub fn drag_started() {
    controller().dragging.set(true);
}

/// The pointer button went down on the strip (`LineView::mouseDown`), before
/// anything is known about the gesture. The clock stands off until
/// [`interaction_ended`]: while the button is held the window must keep taking
/// mouse events, or the mouse-up is dropped and the gesture never ends.
pub fn interaction_started() {
    controller().interacting.set(true);
}

/// The gesture's own end (the view's mouse-up). If a drag-out began, its
/// separate flag is released too, and the click-through flip is re-evaluated
/// against the pointer's current place.
pub fn interaction_ended() {
    let c = controller();
    if !c.interacting.get() && !c.dragging.get() {
        return;
    }
    c.interacting.set(false);
    // A drag-out also trips `drag_ended`, which re-runs the flip; a plain pan
    // has no drag to end, so run it here. `drag_ended` is idempotent when the
    // flag is already clear.
    if c.dragging.get() {
        drag_ended();
    } else {
        hover_check_at(NSEvent::mouseLocation());
    }
}

/// A drag session has finished (the view's mouse-up, or the clock noticing the
/// flag cleared). Re-evaluate the click-through flip at once so the strip is in
/// the right state for the pointer's current place, rather than whatever it was
/// when the drag began.
pub fn drag_ended() {
    let c = controller();
    if !c.dragging.get() {
        return;
    }
    c.dragging.set(false);
    // Re-run the same flip the clock runs, now that the gate is open: the
    // window's state on the pointer's current place, not where it was when the
    // gesture began.
    hover_check_at(NSEvent::mouseLocation());
}

/// Whether the view still says a drag is in flight (`LineView::is_dragging`).
fn view_is_dragging() -> bool {
    controller()
        .view
        .borrow()
        .as_ref()
        .map(|v| v.dragging())
        .unwrap_or(false)
}

/// Whether the view still says the pointer button is down (`LineView::interacting`).
fn view_is_interacting() -> bool {
    controller()
        .view
        .borrow()
        .as_ref()
        .map(|v| v.interacting())
        .unwrap_or(false)
}

/// The pasteboard object for an item: a file URL, else its text. Typed as the
/// protocol itself — `AnyObject` cannot be converted into
/// `ProtocolObject<dyn NSPasteboardWriting>` (`ImplementedBy` needs a concrete
/// conformer), so the choice is erased to the protocol at the point of use.
fn pasteboard_writer(item: &ClipItem) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
    if matches!(item.kind, ClipKind::Image | ClipKind::Video | ClipKind::Files) {
        let path = crate::shelf::panel::with_model_mut(|m| m.with_store(|s| s.payload_url(item)));
        if path.exists() {
            let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(
                &path.to_string_lossy(),
            ));
            return Some(ProtocolObject::from_retained(url));
        }
    }
    let text = crate::shelf::panel::with_model_mut(|m| m.with_store(|s| s.text(item)));
    text.map(|t| ProtocolObject::from_retained(NSString::from_str(&t)))
}

/// A small bitmap of the card frame as the drag image (nil when it cannot be
/// built — the drop still works).
fn drag_image(view: &LineView, frame: CGRect) -> Option<Retained<objc2_app_kit::NSImage>> {
    use objc2_app_kit::{NSBitmapImageRep, NSImage, NSImageRep};
    let rep = view.bitmapImageRepForCachingDisplayInRect(frame)?;
    view.cacheDisplayInRect_toBitmapImageRep(frame, &rep);
    let img = NSImage::initWithSize(NSImage::alloc(), frame.size);
    let rep_ref: &NSImageRep =
        unsafe { &*(Retained::as_ptr(&rep) as *const NSBitmapImageRep as *const NSImageRep) };
    img.addRepresentation(rep_ref);
    Some(img)
}

// MARK: Animations (NSAnimationContext, as in panel.rs)

fn set_alpha_animated(window: &LinePanel, alpha: f64) {
    unsafe {
        let proxy: Retained<AnyObject> = msg_send![&*window, animator];
        let _: () = msg_send![&*proxy, setAlphaValue: alpha];
    }
}

/// `ctx.duration = secs; ctx.timingFunction = CAMediaTimingFunction(name:)`.
fn set_timing(secs: f64, curve: &str) {
    let ctx = objc2_app_kit::NSAnimationContext::currentContext();
    ctx.setDuration(secs);
    unsafe {
        let Some(cls) = AnyClass::get(c"CAMediaTimingFunction") else { return };
        let f: Retained<AnyObject> = msg_send![cls, functionWithName: &*NSString::from_str(curve)];
        let _: () = msg_send![&*ctx, setTimingFunction: &*f];
    }
}

fn animate_alpha_reveal(window: Retained<LinePanel>) {
    let w = window.clone();
    let changes =
        block2::RcBlock::new(move |_: std::ptr::NonNull<objc2_app_kit::NSAnimationContext>| {
            set_timing(REVEAL_SECS, "easeOut");
            set_alpha_animated(&w, 1.0);
        });
    objc2_app_kit::NSAnimationContext::runAnimationGroup(&changes);
}

// MARK: Window class

pub struct LinePanelIvars;

define_class!(
    // SAFETY: NSPanel subclass, main-thread only.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[ivars = LinePanelIvars]
    pub struct LinePanel;

    unsafe impl NSObjectProtocol for LinePanel {}

    impl LinePanel {
        // It never takes the keyboard — the whole point of the form.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> objc2::runtime::Bool {
            false.into()
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> objc2::runtime::Bool {
            false.into()
        }

        /// It is `ignoresMouseEvents` by default, but while a photo is hot it
        /// must take the click — and a single click on an inactive window is
        /// routed through this.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }
    }
);

impl LinePanel {
    fn new(mtm: MainThreadMarker, content_rect: CGRect, style: NSWindowStyleMask) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(LinePanelIvars);
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

pub struct LinePanelWinDelegateIvars;

define_class!(
    // SAFETY: NSObject window delegate, main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = LinePanelWinDelegateIvars]
    pub struct LinePanelWinDelegate;

    unsafe impl NSObjectProtocol for LinePanelWinDelegate {}

    unsafe impl NSWindowDelegate for LinePanelWinDelegate {
        // Never key, so a resign must not close it; the hover timer owns that.
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _n: &NSNotification) {}
    }
);

impl LinePanelWinDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(LinePanelWinDelegateIvars);
        unsafe { msg_send![super(this), init] }
    }
}

// MARK: Self-test hooks

/// Turn the automatic reveal/retract off, so a self-test can hold the strip in
/// place by hand while the real mouse sits wherever it happens to be.
pub fn debug_freeze_auto() {
    controller().auto.set(false);
}

/// Self-test only: drop the no-retract grace a reveal just granted, so a test
/// that drives `presence_check` itself can assert the retract in the same
/// turn instead of sleeping out `MANUAL_GRACE_SECS`.
pub fn debug_clear_grace() {
    controller().grace.set(0.0);
}

/// `--selftest linehover`: the automatic reveal and retract. Drives the very
/// decision the clock runs, with an explicit pointer, so it does not depend on
/// where the real mouse is.
pub fn selftest_hover_report() -> bool {
    // The real clock would race this test's own calls: stop it, and drive the
    // decision it makes by hand.
    stop_clock();
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let Some(screen) = screen_for_pointer(mtm) else { return false };
    // Start parked, whatever the clock has done so far.
    hide();
    let bar = CGPoint::new(screen.frame().mid().x, screen.visibleFrame().max().y - 1.0);
    presence_check(bar);
    let out = is_visible();
    println!(
        "{} pointer at the menu bar → strip revealed={}",
        if out { "ok  " } else { "FAIL" },
        out
    );
    let away = CGPoint::new(screen.frame().mid().x, screen.frame().mid().y);
    // The reveal above granted the no-retract grace; this test is about the
    // decision, so step over the window rather than sleeping it out.
    debug_clear_grace();
    presence_check(away);
    let gone = !is_visible();
    println!(
        "{} pointer away from the strip → retracted={}",
        if gone { "ok  " } else { "FAIL" },
        gone
    );
    // Park it again so the run leaves nothing on screen.
    hide();
    out && gone
}

/// Whether the strip is currently swallowing clicks (pointer on a photo).
pub fn debug_clickable() -> bool {
    controller()
        .panel
        .borrow()
        .as_ref()
        .map(|p| !p.ignoresMouseEvents())
        .unwrap_or(false)
}

/// Run one hover pass with an explicit screen-space pointer.
///
/// The real path, not a copy of it: the click-through rule lives in one place
/// (`hover_check_at`), so a change there cannot leave the self-test asserting
/// yesterday's contract.
pub fn debug_hover_at(screen_point: CGPoint) {
    hover_check_at(screen_point);
}

pub fn debug_card_count() -> usize {
    controller().cards.borrow().len()
}

/// Widen the document view past the strip, pull the row by hand, and report
/// whether the clip actually moved — the live half of `--selftest linewheel`.
/// Restores the real document width afterwards.
fn debug_pan_probe() -> bool {
    let c = controller();
    let (Some(view), Some(p)) = (c.view.borrow().clone(), c.panel.borrow().clone()) else {
        return false;
    };
    let clip_w = p.frame().size.width;
    let real_w = line_view::row_width(c.cards.borrow().len(), clip_w);
    // Make the row genuinely wider than the strip so there is somewhere to go.
    view.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(real_w + 400.0, strip_height())));
    let before = view.clip_origin_pub();
    // Negative delta pulls the row towards the end (same convention as the
    // wheel: content towards the user walks back to the start).
    view.pan_from_current(-80.0);
    let after = view.clip_origin_pub();
    // Put the clip back where it started, then the document view to real width.
    view.pan_from_current(after - before);
    view.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(real_w, strip_height())));
    (after - before).abs() > 1.0
}

pub fn debug_view_width() -> f64 {
    controller()
        .view
        .borrow()
        .as_ref()
        .map(|v| v.bounds().size.width)
        .unwrap_or(0.0)
}

/// Move the strip's frame without animation (a self-test settling it).
pub fn debug_settle(shown: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(p) = controller().panel.borrow().clone() else { return };
    let Some(screen) = screen_for_pointer(mtm) else { return };
    let frame = if shown { strip_frame(&screen) } else { parked_frame(&screen) };
    p.setFrame_display(frame, false);
    p.setAlphaValue(1.0);
    controller().revealed.set(shown);
}

/// `--selftest linehit`: with the strip shown, assert the pointer inside each
/// photo's rect is clickable and a pointer past the row falls through.
pub fn selftest_hit_report() -> bool {
    let rects = line_hit_rects();
    if rects.is_empty() {
        println!("FAIL linehit: no hit rects (no cards on the rope?)");
        return false;
    }
    let mut ok = true;
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let Some(screen) = screen_for_pointer(mtm) else { return false };
    let strip = strip_frame(&screen);
    for (i, r) in rects.iter().enumerate() {
        let centre = CGPoint::new(r.mid().x, r.mid().y);
        debug_hover_at(centre);
        let clickable = debug_clickable();
        println!(
            "{} photo {} centre ({:.1},{:.1}) → clickable={}",
            if clickable { "ok  " } else { "FAIL" },
            i,
            centre.x,
            centre.y,
            clickable
        );
        ok &= clickable;
    }
    // The far right of the strip, past the row: it must fall through so the
    // desktop behind stays clickable.
    let gap = CGPoint::new(strip.max().x - 4.0, strip.mid().y);
    debug_hover_at(gap);
    let falls = !debug_clickable();
    println!("{} strip right edge → falls through={}", if falls { "ok  " } else { "FAIL" }, falls);
    ok &= falls;
    // Reset, so a stray flip cannot leave the desktop unclickable.
    debug_hover_at(CGPoint::new(strip.mid().x, strip.min().y - 60.0));
    ok
}

/// `--selftest linewind`: sample the swing at three moments. A pure check of
/// `motion` — no window needed, so it runs on any machine.
pub fn selftest_wind_report() -> bool {
    let c0 = line_card::seed_for("selftest-card-a");
    let c1 = line_card::seed_for("selftest-card-b");
    let a0 = motion::card_angle(c0, 0.0);
    let a1 = motion::card_angle(c0, 0.4);
    let a2 = motion::card_angle(c0, 3.0);
    let per_card = motion::card_angle(c0, 1.3) != motion::card_angle(c1, 1.3);
    let settled = a2.abs() < 4.0;
    println!("ok   swing: hang {:.2}° → mid {:.2}° → settled {:.2}°", a0, a1, a2);
    println!(
        "{} two cards swing apart (seeds differ)",
        if per_card { "ok  " } else { "FAIL" }
    );
    println!(
        "{} a hanging photo starts tilted and comes to rest",
        if settled && a0.abs() > 1.0 { "ok  " } else { "FAIL" }
    );
    settled && a0.abs() > 1.0 && per_card
}

/// `--selftest linedrag`: the drag gate. A drag session must (a) keep the
/// window on screen and clickable for its whole life — a clock flip back to
/// `ignoresMouseEvents` mid-gesture would cancel it — and (b) release the gate
/// on `drag_ended`, after which the pointer's own place decides the flip again.
///
/// Drives the flag the view sets (`drag_started` / a dropped `mouseUp`) and the
/// flip the clock runs (`hover_check_at`), so it asserts the two halves talk to
/// each other rather than testing a copy of them.
pub fn selftest_drag_report() -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let Some(screen) = screen_for_pointer(mtm) else { return false };
    // The real mouse may be anywhere: hold the strip by hand, and run the two
    // clock decisions with an explicit pointer. `prep_strip` has already shown
    // it; stop the clock so a stray tick cannot flip the window under us.
    show();
    debug_settle(true);
    stop_clock();
    let rects = line_hit_rects();
    if rects.is_empty() {
        println!("FAIL linedrag: no photo rects on the rope");
        return false;
    }
    let on_card = CGPoint::new(rects[0].mid().x, rects[0].mid().y);
    // The pointer sweeps far off the strip — the normal shape of a drag to
    // another app (Finder, Mail).
    let off = CGPoint::new(screen.frame().mid().x, screen.frame().min().y + 200.0);
    // With the gate shut (no drag) that pointer must retract the strip: the
    // baseline the gate has to change.
    controller().auto.set(true);
    debug_clear_grace();
    presence_check(off);
    let retracts_without_drag = !is_visible();
    println!(
        "{} no drag, pointer off the strip → strip retracts (baseline)",
        if retracts_without_drag { "ok  " } else { "FAIL" }
    );

    // Now the real case: shown, pointer on a photo, a drag in flight.
    show();
    debug_settle(true);
    debug_hover_at(on_card);
    let clickable_before = debug_clickable();
    drag_started();
    // (a) the presence gate: the same pointer that retracted the strip a moment
    // ago must leave it alone while the drag is in flight, or the source window
    // would vanish and cancel the session.
    presence_check(off);
    let held_visible = is_visible();
    // (b) the hover gate: the window must keep taking mouse events, or the
    // click-through re-flip would cut the session short.
    hover_check_at(off);
    let held_clickable = debug_clickable();
    println!(
        "{} pointer off the strip mid-drag → strip stays on screen (no retract)",
        if held_visible { "ok  " } else { "FAIL" }
    );
    println!(
        "{} pointer off the strip mid-drag → window still clickable (session survives)",
        if held_clickable { "ok  " } else { "FAIL" }
    );

    // End the gesture: the gate opens, and the pointer's place decides again —
    // off a photo, the window goes back to click-through and the strip retracts.
    drag_ended();
    hover_check_at(off);
    let released_clickthrough = !debug_clickable();
    debug_clear_grace();
    presence_check(off);
    let released_retract = !is_visible();
    println!(
        "{} drag ends → click-through restored off a photo",
        if released_clickthrough { "ok  " } else { "FAIL" }
    );
    println!(
        "{} drag ends → the strip retracts again",
        if released_retract { "ok  " } else { "FAIL" }
    );
    hide();
    debug_hover_at(CGPoint::new(0.0, 0.0));
    retracts_without_drag && held_visible && held_clickable && clickable_before
        && released_clickthrough && released_retract
}

/// `--selftest linewheel`: the strip's horizontal wheel. A row wider than the
/// strip must scroll with the wheel and stop at both ends; a row that fits must
/// not scroll at all. Pure (see `line_view::scrolled_origin`) plus one live
/// check that the document view is built as wide as the cards + gaps need. The
/// caller has already prewarmed and shown the strip.
pub fn selftest_wheel_report() -> bool {
    // Pure mapping — the part that owns the convention.
    let start = line_view::scrolled_origin(0.0, -120.0, 2000.0, 800.0);
    let back = line_view::scrolled_origin(start, 120.0, 2000.0, 800.0);
    let clamped_hi = line_view::scrolled_origin(0.0, -99999.0, 2000.0, 800.0);
    let max_x = 2000.0 - 800.0;
    let fits = line_view::scrolled_origin(0.0, -120.0, 400.0, 800.0);
    println!("ok   wheel: origin 0 → {:.0} → back to {:.0}", start, back);
    let forward = start > 0.0;
    let returns = back.abs() < 0.5;
    let clamps = (clamped_hi - max_x).abs() < 0.5;
    let no_scroll_when_fits = fits == 0.0;
    println!(
        "{} wheel forward scrolls the row, the other way returns to the start",
        if forward && returns { "ok  " } else { "FAIL" }
    );
    println!("{} the row stops at its end", if clamps { "ok  " } else { "FAIL" });
    println!(
        "{} a row that fits does not scroll",
        if no_scroll_when_fits { "ok  " } else { "FAIL" }
    );
    // Live: the document view is built at exactly the width the layout asks
    // for (`row_width`), so what is drawn and what scrolls cannot drift apart.
    let n = debug_card_count();
    let view_w = debug_view_width();
    let clip_w = controller()
        .panel
        .borrow()
        .as_ref()
        .map(|p| p.frame().size.width)
        .unwrap_or(0.0);
    let expect = line_view::row_width(n, clip_w);
    let matches = n > 0 && (view_w - expect).abs() < 0.5;
    println!(
        "ok   strip: {} cards → doc width {:.0}pt (row_width {:.0}pt, strip {:.0}pt)",
        n, view_w, expect, clip_w
    );
    println!(
        "{} the document view is built at the layout's own width",
        if matches { "ok  " } else { "FAIL" }
    );
    // And a row that really overflows asks for a document wider than the strip,
    // which is what gives the wheel something to move.
    let overflows = line_view::row_width(60, clip_w) > clip_w + 1.0;
    println!(
        "{} a full row is wider than the strip (the wheel has something to move)",
        if overflows { "ok  " } else { "FAIL" }
    );
    // The same row can be pulled with the hand (拖动手势): `pan_from_current`
    // moves the clip exactly like the wheel does, so a drag reveals more photos.
    // The seeded row may fit the strip exactly (nothing to move), so widen the
    // document view for the probe and let `reload_cards` put it back.
    let live_moves = debug_pan_probe();
    println!(
        "{} dragging the row moves it (pull to reveal more photos)",
        if live_moves { "ok  " } else { "FAIL" }
    );
    // And the press/grab rules are pinned: leaving the band is a drag-out, a
    // sideways pull inside the band is a pan, a nudge is still a click.
    let g = |dx: f64, y: f64| line_view::classify_press(dx, y, 184.0);
    let sid = g(20.0, 90.0) == line_view::PressGesture::Pan;
    let out = g(2.0, -40.0) == line_view::PressGesture::DragOut
        && g(2.0, 240.0) == line_view::PressGesture::DragOut;
    let click = g(1.0, 90.0) == line_view::PressGesture::None;
    let diag = g(200.0, -60.0) == line_view::PressGesture::DragOut; // leaving wins
    println!(
        "{} sideways pull inside the strip is a pan; leaving the band is a drag-out; a nudge is a click",
        if sid && out && click { "ok  " } else { "FAIL" }
    );
    println!(
        "{} leaving the band wins over a sideways pull (no accidental pan)",
        if diag { "ok  " } else { "FAIL" }
    );
    forward && returns && clamps && no_scroll_when_fits && matches && overflows && live_moves
        && sid && out && click && diag
}

/// `--selftest lineexternal`: the external-window gate. While one of our own
/// windows owns the foreground (an editor, the picture viewer, Quick Look) the
/// strip must neither reveal itself nor retract — the user is working in that
/// window and is coming back to the strip. When it closes the gate opens with a
/// grace, so a pointer left at the menu bar does not drag the strip out before
/// the user has moved it.
///
/// Drives `presence_check` with explicit pointers, the way `linedrag` does:
/// the real mouse is wherever the user left it.
pub fn selftest_external_report() -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let Some(screen) = screen_for_pointer(mtm) else { return false };
    // This is about the clock's own decision, so it has to be running.
    controller().auto.set(true);
    stop_clock();
    let bar = CGPoint::new(screen.frame().mid().x, screen.visibleFrame().max().y - 1.0);
    let away = CGPoint::new(screen.frame().mid().x, screen.frame().mid().y);

    // 1. Parked with the gate shut: the menu bar must not pull the strip out
    //    over the window the user is in.
    hide();
    external_window_opened();
    presence_check(bar);
    let held_parked = !is_visible();
    println!(
        "{} pointer at the menu bar with a window of ours in front → strip stays parked",
        if held_parked { "ok  " } else { "FAIL" }
    );

    // 2. Shown with the gate shut: the pointer moving away must not take it
    //    down either.
    debug_settle(true);
    presence_check(away);
    let held_shown = is_visible();
    println!(
        "{} pointer away with a window of ours in front → strip stays up (no retract)",
        if held_shown { "ok  " } else { "FAIL" }
    );

    // 3. The window closes: the gate opens, and the grace it grants holds the
    //    strip against a pointer that is nowhere near it. This is the case the
    //    live path hits — the click that opened the window put the pointer
    //    mid-screen, and without the grace the clock would take the strip down
    //    on its very next pass.
    external_window_closed();
    let gate_open = !debug_external_hold();
    presence_check(away);
    let after_close = is_visible();
    println!(
        "{} the window closes → the grace holds the strip until the pointer moves away",
        if gate_open && after_close { "ok  " } else { "FAIL" }
    );

    // 4. Grace over, the clock behaves exactly as it did before: away retracts,
    //    the menu bar reveals.
    controller().grace.set(0.0);
    presence_check(away);
    let retracts = !is_visible();
    presence_check(bar);
    let reveals = is_visible();
    println!(
        "{} grace over → pointer away retracts it, pointer at the menu bar brings it back",
        if retracts && reveals { "ok  " } else { "FAIL" }
    );

    hide();
    controller().auto.set(false);
    held_parked && held_shown && gate_open && after_close && retracts && reveals
}

/// `--selftest linesettings`: opening the settings page from the strip must not
/// cost the user their strip.
///
/// The page is hosted by the panel form — it needs the panel's keyboard
/// channel (⎋ / ⌘F), and the strip by design owns no keyboard — so asking for
/// the page necessarily switches forms. What must not happen is that switch
/// being *recorded*: the `layout` preference has to come out the other side
/// exactly as it went in, or 「看一眼设置」 silently ends the strip for good.
pub fn selftest_settings_report() -> bool {
    // Start from the strip *and* from a preference that says strip, so the
    // 「was the preference dirtied」 assertion below means something.
    layout_skin::store("line");
    layout_skin::select("line");

    // 1. Open the page: the form switches (the page needs the panel), the
    //    preference does not.
    switch_to_settings();
    let pref_kept = layout_skin::stored_id() == "line";
    let switched = layout_skin::current_id() == layout_skin::DEFAULT_ID;
    let page_open = crate::shelf::panel::with_model(|m| m.show_settings);
    println!(
        "{} opening the page from the strip → panel form, page up, `layout` preference untouched",
        if pref_kept && switched && page_open { "ok  " } else { "FAIL" }
    );

    // 2. Close it: the strip comes back, and the preference is still the
    //    user's, not the page's.
    close_settings();
    let back = layout_skin::current_id() == "line";
    let strip_up = is_visible();
    let pref_still = layout_skin::stored_id() == "line";
    let page_gone = !crate::shelf::panel::with_model(|m| m.show_settings);
    println!(
        "{} closing the page → strip back on screen, still the user's chosen form",
        if back && strip_up && pref_still && page_gone { "ok  " } else { "FAIL" }
    );

    // 3. The other exit: a page entered from the strip where the user then
    //    picks a form by hand closes into *that* form. The record is dropped
    //    when the choice is made (not when the page closes), which is what
    //    keeps the automatic restore from overriding the explicit pick.
    switch_to_settings();
    forget_return_to_line();
    close_settings();
    let stays_panel = layout_skin::current_id() == layout_skin::DEFAULT_ID;
    println!(
        "{} a form picked by hand in the page → closing it does not drag the strip back",
        if stays_panel { "ok  " } else { "FAIL" }
    );

    // 4. The 晾衣绳 display rows: a size change made while the page is up has
    //    to reach the next strip. The rows store through `metrics::store_*`,
    //    which drops the cache by itself — driving that would leave the pane's
    //    change handler unproven, since either way the new value is read. So
    //    the preference is written *behind the cache's back* and then the
    //    handler is driven: only the handler can be what invalidates it.
    crate::shelf::line_card::metrics::reset();
    let height_before = strip_height();
    crate::shelf::line_card::metrics::debug_write_size(150);
    let stale_scale = crate::shelf::line_card::metrics::scale();
    crate::shelf::settings_pane::debug_line_settings_changed();
    let fresh_scale = crate::shelf::line_card::metrics::scale();
    let height_after = strip_height();
    let metrics_adopted = stale_scale == 1.0 && fresh_scale == 1.5 && height_after > height_before;
    println!(
        "{} card size changed in the page → the next strip uses it (scale {stale_scale} → {fresh_scale}, strip {height_before} → {height_after})",
        if metrics_adopted { "ok  " } else { "FAIL" }
    );
    // Back to the default so the run leaves no preference behind.
    crate::shelf::line_card::metrics::store_size(100);

    // Hand the process back the strip it started on.
    layout_skin::select("line");
    crate::shelf::panel::relayout();
    show();

    pref_kept
        && switched
        && page_open
        && back
        && strip_up
        && pref_still
        && page_gone
        && stays_panel
        && metrics_adopted
}

/// Let the main run loop turn for `seconds`. The form switch is deferred to the
/// next main-queue hop (`settings_pane::queue_panel_command`), and the drawer's
/// fade-out orders its window out from its animation's completion block — both
/// need the loop to actually run, so sleeping would race them.
///
/// Punched in 20 ms slices on the *default* mode, the way the other self-tests
/// spin: `NSRunLoopCommonModes` handed to `runMode:beforeDate:` is rejected by
/// CFRunLoop ("invalid mode"), which returns immediately and would leave the
/// completion block unrun.
fn pump(seconds: f64) {
    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    let mode: &'static objc2_foundation::NSRunLoopMode =
        unsafe { objc2_foundation::NSDefaultRunLoopMode };
    while std::time::Instant::now() < deadline {
        let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.02);
        let _ = run_loop.runMode_beforeDate(mode, &limit);
    }
}

/// `--selftest lineswitch`: picking 晾衣绳 in 设置 › 外观 › 形态 has to take the
/// drawer down, not merely raise the strip over it.
///
/// The defect this pins: the form switch rebuilt the tree and revealed the
/// strip, but nothing ever put the panel's *window* away — and the settings
/// page is hosted on that window, so choosing 晾衣绳 looked like it had done
/// nothing at all.
///
/// The click goes through the pane's real `mouseUp:` route (hotspot →
/// `Choice::Layout` → queued `PanelCommand::Relayout` → `panel::relayout` on
/// the next main-queue turn), so the path the user takes is the path under
/// test, not just the function at the end of it.
pub fn selftest_form_switch_report() -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);

    // Start where the user was: the drawer form, up, with its settings page
    // open. `show()` resets the model itself, so the page opens after it.
    layout_skin::select(layout_skin::DEFAULT_ID);
    crate::shelf::panel::prewarm();
    crate::shelf::panel::show();
    crate::shelf::panel::with_model_mut(|m| m.show_settings = true);
    crate::shelf::view::refresh();
    pump(0.35);

    if crate::shelf::view::settings_view().is_none() {
        println!("FAIL lineswitch: the settings page never came up");
        return false;
    }
    let drawer_was_up = crate::shelf::panel::debug_panel_window_visible();

    // Press the 晾衣绳 row. 外观 is the page a fresh pane opens on, and 形态 is
    // its first group, so no tab switch is needed. The hotspots come from
    // `drawRect:`, which an occluded window skips.
    let Some(view) = crate::shelf::view::settings_view() else { return false };
    crate::app::selftest_draw::force_draw(&view);
    let Some(center) = crate::shelf::settings_pane::selftest_hotspot_center(
        &view,
        crate::shelf::settings_pane::Choice::Layout("line"),
    ) else {
        println!("FAIL lineswitch: no 晾衣绳 row on the 外观 page");
        return false;
    };
    crate::shelf::settings_pane::selftest_click_at(&view, center);
    // The switch rebuilds the tree, which releases the pane that handled this
    // very click — so nothing may touch `view` past here.
    drop(view);
    pump(0.6);

    let switched = layout_skin::current_id() == "line";
    let drawer_gone = !crate::shelf::panel::debug_panel_window_visible();
    let strip_up = is_visible();
    println!(
        "{} picking 晾衣绳 in 设置 → form is 晾衣绳, drawer window gone (up before: {drawer_was_up}), strip up",
        if switched && drawer_gone && strip_up { "ok  " } else { "FAIL" }
    );

    // Hand the process back the drawer it started on.
    layout_skin::select(layout_skin::DEFAULT_ID);
    crate::shelf::panel::relayout();
    crate::shelf::panel::hide();

    drawer_was_up && switched && drawer_gone && strip_up
}

/// `--selftest linegear`: the empty strip's way into 设置.
///
/// With no clips the row has nothing to click, so the right end carries a
/// settings hot zone. Two things have to hold: that corner must take the click
/// (or a fresh install can never reach the page or the drawer), and the rest of
/// the strip must go on letting clicks through (or the zone has eaten 44pt of
/// whatever is behind the strip). Also asserts the zone is *not* offered with
/// photos on the rope, where a card's menu already has the row.
pub fn selftest_gear_report() -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    if screen_for_pointer(mtm).is_none() {
        return false;
    }
    let (Some(p), Some(view)) = (controller().panel.borrow().clone(), controller().view.borrow().clone())
    else {
        return false;
    };
    show();
    debug_settle(true);
    stop_clock();

    // 1. With photos on the rope there is no zone at all — a card's right-click
    //    menu already offers 设置…, and a zone here would sit on the row.
    debug_hover_at(screen_point_of(&p, &view, CGPoint::new(p.frame().max().x - 8.0, p.frame().mid().y)));
    let no_zone_with_cards = !line_view::gear_visible(debug_card_count());
    println!(
        "{} photos on the rope → no settings hot zone (the card menu has the row)",
        if no_zone_with_cards { "ok  " } else { "FAIL" }
    );

    // Empty the rope for the real case.
    let saved = debug_take_cards();
    let r = line_view::gear_rect(CGRect::new(CGPoint::ZERO, view.bounds().size));
    let at_gear = screen_point_of(&p, &view, CGPoint::new(r.mid().x, r.mid().y));
    let bare = screen_point_of(&p, &view, CGPoint::new(r.min().x - 60.0, r.mid().y));

    // 2. The gear takes the click.
    debug_hover_at(at_gear);
    let gear_clickable = debug_clickable();
    println!(
        "{} empty rope → the settings corner takes the click",
        if gear_clickable { "ok  " } else { "FAIL" }
    );

    // 3. Stepping off it onto bare strip puts the click-through back. This is
    //    the case a card-index-keyed flip would miss: both points report no
    //    card, so only the zone's own state distinguishes them.
    debug_hover_at(bare);
    let bare_falls_through = !debug_clickable();
    println!(
        "{} empty rope → off the corner the strip still lets clicks through",
        if bare_falls_through { "ok  " } else { "FAIL" }
    );

    // 4. And back, so the zone is not a one-way flip.
    debug_hover_at(at_gear);
    let back = debug_clickable();
    println!(
        "{} returning to the corner takes the click again",
        if back { "ok  " } else { "FAIL" }
    );

    // 5. The zone has to be *drawn*: an invisible hot zone would silently eat
    //    clicks meant for the desktop. Render the empty strip offscreen where a
    //    human (and a pixel diff) can see the gear — at a plausible strip width
    //    rather than the full 3440pt desktop, or the 44pt corner is a few
    //    pixels and tells nobody anything.
    let mtm = MainThreadMarker::new().expect("main thread");
    let size = CGSize::new(900.0, strip_height());
    let host = NSView::initWithFrame(NSView::alloc(mtm), CGRect::new(CGPoint::ZERO, size));
    let offscreen = LineView::new(mtm, CGRect::new(CGPoint::ZERO, size));
    host.addSubview(&offscreen);
    let gear_drawn = crate::app::selftest_m2::snapshot(&host, "pastory-line-empty.png");
    println!(
        "{} the empty strip renders with the settings corner drawn",
        if gear_drawn { "ok  " } else { "FAIL" }
    );

    // Put the rope back and leave nothing on screen.
    debug_put_cards(saved);
    hide();
    no_zone_with_cards && gear_clickable && bare_falls_through && back && gear_drawn
}

/// `--selftest lineselect`: the rope's selected ring.
///
/// The defect this pins: 「编辑」 on the fifth photo left the first one lit,
/// because the rope drew `items.first()` where the panel draws the model's
/// `selectedID`. The two marks are separate on purpose and both are asserted
/// here — the green check is 「在剪贴板上」 (the top of the shelf, which does
/// **not** move when the user picks another card), the ring is 「当前操作对象」.
///
/// The action is driven at the model half of `item_actions::edit` (its first
/// line, `select_for_action`); the window half belongs to `--selftest editors`
/// and is deliberately not opened here, so this test needs no window server and
/// leaves nothing on screen.
pub fn selftest_selection_report() -> bool {
    // Three cards are the minimum that lets a third span act as a control.
    if debug_card_count() < 3 {
        println!("FAIL rope has {} cards, need 3", debug_card_count());
        return false;
    }
    let items: Vec<ClipItem> = (0..3)
        .filter_map(|i| item_at(i as i64))
        .collect();
    if items.len() < 3 {
        return false;
    }

    // 1. Selecting card 2 (not the first) marks card 2 and nothing else.
    crate::shelf::panel::with_model_mut(|m| m.select_for_action(&items[2]));
    reload_cards();
    let cards = current_cards();
    let third_only = cards[2].selected && !cards[0].selected && !cards[1].selected;
    println!(
        "{} 编辑第三张 → 只有第三张选中 (selected = {}, {}, {})",
        if third_only { "ok  " } else { "FAIL" },
        cards[0].selected,
        cards[1].selected,
        cards[2].selected
    );

    // 2. The green check has not moved: it still marks the shelf's first clip.
    let [c0, c1, c2] = [&cards[0], &cards[1], &cards[2]];
    let check_kept = c0.on_clipboard && !c1.on_clipboard && !c2.on_clipboard;
    println!(
        "{} 绿勾仍只在第一条 (on_clipboard = {}, {}, {})",
        if check_kept { "ok  " } else { "FAIL" },
        c0.on_clipboard,
        c1.on_clipboard,
        c2.on_clipboard
    );
    drop(cards);

    // 3. The ring is actually painted. Two offscreen renders — selection on the
    //    first card, then on the third — and the differing pixels counted per
    //    card. The card that lost the ring must change, the card that gained it
    //    must change, and the untouched middle card must not move by so much as
    //    a pixel: an assertion that only looked at the newly selected card could
    //    not tell a ring from a redraw of the whole row.
    //
    //    The card list itself is held fixed — one snapshot, one bit flipped —
    //    because a second `reload_cards()` is not guaranteed to produce the same
    //    pictures (thumbnails decode on their own thread, and the breeze is a
    //    function of the clock, so two renders milliseconds apart would differ
    //    on every card for reasons that have nothing to do with the ring). With
    //    the list frozen, the only thing the two renders can differ by is
    //    `selected` — which is exactly the claim. The in-memory swing switch is
    //    turned off for the same reason; the stored preference is left alone.
    let swing_was = controller().swing.get();
    controller().swing.set(false);
    let width = 900.0;
    let n = debug_card_count();
    let doc_w = line_view::row_width(n, width).max(width);
    let slots = line_view::layout(n, doc_w, line_card::spacing());
    let span = |i: usize| {
        let f = line_card::frame_rect(slots[i].attach);
        (f.min().x, f.max().x)
    };
    let base = current_cards();
    let with_only = |picked: usize| {
        let mut v = base.clone();
        for (i, c) in v.iter_mut().enumerate() {
            c.selected = i == picked;
        }
        v
    };
    let (Some(before), Some(after)) = (
        {
            debug_put_cards(with_only(0));
            rope_bitmap(doc_w)
        },
        {
            debug_put_cards(with_only(2));
            rope_bitmap(doc_w)
        },
    ) else {
        controller().swing.set(swing_was);
        debug_put_cards(base);
        println!("FAIL offscreen render failed");
        return false;
    };
    controller().swing.set(swing_was);
    debug_put_cards(base);
    let (a0, b0) = span(0);
    let (a1, b1) = span(1);
    let (a2, b2) = span(2);
    let lost = changed_pixels(&before, &after, a0, b0);
    let same = changed_pixels(&before, &after, a1, b1);
    let gained = changed_pixels(&before, &after, a2, b2);
    let painted = lost > 40 && gained > 40 && same == 0;
    println!(
        "{} 离屏前后对比：第一张掉了描边 ({lost} px)，第三张长出来了 ({gained} px)，中间那张一个像素没动 ({same} px)",
        if painted { "ok  " } else { "FAIL" }
    );

    // Leave the shelf as it was found.
    crate::shelf::panel::with_model_mut(|m| m.select_for_action(&items[0]));
    reload_cards();
    repaint();
    third_only && check_kept && painted
}

/// Draw the rope offscreen at `doc_w` and hand back its raw bitmap
/// (`bytes, bytes_per_row, height, samples_per_pixel`). Same route as
/// `render_offscreen`, minus the PNG: a diff wants the bytes.
fn rope_bitmap(doc_w: f64) -> Option<(Vec<u8>, isize, isize, isize)> {
    let mtm = MainThreadMarker::new()?;
    // No material behind an offscreen draw; without this the frosted fill
    // resolves against whatever the last live window left in the graphics
    // context and the two renders would differ for a reason that is not the
    // ring.
    crate::app::theme::set_panel_material_alpha(0.0);
    let view = LineView::new(mtm, CGRect::new(CGPoint::ZERO, CGSize::new(doc_w, strip_height())));
    // The swing frozen at a settled moment, so both renders show the same
    // angles and the diff is about the ring alone.
    controller().started.set(now_secs() - 5.0);
    view.layoutSubtreeIfNeeded();
    view.displayIfNeeded();
    let rep = view.bitmapImageRepForCachingDisplayInRect(view.bounds())?;
    view.cacheDisplayInRect_toBitmapImageRep(view.bounds(), &rep);
    let h = rep.pixelsHigh() as isize;
    let stride = rep.bytesPerRow() as isize;
    let spp = rep.samplesPerPixel() as isize;
    if h <= 0 || stride <= 0 || spp < 3 {
        return None;
    }
    let base = rep.bitmapData();
    // SAFETY: the rep owns `stride * h` bytes, and the copy happens while the
    // rep is alive.
    let bytes = unsafe { std::slice::from_raw_parts(base, (stride * h) as usize) }.to_vec();
    Some((bytes, stride, h, spp))
}

/// Pixels that differ between two rope bitmaps inside `[x0, x1)`. Whole columns
/// only — the cards' x spans are disjoint, so a span identifies a card without
/// depending on the bitmap's row order.
fn changed_pixels(before: &(Vec<u8>, isize, isize, isize), after: &(Vec<u8>, isize, isize, isize), x0: f64, x1: f64) -> usize {
    let (a, stride, h, spp) = before;
    let b = &after.0;
    let lo = x0.floor().max(0.0) as isize;
    let hi = x1.ceil() as isize;
    let mut n = 0usize;
    for y in 0..*h {
        for x in lo..hi {
            let off = (y * *stride + x * *spp) as usize;
            // SAFETY: `off` is inside both bitmaps (same geometry, same rep
            // size, and the loop stays within the columns).
            let (pa, pb) = unsafe {
                (
                    std::slice::from_raw_parts(a.as_ptr().add(off), 3),
                    std::slice::from_raw_parts(b.as_ptr().add(off), 3),
                )
            };
            if pa != pb {
                n += 1;
            }
        }
    }
    n
}

/// `--selftest linescroll`: the row's scroll position survives a rebuild.
///
/// The defect this pins: 设置 › 外观 › 晾衣绳 › 卡片大小 rebuilds the tree, and the
/// new clip view starts at origin 0, so the row jumped back to the first photo
/// — 「改完卡片大小后，绳子回到第一张」. The position is carried through
/// `rebuild` → `reload_cards`; this asserts the number before and after a real
/// `refresh_metrics()` (the call the settings row makes) and, so a stuck
/// constant cannot pass, that a row wider than the strip can still be moved at
/// all.
pub fn selftest_scroll_report() -> bool {
    let Some(_) = MainThreadMarker::new() else { return false };
    let (Some(p), Some(_)) = (controller().panel.borrow().clone(), controller().view.borrow().clone())
    else {
        return false;
    };
    if debug_card_count() == 0 {
        println!("FAIL rope has no cards");
        return false;
    }
    let clip_w = p.frame().size.width;

    // The seeded row fits the strip, and a row that fits has nowhere to scroll
    // to — a rebuild of it cannot show the defect either way. Fill the *model*
    // (not the card list: `refresh_metrics` rebuilds the cards from the model,
    // so a row injected straight into the controller would be replaced before
    // the assertion) until the row genuinely overflows. This is the scratch
    // store the self-tests are pinned to, never the user's.
    let filler = "scrolling probe line";
    let source = crate::clipboard::store::Source::pastory();
    for i in 0..70 {
        crate::clipboard::store::with(|s| {
            s.insert_text(&format!("{filler} {i}"), None, &source);
        });
    }
    crate::shelf::panel::with_model_mut(|m| m.refresh_order());
    reload_cards();
    let n = debug_card_count();
    let overflow = line_view::row_width(n, clip_w) > clip_w + 800.0;
    if !overflow {
        println!("FAIL the probe row does not overflow the strip ({n} cards)");
        return false;
    }

    // Move the row, then run the real path the metric rows take.
    let v = controller().view.borrow().clone().unwrap();
    v.scroll_to(240.0);
    let before = v.clip_origin_pub();
    let moved = before > 100.0;
    println!(
        "{} the row really moves when it is wider than the strip (x = {before:.0}, {n} cards)",
        if moved { "ok  " } else { "FAIL" }
    );

    refresh_metrics();
    let after = controller()
        .view
        .borrow()
        .as_ref()
        .map(|v| v.clip_origin_pub())
        .unwrap_or(-1.0);
    // A rebuild with the metrics unchanged gives the same document width, so the
    // same x is expected — not merely "some non-zero value".
    let kept = (after - before).abs() < 1.0;
    println!(
        "{} 改卡片大小重建之后位置不变 ({before:.0} → {after:.0})",
        if kept { "ok  " } else { "FAIL" }
    );

    // The restore is clamped by the row, not written blindly: ask for a position
    // past the end and the rebuild has to bring it down to the row's own limit
    // rather than leaving the clip past its document.
    let v = controller().view.borrow().clone().unwrap();
    v.scroll_to(1.0e6);
    let at_end = v.clip_origin_pub();
    refresh_metrics();
    let after_end = controller()
        .view
        .borrow()
        .as_ref()
        .map(|v| v.clip_origin_pub())
        .unwrap_or(-1.0);
    let doc_w = debug_view_width();
    let limit = (doc_w - clip_w).max(0.0);
    let clamped = at_end > 0.0
        && (at_end - limit).abs() < 2.0
        && (after_end - at_end).abs() < 2.0;
    println!(
        "{} 位置被夹在行的两端之内并在重建后保持 (x = {at_end:.0} → {after_end:.0}, 上限 {limit:.0})",
        if clamped { "ok  " } else { "FAIL" }
    );

    // Leave the size preference as it was found; the filler clips go with the
    // throwaway store.
    crate::shelf::line_card::metrics::store_size(100);

    moved && kept && clamped
}

/// A point in the rope view's own coordinates, in screen space — the inverse of
/// `view_point`, so a test can aim at a rect it computed in view space.
fn screen_point_of(p: &LinePanel, view: &LineView, local: CGPoint) -> CGPoint {
    let in_window: CGPoint = unsafe {
        msg_send![
            view,
            convertPoint: local,
            toView: std::ptr::null::<AnyObject>()
        ]
    };
    p.convertPointToScreen(in_window)
}

/// Take the rope's cards out (and hand them back), so a self-test can exercise
/// the empty strip without a store that happens to be empty.
pub fn debug_take_cards() -> Vec<LineCardData> {
    std::mem::take(&mut *controller().cards.borrow_mut())
}

pub fn debug_put_cards(cards: Vec<LineCardData>) {
    *controller().cards.borrow_mut() = cards;
    sync_doc_width();
    repaint();
}

/// `--selftest line`: render the rope offscreen (no window server needed) so
/// the row can be eyeballed and diffed. Builds one card per clip in the
/// store, laid out exactly as the live strip does.
pub fn render_offscreen(size: CGSize, out: &str) -> bool {
    let mtm = MainThreadMarker::new().expect("main thread");
    crate::app::theme::set_panel_material_alpha(0.0);
    if controller().cards.borrow().is_empty() {
        reload_cards();
    }
    let n = controller().cards.borrow().len().max(1);
    let doc_w = line_view::row_width(n, size.width).max(size.width);
    let view = LineView::new(mtm, CGRect::new(CGPoint::ZERO, CGSize::new(doc_w, size.height)));
    // Freeze the swing at a settled moment so the render is stable.
    controller().started.set(now_secs() - 5.0);
    crate::app::selftest_m2::snapshot(&view, out)
}

/// The card shape both the drawing and the self-tests share.
pub fn card_kind(index: usize) -> Option<ClipKind> {
    controller().cards.borrow().get(index).map(|c| c.item.kind)
}

/// A self-test's own state, for the CLI report.
pub fn debug_tick_count() -> u32 {
    controller().tick_count.get()
}

/// The current form, for a self-test's dispatch assertions.
pub fn form_active() -> bool {
    layout_skin::is_line()
}


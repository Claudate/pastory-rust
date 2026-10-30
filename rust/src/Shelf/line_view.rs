//! The 晾衣绳 view: the rope, the row of hanging photos, and the pointer
//! handling that goes with them.
//!
//! Two pieces of geometry, both pure functions so a self-test can assert them
//! without a window:
//!
//! * [`rope_y`] — the rope's own sag, a parabola `top + 4·sag·f·(1−f)` in the
//!   fraction `f` across the width (Tendedero's `ropeY`). The rope is drawn as
//!   one quadratic through its midpoint, three times over (shadow, core,
//!   highlight) with the two ends faded out.
//! * [`layout`] — where each card's attach point sits along that rope, evenly
//!   spaced and centred, or packed from the left when the row overflows.
//!
//! The view draws every card in one pass and hit-tests the same layout, so a
//! click is exactly a click on what is drawn. It is the window that decides
//! whether a click reaches the view at all (`line_panel::hover_check` flips
//! `ignoresMouseEvents`); everything here assumes the click already arrived.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSDragOperation, NSDraggingContext, NSDraggingSession, NSDraggingSource,
    NSEvent, NSGraphicsContext, NSMenuItem, NSView,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

use crate::app::{motion, theme};
use crate::shelf::line_card::{self, LineCardData};

/// The rope's own top inside the view (`ropeTop` 10).
pub const ROPE_TOP: f64 = 10.0;
/// The strip's rounded top corners.
pub const CORNER_R: f64 = 14.0;
/// How far the pointer must be inside a photo before it counts as on it.
pub const HIT_INFLATE: f64 = 4.0;
/// How far a press on a photo must travel sideways, while staying inside the
/// strip, before it counts as a pan of the row rather than a click.
pub const PAN_START: f64 = 6.0;
/// How far the pointer must leave the strip's band (up or down) before the
/// press counts as a drag-out — carrying the photo off to another app.
pub const DRAG_LEAVE: f64 = 24.0;

/// The rope's sag is clamped at 30pt, so this is its deepest possible value —
/// the strip must be tall enough for a card hung at the trough, or the bottom
/// of every photo at the middle of the row gets clipped by the window.
pub const MAX_SAG: f64 = 30.0;

/// The settings hot zone's width at the strip's right end.
pub const GEAR_W: f64 = 44.0;
/// The hot zone's height (the chip inside it is 30pt).
pub const GEAR_H: f64 = 34.0;

/// The settings hot zone: a rectangle at the strip's right end, centred
/// vertically in the strip's height.
///
/// The strip lets a click through unless it is on a photo, and an empty rope
/// has none — so before the first clip is copied there would be no way to reach
/// 设置 (or the way back to the drawer) at all. This corner is that way in.
///
/// In the view's own (flipped) coordinates, which is also the space the hit
/// test uses, so what is drawn and what is clickable cannot drift apart.
pub fn gear_rect(bounds: CGRect) -> CGRect {
    CGRect::new(
        CGPoint::new(bounds.max().x - GEAR_W, (bounds.size.height - GEAR_H) / 2.0),
        CGSize::new(GEAR_W, GEAR_H),
    )
}

/// Whether the strip shows the settings hot zone. Only an empty rope has one:
/// with photos on it, right-clicking a photo already offers 设置…, and a zone
/// pinned over the last photo would be a strip of the row the pointer could no
/// longer pass through to the desktop behind it.
pub fn gear_visible(cards: usize) -> bool {
    cards == 0
}

/// Rope sag: `min(30, width·0.018)`.
pub fn sag(width: f64) -> f64 {
    (width * 0.018).min(MAX_SAG)
}

/// The content height: the rope's deepest run, the clip, the frame and a
/// margin. Width-independent on purpose — the strip's height is fixed at build
/// time, before any particular card's place on the rope is known, so it has to
/// fit the worst case.
pub fn content_height() -> f64 {
    ROPE_TOP + MAX_SAG + line_card::CLIP_H - 10.0 + line_card::frame_h() + 16.0
}

/// The rope's y at `x` over a view `width` wide: a parabola dipping by `sag`
/// at the centre and meeting `ROPE_TOP` at both edges.
pub fn rope_y(x: f64, width: f64) -> f64 {
    if width <= 0.0 {
        return ROPE_TOP;
    }
    let f = (x / width).clamp(0.0, 1.0);
    ROPE_TOP + 4.0 * sag(width) * f * (1.0 - f)
}

/// The clip's new horizontal origin after a wheel of `delta` points, clamped to
/// the row. Pure, so `--selftest linewheel` can assert the mapping without a
/// live window: content towards the user (`delta > 0`) walks back to the start.
pub fn scrolled_origin(origin_x: f64, delta: f64, doc_w: f64, clip_w: f64) -> f64 {
    let max_x = (doc_w - clip_w).max(0.0);
    (origin_x - delta).clamp(0.0, max_x)
}

/// What a press on the strip has turned into, as the pointer moves.
///
/// The strip is one surface with two jobs, so a press has to be told apart:
///
/// * leaving the strip's band means the photo is being carried out to another
///   app (`DragOut`);
/// * moving sideways while still inside the band means the row is being pulled
///   along, the way you drag a real clothesline to reach the far end (`Pan`).
///
/// The order matters: leaving the band wins, so a diagonal drag that leaves the
/// strip never starts a pan first. Pure, so a self-test can pin the rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PressGesture {
    /// Not far enough yet: still a click.
    None,
    /// Pull the row along.
    Pan,
    /// Carry the photo off the strip.
    DragOut,
}

pub fn classify_press(dx: f64, y: f64, height: f64) -> PressGesture {
    if y < -DRAG_LEAVE || y > height + DRAG_LEAVE {
        return PressGesture::DragOut;
    }
    if dx.abs() >= PAN_START {
        return PressGesture::Pan;
    }
    PressGesture::None
}

/// One card's place in the row.
#[derive(Clone, Copy)]
pub struct Slot {
    /// Centre x of the attach point.
    pub cx: f64,
    /// The attach point (on the rope).
    pub attach: CGPoint,
    /// The card's index into the model list.
    pub index: usize,
}

/// Lay the row out over `width` with `n` cards, centred when they fit and
/// packed from the left with `spacing` when they do not.
pub fn layout(n: usize, width: f64, spacing: f64) -> Vec<Slot> {
    let mut out = Vec::with_capacity(n);
    if n == 0 || width <= 0.0 {
        return out;
    }
    let row_w = n as f64 * line_card::card_w() + n.saturating_sub(1) as f64 * (spacing - line_card::card_w());
    let start = if row_w < width { (width - row_w) / 2.0 } else { 24.0 };
    for i in 0..n {
        let cx = start + line_card::card_w() / 2.0 + spacing * i as f64;
        out.push(Slot {
            cx,
            attach: CGPoint::new(cx, rope_y(cx, width)),
            index: i,
        });
    }
    out
}

/// The doc-view width for `n` cards: the row's own width when it overflows,
/// else the clip's (so a short row still centres).
pub fn row_width(n: usize, clip_width: f64) -> f64 {
    let row = 48.0
        + n as f64 * line_card::card_w()
        + n.saturating_sub(1) as f64 * (line_card::spacing() - line_card::card_w());
    row.max(clip_width).max(1.0)
}

/// The index of the photo under a view-coordinate point, or -1. The rect is
/// the frame the drawing uses, inflated 4pt — the same test the window's
/// hover flip runs.
pub fn hit_index(cards: &[LineCardData], width: f64, p: CGPoint) -> i64 {
    for slot in layout(cards.len(), width, line_card::spacing()).iter() {
        let frame = line_card::frame_rect(slot.attach);
        let r = CGRect::new(
            CGPoint::new(frame.min().x - HIT_INFLATE, frame.min().y - HIT_INFLATE),
            CGSize::new(frame.size.width + 2.0 * HIT_INFLATE, frame.size.height + 2.0 * HIT_INFLATE),
        );
        if p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y {
            return slot.index as i64;
        }
    }
    -1
}

pub struct LineViewIvars {
    /// The card frame under the pointer, if any (-1 = none).
    hot: Cell<i64>,
    /// The pointer is in the settings hot zone (the empty strip's way in).
    gear_hot: Cell<bool>,
    /// The card a press landed on, so its release knows what it was on.
    pressed: Cell<i64>,
    /// The pointer button is down on the strip. The clock reads this (through
    /// `interacting`) and leaves the window alone for as long as it is true —
    /// a re-flip mid-gesture would drop the mouse-up, and a retract would take
    /// the strip out from under the pointer.
    pressed_down: Cell<bool>,
    /// A drag-out session began for this press; its release must not also copy.
    dragged: Cell<bool>,
    /// This press turned into a pan of the row (the way you pull a clothesline
    /// sideways to reach the photos further along); its release must not copy
    /// either.
    panning: Cell<bool>,
    /// Where the press landed, and the clip origin at that moment — the pan is
    /// measured from these, so it tracks the pointer exactly.
    press: Cell<CGPoint>,
    pan_origin: Cell<f64>,
    // Cached layout for performance.
    cached_layout: RefCell<Option<Vec<Slot>>>,
    cached_width: Cell<f64>,
    cached_card_count: Cell<usize>,
    // Cached rope path for performance.
    cached_rope_path: RefCell<Option<Retained<NSBezierPath>>>,
    cached_rope_width: Cell<f64>,
}

define_class!(
    // SAFETY:
    // - NSView subclass; flipped, main-thread UI like every AppKit view.
    // - Ivars are plain data behind `Cell`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = LineViewIvars]
    pub struct LineView;

    unsafe impl NSObjectProtocol for LineView {}

    // The row is a drag source: a photo dragged off the rope carries its file
    // (Finder, Mail) or its text. Only the one required method is implemented;
    // a copy operation is what every drop target expects.
    unsafe impl NSDraggingSource for LineView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            _ctx: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }

    impl LineView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        /// A borderless, non-key strip takes its first click through this.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            let bounds = self.bounds();
            draw_rope(self, bounds);
            draw_cards(self, bounds);
            draw_gear(self, bounds);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let idx = self.index_at(event);
            self.ivars().pressed.set(idx);
            self.ivars().dragged.set(false);
            self.ivars().panning.set(false);
            self.ivars().pressed_down.set(true);
            self.ivars().press.set(self.point_of(event));
            self.ivars().pan_origin.set(self.clip_origin());
            // The clock must not touch the window while the button is down: see
            // the `pressed_down` note.
            super::line_panel::interaction_started();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let was_held = self.ivars().pressed_down.get();
            self.ivars().pressed_down.set(false);
            // A drag-out or a pan swallows the click; either way the gesture's
            // end is what releases the clock's no-touch window.
            let was_drag = self.ivars().dragged.get();
            let was_pan = self.ivars().panning.get();
            self.ivars().panning.set(false);
            self.ivars().dragged.set(false);
            if was_held {
                super::line_panel::interaction_ended();
            }
            if was_drag || was_pan {
                return;
            }
            let idx = self.ivars().pressed.get();
            if event.clickCount() >= 2 {
                super::line_panel::double_click_paste(idx);
            } else {
                super::line_panel::click_copy(idx);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if self.ivars().dragged.get() {
                return;
            }
            let p = self.point_of(event);
            let start = self.ivars().press.get();
            let h = self.bounds().size.height;
            if self.ivars().panning.get() {
                // Already pulling the row: track the pointer from the origin it
                // had when the pan began, so it moves exactly with the hand.
                self.pan(self.ivars().pan_origin.get(), p.x - start.x);
                return;
            }
            match classify_press(p.x - start.x, p.y, h) {
                PressGesture::DragOut => {
                    // Only a session that actually started counts as a drag;
                    // otherwise the release still copies (see `begin_drag`).
                    if super::line_panel::begin_drag(self, event, self.ivars().pressed.get()) {
                        self.ivars().dragged.set(true);
                    }
                }
                PressGesture::Pan => {
                    self.ivars().panning.set(true);
                    self.pan(self.ivars().pan_origin.get(), p.x - start.x);
                }
                PressGesture::None => {}
            }
        }

        /// The pointer wheel pages the row horizontally, the way the panel
        /// form's `CardsRowView` does — a bare `NSScrollView` leaves the wheel
        /// on the vertical axis, which the strip has no use for. Shift the clip
        /// origin by hand.
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let Some(scroll) = self.enclosingScrollView() else { return };
            let dx = event.scrollingDeltaX();
            let dy = event.scrollingDeltaY();
            let mut delta = if dx.abs() >= dy.abs() { dx } else { dy };
            if !event.hasPreciseScrollingDeltas() {
                // Wheel notches arrive ~0.1/notch ("lines"); scroll 10pt a line.
                delta *= 10.0;
            }
            if delta == 0.0 {
                return;
            }
            let clip = scroll.contentView();
            let doc_w = scroll
                .documentView()
                .map(|d| d.frame().size.width)
                .unwrap_or(0.0);
            // Content towards the user (delta > 0) walks back to the start, the
            // same convention the panel's row uses.
            let x = scrolled_origin(clip.bounds().origin.x, delta, doc_w, clip.bounds().size.width);
            if x != clip.bounds().origin.x {
                clip.scrollToPoint(CGPoint::new(x, clip.bounds().origin.y));
                scroll.reflectScrolledClipView(&clip);
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let idx = self.index_at(event);
            if idx < 0 && self.gear_at(event) {
                // The empty strip: no card to act on, but the shelf's own two
                // rows still have to be reachable.
                super::line_panel::show_shelf_menu(event, self);
                return;
            }
            super::line_panel::show_menu(idx, event, self);
        }

        // The context menu's rows all dispatch through this one selector; the
        // card index is the item's `tag` and the action id rides in its
        // `representedObject`, so a new row needs no new method here.
        #[unsafe(method(lineAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            let action = sender
                .representedObject()
                .and_then(|o| o.downcast_ref::<NSString>().map(|s| s.to_string()))
                .unwrap_or_default();
            super::line_panel::perform_action(sender.tag() as i64, &action, self);
        }
    }
);

impl LineView {
    pub fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(LineViewIvars {
            hot: Cell::new(-1),
            gear_hot: Cell::new(false),
            pressed: Cell::new(-1),
            pressed_down: Cell::new(false),
            dragged: Cell::new(false),
            panning: Cell::new(false),
            press: Cell::new(CGPoint::ZERO),
            pan_origin: Cell::new(0.0),
            cached_layout: RefCell::new(None),
            cached_width: Cell::new(0.0),
            cached_card_count: Cell::new(0),
            // Cached rope path for performance.
            cached_rope_path: RefCell::new(None),
            cached_rope_width: Cell::new(0.0),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub fn set_hot(&self, index: i64) {
        if self.ivars().hot.get() != index {
            self.ivars().hot.set(index);
            self.setNeedsDisplay(true);
        }
    }

    /// Highlight (or stop highlighting) the settings hot zone.
    pub fn set_gear_hot(&self, on: bool) {
        if self.ivars().gear_hot.get() != on {
            self.ivars().gear_hot.set(on);
            self.setNeedsDisplay(true);
        }
    }

    /// Whether a drag session is in flight (read by the clock's end-poll).
    pub fn dragging(&self) -> bool {
        self.ivars().dragged.get()
    }

    /// Pull the row `delta` points from where it sits now. Exposed for the
    /// self-test, which has no way to deliver a real `mouseDragged:`.
    pub fn pan_from_current(&self, delta: f64) {
        self.pan(self.clip_origin(), delta);
    }

    /// Put the row's left edge at `x`, clamped to the row's ends — the absolute
    /// counterpart of [`Self::pan_from_current`], for restoring a scroll
    /// position into a freshly built clip view. Goes through the same
    /// `scrolled_origin` mapping, so a restored position and a wheel can never
    /// disagree about where x means.
    pub fn scroll_to(&self, x: f64) {
        self.pan(0.0, -x);
    }

    /// The row's current horizontal origin (the clip view's), for the report.
    pub fn clip_origin_pub(&self) -> f64 {
        self.clip_origin()
    }

    /// Whether the pointer button is down on the strip (read by the clock's
    /// poll, so a mouse-up the timer never sees cannot leave the gate shut).
    pub fn interacting(&self) -> bool {
        self.ivars().pressed_down.get()
    }

    /// The event's point in the view's own (flipped) coordinates.
    fn point_of(&self, event: &NSEvent) -> CGPoint {
        unsafe {
            msg_send![
                self,
                convertPoint: event.locationInWindow(),
                fromView: std::ptr::null::<AnyObject>()
            ]
        }
    }

    /// The row's current horizontal origin.
    fn clip_origin(&self) -> f64 {
        self.enclosingScrollView()
            .map(|s| s.contentView().bounds().origin.x)
            .unwrap_or(0.0)
    }

    /// Pull the row `delta` points from `base`, clamped to the row's ends — the
    /// same mapping the wheel uses (`scrolled_origin`), so a drag and a wheel
    /// can never disagree about which way the row moves.
    fn pan(&self, base: f64, delta: f64) {
        let Some(scroll) = self.enclosingScrollView() else { return };
        let clip = scroll.contentView();
        let doc_w = scroll
            .documentView()
            .map(|d| d.frame().size.width)
            .unwrap_or(0.0);
        let x = scrolled_origin(base, delta, doc_w, clip.bounds().size.width);
        if x != clip.bounds().origin.x {
            clip.scrollToPoint(CGPoint::new(x, clip.bounds().origin.y));
            scroll.reflectScrolledClipView(&clip);
        }
    }

    pub fn hot(&self) -> i64 {
        self.ivars().hot.get()
    }

    /// Whether an event lands in the settings hot zone (an empty strip only).
    fn gear_at(&self, event: &NSEvent) -> bool {
        if !gear_visible(super::line_panel::current_cards().len()) {
            return false;
        }
        let p = self.point_of(event);
        let r = gear_rect(self.bounds());
        p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y
    }

    /// The card under an event, in view coordinates.
    fn index_at(&self, event: &NSEvent) -> i64 {
        let p: CGPoint = unsafe {
            msg_send![
                self,
                convertPoint: event.locationInWindow(),
                fromView: std::ptr::null::<AnyObject>()
            ]
        };
        hit_index(&super::line_panel::current_cards(), self.bounds().size.width, p)
    }

    /// Returns the cached layout, updating it if the card count or view width
    /// has changed.
    fn get_cached_layout(&self, bounds: CGRect) -> Vec<Slot> {
        let cards = super::line_panel::current_cards();
        let width = bounds.size.width;
        let card_count = cards.len();
        let mut cached = self.ivars().cached_layout.borrow_mut();
        if cached.is_none()
            || self.ivars().cached_width.get() != width
            || self.ivars().cached_card_count.get() != card_count
        {
            *cached = Some(layout(card_count, width, line_card::spacing()));
            self.ivars().cached_width.set(width);
            self.ivars().cached_card_count.set(card_count);
        }
        cached.clone().unwrap() // safe because we just set it if None
    }

    /// Returns the cached rope path, updating it if the view width
    /// has changed.
    fn get_cached_rope_path(&self, bounds: CGRect) -> Retained<NSBezierPath> {
        let width = bounds.size.width;
        {
            let mut cached = self.ivars().cached_rope_path.borrow_mut();
            if cached.is_none()
                || self.ivars().cached_rope_width.get() != width
            {
                let path = Self::build_rope_path(width);
                *cached = Some(path);
                self.ivars().cached_rope_width.set(width);
            }
        }
        let cached = self.ivars().cached_rope_path.borrow();
        cached.as_ref().expect("rope path is set").clone()
    }

    /// Builds the rope path for a given width (without stroking).
    fn build_rope_path(width: f64) -> Retained<NSBezierPath> {
        if width <= 1.0 {
            return NSBezierPath::new();
        }
        let y0 = rope_y(0.0, width);
        let y1 = rope_y(width, width);
        let mid = rope_y(width / 2.0, width);
        let path = NSBezierPath::new();
        path.moveToPoint(CGPoint::new(0.0, y0));
        path.curveToPoint_controlPoint(
            CGPoint::new(width, y1),
            CGPoint::new(width / 2.0, 2.0 * mid - (y0 + y1) / 2.0),
        );
        path
    }
}

/// The rope: a quadratic through the sag midpoint, drawn shadow → core →
/// highlight with the two ends faded out.
fn draw_rope(view: &LineView, bounds: CGRect) {
    let w = bounds.size.width;
    if w <= 1.0 {
        return;
    }
    let _y0 = rope_y(0.0, w);
    let _y1 = rope_y(w, w);
    let _mid = rope_y(w / 2.0, w);
    let path = view.get_cached_rope_path(bounds);

    NSGraphicsContext::saveGraphicsState_class();
    // shadow
    path.setLineWidth(3.0);
    NSColor::blackColor().colorWithAlphaComponent(0.10).setStroke();
    path.stroke();
    // core
    path.setLineWidth(1.6);
    theme::separator().setStroke();
    path.stroke();
    // highlight
    path.setLineWidth(0.7);
    NSColor::whiteColor().colorWithAlphaComponent(0.35).setStroke();
    path.stroke();
    NSGraphicsContext::restoreGraphicsState_class();

    // The two ends fade out: a ground-coloured wash over a short run each side.
    let fade_w = (w * 0.06).clamp(18.0, 64.0);
    let ground = theme::ground_over_material();
    for (x0, dir) in [(0.0, 1.0), (w - fade_w, -1.0)] {
        NSGraphicsContext::saveGraphicsState_class();
        for k in 0..fade_w as i64 {
            let x = (x0 + dir * k as f64).clamp(0.0, w);
            let a = if dir > 0.0 {
                1.0 - (k as f64 / fade_w)
            } else {
                k as f64 / fade_w
            };
            ground.colorWithAlphaComponent(a.clamp(0.0, 1.0)).setFill();
            objc2_app_kit::NSRectFill(CGRect::new(
                CGPoint::new(x, rope_y(x, w) - 2.0),
                CGSize::new(1.0, 5.0),
            ));
        }
        NSGraphicsContext::restoreGraphicsState_class();
    }
}

/// The settings hot zone's gear. Drawn whenever the zone exists: an invisible
/// hot zone would silently swallow clicks meant for the desktop behind the
/// strip's right end.
fn draw_gear(view: &LineView, bounds: CGRect) {
    if !gear_visible(super::line_panel::current_cards().len()) {
        return;
    }
    let r = gear_rect(bounds);
    let hot = view.ivars().gear_hot.get();
    let tint = if hot { theme::accent() } else { theme::label_tertiary() };
    // A soft chip behind the glyph, so the zone reads as a control rather than
    // a stray icon hanging on the rope.
    let chip = CGRect::new(
        CGPoint::new(r.mid().x - 15.0, r.mid().y - 15.0),
        CGSize::new(30.0, 30.0),
    );
    NSGraphicsContext::saveGraphicsState_class();
    tint.colorWithAlphaComponent(if hot { 0.16 } else { 0.08 }).setFill();
    NSBezierPath::bezierPathWithOvalInRect(chip).fill();
    tint.colorWithAlphaComponent(if hot { 0.9 } else { 0.55 }).setStroke();
    let ring = NSBezierPath::bezierPathWithOvalInRect(chip);
    ring.setLineWidth(1.0);
    ring.stroke();
    NSGraphicsContext::restoreGraphicsState_class();
    if let Some(img) = crate::shelf::card::symbol(
        "gearshape",
        15.0,
        crate::shelf::card::SymbolWeight::Regular,
        &tint,
    ) {
        crate::shelf::card::draw_centered(&img, chip, 1.0);
    }
}

/// Draw every card at its place and angle.
fn draw_cards(view: &LineView, bounds: CGRect) {
    let cards = super::line_panel::current_cards();
    if cards.is_empty() {
        return;
    }
    let t = super::line_panel::swing_time();
    // 摆动关闭时整排静止（设置 › 外观 › 晾衣绳 › 摆动）。
    let swinging = super::line_panel::swing_active();
    let slots = view.get_cached_layout(bounds);
    for slot in slots.iter() {
        let Some(card) = cards.get(slot.index) else { continue };
        // Every card swings on its own breeze; the pointer's target leans a
        // touch further as its hover lift.
        let mut angle = if swinging { motion::card_angle(card.seed, t) } else { 0.0 };
        if view.hot() == slot.index as i64 {
            angle -= 2.0;
        }
        line_card::draw(slot.attach, angle, card);
    }
}


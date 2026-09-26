//! Port of `Capture/SelectionOverlay.swift` (M3 slice: picker + frozen frame
//! + handles + record-ready chrome; the annotation canvas is M4).
//!
//! Full-screen picker: drag a region, tap a window, F for the whole display.
//! Every display gets one borderless nonactivating panel at the shielding
//! window level; while picking, keys arrive through system-wide Carbon hooks
//! (unbound the moment the frame is held), because the overlay only becomes
//! key after the picture under the cursor is already frozen.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSEvent, NSPanel, NSRunningApplication, NSScreen, NSStringDrawing, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::NSString;
use objc2_screen_capture_kit::{SCDisplay, SCWindow};

use crate::app::{coordinates, hotkey::HotKeyCenter, localization::l, theme};
use crate::capture::target::{CaptureTarget, ShareableSnapshot};

/// `PickMode`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    Region,
    Window,
}

// MARK: Controller singleton

pub struct SelectionOverlayController {
    overlays: RefCell<Vec<Retained<OverlayWindow>>>,
    completion: RefCell<Option<Box<dyn FnOnce(Option<CaptureTarget>) + 'static>>>,
    is_presenting: Cell<bool>,
    mode: Cell<PickMode>,
    hovered_window: RefCell<Option<Retained<SCWindow>>>,
    top_bar: RefCell<Option<Retained<TopBar>>>,
    record_bar: RefCell<Option<Retained<RecordReadyBar>>>,
    annotator: RefCell<Option<Retained<crate::annotate::annotate_view::AnnotateView>>>,
    toolbar: RefCell<Option<Retained<crate::annotate::toolbar::AnnotateToolbar>>>,
    app_to_restore: RefCell<Option<Retained<NSRunningApplication>>>,
    /// In record-ready the CropHook would re-crop as the frame moves; M4's
    /// annotator consumes it. Kept as a field name for parity.
    crop_provider: RefCell<Option<Box<dyn Fn(CGRect) -> Option<CFRetained<CGImage>> + 'static>>>,
    /// M3 record-ready → commit hook (the Swift `onShot` → canvas path).
    commit_action: RefCell<Option<Box<dyn Fn() + 'static>>>,
    /// M5 anchor: 开始录制/⏎/双击 in record-ready.
    record_action: RefCell<Option<Box<dyn Fn() + 'static>>>,
    cancel_action: RefCell<Option<Box<dyn Fn() + 'static>>>,
}

static CONTROLLER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

pub fn controller() -> &'static SelectionOverlayController {
    let ptr = *CONTROLLER.get_or_init(|| {
        Box::into_raw(Box::new(SelectionOverlayController {
            overlays: RefCell::new(Vec::new()),
            completion: RefCell::new(None),
            is_presenting: Cell::new(false),
            mode: Cell::new(PickMode::Region),
            hovered_window: RefCell::new(None),
            top_bar: RefCell::new(None),
            record_bar: RefCell::new(None),
            annotator: RefCell::new(None),
            toolbar: RefCell::new(None),
            app_to_restore: RefCell::new(None),
            crop_provider: RefCell::new(None),
            commit_action: RefCell::new(None),
            record_action: RefCell::new(None),
            cancel_action: RefCell::new(None),
        })) as usize
    });
    // SAFETY: boxed for the process lifetime; everything here is main-thread.
    unsafe { &*(ptr as *const SelectionOverlayController) }
}

// MARK: OverlayWindow

pub struct OverlayWindowIvars {
    screen_frame: Cell<CGRect>,
    backing_scale: Cell<f64>,
    display: RefCell<Option<Retained<SCDisplay>>>,
    overlay_view: RefCell<Option<Retained<OverlayView>>>,
}

impl Default for OverlayWindowIvars {
    fn default() -> Self {
        Self {
            screen_frame: Cell::new(CGRect::ZERO),
            backing_scale: Cell::new(1.0),
            display: RefCell::new(None),
            overlay_view: RefCell::new(None),
        }
    }
}

impl OverlayWindow {
    pub fn screen_frame(&self) -> CGRect {
        self.ivars().screen_frame.get()
    }
    pub fn backing_scale(&self) -> f64 {
        self.ivars().backing_scale.get()
    }
    pub fn display(&self) -> Option<Retained<SCDisplay>> {
        self.ivars().display.borrow().clone()
    }
    pub fn overlay_view(&self) -> Retained<OverlayView> {
        self.ivars().overlay_view.borrow().clone().expect("set at init")
    }
}

define_class!(
    // SAFETY:
    // - NSPanel subclass, main-thread only, like every window in this app.
    // - canBecomeKey (yes) + canBecomeMain (no): becoming main would activate
    //   the app and the menu bar switch would land in the frozen picture.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[ivars = OverlayWindowIvars]
    pub struct OverlayWindow;

    unsafe impl NSObjectProtocol for OverlayWindow {}

    impl OverlayWindow {
        #[unsafe(method(canBecomeKey))]
        fn can_become_key(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(canBecomeMain))]
        fn can_become_main(&self) -> objc2::runtime::Bool {
            false.into()
        }
    }
);

/// `OverlayWindow.init(screen:display:)` — one full-screen borderless panel.
fn make_overlay_window(screen: &NSScreen, display: &SCDisplay) -> Retained<OverlayWindow> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let frame = screen.frame();
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let this = mtm.alloc::<OverlayWindow>().set_ivars(OverlayWindowIvars::default());
    let w: Retained<OverlayWindow> = unsafe {
        msg_send![
            super(this),
            initWithContentRect: frame,
            styleMask: style,
            backing: objc2_app_kit::NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    w.ivars().screen_frame.set(frame);
    w.ivars().backing_scale.set(screen.backingScaleFactor());
    // SAFETY: plain retain for the ivar.
    *w.ivars().display.borrow_mut() = Some(unsafe {
        Retained::retain(display as *const SCDisplay as *mut SCDisplay).expect("non-null")
    });
    unsafe {
        w.setReleasedWhenClosed(false);
    }
    w.setOpaque(false);
    w.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
    w.setHasShadow(false);
    // Above other tools' floating bars (they use screenSaver+).
    w.setLevel(objc2_core_graphics::CGShieldingWindowLevel() as isize);
    w.setAcceptsMouseMovedEvents(true);
    w.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    let view = make_overlay_view(mtm, frame.size);
    w.setContentView(Some(&view));
    *w.ivars().overlay_view.borrow_mut() = Some(view);
    w.setFrame_display(frame, false);
    w
}

// MARK: OverlayView

pub struct OverlayViewIvars {
    drag_start: Cell<Option<CGPoint>>,
    drag_current: Cell<Option<CGPoint>>,
    /// After a pick: freeze the drawing; the frame gets handles.
    held: Cell<bool>,
    held_rect: RefCell<Option<CGRect>>,
    /// The screen as it was the instant the hotkey was pressed (drawn under
    /// the mask, so whatever closes when our windows appear is still there).
    backdrop: RefCell<Option<CFRetained<CGImage>>>,
    /// (SCWindow, its rect in Cocoa global) — precomputed by the controller.
    candidates: RefCell<Vec<(Retained<SCWindow>, CGRect)>>,
    screen_frame: Cell<CGRect>,
    backing_scale: Cell<f64>,
    resizing: RefCell<Option<(usize, CGRect)>>,
    record_mode: Cell<bool>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
}

impl Default for OverlayViewIvars {
    fn default() -> Self {
        Self {
            drag_start: Cell::new(None),
            drag_current: Cell::new(None),
            held: Cell::new(false),
            held_rect: RefCell::new(None),
            backdrop: RefCell::new(None),
            candidates: RefCell::new(Vec::new()),
            screen_frame: Cell::new(CGRect::ZERO),
            backing_scale: Cell::new(1.0),
            resizing: RefCell::new(None),
            record_mode: Cell::new(false),
            tracking: RefCell::new(None),
        }
    }
}

impl OverlayView {
    pub fn set_backdrop(&self, image: Option<CFRetained<CGImage>>) {
        *self.ivars().backdrop.borrow_mut() = image;
    }
    pub fn set_candidates(&self, windows: Vec<(Retained<SCWindow>, CGRect)>) {
        *self.ivars().candidates.borrow_mut() = windows;
    }
    pub fn set_screen_frame(&self, frame: CGRect) {
        self.ivars().screen_frame.set(frame);
    }
    pub fn set_backing_scale(&self, scale: f64) {
        self.ivars().backing_scale.set(scale);
    }
    pub fn set_held(&self, held: bool) {
        self.ivars().held.set(held);
    }
    pub fn held(&self) -> bool {
        self.ivars().held.get()
    }
    pub fn held_rect(&self) -> Option<CGRect> {
        *self.ivars().held_rect.borrow()
    }
    pub fn set_held_rect(&self, rect: Option<CGRect>) {
        *self.ivars().held_rect.borrow_mut() = rect;
    }
    pub fn set_record_mode(&self, on: bool) {
        self.ivars().record_mode.set(on);
    }
    pub fn record_mode(&self) -> bool {
        self.ivars().record_mode.get()
    }

    /// 8 handles: corners then edge midpoints (index → which sides move).
    fn handles(r: CGRect) -> [CGPoint; 8] {
        let (min, max, mid) = (r.min(), r.max(), r.mid());
        [
            CGPoint::new(min.x, min.y),
            CGPoint::new(max.x, min.y),
            CGPoint::new(min.x, max.y),
            CGPoint::new(max.x, max.y),
            CGPoint::new(mid.x, min.y),
            CGPoint::new(mid.x, max.y),
            CGPoint::new(min.x, mid.y),
            CGPoint::new(max.x, mid.y),
        ]
    }

    fn selection_rect(&self) -> Option<CGRect> {
        let a = self.ivars().drag_start.get()?;
        let b = self.ivars().drag_current.get()?;
        let r = CGRect::new(
            CGPoint::new(a.x.min(b.x), a.y.min(b.y)),
            CGSize::new((a.x - b.x).abs(), (a.y - b.y).abs()),
        );
        (r.size.width >= 2.0 && r.size.height >= 2.0).then_some(r)
    }

    /// The hovered window's rect in this view (`windowRectInView` chain).
    fn hovered_rect_in_view(&self) -> Option<CGRect> {
        let hovered = controller().hovered_window.borrow();
        let hovered = hovered.as_ref()?;
        let id = unsafe { hovered.windowID() };
        let (_, global) = self.ivars().candidates.borrow().iter().find(|(w, _)| unsafe {
            w.windowID() == id
        }).map(|(w, r)| (w, *r))?;
        self.window_rect_in_view(global)
    }

    /// Cocoa-global rect → this view (the view covers its whole screen).
    fn window_rect_in_view(&self, global: CGRect) -> Option<CGRect> {
        let sf = self.ivars().screen_frame.get();
        let local = CGRect::new(
            CGPoint::new(global.origin.x - sf.origin.x, global.origin.y - sf.origin.y),
            global.size,
        );
        let clipped = coordinates::intersect_rect(local, self.bounds());
        (clipped.size.width > 0.0 && clipped.size.height > 0.0).then_some(clipped)
    }

    fn overlay_window(&self) -> Option<Retained<OverlayWindow>> {
        let w = self.window()?;
        // SAFETY: the content view only ever lives in an OverlayWindow.
        Some(unsafe {
            Retained::retain(objc2::rc::Retained::as_ptr(&w) as *mut OverlayWindow)
        }?)
    }

    fn point_from(&self, event: &NSEvent) -> CGPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }

    fn point_clamped(&self, event: &NSEvent) -> CGPoint {
        let b = self.bounds();
        let p = self.point_from(event);
        CGPoint::new(
            p.x.min(b.size.width).max(0.0),
            p.y.min(b.size.height).max(0.0),
        )
    }
}

// MARK: OverlayView class

define_class!(
    // SAFETY:
    // - NSView subclass, main-thread only.
    // - Not flipped: the y-up NSView space + Cocoa-global math matches the
    //   Swift overlay exactly (the shelf's y-down flip does not apply here).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = OverlayViewIvars]
    pub struct OverlayView;

    unsafe impl NSObjectProtocol for OverlayView {}

    impl OverlayView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> objc2::runtime::Bool {
            true.into()
        }

        /// The very first press must start the drag even when macOS has not
        /// made us key yet.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: CGRect) {
            self.draw_contents();
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            if !self.held() {
                self.addCursorRect_cursor(self.bounds(), &objc2_app_kit::NSCursor::crosshairCursor());
                return;
            }
            if let Some(r) = self.held_rect() {
                for h in Self::handles(r) {
                    self.addCursorRect_cursor(
                        CGRect::new(
                            CGPoint::new(h.x - 8.0, h.y - 8.0),
                            CGSize::new(16.0, 16.0),
                        ),
                        &objc2_app_kit::NSCursor::arrowCursor(),
                    );
                }
            }
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            unsafe {
                let _: () = msg_send![super(self), updateTrackingAreas];
            }
            if let Some(t) = self.ivars().tracking.borrow_mut().take() {
                self.removeTrackingArea(&t);
            }
            let t = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    objc2::AllocAnyThread::alloc(),
                    self.bounds(),
                    NSTrackingAreaOptions::MouseMoved
                        | NSTrackingAreaOptions::MouseEnteredAndExited
                        | NSTrackingAreaOptions::ActiveAlways
                        | NSTrackingAreaOptions::InVisibleRect,
                    Some(&*(self as *const Self as *const AnyObject)),
                    None,
                )
            };
            self.addTrackingArea(&t);
            *self.ivars().tracking.borrow_mut() = Some(t);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.overlay_mouse_down(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.overlay_mouse_dragged(event);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.overlay_mouse_up(event);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, _event: &NSEvent) {
            if !self.held() {
                controller_finish_none();
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            if self.held() {
                return;
            }
            let p = self.point_from(event);
            if self.subviews().iter().any(|v| {
                v.isKindOfClass(<TopBar as objc2::ClassType>::class()) && coordinates::contains_pt(v.frame(), p)
            }) {
                objc2_app_kit::NSCursor::arrowCursor().set();
            } else {
                objc2_app_kit::NSCursor::crosshairCursor().set();
            }
            controller_update_hover(NSEvent::mouseLocation());
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            if !self.held() {
                objc2_app_kit::NSCursor::crosshairCursor().set();
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.overlay_key_down(event);
        }

        // ⎋ routed by NSApp even when nobody bound it.
        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            if !self.held() {
                controller_finish_none();
            } else if self.record_mode() {
                run_cancel_hook();
            }
        }
    }
);

impl OverlayView {
    /// `OverlayView.draw(_:)` — frozen backdrop, dim, punched hole, ring,
    /// handles, size badge.
    fn draw_contents(&self) {
        let bounds = self.bounds();
        let backdrop = self.ivars().backdrop.borrow().clone();
        if let Some(img) = &backdrop {
            if let Some(gc) = objc2_app_kit::NSGraphicsContext::currentContext() {
                let cg = gc.CGContext();
                objc2_core_graphics::CGContext::set_interpolation_quality(
                    Some(&cg),
                    objc2_core_graphics::CGInterpolationQuality::None,
                );
                objc2_core_graphics::CGContext::draw_image(Some(&cg), bounds, Some(img));
            }
        }
        objc2_app_kit::NSColor::colorWithCalibratedWhite_alpha(0.0, 0.5).setFill();
        objc2_app_kit::NSRectFill(bounds);
        let dragging = self.ivars().drag_start.get().is_some();
        let hole = if self.held() {
            self.held_rect()
        } else if dragging {
            self.selection_rect()
        } else {
            self.hovered_rect_in_view()
        };
        let Some(hole) = hole else { return };

        // Punch the hole in the dimming only: the frozen picture shows
        // through undimmed.
        if let Some(img) = &backdrop {
            objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
            objc2_app_kit::NSBezierPath::bezierPathWithRect(hole).addClip();
            if let Some(gc) = objc2_app_kit::NSGraphicsContext::currentContext() {
                let cg = gc.CGContext();
                objc2_core_graphics::CGContext::set_interpolation_quality(
                    Some(&cg),
                    objc2_core_graphics::CGInterpolationQuality::None,
                );
                objc2_core_graphics::CGContext::draw_image(Some(&cg), bounds, Some(img));
            }
            objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
        } else if let Some(gc) = objc2_app_kit::NSGraphicsContext::currentContext() {
            gc.setCompositingOperation(objc2_app_kit::NSCompositingOperation::Copy);
            objc2_app_kit::NSColor::clearColor().setFill();
            objc2_app_kit::NSRectFill(hole);
            gc.setCompositingOperation(objc2_app_kit::NSCompositingOperation::SourceOver);
        }

        // Square drop shadow outside the frame only (clipped away from the
        // hole), so the edge reads on white too.
        objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
        let outside = objc2_app_kit::NSBezierPath::bezierPathWithRect(bounds);
        outside.appendBezierPathWithRect(hole);
        outside.setWindingRule(objc2_app_kit::NSWindingRule::EvenOdd);
        outside.addClip();
        let shadow = objc2_app_kit::NSShadow::new();
        shadow.setShadowColor(Some(&objc2_app_kit::NSColor::colorWithCalibratedWhite_alpha(
            0.0, 0.55,
        )));
        shadow.setShadowBlurRadius(10.0);
        shadow.setShadowOffset(CGSize::ZERO);
        shadow.set();
        objc2_app_kit::NSColor::colorWithCalibratedWhite_alpha(0.0, 0.6).setFill();
        objc2_app_kit::NSBezierPath::bezierPathWithRect(coordinates::inset_rect(hole, -1.0, -1.0)).fill();
        objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();

        theme::paper_blue().setStroke();
        let ring = objc2_app_kit::NSBezierPath::bezierPathWithRect(coordinates::inset_rect(hole, -1.0, -1.0));
        ring.setLineWidth(2.0);
        ring.stroke();

        if self.held() {
            let s = 9.0; // OverlayView.handleSize
            for h in Self::handles(hole) {
                let sq = CGRect::new(
                    CGPoint::new(h.x - s / 2.0, h.y - s / 2.0),
                    CGSize::new(s, s),
                );
                theme::paper().setFill();
                objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    sq, 1.5, 1.5,
                )
                .fill();
                theme::ink().colorWithAlphaComponent(0.7).setStroke();
                let o = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    coordinates::inset_rect(sq, -0.5, -0.5),
                    2.0,
                    2.0,
                );
                o.setLineWidth(1.0);
                o.stroke();
            }
        }
        if self.held() || dragging {
            self.draw_badge(hole);
        }
    }

    /// "918 × 502" pill at the top-right, outside the frame when there is
    /// room (the hover-name branch of the Swift badge is dead upstream —
    /// `drawBadge` only runs when held/dragging).
    fn draw_badge(&self, rect: CGRect) {
        let s = self.ivars().backing_scale.get();
        let text = format!(
            "{} × {}",
            (rect.size.width * s).round() as i64,
            (rect.size.height * s).round() as i64
        );
        let font = theme::serif(13.0, false);
        let text_color = theme::on_brown();
        let attrs = crate::shelf::card::attrs(&font, &text_color, None);
        let text_ns = NSString::from_str(&text);
        let size = unsafe { text_ns.sizeWithAttributes(Some(&attrs)) };
        let pad = 9.0;
        let bounds = self.bounds();
        let mut box_x = (rect.origin.x + rect.size.width) - size.width - pad * 2.0;
        let mut box_y = rect.origin.y + rect.size.height + 10.0;
        let box_w = size.width + pad * 2.0;
        let box_h = size.height + 8.0;
        if box_y + box_h > bounds.origin.y + bounds.size.height - 4.0 {
            box_y = rect.origin.y + rect.size.height - box_h - 10.0;
        }
        box_x = box_x.max(4.0).min(bounds.origin.x + bounds.size.width - box_w - 4.0);
        let badge = CGRect::new(CGPoint::new(box_x, box_y), CGSize::new(box_w, box_h));
        theme::draw_desk(&objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            badge, 4.0, 4.0,
        ));
        unsafe {
            text_ns.drawAtPoint_withAttributes(
                CGPoint::new(badge.origin.x + pad, badge.origin.y + 4.0),
                Some(&attrs),
            );
        }
    }
}

// MARK: Chrome helpers (TopBar / RecordReadyBar)

/// Single-run attributed title for capture chrome buttons.
fn attr_title(s: &str, font: &objc2_app_kit::NSFont, color: &objc2_app_kit::NSColor) -> Retained<objc2_foundation::NSAttributedString> {
    let dict = crate::shelf::card::attrs(font, color, None);
    unsafe {
        objc2_foundation::NSAttributedString::initWithString_attributes(
            objc2::AllocAnyThread::alloc(),
            &NSString::from_str(s),
            Some(&dict),
        )
    }
}

/// `Theme.deskDivider(height:)` — 1pt onBrown 0.25 strip.
fn desk_divider(height: f64) -> Retained<NSView> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let v = NSView::new(mtm);
    v.setWantsLayer(true);
    if let Some(layer) = v.layer() {
        let c = theme::on_brown().colorWithAlphaComponent(0.25).CGColor();
        unsafe {
            let _: () = msg_send![&*layer, setBackgroundColor: &*c];
        }
    }
    v.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(1.0, height)));
    v
}

/// `Theme.paperButton` (M3 slice: fixed 34pt height, width = title + 40,
/// min 88).
fn paper_button(
    title: &str,
    primary: bool,
    on_ground: bool,
    target: Option<&AnyObject>,
    action: objc2::runtime::Sel,
) -> Retained<objc2_app_kit::NSButton> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let b = unsafe {
        objc2_app_kit::NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            target,
            Some(action),
            mtm,
        )
    };
    b.setBordered(false);
    let color = if primary {
        theme::ink()
    } else if on_ground {
        theme::on_brown()
    } else {
        theme::ink()
    };
    b.setAttributedTitle(&attr_title(title, &theme::serif(14.0, true), &color));
    b.setWantsLayer(true);
    if let Some(layer) = b.layer() {
        unsafe {
            let _: () = msg_send![&*layer, setCornerRadius: 17.0f64];
            if primary {
                let c = theme::paper_blue().CGColor();
                let _: () = msg_send![&*layer, setBackgroundColor: &*c];
                let _: () = msg_send![&*layer, setBorderWidth: 0.0f64];
            } else {
                let c = if on_ground {
                    theme::on_brown().colorWithAlphaComponent(0.45).CGColor()
                } else {
                    theme::ink().colorWithAlphaComponent(0.55).CGColor()
                };
                let _: () = msg_send![&*layer, setBorderWidth: 1.0f64];
                let _: () = msg_send![&*layer, setBorderColor: &*c];
            }
        }
    }
    let w = paper_button_width(title);
    b.setFrameSize(CGSize::new(w, 34.0));
    b
}

/// max(88, measured title width (serif 14 bold) + 40).
/// Public for other windows that need the same chrome buttons (OCR panel).
pub fn paper_button_export(
    title: &str,
    primary: bool,
    on_ground: bool,
    target: Option<&AnyObject>,
    action: objc2::runtime::Sel,
) -> Retained<objc2_app_kit::NSButton> {
    paper_button(title, primary, on_ground, target, action)
}

fn paper_button_width(title: &str) -> f64 {
    let font = theme::serif(14.0, true);
    let dict = crate::shelf::card::attrs(&font, &theme::ink(), None);
    let size = unsafe { NSString::from_str(title).sizeWithAttributes(Some(&dict)) };
    88.0f64.max((size.width + 40.0).ceil())
}

/// One line's width in a given font (TopBar fixed-geometry measuring).
fn text_width(s: &str, font: &objc2_app_kit::NSFont) -> f64 {
    let dict = crate::shelf::card::attrs(font, &theme::ink(), None);
    unsafe { NSString::from_str(s).sizeWithAttributes(Some(&dict)) }.width
}

// MARK: TopBar

pub struct TopBarIvars {
    shot: RefCell<Option<Retained<objc2_app_kit::NSButton>>>,
    rec: RefCell<Option<Retained<objc2_app_kit::NSButton>>>,
    wants_recording: Cell<bool>,
    /// During picking 录屏 only marks the intent; once the frame is held it
    /// switches the bars (the Swift `immediateRecord`).
    immediate_record: Cell<bool>,
    on_record: RefCell<Option<Box<dyn Fn() + 'static>>>,
    on_shot: RefCell<Option<Box<dyn Fn() + 'static>>>,
    on_close: RefCell<Option<Box<dyn Fn() + 'static>>>,
    drag_origin: Cell<Option<CGPoint>>,
}

impl Default for TopBarIvars {
    fn default() -> Self {
        Self {
            shot: RefCell::new(None),
            rec: RefCell::new(None),
            wants_recording: Cell::new(false),
            immediate_record: Cell::new(false),
            on_record: RefCell::new(None),
            on_shot: RefCell::new(None),
            on_close: RefCell::new(None),
            drag_origin: Cell::new(None),
        }
    }
}

define_class!(
    // SAFETY:
    // - Plain NSView with hand-placed children (no AutoLayout, like M2).
    // - All callbacks run on the main thread.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TopBarIvars]
    pub struct TopBar;

    unsafe impl NSObjectProtocol for TopBar {}

    impl TopBar {
        #[unsafe(method(pickShot:))]
        fn pick_shot(&self, _sender: &AnyObject) {
            self.style(false);
            self.ivars().wants_recording.set(false);
            if self.ivars().immediate_record.get() {
                if let Some(f) = self.ivars().on_shot.borrow().as_ref() {
                    f();
                }
            }
        }

        #[unsafe(method(pickRec:))]
        fn pick_rec(&self, _sender: &AnyObject) {
            self.style(true);
            self.ivars().wants_recording.set(true);
            if self.ivars().immediate_record.get() {
                if let Some(f) = self.ivars().on_record.borrow().as_ref() {
                    f();
                }
            }
        }

        #[unsafe(method(closeTapped:))]
        fn close_tapped(&self, _sender: &AnyObject) {
            if let Some(f) = self.ivars().on_close.borrow().as_ref() {
                f();
            }
        }

        // Drag the bar anywhere on its ground.
        #[unsafe(method(mouseDown:))]
        fn tb_mouse_down(&self, event: &NSEvent) {
            self.ivars()
                .drag_origin
                .set(Some(self.convertPoint_fromView(event.locationInWindow(), None)));
        }

        #[unsafe(method(mouseDragged:))]
        fn tb_mouse_dragged(&self, event: &NSEvent) {
            let Some(o) = self.ivars().drag_origin.get() else { return };
            let Some(host) = (unsafe { self.superview() }) else { return };
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let hb = host.bounds();
            let mut f = self.frame();
            f.origin.x += p.x - o.x;
            f.origin.y += p.y - o.y;
            f.origin.x = f.origin.x.max(hb.min().x).min(hb.max().x - f.size.width);
            f.origin.y = f.origin.y.max(hb.min().y).min(hb.max().y - f.size.height);
            self.setFrame(f);
        }

        #[unsafe(method(mouseUp:))]
        fn tb_mouse_up(&self, _event: &NSEvent) {
            self.ivars().drag_origin.set(None);
        }

        #[unsafe(method(mouseEntered:))]
        fn tb_mouse_entered(&self, _event: &NSEvent) {
            objc2_app_kit::NSCursor::arrowCursor().set();
        }

        #[unsafe(method(mouseExited:))]
        fn tb_mouse_exited(&self, _event: &NSEvent) {
            if !self.ivars().immediate_record.get() {
                objc2_app_kit::NSCursor::crosshairCursor().set();
            }
        }

        #[unsafe(method(resetCursorRects))]
        fn tb_reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &objc2_app_kit::NSCursor::arrowCursor());
        }

        #[unsafe(method(updateTrackingAreas))]
        fn tb_update_tracking_areas(&self) {
            unsafe {
                let _: () = msg_send![super(self), updateTrackingAreas];
            }
            for t in self.trackingAreas().iter() {
                self.removeTrackingArea(&t);
            }
            let t = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    objc2::AllocAnyThread::alloc(),
                    self.bounds(),
                    NSTrackingAreaOptions::MouseEnteredAndExited
                        | NSTrackingAreaOptions::ActiveAlways
                        | NSTrackingAreaOptions::InVisibleRect,
                    Some(&*(self as *const Self as *const AnyObject)),
                    None,
                )
            };
            self.addTrackingArea(&t);
        }

        #[unsafe(method(drawRect:))]
        fn tb_draw_rect(&self, _dirty: CGRect) {
            theme::draw_desk(
                &objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    coordinates::inset_rect(self.bounds(), 0.5, 0.5),
                    6.0,
                    6.0,
                ),
            );
        }
    }
);

impl TopBar {
    /// `TopBar.init()` — brand + [截屏|录屏] segmented + divider + ✕,
    /// hand-placed at the Swift coordinates (height 52).
    fn new_bar() -> Retained<TopBar> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<TopBar>().set_ivars(TopBarIvars::default());
        let bar: Retained<TopBar> = unsafe { msg_send![super(this), init] };
        theme::paper_sheet(&bar, 6.0);

        let font_name = theme::brand_font(19.0);
        let name_w = text_width("Pastory", &font_name).ceil();
        let seg_font = theme::serif(14.0, true);
        let widest = text_width(&l("截屏"), &seg_font)
            .max(text_width(&l("录屏"), &seg_font));
        let btn_w = (widest + 44.0).ceil();
        let seg_w = 3.0 + btn_w + 3.0 + btn_w + 3.0;
        let total_w = (18.0 + (30.0 + 9.0 + name_w) + 22.0 + seg_w + 14.0 + 1.0 + 14.0 + 40.0 + 12.0).ceil();
        let h = 52.0;
        bar.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(total_w, h)));

        // [logo] Pastory
        if let Some(logo_img) = theme::logo() {
            let logo_view = objc2_app_kit::NSImageView::new(mtm);
            logo_view.setImage(Some(&logo_img));
            logo_view.setImageScaling(objc2_app_kit::NSImageScaling::ScaleProportionallyUpOrDown);
            logo_view.setFrame(CGRect::new(CGPoint::new(18.0, (h - 30.0) / 2.0), CGSize::new(30.0, 30.0)));
            bar.addSubview(&logo_view);
        }
        let name = make_label(&l_nonl8("Pastory"));
        name.setFont(Some(&font_name));
        name.setTextColor(Some(&theme::on_brown()));
        let name_h = crate::shelf::card::line_height(&font_name);
        name.setFrame(CGRect::new(
            CGPoint::new(18.0 + 30.0 + 9.0, (h - name_h) / 2.0 - 1.0),
            CGSize::new(name_w + 2.0, name_h),
        ));
        bar.addSubview(&name);

        // [ 截屏 | 录屏 ]
        let seg = NSView::new(mtm);
        seg.setWantsLayer(true);
        if let Some(layer) = seg.layer() {
            let border = theme::on_brown().colorWithAlphaComponent(0.35).CGColor();
            unsafe {
                let _: () = msg_send![&*layer, setBorderWidth: 1.0f64];
                let _: () = msg_send![&*layer, setBorderColor: &*border];
                let _: () = msg_send![&*layer, setCornerRadius: 9.0f64];
            }
        }
        let seg_x = 18.0 + 30.0 + 9.0 + name_w + 22.0;
        seg.setFrame(CGRect::new(
            CGPoint::new(seg_x, (h - 34.0) / 2.0),
            CGSize::new(seg_w, 34.0),
        ));
        bar.addSubview(&seg);

        // SAFETY: the bar outlives its buttons (children live in the same
        // window); the usual weak-target dance is unnecessary.
        let bar_obj: &AnyObject = unsafe { &*(&*bar as *const TopBar as *const AnyObject) };
        let target: Option<&AnyObject> = Some(bar_obj);
        let shot = make_seg_button(&l("截屏"), "viewfinder", target, sel!(pickShot:));
        let rec = make_seg_button(&l("录屏"), "camera", target, sel!(pickRec:));
        shot.setFrame(CGRect::new(CGPoint::new(3.0, 3.0), CGSize::new(btn_w, 28.0)));
        rec.setFrame(CGRect::new(
            CGPoint::new(3.0 + btn_w + 3.0, 3.0),
            CGSize::new(btn_w, 28.0),
        ));
        seg.addSubview(&shot);
        seg.addSubview(&rec);
        *bar.ivars().shot.borrow_mut() = Some(shot);
        *bar.ivars().rec.borrow_mut() = Some(rec);

        let divider = desk_divider(28.0);
        divider.setFrameOrigin(CGPoint::new(seg_x + seg_w + 14.0, (h - 28.0) / 2.0));
        bar.addSubview(&divider);

        let close = symbol_button("xmark", 15.0, &l("取消 ⎋"), target, sel!(closeTapped:));
        close.setToolTip(Some(&NSString::from_str(&l("取消 ⎋"))));
        close.setFrame(CGRect::new(
            CGPoint::new(seg_x + seg_w + 14.0 + 1.0 + 14.0, (h - 40.0) / 2.0),
            CGSize::new(40.0, 40.0),
        ));
        bar.addSubview(&close);

        bar.style(false);
        bar
    }

    fn wants_recording(&self) -> bool {
        self.ivars().wants_recording.get()
    }

    fn set_immediate_record(&self, on: bool) {
        self.ivars().immediate_record.set(on);
    }

    fn set_on_record(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_record.borrow_mut() = Some(f);
    }
    fn set_on_shot(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_shot.borrow_mut() = Some(f);
    }
    fn set_on_close(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_close.borrow_mut() = Some(f);
    }

    /// `TopBar.style(active:)` — paper-blue chip on the active segment.
    fn style(&self, record_active: bool) {
        for (b, on) in [
            (self.ivars().shot.borrow().clone(), !record_active),
            (self.ivars().rec.borrow().clone(), record_active),
        ] {
            let Some(b) = b else { continue };
            if let Some(layer) = b.layer() {
                unsafe {
                    if on {
                        let c = theme::paper_blue().CGColor();
                        let _: () = msg_send![&*layer, setBackgroundColor: &*c];
                    } else {
                        let none: Option<&objc2_core_graphics::CGColor> = None;
                        let _: () = msg_send![&*layer, setBackgroundColor: none];
                    }
                }
            }
            let tint = if on { theme::ink() } else { theme::on_brown() };
            b.setContentTintColor(Some(&tint));
            let title = b.title().to_string();
            let trimmed = title.trim().to_string();
            let title_color = if on { theme::ink() } else { theme::on_brown() };
            b.setAttributedTitle(&attr_title(
                &format!(" {trimmed}"),
                &theme::serif(14.0, on),
                &title_color,
            ));
        }
    }
}

/// An unbordered segment button with an SF-Symbol image left of the title.
fn make_seg_button(
    title: &str,
    symbol: &str,
    target: Option<&AnyObject>,
    action: objc2::runtime::Sel,
) -> Retained<objc2_app_kit::NSButton> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let b = unsafe {
        objc2_app_kit::NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            target,
            Some(action),
            mtm,
        )
    };
    b.setBordered(false);
    b.setWantsLayer(true);
    if let Some(layer) = b.layer() {
        unsafe {
            let _: () = msg_send![&*layer, setCornerRadius: 8.0f64];
        }
    }
    if let Some(img) = objc2_app_kit::NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        None,
    ) {
        let cfg = objc2_app_kit::NSImageSymbolConfiguration::configurationWithPointSize_weight(
            12.0,
            unsafe { objc2_app_kit::NSFontWeightMedium },
        );
        if let Some(configured) = img.imageWithSymbolConfiguration(&cfg) {
            b.setImage(Some(&configured));
        }
    }
    b.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageLeading);
    b.setImageHugsTitle(true);
    b
}

/// An unbordered symbol-only button (the ✕).
fn symbol_button(
    symbol: &str,
    point: f64,
    tooltip: &str,
    target: Option<&AnyObject>,
    action: objc2::runtime::Sel,
) -> Retained<objc2_app_kit::NSButton> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let img = objc2_app_kit::NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(&NSString::from_str(tooltip)),
    )
    .expect("symbol exists");
    let cfg = objc2_app_kit::NSImageSymbolConfiguration::configurationWithPointSize_weight(
        point,
        unsafe { objc2_app_kit::NSFontWeightSemibold },
    );
    let img = img.imageWithSymbolConfiguration(&cfg).unwrap_or(img);
    let b = unsafe {
        objc2_app_kit::NSButton::buttonWithImage_target_action(&img, target, Some(action), mtm)
    };
    b.setBordered(false);
    b.setContentTintColor(Some(&theme::on_brown()));
    b
}

/// A label-styled NSTextField (no bezel, no background, not editable).
fn make_label(s: &str) -> Retained<objc2_app_kit::NSTextField> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let f = objc2_app_kit::NSTextField::labelWithString(&NSString::from_str(s), mtm);
    f
}

/// Placeholder never rendered: brand name has no localization entry.
fn l_nonl8(s: &str) -> String {
    s.to_string()
}

// MARK: RecordReadyBar

pub struct RecordReadyBarIvars {
    on_start: RefCell<Option<Box<dyn Fn() + 'static>>>,
    on_cancel: RefCell<Option<Box<dyn Fn() + 'static>>>,
}

impl Default for RecordReadyBarIvars {
    fn default() -> Self {
        Self {
            on_start: RefCell::new(None),
            on_cancel: RefCell::new(None),
        }
    }
}

define_class!(
    // SAFETY:
    // - Plain NSView, hand-placed children, main thread.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RecordReadyBarIvars]
    pub struct RecordReadyBar;

    unsafe impl NSObjectProtocol for RecordReadyBar {}

    impl RecordReadyBar {
        #[unsafe(method(startTapped:))]
        fn start_tapped(&self, _sender: &AnyObject) {
            if let Some(f) = self.ivars().on_start.borrow().as_ref() {
                f();
            }
        }

        #[unsafe(method(cancelTapped:))]
        fn cancel_tapped(&self, _sender: &AnyObject) {
            if let Some(f) = self.ivars().on_cancel.borrow().as_ref() {
                f();
            }
        }

        #[unsafe(method(resetCursorRects))]
        fn rb_reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &objc2_app_kit::NSCursor::arrowCursor());
        }

        #[unsafe(method(drawRect:))]
        fn rb_draw_rect(&self, _dirty: CGRect) {
            theme::draw_desk(
                &objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    coordinates::inset_rect(self.bounds(), 0.5, 0.5),
                    6.0,
                    6.0,
                ),
            );
        }
    }
);

impl RecordReadyBar {
    /// `RecordReadyBar.init()` — hint · divider · 取消 · 开始录制 ⏎, height
    /// 56 (edge insets: left 16, right 10, spacing 12).
    fn new_bar() -> Retained<RecordReadyBar> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<RecordReadyBar>().set_ivars(RecordReadyBarIvars::default());
        let bar: Retained<RecordReadyBar> = unsafe { msg_send![super(this), init] };
        let h = 56.0;
        let hint_text = l("拖动边框调整录制范围");
        let hint_w = text_width(&hint_text, &theme::serif(13.0, false)).ceil() + 2.0;
        let cancel_w = paper_button_width(&l("取消"));
        let start_w = paper_button_width(&l("开始录制 ⏎"));
        let total_w = 16.0 + hint_w + 12.0 + 1.0 + 12.0 + cancel_w + 12.0 + start_w + 10.0;
        bar.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(total_w.ceil(), h)));

        let hint = make_label(&hint_text);
        hint.setFont(Some(&theme::serif(13.0, false)));
        hint.setTextColor(Some(&theme::on_brown_muted()));
        let hint_h = crate::shelf::card::line_height(&theme::serif(13.0, false));
        hint.setFrame(CGRect::new(
            CGPoint::new(16.0, (h - hint_h) / 2.0 - 1.0),
            CGSize::new(hint_w, hint_h),
        ));
        bar.addSubview(&hint);

        let divider = desk_divider(24.0);
        divider.setFrameOrigin(CGPoint::new(16.0 + hint_w + 12.0, (h - 24.0) / 2.0));
        bar.addSubview(&divider);

        // SAFETY: same lifetime shape as the TopBar targets.
        let bar_obj: &AnyObject = unsafe { &*(&*bar as *const RecordReadyBar as *const AnyObject) };
        let target = Some(bar_obj);

        let cancel = paper_button(&l("取消"), false, true, target, sel!(cancelTapped:));
        cancel.setFrameOrigin(CGPoint::new(16.0 + hint_w + 12.0 + 1.0 + 12.0, (h - 34.0) / 2.0));
        bar.addSubview(&cancel);

        let start = paper_button(&l("开始录制 ⏎"), true, true, target, sel!(startTapped:));
        start.setFrameOrigin(CGPoint::new(
            16.0 + hint_w + 12.0 + 1.0 + 12.0 + cancel_w + 12.0,
            (h - 34.0) / 2.0,
        ));
        bar.addSubview(&start);
        bar
    }

    fn set_on_start(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_start.borrow_mut() = Some(f);
    }
    fn set_on_cancel(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_cancel.borrow_mut() = Some(f);
    }
}

// MARK: Controller

/// Plain view construction (`OverlayView(frame:)`).
fn make_overlay_view(mtm: MainThreadMarker, size: CGSize) -> Retained<OverlayView> {
    let this = mtm.alloc::<OverlayView>().set_ivars(OverlayViewIvars::default());
    unsafe {
        msg_send![
            super(this),
            initWithFrame: CGRect::new(CGPoint::ZERO, size)
        ]
    }
}

/// `ownWindowIDs` — the mask windows, so captures can leave out exactly
/// these and nothing else.
pub fn own_window_ids() -> Vec<u32> {
    controller()
        .overlays
        .borrow()
        .iter()
        .map(|w| w.windowNumber() as u32)
        .collect()
}

fn held_window() -> Option<Retained<OverlayWindow>> {
    controller()
        .overlays
        .borrow()
        .iter()
        .find(|w| w.overlay_view().held_rect().is_some())
        .cloned()
}

/// `heldDisplay` — only after a pick.
pub fn held_display() -> Option<Retained<SCDisplay>> {
    held_window()?.display()
}

/// `heldScreenSize`.
pub fn held_screen_size() -> Option<CGSize> {
    held_window().map(|w| w.screen_frame().size)
}

/// `heldDisplayLocalRect` — top-left points inside the held display.
pub fn held_display_local_rect() -> Option<CGRect> {
    let w = held_window()?;
    let r = w.overlay_view().held_rect()?;
    Some(coordinates::display_local_rect(
        r,
        w.screen_frame().size.height,
    ))
}

/// `heldScreenRect` — screen-space rect of the held selection (Cocoa global).
pub fn held_screen_rect() -> Option<CGRect> {
    let w = held_window()?;
    let r = w.overlay_view().held_rect()?;
    Some(w.convertRectToScreen(r))
}

/// `wantsRecording` — 录屏 chosen on the bar before the selection was made.
pub fn wants_recording() -> bool {
    controller()
        .top_bar
        .borrow()
        .as_ref()
        .map(|t| t.wants_recording())
        .unwrap_or(false)
}

pub fn is_presenting() -> bool {
    controller().is_presenting.get()
}

/// The hooks the coordinator installs before the pick completes:
/// `commit` = 截图完成（crop → 复制+入库）, `record` = 开始录制 (M5 anchor),
/// `cancel` = overall cancel.
pub fn set_capture_hooks(
    commit: Box<dyn Fn() + 'static>,
    record: Box<dyn Fn() + 'static>,
    cancel: Box<dyn Fn() + 'static>,
) {
    let c = controller();
    *c.commit_action.borrow_mut() = Some(commit);
    *c.record_action.borrow_mut() = Some(record);
    *c.cancel_action.borrow_mut() = Some(cancel);
}

fn run_commit_hook() {
    if let Some(f) = controller().commit_action.borrow().as_ref() {
        f();
    }
}
fn run_record_hook() {
    if let Some(f) = controller().record_action.borrow().as_ref() {
        f();
    }
}
fn run_cancel_hook() {
    if let Some(f) = controller().cancel_action.borrow().as_ref() {
        f();
    }
}

/// `SelectionOverlayController.present(snapshot:mode:frozen:completion:)`.
/// Keys arrive through Carbon hooks while picking (the overlay is not the
/// front app's key window until the picture under the cursor is frozen).
pub fn present(
    snapshot: &ShareableSnapshot,
    mode: PickMode,
    frozen: Vec<(u32, CFRetained<CGImage>)>,
    completion: Box<dyn FnOnce(Option<CaptureTarget>) + 'static>,
) {
    let c = controller();
    if c.is_presenting.get() || !c.overlays.borrow().is_empty() {
        release(true);
    }
    let c = controller();
    c.is_presenting.set(true);
    bind_picker_hooks();
    *c.completion.borrow_mut() = Some(completion);
    c.mode.set(mode);
    *c.hovered_window.borrow_mut() = None;

    let also: Vec<u32> = crate::shelf::panel::window_id().into_iter().collect();
    let pickable = snapshot.pickable_windows(&also);
    let mtm = MainThreadMarker::new().expect("main thread");
    for screen_iter in NSScreen::screens(mtm).iter() {
        let Some(display) = snapshot.display_for_screen(&screen_iter) else {
            continue;
        };
        let w = make_overlay_window(&screen_iter, &display);
        let view = w.overlay_view();
        let display_id = unsafe { display.displayID() };
        view.set_backdrop(
            frozen
                .iter()
                .find(|(id, _)| *id == display_id)
                .map(|(_, img)| img.clone()),
        );
        view.set_screen_frame(screen_iter.frame());
        view.set_backing_scale(screen_iter.backingScaleFactor());
        view.set_candidates(
            pickable
                .iter()
                .map(|w| {
                    let frame = unsafe { w.frame() };
                    (w.clone(), coordinates::cocoa_rect_from_cg(frame))
                })
                .collect(),
        );
        c.overlays.borrow_mut().push(w);
    }
    for w in c.overlays.borrow().iter() {
        w.orderFrontRegardless();
    }
    let mouse = NSEvent::mouseLocation();
    // The screen under the pointer was photographed before this point, so
    // whatever the app in front closes when it loses focus is already in the
    // picture. Becoming the active app is what makes the crosshair possible:
    // macOS ignores cursor changes from background apps. Focus goes back to
    // that app in release().
    let ws = NSWorkspace::sharedWorkspace();
    if let Some(front) = ws.frontmostApplication() {
        if front.processIdentifier() != std::process::id() as i32 {
            *c.app_to_restore.borrow_mut() = Some(front);
        }
    }
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    unsafe {
        // SAFETY: modern activation first; the legacy call is the fallback.
        let _: () = msg_send![&app, activate];
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let host = {
        let borrowed = c.overlays.borrow();
        borrowed
            .iter()
            .find(|w| coordinates::contains_pt(w.screen_frame(), mouse))
            .or_else(|| borrowed.first())
            .cloned()
    };
    if let Some(h) = &host {
        unsafe {
            // SAFETY: msg_send for the uncovered `makeKey`.
            let _: () = msg_send![&**h, makeKey];
        }
    }
    for w in c.overlays.borrow().iter() {
        w.invalidateCursorRectsForView(&w.overlay_view());
    }
    // The 截屏 / 录屏 bar is up from the first frame, on the screen under
    // the pointer.
    if let Some(host) = host {
        let top = TopBar::new_bar();
        top.set_on_close(Box::new(controller_finish_none));
        let size = top.frame().size;
        let b = host.overlay_view().bounds();
        top.setFrame(CGRect::new(
            CGPoint::new(
                ((b.min().x + b.size.width / 2.0) - size.width / 2.0).round(),
                b.max().y - 28.0 - size.height,
            ),
            size,
        ));
        host.overlay_view().addSubview(&top);
        *c.top_bar.borrow_mut() = Some(top);
    }
    update_hover(mouse);
    objc2_app_kit::NSCursor::crosshairCursor().set();
    refresh_all();
}

/// Selftest (`renderAnnotate`): a plain overlay view carrying no controller
/// state (the Swift SelfTest constructs OverlayView standalone the same way).
pub fn debug_overlay_view(size: CGSize, screen_frame: CGRect) -> Retained<OverlayView> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let view = make_overlay_view(mtm, size);
    view.set_screen_frame(screen_frame);
    view.set_backing_scale(
        NSScreen::mainScreen(mtm)
            .map(|s| s.backingScaleFactor())
            .unwrap_or(1.0),
    );
    view
}

/// Selftest: the brand bar without a session (Swift `TopBar()`).
pub fn debug_top_bar() -> Retained<TopBar> {
    TopBar::new_bar()
}

const PICKER_HOOKS: &[&str] = &[
    "picker.esc",
    "picker.space",
    "picker.f",
    "picker.return",
    "picker.enter",
];

fn bind_picker_hooks() {
    let hk = HotKeyCenter::shared();
    hk.bind_raw(53, 0, "picker.esc", Box::new(|| {
        crate::capture::coordinator::cancel();
    }));
    hk.bind_raw(49, 0, "picker.space", Box::new(controller_toggle_mode));
    for (code, name) in [(3u32, "picker.f"), (36, "picker.return"), (76, "picker.enter")] {
        hk.bind_raw(code, 0, name, Box::new(controller_pick_whole_screen));
    }
}

/// Unbound the moment the frame is held (`unbindPickerHooks`).
pub fn unbind_picker_hooks() {
    for name in PICKER_HOOKS {
        HotKeyCenter::shared().unbind(name);
    }
}

/// `pickWholeScreen` — F / ⏎ during picking: the display under the pointer.
pub fn controller_pick_whole_screen() {
    let c = controller();
    if !c.is_presenting.get() {
        return;
    }
    let w = {
        let borrowed = c.overlays.borrow();
        let mouse = NSEvent::mouseLocation();
        borrowed
            .iter()
            .find(|w| coordinates::contains_pt(w.screen_frame(), mouse))
            .or_else(|| borrowed.first())
            .cloned()
    };
    let Some(w) = w else { return };
    let Some(display) = w.display() else { return };
    finish(
        Some(CaptureTarget::Display(display)),
        Some(w.overlay_view().bounds()),
        Some(&w),
    );
}

/// `finish(nil)` from views/menus (`cancelOperation`, right-click, ✕).
pub fn controller_finish_none() {
    finish(None, None, None);
}

pub fn controller_toggle_mode() {
    let c = controller();
    c.mode.set(match c.mode.get() {
        PickMode::Region => PickMode::Window,
        PickMode::Window => PickMode::Region,
    });
    *c.hovered_window.borrow_mut() = None;
    update_hover(NSEvent::mouseLocation());
    refresh_all();
}

/// The window under the pointer is lit in both modes (`updateHover`): in
/// region mode a plain click takes it, a drag takes a region.
pub fn update_hover(p: CGPoint) {
    let c = controller();
    let Some(first) = c.overlays.borrow().first().cloned() else {
        return;
    };
    let hit = first
        .overlay_view()
        .ivars()
        .candidates
        .borrow()
        .iter()
        .find(|(_, r)| coordinates::contains_pt(*r, p))
        .map(|(w, _)| w.clone());
    let changed = match (&*c.hovered_window.borrow(), &hit) {
        (Some(a), Some(b)) => unsafe { a.windowID() != b.windowID() },
        (None, None) => false,
        _ => true,
    };
    if changed {
        *c.hovered_window.borrow_mut() = hit;
        refresh_all();
    }
}

pub fn refresh_all() {
    for w in controller().overlays.borrow().iter() {
        w.overlay_view().setNeedsDisplay(true);
    }
}

fn controller_update_hover(p: CGPoint) {
    update_hover(p);
}

/// `finish(_:viewRect:on:)` — a target was picked on `window`; `view_rect`
/// is where it sits in that overlay (nil = whole screen on that overlay).
pub fn finish(
    target: Option<CaptureTarget>,
    view_rect: Option<CGRect>,
    on_window: Option<&OverlayWindow>,
) {
    let c = controller();
    if !c.is_presenting.get() {
        return;
    }
    c.is_presenting.set(false);
    let done = c.completion.borrow_mut().take();
    let Some(target) = target else {
        release(true);
        if let Some(done) = done {
            done(None);
        }
        return;
    };
    let Some(done) = done else { return };
    for o in c.overlays.borrow().iter() {
        let view = o.overlay_view();
        view.set_held(true);
        let same = on_window
            .map(|w| w.windowNumber() == o.windowNumber())
            .unwrap_or(false);
        view.set_held_rect(if same {
            Some(view_rect.unwrap_or_else(|| view.bounds()))
        } else {
            None
        });
        view.setNeedsDisplay(true);
        o.invalidateCursorRectsForView(&view);
    }
    done(Some(target));
}

/// Place the brand bar and the annotation canvas over the frozen selection
/// (`showAnnotator`). The toolbar follows; the top bar that has been up
/// since the picker opened is re-added so it sits above the canvas.
pub fn show_annotator(
    image: CFRetained<CGImage>,
    delegate: Box<dyn crate::annotate::annotate_view::AnnotateDelegate>,
) {
    let c = controller();
    let Some(win) = held_window() else { return };
    let Some(rect) = win.overlay_view().held_rect() else { return };
    for o in c.overlays.borrow().iter() {
        if o.windowNumber() != win.windowNumber() {
            o.setIgnoresMouseEvents(true);
        }
    }
    let view = win.overlay_view();
    let canvas = crate::annotate::annotate_view::AnnotateView::make(rect, image);
    canvas.set_delegate(delegate);
    view.addSubview(&canvas);
    let bar = crate::annotate::toolbar::AnnotateToolbar::make(&canvas, &l("复制"));
    view.addSubview(&bar);
    // Keep the bar that has been up since the picker opened; re-add so it
    // sits above the canvas that was just added.
    let top = c.top_bar.borrow().clone();
    if let Some(top) = top {
        top.removeFromSuperview();
        view.addSubview(&top);
        top.set_immediate_record(true);
        top.set_on_record(Box::new(enter_record_mode));
        top.set_on_shot(Box::new(leave_record_mode));
        top.set_on_close(Box::new(move || {
            if let Some(cv) = controller().annotator.borrow().clone() {
                cv.cancel();
            }
        }));
        *c.top_bar.borrow_mut() = Some(top);
    }
    *c.annotator.borrow_mut() = Some(canvas.clone());
    *c.toolbar.borrow_mut() = Some(bar);
    // 录屏 was chosen before the pick: land in the record-ready frame.
    if wants_recording() {
        enter_record_mode();
    }
    // The picture is taken; from here the canvas owns the keyboard (⎋
    // deselects, leaves the text box, then cancels).
    unbind_picker_hooks();
    layout_chrome();
    objc2_app_kit::NSCursor::arrowCursor().set();
    let sender: Option<&AnyObject> = None;
    win.makeKeyAndOrderFront(sender);
    win.makeFirstResponder(Some(&canvas));
}

/// 录屏 on the top bar: keep the frame and its handles, hide the annotation
/// tools, offer 开始录制 (`enterRecordMode`).
fn enter_record_mode() {
    let c = controller();
    let Some(canvas) = c.annotator.borrow().clone() else { return };
    if c.record_bar.borrow().is_some() {
        return;
    }
    canvas.set_record_mode(true);
    if let Some(t) = c.toolbar.borrow().as_ref() {
        t.setHidden(true);
        t.hide_sub_bar();
    }
    let Some(win) = held_window() else { return };
    let bar = RecordReadyBar::new_bar();
    bar.set_on_start(Box::new(|| {
        if let Some(cv) = controller().annotator.borrow().clone() {
            cv.request_record();
        }
    }));
    bar.set_on_cancel(Box::new(|| {
        if let Some(cv) = controller().annotator.borrow().clone() {
            cv.cancel();
        }
    }));
    win.overlay_view().addSubview(&bar);
    *c.record_bar.borrow_mut() = Some(bar);
    layout_chrome();
}

/// 截屏 on the top bar: back to the annotation tools (`leaveRecordMode`).
fn leave_record_mode() {
    let c = controller();
    let Some(canvas) = c.annotator.borrow().clone() else { return };
    if let Some(rb) = c.record_bar.borrow_mut().take() {
        rb.removeFromSuperview();
    }
    canvas.set_record_mode(false);
    if let Some(t) = c.toolbar.borrow().as_ref() {
        t.setHidden(false);
        t.refresh();
    }
    layout_chrome();
}

// MARK: Layout / region / release

/// `toolbarFrame(for:size:in:)` — below the frame, else above, else inside.
fn toolbar_frame(rect: CGRect, size: CGSize, bounds: CGRect) -> CGRect {
    let gap = 12.0;
    let mut y = rect.min().y - size.height - gap;
    if y < bounds.min().y + 4.0 {
        y = rect.max().y + gap;
        if y + size.height > bounds.max().y - 4.0 {
            y = rect.min().y + gap;
        }
    }
    let mut x = (rect.min().x + rect.size.width / 2.0) - size.width / 2.0;
    x = x.max(bounds.min().x + 4.0).min(bounds.max().x - size.width - 4.0);
    CGRect::new(CGPoint::new(x.round(), y.round()), size)
}

/// Brand bar: top middle, flipped to the bottom when it would cover the
/// frame, nudged clear of the record bar (`layoutChrome`).
fn layout_chrome() {
    let c = controller();
    let Some(win) = held_window() else { return };
    let Some(rect) = win.overlay_view().held_rect() else { return };
    let bounds = win.overlay_view().bounds();
    if let Some(tb) = c.toolbar.borrow().as_ref() {
        let size = tb.fitting();
        tb.setFrame(toolbar_frame(rect, size, bounds));
        tb.did_layout();
    }
    if let Some(rb) = c.record_bar.borrow().as_ref() {
        let size = rb.frame().size;
        rb.setFrame(toolbar_frame(rect, size, bounds));
    }
    if let Some(top) = c.top_bar.borrow().as_ref() {
        let size = top.frame().size;
        let mut f = CGRect::new(
            CGPoint::new(
                ((bounds.min().x + bounds.size.width / 2.0) - size.width / 2.0).round(),
                bounds.max().y - 28.0 - size.height,
            ),
            size,
        );
        if coordinates::intersects_rect(f, coordinates::inset_rect(rect, -8.0, -8.0)) {
            f.origin.y = bounds.min().y + 28.0;
        }
        // Swift `below`: the record bar when up, otherwise the toolbar.
        let lower = c
            .record_bar
            .borrow()
            .as_ref()
            .map(|rb| rb.frame())
            .or_else(|| c.toolbar.borrow().as_ref().map(|t| t.frame()));
        let hits_lower = lower
            .map(|b| coordinates::intersects_rect(f, b))
            .unwrap_or(false);
        if hits_lower {
            f.origin.y = bounds.max().y - 28.0 - size.height;
        }
        top.setFrame(f);
    }
}

/// Frame handle dragged (`regionChanged`): re-crop and re-flow the chrome.
pub fn region_changed(view_rect: CGRect) {
    let c = controller();
    let Some(win) = held_window() else { return };
    let view = win.overlay_view();
    view.set_held_rect(Some(view_rect));
    view.setNeedsDisplay(true);
    let canvas = c.annotator.borrow().clone();
    if let Some(canvas) = canvas {
        let local = coordinates::display_local_rect(view_rect, win.screen_frame().size.height);
        if let Some(provider) = c.crop_provider.borrow().as_ref() {
            if let Some(img) = provider(local) {
                canvas.replace_image(img, view_rect);
            }
        }
    }
    layout_chrome();
}

/// The coordinator hands over its crop closure (`cropProvider`).
pub fn set_crop_provider(provider: Box<dyn Fn(CGRect) -> Option<CFRetained<CGImage>> + 'static>) {
    *controller().crop_provider.borrow_mut() = Some(provider);
}

/// `moveRegion(dx:dy:)` — slide the whole selection, kept inside the screen
/// (M4's arrow-key path; ported now because the math is picky: round the
/// origin only or the frame grows a pixel per event).
pub fn move_region(dx: f64, dy: f64) {
    let Some(win) = held_window() else { return };
    let view = win.overlay_view();
    let Some(r) = view.held_rect() else { return };
    let b = view.bounds();
    let mut x = r.origin.x + dx;
    let mut y = r.origin.y + dy;
    x = x.max(b.min().x).min(b.max().x - r.size.width);
    y = y.max(b.min().y).min(b.max().y - r.size.height);
    region_changed(CGRect::new(CGPoint::new(x.round(), y.round()), r.size));
}

pub fn region_commit() {
    if let Some(win) = held_window() {
        win.invalidateCursorRectsForView(&win.overlay_view());
    }
}

/// A picked window is captured live: give the front app its focus back
/// first, so it is drawn active (`reactivateFrontApp`).
pub fn reactivate_front_app() {
    let Some(app) = controller().app_to_restore.borrow().clone() else {
        return;
    };
    if !app.isTerminated() {
        app.activateWithOptions(
            objc2_app_kit::NSApplicationActivationOptions::ActivateAllWindows
        );
    }
}

/// Tear everything down (`release(restoreFocus:)`). `restore_focus: false`
/// is the restart path (hotkey pressed again mid-capture): the picker comes
/// straight back and the app to return to must survive until then.
pub fn release(restore_focus: bool) {
    let c = controller();
    c.is_presenting.set(false);
    *c.completion.borrow_mut() = None;
    unbind_picker_hooks();
    objc2_app_kit::NSCursor::arrowCursor().set();
    if let Some(rb) = c.record_bar.borrow_mut().take() {
        rb.removeFromSuperview();
    }
    if let Some(t) = c.toolbar.borrow_mut().take() {
        t.remove_self_and_sub();
    }
    if let Some(a) = c.annotator.borrow_mut().take() {
        a.removeFromSuperview();
    }
    if let Some(t) = c.top_bar.borrow_mut().take() {
        t.removeFromSuperview();
    }
    *c.crop_provider.borrow_mut() = None;
    for w in c.overlays.borrow_mut().drain(..) {
        let sender: Option<&AnyObject> = None;
        w.orderOut(sender);
        w.close();
    }
    if !restore_focus {
        return;
    }
    // Hand the keyboard back, unless one of our own titled windows (image
    // editor, text editor, recording preview, OCR panel) has it.
    let restore = {
        let app_rt = c.app_to_restore.borrow().clone();
        let mtm = MainThreadMarker::new().expect("main thread");
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        let own_titled_key = app
            .keyWindow()
            .map(|k| k.styleMask().contains(NSWindowStyleMask::Titled))
            .unwrap_or(false);
        app_rt.filter(|a| !a.isTerminated() && !own_titled_key)
    };
    if let Some(app) = restore {
        app.activateWithOptions(
            objc2_app_kit::NSApplicationActivationOptions::ActivateAllWindows
        );
    }
    *c.app_to_restore.borrow_mut() = None;
}

// MARK: OverlayView mouse / key logic

impl OverlayView {
    fn overlay_mouse_down(&self, event: &NSEvent) {
        let p = self.point_from(event);
        if self.held() {
            let Some(r) = self.held_rect() else { return };
            let Some(i) = Self::handles(r)
                .iter()
                .position(|h| (h.x - p.x).abs() <= 10.0 && (h.y - p.y).abs() <= 10.0)
            else {
                return;
            };
            *self.ivars().resizing.borrow_mut() = Some((i, r));
            return;
        }
        if controller().mode.get() == PickMode::Window {
            update_hover(NSEvent::mouseLocation());
            return;
        }
        self.ivars().drag_start.set(Some(p));
        self.ivars().drag_current.set(Some(p));
        // The hovered window's highlight gives way to the region as soon as
        // the drag moves.
        self.setNeedsDisplay(true);
    }

    fn overlay_mouse_dragged(&self, event: &NSEvent) {
        let p = self.point_clamped(event);
        if self.held() {
            let Some((i, a)) = *self.ivars().resizing.borrow() else { return };
            let (mut min_x, mut max_x, mut min_y, mut max_y) =
                (a.min().x, a.max().x, a.min().y, a.max().y);
            match i {
                0 => {
                    min_x = p.x;
                    min_y = p.y;
                }
                1 => {
                    max_x = p.x;
                    min_y = p.y;
                }
                2 => {
                    min_x = p.x;
                    max_y = p.y;
                }
                3 => {
                    max_x = p.x;
                    max_y = p.y;
                }
                4 => min_y = p.y,
                5 => max_y = p.y,
                6 => min_x = p.x,
                _ => max_x = p.x,
            }
            let r = coordinates::integral_rect(CGRect::new(
                CGPoint::new(min_x.min(max_x), min_y.min(max_y)),
                CGSize::new((max_x - min_x).abs(), (max_y - min_y).abs()),
            ));
            if r.size.width >= 8.0 && r.size.height >= 8.0 {
                region_changed(r);
            }
            return;
        }
        if controller().mode.get() != PickMode::Region || self.ivars().drag_start.get().is_none() {
            return;
        }
        self.ivars().drag_current.set(Some(p));
        self.setNeedsDisplay(true);
    }

    fn overlay_mouse_up(&self, event: &NSEvent) {
        if self.held() {
            if self.ivars().resizing.borrow_mut().take().is_some() {
                region_commit();
                return;
            }
            // M3 has no canvas; a double-click on the held frame is the
            // canvas's double-click (only 录屏-ready listens for it).
            if self.record_mode() && event.clickCount() >= 2 {
                let p = self.point_from(event);
                if self.held_rect().map(|r| coordinates::contains_pt(r, p)).unwrap_or(false) {
                    run_record_hook();
                }
            }
            return;
        }
        if controller().mode.get() == PickMode::Window {
            update_hover(NSEvent::mouseLocation());
            self.pick_hovered_window_or_cancel();
            return;
        }
        // A plain click takes the window under the pointer; on bare desktop
        // it cancels.
        let rect = self.selection_rect();
        self.ivars().drag_start.set(None);
        self.ivars().drag_current.set(None);
        let Some(rect) = rect.filter(|r| r.size.width >= 8.0 && r.size.height >= 8.0) else {
            self.pick_hovered_window_or_cancel();
            return;
        };
        let snapped = coordinates::integral_rect(rect);
        let Some(win) = self.overlay_window() else { return };
        let Some(display) = win.display() else { return };
        finish(
            Some(CaptureTarget::Region(
                display,
                coordinates::display_local_rect(snapped, win.screen_frame().size.height),
            )),
            Some(snapped),
            Some(&win),
        );
    }

    /// Click-without-drag in either mode: hovered window or cancel.
    fn pick_hovered_window_or_cancel(&self) {
        let hovered = controller().hovered_window.borrow().clone();
        let (Some(w), Some(win)) = (hovered, self.overlay_window()) else {
            controller_finish_none();
            return;
        };
        let global = coordinates::cocoa_rect_from_cg(unsafe { w.frame() });
        let rect_in_view = self.window_rect_in_view(global);
        finish(
            Some(CaptureTarget::Window(w)),
            rect_in_view,
            Some(&win),
        );
    }

    fn overlay_key_down(&self, event: &NSEvent) {
        if self.held() {
            // The picker hooks are unbound by now; the record frame owns the
            // keys (⎋ cancel, ⏎ start). Anything else is ignored, not beeped.
            if self.record_mode() {
                match event.keyCode() {
                    36 | 76 => run_record_hook(),
                    53 => run_cancel_hook(),
                    _ => {}
                }
            }
            return;
        }
        match event.keyCode() {
            53 => controller_finish_none(),
            49 => controller_toggle_mode(),
            3 | 36 | 76 => {
                if let Some(win) = self.overlay_window() {
                    if let Some(display) = win.display() {
                        finish(
                            Some(CaptureTarget::Display(display)),
                            Some(self.bounds()),
                            Some(&win),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

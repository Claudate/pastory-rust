//! Port of `Capture/CaptureCoordinator.swift` (M3 slice).
//!
//! Hotkey → picker → ScreenCaptureKit → copy + shelf. The M4 slice (annotate
//! canvas, OCR panel) and the M5 slice (RecordingSession) hook in where
//! marked; the record-ready state machine and its UI already stand.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSScreen};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGImage,
};
use objc2_screen_capture_kit::{SCDisplay, SCWindow};

use crate::app::{coordinates, permissions};
use crate::capture::selection_overlay as overlay;
use crate::capture::target::{CaptureTarget, ShareableSnapshot};
use crate::capture::screenshotter;
use crate::clipboard::store::Source;

/// premultipliedFirst | byteOrder32Little (the Swift composite context).
const BITMAP_INFO_FIRST_LITTLE: u32 = 2 | (2 << 12);

pub struct CaptureCoordinator {
    snapshot: RefCell<Option<ShareableSnapshot>>,
    full_image: RefCell<Option<CFRetained<CGImage>>>,
    full_scale: Cell<f64>,
    /// Which capture asked the OCR panel for text (`ocrToken`).
    /// Pictures of the displays taken before any of our windows existed;
    /// `picked` crops from these.
    frozen: RefCell<Vec<(u32, CFRetained<CGImage>)>>,
    is_busy: Cell<bool>,
    /// Bumped by every start(); work resumed from an async hop belongs to a
    /// capture only while it matches (`generation`).
    generation: Cell<u64>,
    /// Record-ready is up (M5 grows this into the RecordingSession state).
    record_ready: Cell<bool>,
    ocr_token: Cell<u64>,
}

static COORDINATOR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn coordinator() -> &'static CaptureCoordinator {
    let ptr = *COORDINATOR.get_or_init(|| {
        Box::into_raw(Box::new(CaptureCoordinator {
            snapshot: RefCell::new(None),
            full_image: RefCell::new(None),
            full_scale: Cell::new(2.0),
            frozen: RefCell::new(Vec::new()),
            is_busy: Cell::new(false),
            generation: Cell::new(0),
            record_ready: Cell::new(false),
            ocr_token: Cell::new(0),
        })) as usize
    });
    // SAFETY: boxed for the process lifetime; everything here is main-thread.
    unsafe { &*(ptr as *const CaptureCoordinator) }
}

pub fn is_busy() -> bool {
    coordinator().is_busy.get()
}

/// System beep (`NSSound.beep`) — the failed-capture / M5-anchor signal.
fn beep() {
    unsafe {
        let _: () = objc2::msg_send![objc2::class!(NSSound), beep];
    }
}

/// `CaptureCoordinator.start(mode: .region)` — the ⌥⌘S entry.
pub fn start() {
    let c = coordinator();
    // Hotkey again: stop recording; preview / encoding up: beep only (Swift).
    if crate::capture::recording_session::has_active() {
        if crate::capture::recording_session::current().is_recording.get() {
            crate::capture::recording_session::current().stop();
        } else {
            beep();
        }
        return;
    }
    // Hotkey again while the picker / record frame is up: start over (Swift
    // `finish(keepOCRPanel: true)`; the OCR panel is M4).
    if c.is_busy.get() {
        finish(false);
    }
    if !permissions::ensure_screen_recording() {
        return;
    }
    c.is_busy.set(true);
    c.generation.set(c.generation.get() + 1);
    let gen = c.generation.get();
    crate::shelf::panel::set_hold_open(true); // the shelf may be what you want to capture
    // A panel left over from the last capture is not this capture's text:
    // sink it so it can be photographed like any other window, and mark the
    // new token (Swift `OCRPanelController.shared.sinkBelowPicker()` + token).
    crate::annotate::ocr_panel::controller().sink_below_picker();
    c.ocr_token.set(fresh_ocr_token());
    ShareableSnapshot::fetch(Box::new(move |snap| {
        let c = coordinator();
        if gen != c.generation.get() {
            return;
        }
        let Some(snap) = snap else {
            beep();
            finish(true);
            return;
        };
        // Photograph the screen under the pointer *before* any picker window
        // exists: an open menu or drop-down closes the instant another
        // window appears, but by then it is already in the picture.
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        let mouse = NSEvent::mouseLocation();
        let screen = NSScreen::screens(mtm)
            .iter()
            .find(|s| coordinates::contains_pt(s.frame(), mouse))
            .or_else(|| NSScreen::mainScreen(mtm));
        let display = screen.as_ref().and_then(|s| snap.display_for_screen(s));
        let (Some(screen), Some(display)) = (screen, display) else {
            present_with_frozen(snap, Vec::new());
            return;
        };
        let backing = screen.backingScaleFactor();
        let cs_name = screenshotter::screen_color_space_name(&screen);
        // The snapshot + display cross the capture hop as one raw box (SC
        // objects are immutable, safe to hand between queues).
        type Hop = (ShareableSnapshot, Retained<SCDisplay>);
        let raw = Box::into_raw(Box::new((snap, display))) as usize;
        let hop_display: Retained<SCDisplay> =
            unsafe { &*(raw as *const Hop) }.1.clone();
        screenshotter::capture_display(
            &hop_display,
            &[],
            Some(backing),
            cs_name.as_deref(),
            Box::new(move |img| {
                let c = coordinator();
                let (snap, display) = *unsafe { Box::from_raw(raw as *mut Hop) };
                if gen != c.generation.get() {
                    return; // a newer capture owns the overlay now
                }
                let frozen: Vec<(u32, CFRetained<CGImage>)> = img
                    .map(|i| (unsafe { display.displayID() }, i))
                    .into_iter()
                    .collect();
                present_with_frozen(snap, frozen);
            }),
        );
    }));
}

/// Stash everything and open the picker.
fn present_with_frozen(snap: ShareableSnapshot, frozen: Vec<(u32, CFRetained<CGImage>)>) {
    let c = coordinator();
    *c.frozen.borrow_mut() = frozen.clone();
    *c.snapshot.borrow_mut() = Some(snap);
    overlay::present(
        c.snapshot.borrow().as_ref().expect("just stored"),
        overlay::PickMode::Region,
        frozen,
        Box::new(picked),
    );
}

/// Public cancel — ⎋ / right-click / the TopBar ✕ (Swift `cancel()`: a
/// live recording cancels, anything else just finishes).
pub fn cancel() {
    if crate::capture::recording_session::has_active() {
        crate::capture::recording_session::current().cancel();
    } else {
        finish(true);
    }
}

/// `annotateRequestRecord()` — record whatever the (possibly resized) frame
/// covers now. Start the M5 chain: release the picker, keep the shelf held,
/// fire the session.
fn request_record_flow() {
    let Some(rect) = overlay::held_screen_rect() else {
        finish(true);
        return;
    };
    let Some(display) = overlay::held_display() else {
        finish(true);
        return;
    };
    let Some(local) = overlay::held_display_local_rect() else {
        finish(true);
        return;
    };
    let whole = overlay::held_screen_size()
        .map(|s| {
            (s.width - local.size.width).abs() < 0.5
                && (s.height - local.size.height).abs() < 0.5
        })
        .unwrap_or(false);
    let target = if whole {
        CaptureTarget::Display(display)
    } else {
        CaptureTarget::Region(display, local)
    };
    crate::annotate::ocr_panel::controller().close();
    // The frame is fixed now; the shelf goes back to hiding on outside clicks.
    crate::shelf::panel::set_hold_open(false);
    overlay::release(true);
    let session = crate::capture::recording_session::current();
    session.configure(target, rect);
    session.set_on_finish(Box::new(session_finished));
    session.start();
}

/// `session.onFinish` (Swift's `recording = nil; finish()`).
fn session_finished() {
    coordinator().record_ready.set(false);
    finish(true);
}

/// `picked(_:)` — the picker's completion. Crops from the frozen shot when
/// possible; a window pick gets its own live capture laid over the frozen
/// picture.
fn picked(target: Option<CaptureTarget>) {
    let c = coordinator();
    let gen = c.generation.get();
    let Some(target) = target else {
        finish(true);
        return;
    };
    if c.snapshot.borrow().is_none() {
        finish(true);
        return;
    }
    let Some(display) = overlay::held_display() else {
        finish(true);
        return;
    };
    let display_id = unsafe { display.displayID() };
    let base = c
        .frozen
        .borrow()
        .iter()
        .find(|(id, _)| *id == display_id)
        .map(|(_, i)| i.clone());
    if let Some(base) = base {
        picked_with_base(gen, target, display, base);
        return;
    }
    // No frozen shot of this display: take one now, leaving out exactly the
    // mask windows (other Pastory windows, the shelf included, stay in).
    let screen = c
        .snapshot
        .borrow()
        .as_ref()
        .and_then(|s| s.screen_for_display(&display));
    let backing = screen.as_ref().map(|s| s.backingScaleFactor());
    let cs_name = screen
        .as_ref()
        .and_then(|s| screenshotter::screen_color_space_name(s));
    type Hop = (CaptureTarget, Retained<SCDisplay>);
    let raw = Box::into_raw(Box::new((target, display))) as usize;
    let hop_display: Retained<SCDisplay> = unsafe { &*(raw as *const Hop) }.1.clone();
    let ids = overlay::own_window_ids();
    screenshotter::capture_display(
        &hop_display,
        &ids,
        backing,
        cs_name.as_deref(),
        Box::new(move |img| {
            let (target, display) = *unsafe { Box::from_raw(raw as *mut Hop) };
            let c = coordinator();
            if gen != c.generation.get() {
                return;
            }
            match img {
                Some(base) => picked_with_base(gen, target, display, base),
                None => {
                    beep();
                    finish(true);
                }
            }
        }),
    );
}

/// `picked` once the base frame is settled: window picks re-render live.
fn picked_with_base(
    gen: u64,
    target: CaptureTarget,
    display: Retained<SCDisplay>,
    base: CFRetained<CGImage>,
) {
    let CaptureTarget::Window(w) = &target else {
        post_image_ready(gen, display, base);
        return;
    };
    let w = w.clone();
    // We became the active app to show the crosshair, so by now that window
    // is drawn inactive (grey traffic lights, no caret). Hand the focus back
    // and give it a moment to redraw before the shot.
    overlay::reactivate_front_app();
    type Hop = (CFRetained<CGImage>, Retained<SCDisplay>, Retained<SCWindow>);
    let raw = Box::into_raw(Box::new((base, display, w))) as usize;
    crate::app::delegate::dispatch_main_after(0.18, Box::new(move || {
        let c = coordinator();
        let (base, display, w) = *unsafe { Box::from_raw(raw as *mut Hop) };
        if gen != c.generation.get() {
            return;
        }
        let Some(snap_exists) = c.snapshot.borrow().as_ref().map(|_| ()) else {
            finish(true);
            return;
        };
        let _ = snap_exists;
        let raw2 = Box::into_raw(Box::new((base, display, w))) as usize;
        let w_for_filter = unsafe { &*(raw2 as *const Hop) }.2.clone();
        {
            let snap_borrow = c.snapshot.borrow();
            let snap_ref = snap_borrow.as_ref().expect("checked above");
            screenshotter::capture(
                CaptureTarget::Window(w_for_filter),
                snap_ref,
                Box::new(move |own| {
                    let c = coordinator();
                    let (base, display, w) = *unsafe { Box::from_raw(raw2 as *mut Hop) };
                    if gen != c.generation.get() {
                        return;
                    }
                    let mut image = base;
                    if let Some(own) = own {
                        let df = unsafe { display.frame() };
                        let wf = unsafe { w.frame() };
                        let scale = screenshotter::width(&image) as f64 / df.size.width;
                        // Display-local points (top-left origin); window and
                        // display frames both live in CG global space.
                        let local = CGRect::new(
                            CGPoint::new(wf.min().x - df.min().x, wf.min().y - df.min().y),
                            wf.size,
                        );
                        if let Some(composited) = composite(&own, &image, local, scale) {
                            image = composited;
                        }
                    }
                    post_image_ready(gen, display, image);
                }),
            );
        }
    }));
}

/// The Swift `fullImage` store + cropProvider + `showAnnotator` hand-off.
fn post_image_ready(
    gen: u64,
    display: Retained<SCDisplay>,
    image: CFRetained<CGImage>,
) {
    let c = coordinator();
    if gen != c.generation.get() {
        return;
    }
    let screen_w = c
        .snapshot
        .borrow()
        .as_ref()
        .and_then(|s| s.screen_for_display(&display))
        .map(|s| s.frame().size.width)
        .unwrap_or(screenshotter::width(&image) as f64);
    c.full_scale.set(screenshotter::width(&image) as f64 / screen_w);
    *c.full_image.borrow_mut() = Some(image);
    // cropProvider → live re-crop as the handles move (`overlay.cropProvider = { local in self?.crop(local) }`).
    let full = c.full_image.borrow().clone();
    let scale = c.full_scale.get();
    overlay::set_crop_provider(Box::new(move |local| {
        full.as_ref().and_then(|img| crop(img, scale, local))
    }));
    let Some(local) = overlay::held_display_local_rect() else {
        finish(true);
        return;
    };
    let Some(image_ref) = c.full_image.borrow().clone() else {
        finish(true);
        return;
    };
    let Some(cropped) = crop(&image_ref, scale, local) else {
        finish(true);
        return;
    };
    c.record_ready.set(overlay::wants_recording());
    overlay::show_annotator(cropped, Box::new(CoordinatorDelegate));
}

/// The coordinator as an AnnotateDelegate (`CaptureCoordinator:
/// AnnotateDelegate`).
struct CoordinatorDelegate;

impl crate::annotate::annotate_view::AnnotateDelegate for CoordinatorDelegate {
    fn did_finish(&mut self, image: CFRetained<CGImage>) {
        annotate_did_finish(image);
    }
    fn did_cancel(&mut self) {
        finish(true);
    }
    fn request_ocr(&mut self, image: CFRetained<CGImage>) {
        annotate_request_ocr(image);
    }
    fn request_record(&mut self) {
        request_record_flow();
    }
    fn move_region(&mut self, dx: f64, dy: f64) {
        overlay::move_region(dx, dy);
    }
}

/// Kept as a name reference after the M4 wiring rework (empty on purpose).
fn old_request_record_stub_keep() {}

/// `annotateDidFinish(_:)` — the completed screenshot (possibly annotated):
/// copy to the pasteboard and file it in the store, with OCR text attached
/// when this capture's panel already delivered it.
fn annotate_did_finish(image: CFRetained<CGImage>) {
    let c = coordinator();
    let Some(png) = screenshotter::png_data(&image) else {
        finish(true);
        return;
    };
    let panel = crate::annotate::ocr_panel::controller();
    let ocr = (panel.token() == c.ocr_token.get())
        .then(|| panel.current_text())
        .flatten();
    let item = crate::clipboard::store::with(|s| s.insert_image(&png, &Source::pastory(), ocr));
    let id = item.map(|i| i.id).unwrap_or_default();
    crate::capture::pasteboard_writer::write_image(&png, &id);
    // noteWelcomeTried("capture") — M6 welcome anchor.
    finish(true);
}

/// `annotateRequestOCR(_:)` — the 识别文字 button: panel near the frame;
/// copy-as-text on confirm.
fn annotate_request_ocr(image: CFRetained<CGImage>) {
    let c = coordinator();
    let anchor = overlay::held_screen_rect().unwrap_or(CGRect::ZERO);
    c.ocr_token.set(fresh_ocr_token());
    let token = c.ocr_token.get();
    crate::annotate::ocr_panel::controller().show(anchor, image, token, Box::new(|text| {
        // 复制文字 → text item + pasteboard (Swift's onCopy closure).
        let item =
            crate::clipboard::store::with(|s| s.insert_text(&text, None, &Source::pastory()));
        let id = item.map(|i| i.id).unwrap_or_default();
        crate::capture::pasteboard_writer::write_text(&text, None, &id);
        finish(true);
    }));
}

static OCR_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn fresh_ocr_token() -> u64 {
    OCR_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// `annotateRequestRecord` — M5 anchor: the frame is fixed; the actual
/// RecordingSession lands in M5. Until then: beep, nothing thrown away.


/// `crop(_:)` — display-local points (top-left) → pixels of the full
/// capture, integral-rounded and clamped to the image.
fn crop(full: &CGImage, full_scale: f64, local: CGRect) -> Option<CFRetained<CGImage>> {
    let px = coordinates::integral_rect(CGRect::new(
        CGPoint::new(local.origin.x * full_scale, local.origin.y * full_scale),
        CGSize::new(local.size.width * full_scale, local.size.height * full_scale),
    ));
    let bounds = CGRect::new(
        CGPoint::ZERO,
        CGSize::new(
            screenshotter::width(full) as f64,
            screenshotter::height(full) as f64,
        ),
    );
    let px = coordinates::intersect_rect(px, bounds);
    if px.size.width <= 0.0 || px.size.height <= 0.0 {
        return None;
    }
    CGImage::with_image_in_rect(Some(full), px)
}

/// `composite(_:over:atLocal:scale:)` — draw `top` onto `base` at a
/// display-local point rect (top-left origin), keeping base's colour space.
fn composite(
    top: &CGImage,
    base: &CGImage,
    local: CGRect,
    scale: f64,
) -> Option<CFRetained<CGImage>> {
    let (bw, bh) = (screenshotter::width(base), screenshotter::height(base));
    let space =
        CGImage::color_space(Some(base))
        .or_else(CGColorSpace::new_device_rgb)
        .expect("device RGB always exists");
    let ctx = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            bw,
            bh,
            8,
            0,
            Some(&space),
            BITMAP_INFO_FIRST_LITTLE,
        )
    }?;
    let full = CGRect::new(CGPoint::ZERO, CGSize::new(bw as f64, bh as f64));
    objc2_core_graphics::CGContext::draw_image(Some(&ctx), full, Some(base));
    // CG draws bottom-up: flip the y of a top-left rect.
    let px = CGRect::new(
        CGPoint::new(
            local.origin.x * scale,
            bh as f64 - (local.origin.y + local.size.height) * scale,
        ),
        CGSize::new(local.size.width * scale, local.size.height * scale),
    );
    objc2_core_graphics::CGContext::draw_image(Some(&ctx), px, Some(top));
    CGBitmapContextCreateImage(Some(&ctx))
}

/// `finish(restoreFocus:)` — tear down (Swift's private finish; the
/// recording guard `recording.cancel()` is M5). The OCR panel closes unless
/// this is the restart path (hotkey pressed again mid-capture).
fn finish(restore_focus: bool) {
    let c = coordinator();
    *c.full_image.borrow_mut() = None;
    c.frozen.borrow_mut().clear();
    crate::shelf::panel::set_hold_open(false);
    if restore_focus {
        crate::annotate::ocr_panel::controller().close();
    }
    overlay::release(restore_focus);
    *c.snapshot.borrow_mut() = None;
    c.record_ready.set(false);
    c.is_busy.set(false);
}

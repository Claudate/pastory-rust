//! Full-size look at a card's picture, in its own window — the same chrome as
//! 编辑图片 (titled / closable / full-size content view, transparent titlebar,
//! dark aqua, the skin's ground), with the picture on a sheet of paper.
//!
//! The picture is drawn **at its own size**, centered, and never upscaled or
//! cropped: a picture bigger than the screen is scaled down to fit. The wheel
//! (or a trackpad pinch-style scroll) zooms between 0.25× and 8×, a drag pans
//! once zoomed, a double-click goes back to "fit the window". ⎋ and the close
//! button both put it away.
//!
//! This used to be a view sitting over the shelf panel (a scrim + drawing
//! layer), which forced the shelf's own event loop to own the picture. As a
//! window it is the same shelf↔window hand-off 编辑图片 already uses: hide the
//! shelf on open if it was up, show it again on close.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSColor, NSEvent, NSGraphicsContext,
    NSImage, NSPanel, NSView, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::NSString;

use crate::app::localization::l;
use crate::app::theme;
use crate::shelf::card;

/// Bare band kept around the picture (also the "never upscale" fit margin).
const MARGIN: f64 = 24.0;
const MIN_ZOOM: f64 = 0.25;
const MAX_ZOOM: f64 = 8.0;

/// Identifies the load the viewer is currently showing: bumped by every open
/// and close, so a decode that lands afterwards is dropped.
static TICKET: AtomicU64 = AtomicU64::new(1);

fn next_ticket() -> u64 {
    TICKET.fetch_add(1, Ordering::Relaxed)
}

/// Zoom + pan of the one open picture, on the view's own ivars.
#[derive(Clone, Copy)]
struct ViewFrame {
    scale: f64,
    pan_x: f64,
    pan_y: f64,
}

impl Default for ViewFrame {
    fn default() -> Self {
        ViewFrame {
            scale: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

// MARK: Controller (the one live window)

struct Controller {
    panel: RefCell<Option<Retained<NSPanel>>>,
    view: RefCell<Option<Retained<ImageViewerView>>>,
    delegate: RefCell<Option<Retained<ImageViewerWindowDelegate>>>,
    /// The file on show (`None` = closed).
    path: RefCell<Option<PathBuf>>,
    /// Whether the shelf was up when we opened, so close can put it back.
    show_shelf: Cell<bool>,
    ticket: Cell<u64>,
}

static CONTROLLER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn controller() -> &'static Controller {
    let ptr = *CONTROLLER.get_or_init(|| {
        let c = Controller {
            panel: RefCell::new(None),
            view: RefCell::new(None),
            delegate: RefCell::new(None),
            path: RefCell::new(None),
            show_shelf: Cell::new(false),
            ticket: Cell::new(0),
        };
        Box::into_raw(Box::new(c)) as usize
    });
    // SAFETY: boxed for the process lifetime; everything here is main-thread.
    unsafe { &*(ptr as *const Controller) }
}

impl Controller {
    /// The window, built once and reused (`setReleasedWhenClosed(false)`).
    fn ensure(&self, mtm: MainThreadMarker) -> Retained<NSPanel> {
        if let Some(p) = self.panel.borrow().as_ref() {
            return p.clone();
        }
        let rect = CGRect::new(CGPoint::ZERO, CGSize::new(860.0, 620.0));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::FullSizeContentView,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        );
        panel.setTitle(&NSString::from_str(&l("查看图片")));
        panel.setTitlebarAppearsTransparent(true);
        panel.setAppearance(Some(
            &objc2_app_kit::NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua })
                .expect("darkAqua exists"),
        ));
        panel.setBackgroundColor(Some(&theme::ground()));
        unsafe {
            panel.setReleasedWhenClosed(false);
        }
        panel.setCollectionBehavior(
            objc2_app_kit::NSWindowCollectionBehavior::CanJoinAllSpaces
                | objc2_app_kit::NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        let content = crate::annotate::ocr_panel::ground_view_export(rect);
        content.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        panel.setContentView(Some(&content));

        let view = ImageViewerView::new(rect);
        content.addSubview(&view);
        *self.view.borrow_mut() = Some(view.clone());
        panel.makeFirstResponder(Some(&view));

        let delegate: Retained<ImageViewerWindowDelegate> = {
            let d = mtm
                .alloc::<ImageViewerWindowDelegate>()
                .set_ivars(ImageViewerWindowDelegateIvars {});
            unsafe { msg_send![super(d), init] }
        };
        panel.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        *self.delegate.borrow_mut() = Some(delegate);

        *self.panel.borrow_mut() = Some(panel.clone());
        panel
    }
}

// MARK: Public API

/// Open the picture behind a clip card. A preview is display-only, so a file
/// that has moved or cannot be decoded simply does not open.
pub fn open(id: &str) {
    let Some(path) = resolve_path(id) else { return };
    open_path(path);
}

/// `--selftest viewer` drives this directly, with a file it wrote itself.
pub fn open_path(path: PathBuf) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let c = controller();
    let panel = c.ensure(mtm);
    let ticket = next_ticket();
    c.ticket.set(ticket);
    *c.path.borrow_mut() = Some(path.clone());

    // Like 编辑图片: remember the shelf, put it away while the picture is up.
    c.show_shelf.set(crate::shelf::panel::is_visible());
    crate::shelf::panel::hide();
    // The strip must not climb back over this window (`external_hold`).
    crate::shelf::line_panel::external_window_opened();

    if let Some(v) = c.view.borrow().as_ref() {
        *v.ivars().image.borrow_mut() = None;
        v.ivars().ticket.set(ticket);
        v.ivars().frame.set(ViewFrame::default());
        v.setNeedsDisplay(true);
    }
    // `makeKeyAndOrderFront` alone leaves an accessory process in the
    // background, so the ⎋ that closes the viewer never arrives.
    crate::app::activation::activate_app();
    panel.makeKeyAndOrderFront(None);

    // Decoding a 40 MP photo is far too slow for the click that asked for it;
    // the view shows the loading placeholder until this lands. The CGImage
    // crosses as a raw address — it is neither Send nor Sync and only touches
    // the main thread after the hop.
    std::thread::spawn(move || {
        let raw = crate::capture::screenshotter::image_from_file(&path)
            .map(|img| CFRetained::into_raw(img).as_ptr() as usize);
        crate::app::delegate::dispatch_main_async(Box::new(move || {
            let c = controller();
            if c.ticket.get() != ticket {
                if let Some(raw) = raw {
                    // SAFETY: this thread into_raw'd it above; nobody claimed it.
                    unsafe {
                        let _ = CFRetained::from_raw(std::ptr::NonNull::new_unchecked(
                            raw as *mut CGImage,
                        ));
                    }
                }
                return;
            }
            let Some(raw) = raw else {
                // Undecodable: leave the placeholder up rather than an empty
                // window, so it is clear the file is the problem.
                return;
            };
            // SAFETY: as above; this is the single owner of the reference.
            let cg = unsafe {
                CFRetained::from_raw(std::ptr::NonNull::new_unchecked(raw as *mut CGImage))
            };
            let w = CGImage::width(Some(&cg)) as f64;
            let h = CGImage::height(Some(&cg)) as f64;
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            // Size the window to the picture (never larger than the screen),
            // centered.
            if let Some(panel) = c.panel.borrow().as_ref() {
                let vf = crate::annotate::ocr_panel::screens_intersecting(
                    MainThreadMarker::new().expect("main thread"),
                    panel.frame(),
                )
                .map(|s| s.visibleFrame())
                .unwrap_or(CGRect::new(CGPoint::ZERO, CGSize::new(1440.0, 900.0)));
                panel.setContentSize(window_size_for(CGSize::new(w, h), vf));
                panel.center();
            }
            let img = NSImage::initWithCGImage_size(NSImage::alloc(), &cg, CGSize::new(w, h));
            if let Some(v) = c.view.borrow().as_ref() {
                *v.ivars().image.borrow_mut() = Some(img);
                v.fit_to_window();
                v.setNeedsDisplay(true);
            }
        }));
    });
}

/// Put the picture away. Returns whether one was up, so the keyboard table can
/// tell an ⎋ that closed the viewer from one the shelf should handle.
pub fn close() -> bool {
    let c = controller();
    if c.path.borrow().is_none() {
        return false;
    }
    *c.path.borrow_mut() = None;
    c.ticket.set(next_ticket());
    if let Some(p) = c.panel.borrow().as_ref() {
        let sender: Option<&AnyObject> = None;
        p.orderOut(sender);
    }
    if let Some(v) = c.view.borrow().as_ref() {
        *v.ivars().image.borrow_mut() = None;
        v.ivars().ticket.set(next_ticket());
        v.ivars().frame.set(ViewFrame::default());
    }
    if c.show_shelf.get() {
        c.show_shelf.set(false);
        crate::shelf::line_panel::external_window_closed();
        crate::shelf::panel::show();
    } else {
        // Nothing to hand back to, but the gate still has to open or the
        // strip would never reveal itself again this session.
        crate::shelf::line_panel::external_window_closed();
    }
    true
}

/// Whether a picture is open right now.
pub fn is_open() -> bool {
    controller().path.borrow().is_some()
}

/// Self-test only: is the viewer's window the key window? `is_open` only says
/// a path is set, which a window that never came forward also satisfies.
pub fn debug_window_is_key() -> bool {
    controller()
        .panel
        .borrow()
        .as_ref()
        .map(|p| p.isKeyWindow())
        .unwrap_or(false)
}

/// Whether the picture is decoded and on screen — the self-test's wait, and
/// the difference between "open" and "open and rendered".
pub fn is_ready() -> bool {
    let c = controller();
    if c.path.borrow().is_none() {
        return false;
    }
    c.view
        .borrow()
        .as_ref()
        .map(|v| v.ivars().image.borrow().is_some())
        .unwrap_or(false)
}

/// Self-test evidence for `--selftest viewer` in one call: the decoded
/// picture's pixel size, whether it was drawn at its own size (never a
/// crop-fill), the colour the band outside the picture is painted with, and
/// how many samples inside the picture's frame differ from it — the pixels
/// that separate "the picture was drawn" from "only the placeholder is up".
/// `None` while nothing is up.
#[allow(clippy::type_complexity)]
pub fn debug_state() -> Option<(usize, usize, bool, (u8, u8, u8), usize)> {
    let c = controller();
    c.path.borrow().as_ref()?;
    let view = c.view.borrow().clone()?;
    let img = view.ivars().image.borrow();
    let img = img.as_ref()?;
    let size = img.size();
    let bounds = view.bounds();
    let frame = view.image_frame()?;

    let rep = view.bitmapImageRepForCachingDisplayInRect(bounds)?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    let w = rep.pixelsWide() as isize;
    let h = rep.pixelsHigh() as isize;
    if w <= 0 || h <= 0 {
        return None;
    }
    let stride = rep.bytesPerRow() as isize;
    let spp = rep.samplesPerPixel() as isize;
    let base = rep.bitmapData();
    if base.is_null() || spp < 3 {
        return None;
    }
    // The rep is at the backing scale; sample through it.
    let scale = if bounds.size.width > 0.0 {
        w as f64 / bounds.size.width
    } else {
        1.0
    };
    // SAFETY: every offset below is guarded to stay inside the buffer.
    let at = |x: f64, y: f64| -> Option<(u8, u8, u8)> {
        let px = (x * scale).round() as isize;
        let py = (y * scale).round() as isize;
        if px < 0 || py < 0 || px >= w || py >= h {
            return None;
        }
        let off = py * stride + px * spp;
        let rgb = unsafe { std::slice::from_raw_parts(base.offset(off), 3) };
        Some((rgb[0], rgb[1], rgb[2]))
    };
    // The band outside the picture: (4,4) is always ground (the fit keeps a
    // ≥24pt margin), so it is the reference the picture must differ from.
    let corner = at(4.0, 4.0)?;
    let mut distinct = 0usize;
    const GRID: usize = 9;
    for i in 0..GRID {
        for j in 0..GRID {
            let x = frame.min().x + (i as f64 + 0.5) / GRID as f64 * frame.size.width;
            let y = frame.min().y + (j as f64 + 0.5) / GRID as f64 * frame.size.height;
            if let Some(cc) = at(x, y) {
                if cc != corner {
                    distinct += 1;
                }
            }
        }
    }
    Some((
        size.width as usize,
        size.height as usize,
        // Never the crop-fill treatment any more.
        false,
        corner,
        distinct,
    ))
}

/// Self-test only: the 查看图片 window's content view, for `--selftest viewer`.
pub fn debug_content_view() -> Option<Retained<NSView>> {
    controller()
        .panel
        .borrow()
        .as_ref()
        .and_then(|p| p.contentView())
}

/// The stored file for a clip: a screenshot / image / video card's payload.
fn resolve_path(id: &str) -> Option<PathBuf> {
    crate::shelf::panel::with_model_mut(|m| {
        let item = m.items().into_iter().find(|it| it.id == id)?;
        if item.kind != crate::clipboard::item::ClipKind::Image
            && item.kind != crate::clipboard::item::ClipKind::Video
        {
            return None;
        }
        Some(m.with_store(|s| s.payload_url(&item)))
    })
}

/// How big the window wants to be for a picture of `natural` points: its own
/// size plus the bare band on each side (so a picture that fits is drawn 1:1),
/// clamped to (never larger than) the screen.
fn window_size_for(natural: CGSize, vf: CGRect) -> CGSize {
    let max_w = (vf.size.width - 40.0).max(280.0);
    let max_h = (vf.size.height - 40.0).max(200.0);
    CGSize::new(
        (natural.width + 2.0 * MARGIN).clamp(280.0, max_w),
        (natural.height + 2.0 * MARGIN).clamp(200.0, max_h),
    )
}

// MARK: The picture view

pub struct ImageViewerIvars {
    image: RefCell<Option<Retained<NSImage>>>,
    /// Which load this view is showing (`TICKET` value).
    ticket: Cell<u64>,
    frame: Cell<ViewFrame>,
    dragging: Cell<bool>,
    drag_origin: Cell<Option<CGPoint>>,
}

define_class!(
    // SAFETY: plain NSView; main-thread only, ivars are plain data.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ImageViewerIvars]
    pub struct ImageViewerView;

    unsafe impl NSObjectProtocol for ImageViewerView {}

    impl ImageViewerView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            // The click that opens the viewer may be the one that activates
            // the app; it should still count.
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            let bounds = self.bounds();
            let img = self.ivars().image.borrow();
            let Some(img) = img.as_ref() else {
                // Still decoding (or undecodable): the same glyph the card
                // shows, centered.
                let area = CGRect::new(
                    CGPoint::new(MARGIN, MARGIN),
                    CGSize::new(
                        (bounds.size.width - 2.0 * MARGIN).max(1.0),
                        (bounds.size.height - 2.0 * MARGIN).max(1.0),
                    ),
                );
                if let Some(icon) = card::symbol(
                    "photo",
                    44.0,
                    card::SymbolWeight::Regular,
                    &theme::label_secondary().colorWithAlphaComponent(0.4),
                ) {
                    card::draw_centered(&icon, area, 1.0);
                }
                return;
            };
            let Some(frame) = self.image_frame() else { return };
            // Paper mat + shadow behind the picture (the 编辑图片 CanvasMat
            // shape: surface, rounded, shadowed).
            NSGraphicsContext::saveGraphicsState_class();
            let shadow =
                card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.5), 18.0, 0.0, 10.0);
            shadow.set();
            theme::surface_alt().setFill();
            theme::rounded_rect(frame, theme::radius_panel()).fill();
            NSGraphicsContext::restoreGraphicsState_class();
            NSGraphicsContext::saveGraphicsState_class();
            theme::rounded_rect(frame, theme::radius_panel()).addClip();
            card::draw_image(
                img,
                frame,
                objc2_app_kit::NSCompositingOperation::SourceOver,
                1.0,
            );
            NSGraphicsContext::restoreGraphicsState_class();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if event.clickCount() == 2 {
                // Double-click: back to "fit the window".
                self.fit_to_window();
                return;
            }
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            if let Some(f) = self.image_frame() {
                let inside = p.x >= f.min().x
                    && p.x <= f.max().x
                    && p.y >= f.min().y
                    && p.y <= f.max().y;
                if inside {
                    self.ivars().dragging.set(true);
                    self.ivars().drag_origin.set(Some(p));
                    return;
                }
            }
            close();
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if !self.ivars().dragging.get() {
                return;
            }
            let Some(o) = self.ivars().drag_origin.get() else { return };
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut f = self.ivars().frame.get();
            f.pan_x += p.x - o.x;
            f.pan_y += p.y - o.y;
            self.ivars().frame.set(f);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().dragging.set(false);
            self.ivars().drag_origin.set(None);
        }

        /// Wheel / trackpad scroll: zoom toward the pointer, 0.25×–8×.
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let dy = event.scrollingDeltaY();
            let dx = event.scrollingDeltaX();
            let d = if dy.abs() >= dx.abs() { dy } else { dx };
            if d == 0.0 {
                return;
            }
            let anchor = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut f = self.ivars().frame.get();
            let old = f.scale.max(0.0001);
            let new = (old * (1.0 + d * 0.0035)).clamp(MIN_ZOOM, MAX_ZOOM);
            if (new - old).abs() < 1e-6 {
                return;
            }
            let bounds = self.bounds();
            let cx = bounds.size.width / 2.0;
            let cy = bounds.size.height / 2.0;
            let relx = anchor.x - cx - f.pan_x;
            let rely = anchor.y - cy - f.pan_y;
            let ratio = new / old;
            f.scale = new;
            f.pan_x -= relx * (ratio - 1.0);
            f.pan_y -= rely * (ratio - 1.0);
            self.ivars().frame.set(f);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.keyCode() == 53 {
                close();
                return;
            }
            unsafe {
                let _: () = msg_send![super(self), keyDown: event];
            }
        }

        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            close();
        }
    }
);

impl ImageViewerView {
    fn new(frame: CGRect) -> Retained<Self> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<ImageViewerView>().set_ivars(ImageViewerIvars {
            image: RefCell::new(None),
            ticket: Cell::new(next_ticket()),
            frame: Cell::new(ViewFrame::default()),
            dragging: Cell::new(false),
            drag_origin: Cell::new(None),
        });
        let v: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        v.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        v
    }

    fn image_frame(&self) -> Option<CGRect> {
        let img = self.ivars().image.borrow();
        let img = img.as_ref()?;
        let size = img.size();
        if size.width <= 0.0 || size.height <= 0.0 {
            return None;
        }
        let bounds = self.bounds();
        let f = self.ivars().frame.get();
        Some(CGRect::new(
            CGPoint::new(
                (bounds.size.width - size.width * f.scale) / 2.0 + f.pan_x,
                (bounds.size.height - size.height * f.scale) / 2.0 + f.pan_y,
            ),
            CGSize::new(size.width * f.scale, size.height * f.scale),
        ))
    }

    /// Scale that shows the whole picture inside the window, never upscaling
    /// past 1:1 — the initial view and the double-click reset.
    fn fit_to_window(&self) {
        let Some(sz) = self.ivars().image.borrow().as_ref().map(|i| i.size()) else {
            return;
        };
        let bounds = self.bounds();
        let k = ((bounds.size.width - 2.0 * MARGIN) / sz.width.max(1.0))
            .min((bounds.size.height - 2.0 * MARGIN) / sz.height.max(1.0))
            .min(1.0)
            .max(0.05);
        self.ivars().frame.set(ViewFrame {
            scale: k,
            pan_x: 0.0,
            pan_y: 0.0,
        });
        self.setNeedsDisplay(true);
    }
}

// MARK: Window delegate

pub struct ImageViewerWindowDelegateIvars {}

define_class!(
    // SAFETY: NSObject window delegate; main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ImageViewerWindowDelegateIvars]
    pub struct ImageViewerWindowDelegate;

    unsafe impl NSObjectProtocol for ImageViewerWindowDelegate {}

    unsafe impl NSWindowDelegate for ImageViewerWindowDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &objc2_foundation::NSNotification) {
            // The close button routes through the same teardown as ⎋: restore
            // the shelf if it was up, drop the picture.
            close();
        }
    }
);

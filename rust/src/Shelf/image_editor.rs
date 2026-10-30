//! Port of `Shelf/ImageEditorWindow.swift` — the screenshot annotator hosted
//! in a window for a picture already on the shelf. 保存 writes the flattened
//! image back to the same card and copies it.
//!
//! The canvas is the M4 `AnnotateView` unchanged; the window owns the paper
//! mat behind it, the toolbar (with 保存 instead of 复制), and the OCR hook
//! that ends on the shared `OcrPanel`.

use std::cell::RefCell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearanceCustomization, NSBackingStoreType, NSView, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSNotification, NSString};

use crate::annotate::annotate_view::{AnnotateDelegate, AnnotateView};
use crate::annotate::toolbar::AnnotateToolbar;
use crate::app::localization::l;
use crate::app::theme;
use crate::clipboard::item::ClipItem;

struct OpenMap(std::cell::UnsafeCell<Option<HashMap<String, Retained<ImageEditorWindow>>>>);
unsafe impl Sync for OpenMap {}
static OPEN: OpenMap = OpenMap(std::cell::UnsafeCell::new(None));
fn open_map() -> &'static mut HashMap<String, Retained<ImageEditorWindow>> {
    unsafe { (&mut *OPEN.0.get()).get_or_insert_with(HashMap::new) }
}

// MARK: Owner side (AnnotateDelegate + window delegate)

/// The window's Rust-side owner. Lives as a duplicate the ObjC class keeps a
/// raw pointer to; destroy-order is the windowWillClose of the delegate.
struct EditorState {
    item: ClipItem,
    window: RefCell<Option<Retained<ImageEditorWindow>>>,
    canvas: RefCell<Option<Retained<AnnotateView>>>,
    toolbar: RefCell<Option<Retained<AnnotateToolbar>>>,
    mat: RefCell<Option<Retained<CanvasMatView>>>,
    reopen_shelf: RefCell<bool>,
}

impl AnnotateDelegate for EditorState {
    fn did_finish(&mut self, image: CFRetained<CGImage>) {
        if let Some(png) = crate::capture::screenshotter::png_data(&image) {
            let id = self.item.id.clone();
            crate::clipboard::store::with(|s| {
                s.update_image(&id, &png);
                if let Some(updated) = s.items.iter().find(|it| it.id == id).cloned() {
                    s.copy_to_pasteboard(&updated);
                }
            });
        }
        if let Some(w) = self.window.borrow().clone() {
            w.close();
        }
    }

    fn did_cancel(&mut self) {
        if let Some(w) = self.window.borrow().clone() {
            w.close();
        }
    }

    fn request_ocr(&mut self, image: CFRetained<CGImage>) {
        let Some(w) = self.window.borrow().clone() else { return };
        let Some(canvas) = self.canvas.borrow().clone() else { return };
        let anchor = w.convertRectToScreen(canvas.frame());
        crate::annotate::ocr_panel::controller().show(
            anchor,
            image,
            0,
            Box::new(|text| {
                let inserted = crate::clipboard::store::with(|s| {
                    s.insert_text(&text, None, &crate::clipboard::store::Source::pastory())
                });
                crate::capture::pasteboard_writer::write_text(
                    &text,
                    None,
                    inserted.as_ref().map(|i| i.id.as_str()).unwrap_or(""),
                );
                crate::annotate::ocr_panel::controller().close();
            }),
        );
    }

    fn request_record(&mut self) {}
}

// MARK: Window class

pub struct ImageEditorIvars {
    state: RefCell<Option<Box<EditorState>>>,
    canvas: RefCell<Option<Retained<AnnotateView>>>,
    toolbar: RefCell<Option<Retained<AnnotateToolbar>>>,
    /// The paper mat, so `layout` can re-place it with the canvas.
    mat: RefCell<Option<Retained<CanvasMatView>>>,
    /// Natural size of the picture (unscaled points) and its frame as
    /// fractions of that — a resize scales the arrangement with the picture.
    natural: RefCell<Option<CGSize>>,
    rel: RefCell<Vec<(f64, f64, f64, f64)>>,
}

define_class!(
    // SAFETY: NSWindow + own closest hook, main-thread only; one per item.
    #[unsafe(super(objc2_app_kit::NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ImageEditorIvars]
    pub struct ImageEditorWindow;

    unsafe impl NSObjectProtocol for ImageEditorWindow {}

    impl ImageEditorWindow {
        #[unsafe(method(close))]
        fn ie_close(&self) {
            // Route through the Rust state so the same teardown runs for ⌘W as for 保存.
            unsafe {
                let _: () = msg_send![super(self), close];
            }
        }
    }
);

/// `windowWillClose` → OCR close + shelf reopen + drop from the open map.
pub struct ImageEditorWinDelegateIvars {
    window: RefCell<usize>,
}

define_class!(
    // SAFETY: NSObject window delegate; main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ImageEditorWinDelegateIvars]
    struct ImageEditorWinDelegate;

    unsafe impl NSObjectProtocol for ImageEditorWinDelegate {}

    unsafe impl NSWindowDelegate for ImageEditorWinDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            let raw = *self.ivars().window.borrow();
            let w: &ImageEditorWindow = unsafe { &*(raw as *const ImageEditorWindow) };
            let mut state = w.ivars().state.borrow_mut();
            if let Some(s) = state.as_mut() {
                s.toolbar.borrow_mut().take();
                s.canvas.borrow_mut().take();
                s.window.borrow_mut().take();
            }
            crate::annotate::ocr_panel::controller().close();
            if let Some(s) = state.as_ref() {
                open_map().remove(&s.item.id);
                crate::shelf::line_panel::external_window_closed();
                if *s.reopen_shelf.borrow() {
                    crate::shelf::panel::show();
                }
            }
            let _ = state.take(); // EditorState's Rust half drops with its window
        }

        /// The window is resizable: re-place the mat, the canvas and the
        /// toolbar against the new content box (before this the frames were
        /// fixed at what `make` computed).
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _n: &NSNotification) {
            let raw = *self.ivars().window.borrow();
            let w: &ImageEditorWindow = unsafe { &*(raw as *const ImageEditorWindow) };
            if let Some(c) = w.contentView() {
                let b = c.bounds().size;
                w.layout(b.width, b.height);
            }
        }
    }
);

/// Mat padding around the picture on each side.
const PAD: f64 = 40.0;
/// Titlebar strip the full-size content view runs under.
const TITLEBAR_STRIP: f64 = 44.0;
/// Toolbar column below the picture (buttons + room for the sub bar).
const TOOLBAR_H: f64 = 56.0 + 84.0;

/// Where the window and the picture land for a picture of `natural` points on a
/// screen offering `vf`.
///
/// Split out of `make` so the clamp is a testable function rather than a
/// property of one render: the picture is scaled down to whatever the screen
/// can actually hand it (so a tall phone screenshot cannot build a window
/// taller than the display), and the window then takes that picture plus the
/// chrome, never narrower than the toolbar needs and never larger than the
/// visible frame. Returns `(window_size, canvas_frame)`.
pub(crate) fn editor_geometry(natural: CGSize, vf: CGRect) -> (CGSize, CGRect) {
    // Everything the window wraps around the picture: mat padding on both
    // sides, the titlebar strip, and the toolbar column.
    let chrome_h = PAD + TITLEBAR_STRIP + TOOLBAR_H;
    let avail_w = (vf.size.width - 80.0 - PAD * 2.0).max(200.0);
    let avail_h = (vf.size.height - 40.0 - chrome_h).max(200.0);
    let fit = (avail_w / natural.width.max(1.0)).min(avail_h / natural.height.max(1.0));
    // Never upscale: the floor scale only kicks in for pictures so small the
    // editor would otherwise open on a postage stamp.
    let k = fit.min(1.0_f64.max(360.0 / natural.width.max(natural.height)));
    let canvas_size = CGSize::new((natural.width * k).round(), (natural.height * k).round());
    let win_w = (canvas_size.width + PAD * 2.0)
        .max(900.0)
        .min((vf.size.width - 40.0).max(600.0));
    let win_h = (canvas_size.height + chrome_h).min((vf.size.height - 40.0).max(400.0));
    let win = CGSize::new(win_w, win_h);
    // Centered in the room left between the toolbar column and the titlebar
    // strip — what keeps a height-clamped window from cutting the top off.
    let canvas_y =
        (TOOLBAR_H + (win_h - TOOLBAR_H - canvas_size.height) / 2.0).max(TITLEBAR_STRIP + 6.0);
    let canvas = CGRect::new(
        CGPoint::new(((win_w - canvas_size.width) / 2.0).round(), canvas_y.round()),
        canvas_size,
    );
    (win, canvas)
}

impl ImageEditorWindow {
    /// `open(_:)` — reopen the live editor if one exists for this card.
    pub fn open(item: &ClipItem) {
        if let Some(w) = open_map().get(&item.id) {
            w.makeKeyAndOrderFront(None);
            return;
        }
        let png = crate::clipboard::store::read(|s| s.png(item));
        let Some(cg) = png.as_ref().and_then(|png| crate::capture::screenshotter::image_from_png(png)) else {
            unsafe {
                let _: () = msg_send![objc2::class!(NSSound), beep];
            }
            return;
        };
        let e = Self::make(item, cg);
        let editable = e;
        open_map().insert(item.id.clone(), editable.clone());
        crate::app::activation::activate_app();
        *editable.ivars().state.borrow_mut().as_mut().expect("state").reopen_shelf.borrow_mut() =
            crate::shelf::panel::is_visible();
        crate::shelf::panel::hide();
        // The strip must not climb back over this window (`external_hold`).
        crate::shelf::line_panel::external_window_opened();
        editable.makeKeyAndOrderFront(None);
    }

    /// Self-test only: the content view, laid out, without showing the window.
    pub fn debug_view(item: &ClipItem) -> Option<Retained<NSView>> {
        let png = crate::clipboard::store::read(|s| s.png(item))?;
        let cg = crate::capture::screenshotter::image_from_png(&png)?;
        let e = Self::make(item, cg);
        open_map().insert(item.id.clone(), e.clone());
        Some(e.contentView().expect("content"))
    }

    /// Self-test only: build the editor for a real card and report the geometry
    /// `make` settled on — `(content_w, content_h, canvas_x, canvas_y,
    /// canvas_w, canvas_h, toolbar_x, toolbar_y, toolbar_w, toolbar_h)`. A
    /// render only shows the initial fit; these numbers say whether the canvas
    /// and the toolbar actually landed inside the window.
    #[allow(clippy::type_complexity)]
    pub fn debug_geometry(item: &ClipItem) -> Option<(f64, f64, f64, f64, f64, f64, f64, f64, f64, f64)> {
        let png = crate::clipboard::store::read(|s| s.png(item))?;
        let cg = crate::capture::screenshotter::image_from_png(&png)?;
        let e = Self::make(item, cg);
        open_map().insert(item.id.clone(), e.clone());
        let content = e.contentView()?;
        let cb = content.bounds().size;
        let canvas = e.ivars().canvas.borrow().as_ref()?.frame();
        let bar = e.ivars().toolbar.borrow().as_ref()?.frame();
        Some((
            cb.width,
            cb.height,
            canvas.min().x,
            canvas.min().y,
            canvas.size.width,
            canvas.size.height,
            bar.min().x,
            bar.min().y,
            bar.size.width,
            bar.size.height,
        ))
    }

    /// Self-test only: build the editor for a real card, run the resize path
    /// for a content box of `size`, and report the canvas and toolbar frames —
    /// so a resize that does nothing reads as frames still at their
    /// default-size origin.
    pub fn debug_layout_at(
        item: &ClipItem,
        size: CGSize,
    ) -> Option<(f64, f64, f64, f64, f64, f64, f64, f64)> {
        let png = crate::clipboard::store::read(|s| s.png(item))?;
        let cg = crate::capture::screenshotter::image_from_png(&png)?;
        let e = Self::make(item, cg);
        open_map().insert(item.id.clone(), e.clone());
        if let Some(c) = e.contentView() {
            c.setFrame(CGRect::new(CGPoint::ZERO, size));
        }
        e.layout(size.width, size.height);
        let canvas = e.ivars().canvas.borrow().as_ref()?.frame();
        let bar = e.ivars().toolbar.borrow().as_ref()?.frame();
        Some((
            canvas.min().x,
            canvas.min().y,
            canvas.size.width,
            canvas.size.height,
            bar.min().x,
            bar.min().y,
            bar.size.width,
            bar.size.height,
        ))
    }

    fn items(&self) -> Option<(Retained<CanvasMatView>, Retained<AnnotateView>, Retained<AnnotateToolbar>)> {
        let state = self.ivars().state.borrow();
        let s = state.as_ref()?;
        let mat = s.mat.borrow().clone()?;
        let canvas = s.canvas.borrow().clone()?;
        let toolbar = s.toolbar.borrow().clone()?;
        Some((mat, canvas, toolbar))
    }

    /// Place the mat, the canvas and the toolbar against a content box of
    /// `w`×`h`. The picture keeps its aspect and never upscales past 1:1; the
    /// toolbar stays a fixed-size bar, re-centred horizontally.
    pub(crate) fn layout(&self, w: f64, h: f64) {
        let (Some(un), Some((mat, canvas, bar))) =
            (*self.ivars().natural.borrow(), self.items())
        else {
            return;
        };
        let rel = self.ivars().rel.borrow().clone();
        let avail_w = (w - 2.0 * PAD).max(80.0);
        let avail_h = (h - TITLEBAR_STRIP - TOOLBAR_H).max(80.0);
        let k = ((avail_w / un.width.max(1.0)).min(avail_h / un.height.max(1.0)))
            .min(1.0)
            .max(0.02);
        let uw = un.width * k;
        let uh = un.height * k;
        let ux = ((w - uw) / 2.0).round();
        let uy = (TOOLBAR_H + (h - TOOLBAR_H - uh) / 2.0)
            .round()
            .max(TITLEBAR_STRIP + 6.0);
        let frame_of = |r: (f64, f64, f64, f64)| {
            CGRect::new(
                CGPoint::new(ux + r.0 * uw, uy + r.1 * uh),
                CGSize::new((r.2 * uw).max(1.0), (r.3 * uh).max(1.0)),
            )
        };
        if let Some(r) = rel.first() {
            let f = frame_of(*r);
            mat.setFrame(crate::app::coordinates::inset_rect(f, -6.0, -6.0));
            canvas.setFrame(f);
        }
        let ts = bar.fitting();
        bar.setFrame(CGRect::new(
            CGPoint::new(((w - ts.width) / 2.0).round(), (TOOLBAR_H - ts.height - 4.0).max(4.0)),
            ts,
        ));
        bar.did_layout();
    }

    fn make(item: &ClipItem, image: CFRetained<CGImage>) -> Retained<Self> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let screen = objc2_app_kit::NSScreen::mainScreen(mtm);
        let vf = screen
            .as_ref()
            .map(|s| s.visibleFrame())
            .unwrap_or(CGRect::new(CGPoint::ZERO, CGSize::new(1440.0, 900.0)));
        let scale = screen.as_ref().map(|s| s.backingScaleFactor()).unwrap_or(2.0);
        let natural = CGSize::new(
            CGImage::width(Some(&image)) as f64 / scale,
            CGImage::height(Some(&image)) as f64 / scale,
        );
        let (win, canvas_rect) = editor_geometry(natural, vf);
        let rect = CGRect::new(CGPoint::ZERO, win);
        let this = mtm.alloc::<ImageEditorWindow>().set_ivars(ImageEditorIvars {
            state: RefCell::new(None),
            canvas: RefCell::new(None),
            toolbar: RefCell::new(None),
            mat: RefCell::new(None),
            natural: RefCell::new(None),
            rel: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::FullSizeContentView,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        this.setTitle(&NSString::from_str(&l("编辑图片")));
        this.setTitlebarAppearsTransparent(true);
        this.setAppearance(Some(&objc2_app_kit::NSAppearance::appearanceNamed(
            unsafe { objc2_app_kit::NSAppearanceNameDarkAqua },
        ).expect("dark aqua")));
        this.setBackgroundColor(Some(&theme::ground()));
        this.setMinSize(CGSize::new(600.0, 360.0));
        unsafe { this.setReleasedWhenClosed(false) };

        let content = crate::annotate::ocr_panel::ground_view_export(rect);
        content.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        this.setContentView(Some(&content));

        // Paper mat + shadow behind the picture, on the frame `editor_geometry`
        // chose: centered between the toolbar column and the titlebar strip,
        // which is what keeps the top of a height-clamped window from cutting
        // the picture off.
        let canvas_frame = canvas_rect;
        let border = CanvasMatView::new(crate::app::coordinates::inset_rect(canvas_frame, -6.0, -6.0));
        content.addSubview(&*border);

        let canvas = AnnotateView::make(canvas_frame, image.clone());
        content.addSubview(&canvas);

        let bar = AnnotateToolbar::make(&canvas, &l("保存"));
        let ts = bar.fitting();
        bar.setFrame(CGRect::new(
            CGPoint::new(
                ((rect.size.width - ts.width) / 2.0).round(),
                TOOLBAR_H - ts.height - 4.0,
            ),
            ts,
        ));
        content.addSubview(&bar);
        bar.did_layout();
        // Remember the unscaled picture frame so `layout` can rescale the
        // arrangement on every resize.
        *this.ivars().natural.borrow_mut() = Some(canvas_frame.size);
        *this.ivars().rel.borrow_mut() = vec![(0.0, 0.0, 1.0, 1.0)];
        *this.ivars().mat.borrow_mut() = Some(border.clone());
        // Also keep them on the window for the self-test's geometry read; the
        // live path only needs the copies inside `state`.
        *this.ivars().canvas.borrow_mut() = Some(canvas.clone());
        *this.ivars().toolbar.borrow_mut() = Some(bar.clone());

        let delegate: Retained<ImageEditorWinDelegate> = {
            let d = mtm.alloc::<ImageEditorWinDelegate>().set_ivars(ImageEditorWinDelegateIvars {
                window: RefCell::new(Retained::as_ptr(&this) as usize),
            });
            unsafe { msg_send![super(d), init] }
        };
        this.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate)));
        this.makeFirstResponder(Some(&canvas));
        let state = Box::new(EditorState {
            item: item.clone(),
            window: RefCell::new(Some(this.clone())),
            canvas: RefCell::new(Some(canvas.clone())),
            toolbar: RefCell::new(Some(bar.clone())),
            mat: RefCell::new(Some(border.clone())),
            reopen_shelf: RefCell::new(false),
        });
        canvas.set_delegate(Box::new(EditorDelegate(state)));
        *this.ivars().state.borrow_mut() = Some(Box::new(EditorState {
            item: item.clone(),
            window: RefCell::new(Some(this.clone())),
            canvas: RefCell::new(Some(canvas.clone())),
            toolbar: RefCell::new(Some(bar)),
            mat: RefCell::new(Some(border)),
            reopen_shelf: RefCell::new(false),
        }));
        // Open centered on the screen the shelf was on; without it the window
        // landed at its own rect origin, the bottom-left corner.
        this.center();
        this
    }
}

// MARK: Paper mat

struct CanvasMatIvars {}

define_class!(
    // SAFETY: plain NSView with the paper + shadow layer, main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = CanvasMatIvars]
    struct CanvasMatView;

    unsafe impl NSObjectProtocol for CanvasMatView {}
);

impl CanvasMatView {
    fn new(frame: CGRect) -> Retained<Self> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<CanvasMatView>().set_ivars(CanvasMatIvars {});
        let v: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        v.setWantsLayer(true);
        if let Some(layer) = v.layer() {
            unsafe {
                let _: () = msg_send![&*layer, setCornerRadius: 2.0];
                let _: () = msg_send![&*layer, setBackgroundColor: &*theme::surface().CGColor()];
            }
        }
        let shadow = objc2_app_kit::NSShadow::new();
        shadow.setShadowColor(Some(&objc2_app_kit::NSColor::colorWithCalibratedWhite_alpha(0.0, 0.6)));
        shadow.setShadowBlurRadius(18.0);
        shadow.setShadowOffset(CGSize::new(0.0, -4.0));
        v.setShadow(Some(&shadow));
        v
    }
}

// MARK: EditorState → AnnotateDelegate bridge

/// The canvas keeps this half of the state; the window the other. Both eyes
/// work on the same item + close through well-understood paths (state|.
/// window.close() or windowWillClose on the window side).
struct EditorDelegate(Box<EditorState>);

impl AnnotateDelegate for EditorDelegate {
    fn did_finish(&mut self, image: CFRetained<CGImage>) {
        if let Some(png) = crate::capture::screenshotter::png_data(&image) {
            let id = self.0.item.id.clone();
            crate::clipboard::store::with(|s| {
                s.update_image(&id, &png);
                if let Some(updated) = s.items.iter().find(|it| it.id == id).cloned() {
                    s.copy_to_pasteboard(&updated);
                }
            });
        }
        if let Some(w) = self.0.window.borrow().clone() {
            w.close();
        }
    }

    fn did_cancel(&mut self) {
        if let Some(w) = self.0.window.borrow().clone() {
            w.close();
        }
    }

    fn request_ocr(&mut self, image: CFRetained<CGImage>) {
        let Some(w) = self.0.window.borrow().clone() else { return };
        let Some(canvas) = self.0.canvas.borrow().clone() else { return };
        let anchor = w.convertRectToScreen(canvas.frame());
        crate::annotate::ocr_panel::controller().show(
            anchor,
            image,
            0,
            Box::new(|text| {
                let inserted = crate::clipboard::store::with(|s| {
                    s.insert_text(&text, None, &crate::clipboard::store::Source::pastory())
                });
                crate::capture::pasteboard_writer::write_text(
                    &text,
                    None,
                    inserted.as_ref().map(|i| i.id.as_str()).unwrap_or(""),
                );
                crate::annotate::ocr_panel::controller().close();
            }),
        );
    }

    fn request_record(&mut self) {}
}

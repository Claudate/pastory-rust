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
                if *s.reopen_shelf.borrow() {
                    crate::shelf::panel::show();
                }
            }
            let _ = state.take(); // EditorState's Rust half drops with its window
        }
    }
);

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
        let mtm = MainThreadMarker::new().expect("main thread");
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        unsafe {
            let _: () = msg_send![&app, activate];
        }
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        *editable.ivars().state.borrow_mut().as_mut().expect("state").reopen_shelf.borrow_mut() =
            crate::shelf::panel::is_visible();
        crate::shelf::panel::hide();
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

    fn make(item: &ClipItem, image: CFRetained<CGImage>) -> Retained<Self> {
        let vf = objc2_app_kit::NSScreen::mainScreen(MainThreadMarker::new().expect("main"))
            .map(|s| s.visibleFrame())
            .unwrap_or(CGRect::new(CGPoint::ZERO, CGSize::new(1440.0, 900.0)));
        let scale = objc2_app_kit::NSScreen::mainScreen(MainThreadMarker::new().expect("main"))
            .map(|s| s.backingScaleFactor())
            .unwrap_or(2.0);
        let natural = CGSize::new(
            CGImage::width(Some(&image)) as f64 / scale,
            CGImage::height(Some(&image)) as f64 / scale,
        );
        let fit = ((vf.size.width * 0.8 - 80.0) / natural.width).min((vf.size.height * 0.8 - 200.0) / natural.height);
        let k = fit.min(1.0_f64.max(360.0 / natural.width.max(natural.height)));
        let canvas_size = CGSize::new((natural.width * k).round(), (natural.height * k).round());
        let pad = 40.0;
        let bar_h = 56.0 + 84.0; // toolbar + room for the sub bar
        let rect = CGRect::new(
            CGPoint::ZERO,
            CGSize::new((canvas_size.width + pad * 2.0).max(900.0), canvas_size.height + pad + 44.0 + bar_h),
        );
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<ImageEditorWindow>().set_ivars(ImageEditorIvars {
            state: RefCell::new(None),
            canvas: RefCell::new(None),
            toolbar: RefCell::new(None),
        });
        let this: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
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
        this.setBackgroundColor(Some(&theme::brown()));
        unsafe { this.setReleasedWhenClosed(false) };

        let content = crate::annotate::ocr_panel::ground_view_export(rect);
        this.setContentView(Some(&content));

        // Paper mat + shadow behind the picture.
        let canvas_frame = CGRect::new(
            CGPoint::new(
                ((rect.size.width - canvas_size.width) / 2.0).round(),
                bar_h + 20.0,
            ),
            canvas_size,
        );
        let border = CanvasMatView::new(crate::app::coordinates::inset_rect(canvas_frame, -6.0, -6.0));
        content.addSubview(&*border);

        let canvas = AnnotateView::make(canvas_frame, image.clone());
        content.addSubview(&canvas);

        let bar = AnnotateToolbar::make(&canvas, &l("保存"));
        let ts = bar.fitting();
        bar.setFrame(CGRect::new(
            CGPoint::new(
                ((rect.size.width - ts.width) / 2.0).round(),
                bar_h - ts.height - 4.0,
            ),
            ts,
        ));
        content.addSubview(&bar);
        bar.did_layout();

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
            reopen_shelf: RefCell::new(false),
        });
        canvas.set_delegate(Box::new(EditorDelegate(state)));
        *this.ivars().state.borrow_mut() = Some(Box::new(EditorState {
            item: item.clone(),
            window: RefCell::new(Some(this.clone())),
            canvas: RefCell::new(Some(canvas.clone())),
            toolbar: RefCell::new(Some(bar)),
            reopen_shelf: RefCell::new(false),
        }));
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
                let _: () = msg_send![&*layer, setBackgroundColor: &*theme::paper().CGColor()];
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

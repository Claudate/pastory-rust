//! Port of `Annotate/OCRPanel.swift`.
//!
//! Floating result of 识别文字: editable text, copy button. The panel sits
//! one level above the picker (`abovePicker`); `sinkBelowPicker` drops it
//! before a new capture so it can be screenshotted like any other window.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSPanel, NSScrollView, NSTextField,
    NSTextView, NSView, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::NSString;

use crate::app::{localization::l, theme};

/// `CGShieldingWindowLevel() + 1` — above the picker, below nothing else.
fn above_picker_level() -> isize {
    objc2_core_graphics::CGShieldingWindowLevel() as isize + 1
}

pub struct OcrPanel {
    panel: RefCell<Option<Retained<NSPanel>>>,
    text_view: RefCell<Option<Retained<NSTextView>>>,
    status: RefCell<Option<Retained<NSTextField>>>,
    on_copy: RefCell<Option<Box<dyn Fn(String) + 'static>>>,
    /// Which capture asked for this text; a later capture must not inherit it.
    token: Cell<u64>,
    task_gen: Cell<u64>,
    delegate_obj: RefCell<Option<Retained<OcrPanelWinDelegate>>>,
}

static CONTROLLER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

pub fn controller() -> &'static OcrPanel {
    let ptr = *CONTROLLER.get_or_init(|| {
        let c = OcrPanel {
            panel: RefCell::new(None),
            text_view: RefCell::new(None),
            status: RefCell::new(None),
            on_copy: RefCell::new(None),
            token: Cell::new(0),
            task_gen: Cell::new(0),
            delegate_obj: RefCell::new(None),
        };
        Box::into_raw(Box::new(c)) as usize
    });
    // SAFETY: boxed for the process lifetime; everything here is main-thread.
    unsafe { &*(ptr as *const OcrPanel) }
}

impl OcrPanel {
    pub fn token(&self) -> u64 {
        self.token.get()
    }

    /// Text as currently shown (edited or not); nil when the panel is not up.
    pub fn current_text(&self) -> Option<String> {
        let (tv, panel) = (
            self.text_view.borrow().clone()?,
            self.panel.borrow().clone()?,
        );
        if !panel.isVisible() {
            return None;
        }
        let s = tv.string().to_string().trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// A new capture is starting: the panel must not cover the picker — the
    /// frozen backdrop shows it instead (`sinkBelowPicker`).
    pub fn sink_below_picker(&self) {
        if let Some(p) = self.panel.borrow().as_ref() {
            // .floating
            p.setLevel(3);
        }
    }

    pub fn show(
        &self,
        anchor: CGRect,
        image: objc2_core_foundation::CFRetained<CGImage>,
        token: u64,
        on_copy: Box<dyn Fn(String) + 'static>,
    ) {
        *self.on_copy.borrow_mut() = Some(on_copy);
        self.token.set(token);
        let p = self.ensure_panel();
        if let Some(tv) = self.text_view.borrow().as_ref() {
            tv.setString(&NSString::from_str(""));
        }
        if let Some(st) = self.status.borrow().as_ref() {
            st.setStringValue(&NSString::from_str(&l("识别中…")));
        }
        p.setLevel(above_picker_level());
        Self::place(&p, anchor);
        p.orderFrontRegardless();
        unsafe {
            // SAFETY: makeKey has no generated binding.
            let _: () = msg_send![&*p, makeKey];
        }
        // New task cancels the old generation. OCR runs off-main (Vision is
        // thread-safe); the result hops back to the main queue.
        self.task_gen.set(self.task_gen.get() + 1);
        let gen = self.task_gen.get();
        std::thread::spawn(move || {
            let text = crate::annotate::ocr::recognize(&image).unwrap_or_default();
            let raw = Box::into_raw(Box::new(text)) as usize;
            crate::app::delegate::dispatch_main_async(Box::new(move || {
                if gen != controller().task_gen.get() {
                    return;
                }
                let text = unsafe { Box::from_raw(raw as *mut String) };
                controller().show_result(&text);
            }));
        });
    }

    fn show_result(&self, result: &str) {
        if let Some(tv) = self.text_view.borrow().as_ref() {
            tv.setString(&NSString::from_str(result));
        }
        if let Some(st) = self.status.borrow().as_ref() {
            let text = if result.is_empty() {
                l("没有识别到文字")
            } else {
                l("%d 字 · 可直接编辑").replacen("%d", &result.chars().count().to_string(), 1)
            };
            st.setStringValue(&NSString::from_str(&text));
        }
        if !result.is_empty() {
            if let (Some(p), Some(tv)) = (
                self.panel.borrow().as_ref(),
                self.text_view.borrow().as_ref(),
            ) {
                let resp: Option<&objc2_app_kit::NSResponder> = Some(&**tv);
                p.makeFirstResponder(resp);
            }
        }
    }

    /// Self-test only (`debugView`): laid-out content with a sample result.
    pub fn debug_view(&self, sample: &str) -> Retained<NSView> {
        let p = self.ensure_panel();
        self.show_result(sample);
        p.contentView().expect("panel has content")
    }

    pub fn close(&self) {
        self.task_gen.set(self.task_gen.get() + 1);
        if let Some(p) = self.panel.borrow().as_ref() {
            let sender: Option<&AnyObject> = None;
            p.orderOut(sender);
        }
        *self.on_copy.borrow_mut() = None;
    }

    /// `makePanel` — 400×350, hidden title bar, dark aqua, brown ground.
    fn ensure_panel(&self) -> Retained<NSPanel> {
        if let Some(p) = self.panel.borrow().as_ref() {
            return p.clone();
        }
        let mtm = MainThreadMarker::new().expect("main thread");
        let p = {
            let alloc = NSPanel::alloc(mtm);
            NSPanel::initWithContentRect_styleMask_backing_defer(
                alloc,
                CGRect::new(CGPoint::ZERO, CGSize::new(400.0, 350.0)),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::UtilityWindow
                    | NSWindowStyleMask::NonactivatingPanel
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::FullSizeContentView,
                objc2_app_kit::NSBackingStoreType::Buffered,
                false,
            )
        };
        p.setTitle(&NSString::from_str(&l("识别文字")));
        p.setTitleVisibility(objc2_app_kit::NSWindowTitleVisibility::Hidden);
        p.setTitlebarAppearsTransparent(true);
        p.setAppearance(Some(&objc2_app_kit::NSAppearance::appearanceNamed(
            unsafe { NSAppearanceNameDarkAqua },
        )
        .expect("darkAqua exists")));
        p.setBackgroundColor(Some(&theme::brown()));
        p.setLevel(above_picker_level());
        unsafe {
            p.setReleasedWhenClosed(false);
        }
        p.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        p.setMinSize(CGSize::new(280.0, 180.0));

        let (w, h) = (400.0f64, 350.0f64);
        let content = make_ground_view(CGRect::new(CGPoint::ZERO, CGSize::new(w, h)));
        // heading 识别文字 (script 24) at top-left (18, 30 from top).
        let heading = make_label(&l("识别文字"));
        heading.setFont(Some(&theme::script(24.0)));
        heading.setTextColor(Some(&theme::on_brown()));
        let head_h = crate::shelf::card::line_height(&theme::script(24.0));
        heading.setFrame(CGRect::new(
            CGPoint::new(18.0, h - 30.0 - head_h),
            CGSize::new(200.0, head_h),
        ));
        content.addSubview(&heading);

        // paper scroll with the text.
        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
        scroll.setDrawsBackground(true);
        scroll.setBackgroundColor(&theme::paper());
        theme::paper_sheet(&scroll, 4.0);
        let tv = NSTextView::new(mtm);
        tv.setRichText(false);
        tv.setFont(Some(&theme::serif(15.0, false)));
        tv.setTextColor(Some(&theme::ink()));
        tv.setInsertionPointColor(Some(&theme::ink()));
        tv.setBackgroundColor(&theme::paper());
        tv.setTextContainerInset(CGSize::new(14.0, 12.0));
        tv.setAutomaticQuoteSubstitutionEnabled(false);
        tv.setVerticallyResizable(true);
        tv.setHorizontallyResizable(false);
        tv.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
        tv.setMinSize(CGSize::ZERO);
        tv.setMaxSize(CGSize::new(f64::MAX / 2.0_f64.sqrt(), f64::MAX / 2.0_f64.sqrt()));
        if let Some(c) = unsafe { tv.textContainer() } {
            c.setWidthTracksTextView(true);
        }
        scroll.setDocumentView(Some(&tv));
        let btn_y = 12.0;
        let btn_h = 34.0;
        let top_of_scroll = h - 30.0 - head_h - 8.0;
        scroll.setFrame(CGRect::new(
            CGPoint::new(14.0, btn_y + btn_h + 12.0),
            CGSize::new(w - 28.0, top_of_scroll - (btn_y + btn_h + 12.0)),
        ));
        content.addSubview(&scroll);
        *self.text_view.borrow_mut() = Some(tv);

        // status (left), 关闭 + 复制文字 (right).
        let st = make_label("");
        st.setFont(Some(&theme::serif(13.0, false)));
        st.setTextColor(Some(&theme::on_brown_muted()));
        st.setFrame(CGRect::new(
            CGPoint::new(18.0, btn_y + (btn_h - 16.0) / 2.0),
            CGSize::new(200.0, 16.0),
        ));
        content.addSubview(&st);
        *self.status.borrow_mut() = Some(st);

        // SAFETY: the delegate object lives as long as the panel.
        let delegate = OcrPanelWinDelegate::new(mtm);
        let proto: &ProtocolObject<dyn NSWindowDelegate> = ProtocolObject::from_ref(&*delegate);
        p.setDelegate(Some(proto));
        *self.delegate_obj.borrow_mut() = Some(delegate);
        let delegate_obj: &AnyObject =
            unsafe { &*(&**self.delegate_obj.borrow().as_ref().unwrap() as *const OcrPanelWinDelegate as *const AnyObject) };
        let copy = crate::capture::selection_overlay::paper_button_export(
            &l("复制文字"),
            true,
            false,
            Some(delegate_obj),
            sel!(copyTapped:),
        );
        copy.setKeyEquivalent(&NSString::from_str("\r"));
        let cancel = crate::capture::selection_overlay::paper_button_export(
            &l("关闭"),
            false,
            true,
            Some(delegate_obj),
            sel!(closeTapped:),
        );
        let cancel_w = cancel.frame().size.width;
        let copy_w = copy.frame().size.width;
        copy.setFrameOrigin(CGPoint::new(w - 14.0 - copy_w, btn_y));
        cancel.setFrameOrigin(CGPoint::new(w - 14.0 - copy_w - 8.0 - cancel_w, btn_y));
        content.addSubview(&cancel);
        content.addSubview(&copy);

        p.setContentView(Some(&content));
        *self.panel.borrow_mut() = Some(p.clone());
        p
    }

    /// `place(_:near:)` — right of the anchor, left if that overflows.
    fn place(p: &NSPanel, anchor: CGRect) {
        let mtm = MainThreadMarker::new().expect("main thread");
        let screen = screens_intersecting(mtm, anchor);
        let Some(vf) = screen.map(|s| s.visibleFrame()) else {
            p.center();
            return;
        };
        let size = p.frame().size;
        let mut origin = CGPoint::new(
            anchor.max().x + 12.0,
            anchor.max().y - size.height,
        );
        if origin.x + size.width > vf.max().x {
            origin.x = anchor.min().x - size.width - 12.0;
        }
        if origin.x < vf.min().x {
            origin.x = anchor.min().x.min(vf.max().x - size.width);
        }
        origin.y = origin.y.max(vf.min().y).min(vf.max().y - size.height);
        p.setFrameOrigin(origin);
    }
}

// MARK: Grid backdrop + window delegate

pub struct GroundViewIvars;

define_class!(
    // SAFETY: plain backdrop view (RecordingPreviewWindow.GridBackdropView).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = GroundViewIvars]
    pub struct GroundView;

    unsafe impl NSObjectProtocol for GroundView {}

    impl GroundView {
        #[unsafe(method(drawRect:))]
        fn g_draw_rect(&self, _dirty: CGRect) {
            theme::draw_ground(self.bounds());
        }
    }
);

/// `GridBackdropView` for other windows (recording preview shares it).
pub(crate) fn ground_view_export(frame: CGRect) -> Retained<GroundView> {
    make_ground_view(frame)
}

fn make_ground_view(frame: CGRect) -> Retained<GroundView> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let this = mtm.alloc::<GroundView>().set_ivars(GroundViewIvars);
    unsafe { msg_send![super(this), initWithFrame: frame] }
}

fn make_label(s: &str) -> Retained<NSTextField> {
    let mtm = MainThreadMarker::new().expect("main thread");
    NSTextField::labelWithString(&NSString::from_str(s), mtm)
}

/// The first screen intersecting the anchor (place uses its visibleFrame).
pub(crate) fn screens_intersecting(
    mtm: MainThreadMarker,
    anchor: CGRect,
) -> Option<Retained<objc2_app_kit::NSScreen>> {
    objc2_app_kit::NSScreen::screens(mtm)
        .iter()
        .find(|s| crate::app::coordinates::intersects_rect(s.frame(), anchor))
        .or_else(|| objc2_app_kit::NSScreen::mainScreen(mtm))
}

pub struct OcrPanelWinDelegateIvars;

define_class!(
    // SAFETY: NSObject window delegate (windowWillClose cancels the task).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = OcrPanelWinDelegateIvars]
    pub struct OcrPanelWinDelegate;

    unsafe impl NSObjectProtocol for OcrPanelWinDelegate {}

    impl OcrPanelWinDelegate {
        #[unsafe(method(copyTapped:))]
        fn copy_tapped(&self, _sender: &AnyObject) {
            let c = controller();
            let Some(text) = c.current_text() else {
                unsafe {
                    let _: () = msg_send![objc2::class!(NSSound), beep];
                }
                return;
            };
            if let Some(f) = c.on_copy.borrow().as_ref() {
                f(text);
            }
        }

        #[unsafe(method(closeTapped:))]
        fn close_tapped(&self, _sender: &AnyObject) {
            controller().close();
        }
    }

    unsafe impl NSWindowDelegate for OcrPanelWinDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &objc2_foundation::NSNotification) {
            // Swift: task?.cancel() on close.
            controller().task_gen.replace(controller().task_gen.get() + 1);
        }
    }
);

impl OcrPanelWinDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<OcrPanelWinDelegate> {
        let this = mtm.alloc::<OcrPanelWinDelegate>().set_ivars(OcrPanelWinDelegateIvars);
        unsafe { msg_send![super(this), init] }
    }
}

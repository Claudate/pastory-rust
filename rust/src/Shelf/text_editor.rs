//! Port of `Shelf/TextEditorWindow.swift` — a sheet of paper on the desk:
//! read the whole text, change it, 保存 writes it back to the same card and
//! copies it. 620×460 default, min 420×300, paper sheet + handwritten title
//! field + serif 16 with 7pt line spacing.

use std::cell::RefCell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearanceCustomization, NSBackingStoreType, NSButton, NSScrollView, NSTextField,
    NSTextView, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask, };
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSAttributedStringKey, NSNotification, NSString};

use crate::app::localization::l;
use crate::app::theme;
use crate::clipboard::item::ClipItem;

/// One editor per card (`TextEditorWindow.open`). Main-thread only: the map
/// never crosses threads (paste/TCC style like the Swift `@MainActor`).
struct OpenMap(std::cell::UnsafeCell<Option<HashMap<String, Retained<TextEditorWindow>>>>);
unsafe impl Sync for OpenMap {}
static OPEN: OpenMap = OpenMap(std::cell::UnsafeCell::new(None));
fn open_map() -> &'static mut HashMap<String, Retained<TextEditorWindow>> {
    unsafe { (&mut *OPEN.0.get()).get_or_insert_with(HashMap::new) }
}

// MARK: Title field

/// One-line handwritten title box, on the metrics TextEditorWindow expects
/// (line height 30, Caveat at 22). The placeholder is drawn by the owning
/// view; the field itself stays an NSTextView so ⏎/⇥ end editing.
struct TitleFieldIvars {}

define_class!(
    // SAFETY: NSTextView subclass, main-thread only; the field-editor and
    // one-line behaviour is configured in `make`.
    #[unsafe(super(NSTextView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TitleFieldIvars]
    struct TitleField;

    unsafe impl NSObjectProtocol for TitleField {}

    impl TitleField {
        #[unsafe(method(drawRect:))]
        fn tf_draw_rect(&self, _dirty: CGRect) {
            unsafe {
                let _: () = msg_send![super(self), drawRect: _dirty];
            }
            if !self.string().to_string().is_empty() {
                return;
            }
            // The placeholder shares the exact attributes the text carries,
            // so caret, typed text and placeholder share one baseline.
            let attrs = title_attributes();
            let origin = self.textContainerOrigin();
            let placeholder = NSString::from_str(&l("+ 加个标题"));
            let rect = CGRect::new(
                origin,
                CGSize::new(self.bounds().size.width - origin.x * 2.0, 30.0),
            );
            unsafe {
                // SAFETY: NSAttributedStringKey objects from the same font/color
                // set as live typingAttributes.
                use objc2_app_kit::NSStringDrawing;
                placeholder.drawInRect_withAttributes(rect, Some(&attrs));
            }
        }

        #[unsafe(method(didChangeText))]
        fn tf_did_change_text(&self) {
            // One line only: newlines pasted in become spaces; the style never drifts.
            let text = self.string().to_string();
            if text.contains('\n') {
                let flat = text.replace('\n', " ");
                self.setString(&NSString::from_str(&flat));
            }
            apply_title_attributes(self);
            unsafe {
                let _: () = msg_send![super(self), didChangeText];
            }
            self.setNeedsDisplay(true);
        }
    }
);

fn title_attributes() -> Retained<objc2_foundation::NSDictionary<NSAttributedStringKey, AnyObject>> {
    let font = theme::script(22.0);
    let color = theme::label();
    let ps = objc2_app_kit::NSMutableParagraphStyle::new();
    ps.setMinimumLineHeight(30.0);
    ps.setMaximumLineHeight(30.0);
    ps.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    attrs_dict(&font, &color, &ps)
}

fn apply_title_attributes(v: &TitleField) {
    let len = v.string().length();
    let attrs = title_attributes();
    unsafe {
        // SAFETY: attributes cast per title_attributes.
        let _: () = msg_send![&*v, setTypingAttributes: &*attrs];
    }
    if let Some(store) = unsafe { v.textStorage() } {
        let range = objc2_foundation::NSRange { location: 0, length: len };
        unsafe {
            let _: () = msg_send![&*store, setAttributes: &*attrs, range: range];
        }
    }
}

// MARK: Editor window

pub struct TextEditorIvars {
    item_id: RefCell<String>,
    text_view: RefCell<Option<Retained<NSTextView>>>,
    title: RefCell<Option<Retained<TitleField>>>,
    title_wrap: RefCell<Option<Retained<TitleSheetView>>>,
    scroll: RefCell<Option<Retained<NSScrollView>>>,
    count: RefCell<Option<Retained<NSTextField>>>,
    cancel: RefCell<Option<Retained<NSButton>>>,
    save: RefCell<Option<Retained<NSButton>>>,
    /// Whether to re-open the shelf after close (`reopenShelf`).
    reopen_shelf: RefCell<bool>,
    delegate: RefCell<Option<Retained<WindowAndTextDelegate>>>,
}

define_class!(
    // SAFETY: NSWindow + own target, main-thread only; one per item id.
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TextEditorIvars]
    pub struct TextEditorWindow;

    unsafe impl NSObjectProtocol for TextEditorWindow {}

    impl TextEditorWindow {
        #[unsafe(method(cancelTapped:))]
        fn cancel_tapped(&self, _sender: &AnyObject) {
            self.close();
        }

        #[unsafe(method(saveTapped:))]
        fn save_tapped(&self, _sender: &AnyObject) {
            let Some(tv) = self.ivars().text_view.borrow().clone() else { return };
            let text = tv.string().to_string();
            if text.trim().is_empty() {
                unsafe {
                    let _: () = msg_send![objc2::class!(NSSound), beep];
                }
                return;
            }
            let id = self.ivars().item_id.borrow().clone();
            crate::clipboard::store::with(|s| {
                s.update_text(&id, &text);
                if let Some(t) = &self.ivars().title.borrow().clone() {
                    let title = t.string().to_string();
                    s.set_title(if title.trim().is_empty() { None } else { Some(title.trim()) }, &id);
                }
                if let Some(updated) = s.items.iter().find(|it| it.id == id).cloned() {
                    s.copy_to_pasteboard(&updated);
                }
            });
            self.close();
        }
    }
);



impl TextEditorWindow {
    /// `open(_:)` — reopen the live editor if one exists for this card.
    pub fn open(item: &ClipItem) {
        let map = open_map();
        if let Some(w) = map.get(&item.id) {
            w.makeKeyAndOrderFront(None);
            return;
        }
        let e = Self::make(item);
        map.insert(item.id.clone(), e.clone());
        *e.ivars().reopen_shelf.borrow_mut() = crate::shelf::panel::is_visible();
        crate::shelf::panel::hide();
        // The strip must not climb back over this window (`external_hold`).
        crate::shelf::line_panel::external_window_opened();
        crate::app::activation::activate_app();
        e.makeKeyAndOrderFront(None);
    }

    /// Self-test only: is the live editor for `id` the key window? The renders
    /// above never show a window, so they cannot answer this.
    pub fn debug_window_is_key(id: &str) -> Option<bool> {
        open_map().get(id).map(|w| w.isKeyWindow())
    }

    /// Self-test only: close the live editor for `id` (the cancel button's route).
    pub fn debug_close(id: &str) {
        if let Some(w) = open_map().get(id) {
            w.close();
        }
    }

    /// Self-test only: the content view, laid out, without showing the window.
    pub fn debug_view(item: &ClipItem) -> Option<Retained<NSView>> {
        Self::debug_view_sized(item, None)
    }

    /// Self-test only: the same content view, forced to `size` and re-laid-out
    /// through the resize path — the difference between "placed once in
    /// `make`" and "follows the window", which the 620pt render cannot show.
    pub(crate) fn debug_view_sized(
        item: &ClipItem,
        size: Option<CGSize>,
    ) -> Option<Retained<NSView>> {
        let e = Self::make(item);
        open_map().insert(item.id.clone(), e.clone());
        if let (Some(v), Some(s)) = (e.contentView(), size) {
            v.setFrame(CGRect::new(CGPoint::ZERO, s));
            e.layout_content();
            e.update_count();
        }
        e.contentView()
    }

    /// Self-test only: build the editor, run the resize path for a content box
    /// of `size`, and report every placed frame as `(name, x, y, w, h)` — so a
    /// resize that does nothing reads as frames still at the 620pt origin.
    pub(crate) fn debug_layout_at(
        item: &ClipItem,
        size: CGSize,
    ) -> Vec<(&'static str, f64, f64, f64, f64)> {
        let e = Self::make(item);
        open_map().insert(item.id.clone(), e.clone());
        if let Some(v) = e.contentView() {
            v.setFrame(CGRect::new(CGPoint::ZERO, size));
        }
        e.layout_content();
        let mut out: Vec<(&'static str, f64, f64, f64, f64)> = Vec::new();
        let mut push = |name: &'static str, f: Option<CGRect>| {
            if let Some(f) = f {
                out.push((name, f.min().x, f.min().y, f.size.width, f.size.height));
            }
        };
        push("sheet", e.ivars().title_wrap.borrow().as_ref().map(|v| v.frame()));
        push("field", e.ivars().title.borrow().as_ref().map(|v| v.frame()));
        push("scroll", e.ivars().scroll.borrow().as_ref().map(|v| v.frame()));
        push("count", e.ivars().count.borrow().as_ref().map(|v| v.frame()));
        push("cancel", e.ivars().cancel.borrow().as_ref().map(|v| v.frame()));
        push("save", e.ivars().save.borrow().as_ref().map(|v| v.frame()));
        out
    }

    fn make(item: &ClipItem) -> Retained<Self> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let rect = CGRect::new(CGPoint::ZERO, CGSize::new(620.0, 460.0));
        let this = mtm.alloc::<TextEditorWindow>().set_ivars(TextEditorIvars {
            item_id: RefCell::new(item.id.clone()),
            text_view: RefCell::new(None),
            title: RefCell::new(None),
            title_wrap: RefCell::new(None),
            scroll: RefCell::new(None),
            count: RefCell::new(None),
            cancel: RefCell::new(None),
            save: RefCell::new(None),
            reopen_shelf: RefCell::new(false),
            delegate: RefCell::new(None),
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
        this.setTitle(&NSString::from_str(&l("编辑文字")));
        this.setTitlebarAppearsTransparent(true);
        this.setAppearance(Some(&objc2_app_kit::NSAppearance::appearanceNamed(
            unsafe { objc2_app_kit::NSAppearanceNameDarkAqua },
        ).expect("dark aqua")));
        this.setBackgroundColor(Some(&theme::ground()));
        this.setMinSize(CGSize::new(420.0, 300.0));
        unsafe { this.setReleasedWhenClosed(false) };

        // Delegate: windowWillClose + textDidChange.
        let delegate: Retained<WindowAndTextDelegate> = {
            let d = mtm.alloc::<WindowAndTextDelegate>().set_ivars(WindowAndTextDelegateIvars {
                editor: RefCell::new(Retained::as_ptr(&this) as usize),
            });
            unsafe { msg_send![super(d), init] }
        };
        *this.ivars().delegate.borrow_mut() = Some(delegate.clone());
        // window delegate (protocol cast)
        this.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate)));
        let content = crate::annotate::ocr_panel::ground_view_export(rect);

        // Title sheet + field (paper with the handwritten hand).
        let title_wrap = TitleSheetView::new(CGRect::new(CGPoint::new(16.0, 460.0 - 44.0 - 52.0), CGSize::new(620.0 - 32.0, 52.0)));
        let title_field: Retained<TitleField> = {
            let t = mtm.alloc::<TitleField>().set_ivars(TitleFieldIvars {});
            let t: Retained<TitleField> = unsafe {
                msg_send![super(t), initWithFrame: CGRect::new(CGPoint::new(12.0, (52.0 - 36.0) / 2.0), CGSize::new(620.0 - 32.0 - 24.0, 36.0))]
            };
            t
        };
        title_field.setRichText(false);
        unsafe {
            // SAFETY: one-line field editor per Swift.
            let _: () = msg_send![&*title_field, setFieldEditor: true];
        }
        title_field.setDrawsBackground(false);
        title_field.setFocusRingType(objc2_app_kit::NSFocusRingType(0));
        title_field.setInsertionPointColor(Some(&theme::label()));
        title_field.setTextContainerInset(CGSize::new(0.0, 3.0));
        if let Some(container) = unsafe { title_field.textContainer() } {
            container.setLineFragmentPadding(0.0);
            container.setMaximumNumberOfLines(1);
        }
        title_field.setAutomaticQuoteSubstitutionEnabled(false);
        title_field.setAutomaticDashSubstitutionEnabled(false);
        apply_title_attributes(&title_field);
        if let Some(title) = &item.title {
            title_field.setString(&NSString::from_str(title));
            apply_title_attributes(&title_field);
        }
        title_wrap.addSubview(&title_field);
        content.addSubview(&title_wrap);
        *this.ivars().title.borrow_mut() = Some(title_field);
        *this.ivars().title_wrap.borrow_mut() = Some(title_wrap);

        // Body: paper scroll + text view (7pt leading).
        let scroll_rect = CGRect::new(CGPoint::new(16.0, 44.0 + 52.0 + 10.0), CGSize::new(620.0 - 32.0, 460.0 - 44.0 - 52.0 - 10.0 - 14.0 - 44.0));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), scroll_rect);
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(true);
        scroll.setBackgroundColor(&theme::surface());
        scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
        theme::card_layer(&scroll, 4.0);
        let text = crate::clipboard::store::read(|s| {
            s.items.iter().find(|it| it.id == item.id).map(|it| s.text(it).unwrap_or_default())
        }).unwrap_or_default();
        let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), scroll_rect);
        tv.setString(&NSString::from_str(&text));
        tv.setRichText(false);
        tv.setFont(Some(&theme::serif(16.0, false)));
        tv.setTextColor(Some(&theme::label()));
        tv.setInsertionPointColor(Some(&theme::label()));
        tv.setBackgroundColor(&theme::surface());
        tv.setTextContainerInset(CGSize::new(18.0, 16.0));
        let para = objc2_app_kit::NSMutableParagraphStyle::new();
        para.setLineSpacing(7.0);
        para.setParagraphSpacing(4.0);
        tv.setDefaultParagraphStyle(Some(&para));
        let font = theme::serif(16.0, false);
        let color = theme::label();
        unsafe {
            // SAFETY: typing attributes with the live font/color/paragraph.
            let attrs = title_like_attributes(&font, &color, &para);
            let _: () = msg_send![&*tv, setTypingAttributes: &*attrs];
        }
        if let Some(store) = unsafe { tv.textStorage() } {
            let range = objc2_foundation::NSRange { location: 0, length: store.string().length() };
            let color = theme::label();
            let add_attrs = title_like_attributes(&font, &color, &para);
            unsafe { store.addAttributes_range(&add_attrs, range) };
        }
        tv.setAutomaticQuoteSubstitutionEnabled(false);
        tv.setAutomaticDashSubstitutionEnabled(false);
        tv.setVerticallyResizable(true);
        if let Some(container) = unsafe { tv.textContainer() } {
            container.setWidthTracksTextView(true);
        }
        // The text view already fills the scroll's content area.
        scroll.setDocumentView(Some(&tv));
        tv.setFrame(CGRect::new(CGPoint::ZERO, scroll.contentSize()));
        content.addSubview(&scroll);
        *this.ivars().scroll.borrow_mut() = Some(scroll);
        // text delegate for the counter
        unsafe {
            let d: Option<&AnyObject> = Some(&*(&*delegate as *const WindowAndTextDelegate as *const AnyObject));
            let _: () = msg_send![&*tv, setDelegate: d];
        }
        *this.ivars().text_view.borrow_mut() = Some(tv);

        // Footer: count + buttons.
        let count = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        count.setFont(Some(&theme::serif(13.0, false)));
        count.setTextColor(Some(&theme::label_tertiary()));
        place_simple(&*count, 20.0, 14.0, 220.0, 36.0);
        content.addSubview(&count);
        *this.ivars().count.borrow_mut() = Some(count);
        let editor_obj: Option<&AnyObject> = unsafe { Some(&*(&*this as *const TextEditorWindow as *const AnyObject)) };
        let cancel = crate::capture::selection_overlay::chrome_button_export(
            &l("取消"), false, true, editor_obj, sel!(cancelTapped:),
        );
        let save = crate::capture::selection_overlay::chrome_button_export(
            &l("保存并复制"), true, true, editor_obj, sel!(saveTapped:),
        );
        save.setToolTip(Some(&NSString::from_str("⌘⏎")));
        save.setKeyEquivalent(&NSString::from_str("\r"));
        save.setKeyEquivalentModifierMask(objc2_app_kit::NSEventModifierFlags::Command);
        let cb = cancel.frame().size;
        let sb = save.frame().size;
        place_simple(&*cancel, 620.0 - 16.0 - cb.width - 8.0 - sb.width, 14.0, cb.width, cb.height);
        place_simple(&*save, 620.0 - 16.0 - sb.width, 14.0, sb.width, sb.height);
        content.addSubview(&cancel);
        content.addSubview(&save);
        *this.ivars().cancel.borrow_mut() = Some(cancel);
        *this.ivars().save.borrow_mut() = Some(save);
        this.setContentView(Some(&content));
        this.center();
        this.layout_content();
        this.update_count();
        this
    }


    /// Re-place the sheet, the body scroll and the footer for the window's
    /// current size. `make` runs it once; every later resize arrives through
    /// the delegate's `windowDidResize:`. Without it a shorter window cut the
    /// footer off and a wider one left the sheet at its original 620pt.
    pub(crate) fn layout_content(&self) {
        let Some(content) = self.contentView() else { return };
        let size = content.bounds().size;
        let (w, h) = (size.width, size.height);
        /// Titlebar strip the full-size content view runs under.
        const TOP_INSET: f64 = 44.0;
        /// Side margin for the sheet and the body.
        const MARGIN: f64 = 16.0;
        /// Height of the handwritten title sheet.
        const SHEET_H: f64 = 52.0;
        /// Room the count line and the buttons ask for at the bottom.
        const FOOTER: f64 = 58.0;

        let inner_w = (w - MARGIN * 2.0).max(1.0);

        // The sheet hangs from the top; the body takes whatever is left
        // between it and the footer, so neither ever leaves the window.
        let sheet_y = (h - TOP_INSET - SHEET_H).max(FOOTER + 24.0);
        if let Some(wrap) = self.ivars().title_wrap.borrow().as_ref() {
            wrap.setFrame(CGRect::new(
                CGPoint::new(MARGIN, sheet_y),
                CGSize::new(inner_w, SHEET_H),
            ));
        }
        if let Some(field) = self.ivars().title.borrow().as_ref() {
            field.setFrame(CGRect::new(
                CGPoint::new(12.0, (SHEET_H - 36.0) / 2.0),
                CGSize::new((inner_w - 24.0).max(1.0), 36.0),
            ));
        }

        let scroll_h = (sheet_y - 8.0 - FOOTER).max(40.0);
        if let Some(scroll) = self.ivars().scroll.borrow().as_ref() {
            scroll.setFrame(CGRect::new(
                CGPoint::new(MARGIN, FOOTER),
                CGSize::new(inner_w, scroll_h),
            ));
            // The text view tracks the box's width and grows downward with
            // its content, so the document height comes from the box.
            if let Some(tv) = self.ivars().text_view.borrow().as_ref() {
                let box_size = scroll.contentSize();
                let mut f = tv.frame();
                f.size.width = box_size.width;
                f.size.height = f.size.height.max(box_size.height);
                tv.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
                tv.setMinSize(CGSize::new(0.0, box_size.height));
                tv.setMaxSize(CGSize::new(f64::MAX / 2.0_f64.sqrt(), f64::MAX / 2.0_f64.sqrt()));
                tv.setFrame(f);
            }
        }

        // Buttons ride the bottom-right corner at their own widths; the
        // counter gets what is left rather than running under them at the
        // 420pt minimum.
        let cancel = self.ivars().cancel.borrow().clone();
        let save = self.ivars().save.borrow().clone();
        if let (Some(cancel), Some(save)) = (cancel, save) {
            let cb = cancel.frame().size;
            let sb = save.frame().size;
            place_simple(&*save, w - MARGIN - sb.width, 14.0, sb.width, sb.height);
            place_simple(
                &*cancel,
                w - MARGIN - sb.width - 8.0 - cb.width,
                14.0,
                cb.width,
                cb.height,
            );
            if let Some(count) = self.ivars().count.borrow().as_ref() {
                let cw = (w - MARGIN - sb.width - 8.0 - cb.width - 24.0).max(40.0);
                place_simple(&*count, 20.0, 14.0, cw, 36.0);
            }
        }
    }

    fn content_view_raw(&self) -> Option<Retained<NSView>> {
        self.contentView()
    }

    fn update_count(&self) {
        let Some(tv) = self.ivars().text_view.borrow().clone() else { return };
        let n = tv.string().to_string().chars().count();
        if let Some(c) = self.ivars().count.borrow().as_ref() {
            c.setStringValue(&NSString::from_str(&l("%d 字").replacen("%d", &n.to_string(), 1)));
        }
    }
}

// MARK: Delegate side

struct WindowAndTextDelegateIvars {
    editor: RefCell<usize>,
}

define_class!(
    // SAFETY: NSObject window+text delegate; main-thread only. Selector-
    // driven (informal methods — M4 informal-protocol law).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = WindowAndTextDelegateIvars]
    struct WindowAndTextDelegate;

    unsafe impl NSObjectProtocol for WindowAndTextDelegate {}

    unsafe impl NSWindowDelegate for WindowAndTextDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            let raw = *self.ivars().editor.borrow();
            let editor: &TextEditorWindow = unsafe { &*(raw as *const TextEditorWindow) };
            let id = editor.ivars().item_id.borrow().clone();
            open_map().remove(&id);
            crate::shelf::line_panel::external_window_closed();
            if *editor.ivars().reopen_shelf.borrow() {
                crate::shelf::panel::show();
            }
        }
    }

    impl WindowAndTextDelegate {
        /// Re-place the sheet / body / footer as the window changes size.
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _n: &NSNotification) {
            let raw = *self.ivars().editor.borrow();
            let editor: &TextEditorWindow = unsafe { &*(raw as *const TextEditorWindow) };
            editor.layout_content();
        }

        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _n: &NSNotification) {
            let raw = *self.ivars().editor.borrow();
            let editor: &TextEditorWindow = unsafe { &*(raw as *const TextEditorWindow) };
            editor.update_count();
        }
    }
);

/// Title-sheet sub-view (paper with hairline edge + shadow) — reuse the
/// paper sheet shape the Swift's `PaperSheetView` wraps.
fn title_like_attributes(font: &objc2_app_kit::NSFont, color: &objc2_app_kit::NSColor, para: &objc2_app_kit::NSMutableParagraphStyle) -> Retained<objc2_foundation::NSDictionary<NSAttributedStringKey, AnyObject>> {
    let ps_cast: &objc2_app_kit::NSParagraphStyle = para;
    attrs_dict(font, color, ps_cast)
}

// MARK: Title sheet + layout helpers

struct TitleSheetIvars {}

define_class!(
    // SAFETY: plain NSView with paper draws; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TitleSheetIvars]
    struct TitleSheetView;

    unsafe impl NSObjectProtocol for TitleSheetView {}

    impl TitleSheetView {
        #[unsafe(method(drawRect:))]
        fn sheet_draw(&self, _dirty: CGRect) {
            let b = self.bounds();
            let paths = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                crate::app::coordinates::inset_rect(b, 0.5, 0.5),
                4.0,
                4.0,
            );
            theme::draw_surface(&paths, &theme::surface());
        }
    }
);

impl TitleSheetView {
    fn new(frame: CGRect) -> Retained<Self> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<TitleSheetView>().set_ivars(TitleSheetIvars {});
        let v: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        theme::card_layer(&v, 4.0);
        v
    }
}

fn place_simple(view: &NSView, x: f64, y: f64, w: f64, h: f64) {
    view.setFrame(CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)));
}


/// Shared attribute-dictionary construct: font + ink + paragraph.
fn attrs_dict(
    font: &objc2_app_kit::NSFont,
    color: &objc2_app_kit::NSColor,
    para: &objc2_app_kit::NSParagraphStyle,
) -> Retained<objc2_foundation::NSDictionary<NSAttributedStringKey, AnyObject>> {
    use objc2_foundation::NSDictionary;
    let keys = unsafe { [
        objc2_app_kit::NSFontAttributeName,
        objc2_app_kit::NSForegroundColorAttributeName,
        objc2_app_kit::NSParagraphStyleAttributeName,
    ] };
    let f: &AnyObject = unsafe { &*(font as *const objc2_app_kit::NSFont as *const AnyObject) };
    let c: &AnyObject = unsafe { &*(color as *const objc2_app_kit::NSColor as *const AnyObject) };
    let p: &AnyObject = unsafe { &*(para as *const objc2_app_kit::NSParagraphStyle as *const AnyObject) };
    let values: [&AnyObject; 3] = [f, c, p];
    NSDictionary::from_slices(&keys, &values)
}

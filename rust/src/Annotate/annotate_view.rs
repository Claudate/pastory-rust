//! Port of `Annotate/AnnotateView.swift`.
//!
//! The frozen capture with vector annotations on top. Flipped: y grows
//! downward, like the image. The current tool stays active. Clicking a drawn
//! element selects it instead of drawing: drag to move, pull a handle to
//! reshape, hit the ✕ bubble or ⌫ to delete, click selected text to edit it.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSColor, NSEvent, NSImage, NSResponder, NSTextViewDelegate, NSView,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSNotification, NSString};

use crate::annotate::annotation::{
    palette, AnnotateTool, Annotation, StrokeSize,
};
use crate::annotate::renderer::{self, DELETE_RADIUS, HANDLE_RADIUS};
use crate::annotate::text_view::{make as make_text_view, AnnotationTextView};

/// `AnnotateDelegate`.
pub trait AnnotateDelegate {
    fn did_finish(&mut self, image: CFRetained<CGImage>);
    fn did_cancel(&mut self);
    fn request_ocr(&mut self, image: CFRetained<CGImage>);
    fn request_record(&mut self);
    /// Drag on empty canvas with no tool: move the whole selection by
    /// (dx, dy) in window points.
    fn move_region(&mut self, dx: f64, dy: f64) {
        let _ = (dx, dy);
    }
}

enum Drag {
    Move { last: CGPoint },
    Handle { index: usize, original: Annotation, offset: CGPoint },
    Region { last_window: CGPoint },
}

pub struct AnnotateViewIvars {
    image: RefCell<Option<CFRetained<CGImage>>>,
    ns_image: RefCell<Option<Retained<NSImage>>>,
    delegate: RefCell<Option<Box<dyn AnnotateDelegate>>>,
    on_state_change: RefCell<Option<Box<dyn Fn() + 'static>>>,
    /// Record-ready: the frame is adjustable, annotations are hidden,
    /// ⏎ or a double-click starts recording.
    record_mode: Cell<bool>,
    /// nil = no tool: clicks only select / move; nothing gets drawn.
    tool: Cell<Option<AnnotateTool>>,
    color: RefCell<Option<Retained<NSColor>>>,
    size: Cell<StrokeSize>,
    annotations: RefCell<Vec<Annotation>>,
    draft: RefCell<Option<Annotation>>,
    selected_id: Cell<Option<u64>>,
    drag: RefCell<Option<Drag>>,
    moved: Cell<bool>,
    /// Set when a click lands on already-selected text; becomes an edit if
    /// the mouse does not move.
    pending_edit: Cell<Option<u64>>,
    editor: RefCell<Option<Retained<AnnotationTextView>>>,
    editor_anchor: Cell<CGPoint>,
    editor_box_size: RefCell<CGSize>,
    editor_auto_width: Cell<bool>,
    editing_id: Cell<Option<u64>>,
    editor_anchor_for_sizing: Cell<CGPoint>,
}

impl Default for AnnotateViewIvars {
    fn default() -> Self {
        Self {
            image: RefCell::new(None),
            ns_image: RefCell::new(None),
            delegate: RefCell::new(None),
            on_state_change: RefCell::new(None),
            record_mode: Cell::new(false),
            tool: Cell::new(None),
            color: RefCell::new(None),
            size: Cell::new(StrokeSize::S),
            annotations: RefCell::new(Vec::new()),
            draft: RefCell::new(None),
            selected_id: Cell::new(None),
            drag: RefCell::new(None),
            moved: Cell::new(false),
            pending_edit: Cell::new(None),
            editor: RefCell::new(None),
            editor_anchor: Cell::new(CGPoint::ZERO),
            editor_box_size: RefCell::new(CGSize::ZERO),
            editor_auto_width: Cell::new(true),
            editing_id: Cell::new(None),
            editor_anchor_for_sizing: Cell::new(CGPoint::ZERO),
        }
    }
}

define_class!(
    // SAFETY:
    // - NSView subclass, main-thread only, flipped like the image (y-down).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AnnotateViewIvars]
    pub struct AnnotateView;

    unsafe impl NSObjectProtocol for AnnotateView {}

    impl AnnotateView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: CGPoint) -> *mut NSView {
            // The editor fills its box, but its resize handles still belong
            // to the canvas.
            let p = self.convertPoint_fromView(point, unsafe { self.superview() }.as_deref());
            if let Some(a) = self.editing_annotation() {
                if Self::on_text_chrome(&a, p) {
                    return self as *const Self as *mut NSView;
                }
            }
            unsafe { msg_send![super(self), hitTest: point] }
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let c = match self.tool() {
                None => objc2_app_kit::NSCursor::arrowCursor(),
                Some(AnnotateTool::Text) => objc2_app_kit::NSCursor::IBeamCursor(),
                Some(_) => objc2_app_kit::NSCursor::crosshairCursor(),
            };
            self.addCursorRect_cursor(self.bounds(), &c);
            // Selection chrome gets the pointer: the delete button and the
            // handles are buttons, not canvas.
            if let Some(a) = self.editing_annotation().or_else(|| self.selected()) {
                self.addCursorRect_cursor(
                    crate::app::coordinates::inset_rect(renderer::delete_rect(&a), -2.0, -2.0),
                    &objc2_app_kit::NSCursor::pointingHandCursor(),
                );
                for (i, h) in renderer::selection_handles(&a).iter().enumerate() {
                    // Swift uses the classic resize cursors; deprecated but
                    // the visual match matters (M7 note if they go away).
                    #[allow(deprecated)]
                    let cursor = if a.tool == AnnotateTool::Text && i >= 4 {
                        if i < 6 {
                            objc2_app_kit::NSCursor::resizeLeftRightCursor()
                        } else {
                            objc2_app_kit::NSCursor::resizeUpDownCursor()
                        }
                    } else {
                        objc2_app_kit::NSCursor::arrowCursor()
                    };
                    self.addCursorRect_cursor(
                        CGRect::new(CGPoint::new(h.x - 8.0, h.y - 8.0), CGSize::new(16.0, 16.0)),
                        &cursor,
                    );
                }
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            if let Some(img) = self.ivars().ns_image.borrow().as_ref() {
                unsafe { img.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                    self.bounds(),
                    CGRect::ZERO,
                    objc2_app_kit::NSCompositingOperation::SourceOver,
                    1.0,
                    true,
                    None,
                ) };
            }
            if self.record_mode() {
                // The recording will not carry annotations; do not show them.
                return;
            }
            let Some(gc) = objc2_app_kit::NSGraphicsContext::currentContext() else {
                return;
            };
            let cg = gc.CGContext();
            let image = self.ivars().image.borrow().clone();
            let Some(image) = image else { return };
            let ppp = objc2_core_graphics::CGImage::width(Some(&image)) as f64 / self.bounds().size.width;
            for a in self.ivars().annotations.borrow().iter() {
                if Some(a.id) != self.ivars().editing_id.get() {
                    renderer::draw(a, &cg, &image, ppp);
                }
            }
            if let Some(d) = self.ivars().draft.borrow().as_ref() {
                renderer::draw(d, &cg, &image, ppp);
            }
            if let Some(a) = self.editing_annotation().or_else(|| self.selected()) {
                renderer::draw_selection(&a, &cg);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn av_mouse_down(&self, event: &NSEvent) {
            self.annotate_mouse_down(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn av_mouse_dragged(&self, event: &NSEvent) {
            self.annotate_mouse_dragged(event);
        }

        #[unsafe(method(mouseUp:))]
        fn av_mouse_up(&self, event: &NSEvent) {
            self.annotate_mouse_up(event);
        }

        #[unsafe(method(keyDown:))]
        fn av_key_down(&self, event: &NSEvent) {
            self.annotate_key_down(event);
        }

        #[unsafe(method(cancelOperation:))]
        fn av_cancel_operation(&self, _sender: Option<&AnyObject>) {
            self.cancel();
        }
    }

    // NSTextViewDelegate is an informal-protocol shape here (same pattern as
    // the Shelf QL data source): the two methods are just registered with
    // their exact selectors and the canvas is set as the editor's delegate.
    impl AnnotateView {
        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            self.layout_editor();
        }

        #[unsafe(method(textView:doCommandBy:))]
        fn text_view_do_command_by(
            &self,
            text_view: &NSView,
            command_selector: objc2::runtime::Sel,
        ) -> objc2::runtime::Bool {
            self.text_do_command(text_view, command_selector)
        }
    }
);

/// A text box's grips and delete chip. Points inside the box itself are
/// text, never a grip (`onTextChrome`).
fn hypot(a: f64, b: f64) -> f64 {
    (a * a + b * b).sqrt()
}

impl AnnotateView {
    fn on_text_chrome(a: &Annotation, p: CGPoint) -> bool {
        if crate::app::coordinates::contains_pt(renderer::delete_rect(a), p) {
            return true;
        }
        if crate::app::coordinates::contains_pt(a.bounds(), p) {
            return false;
        }
        renderer::selection_handles(a)
            .iter()
            .any(|h| hypot(p.x - h.x, p.y - h.y) <= HANDLE_RADIUS + 4.0)
    }

    /// `init(frame:image:)`.
    pub fn make(frame: CGRect, image: CFRetained<CGImage>) -> Retained<AnnotateView> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<AnnotateView>().set_ivars(AnnotateViewIvars::default());
        let view: Retained<AnnotateView> = unsafe {
            msg_send![super(this), initWithFrame: frame]
        };
        let ns_img = NSImage::initWithCGImage_size(mtm.alloc::<NSImage>(), &image, frame.size);
        *view.ivars().image.borrow_mut() = Some(image);
        *view.ivars().ns_image.borrow_mut() = Some(ns_img);
        let palette = palette();
        *view.ivars().color.borrow_mut() = Some(palette[0].clone());
        view.setWantsLayer(true);
        view
    }

    pub fn set_delegate(&self, d: Box<dyn AnnotateDelegate>) {
        *self.ivars().delegate.borrow_mut() = Some(d);
    }

    pub fn set_on_state_change(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_state_change.borrow_mut() = Some(f);
        self.state_change();
    }

    fn state_change(&self) {
        if let Some(f) = self.ivars().on_state_change.borrow().as_ref() {
            f();
        }
    }

    fn invalidate(&self) {
        self.setNeedsDisplay(true);
    }

    pub fn record_mode(&self) -> bool {
        self.ivars().record_mode.get()
    }
    pub fn set_record_mode(&self, on: bool) {
        self.ivars().record_mode.set(on);
        if on {
            self.set_tool(None);
            self.set_selected_id(None);
        }
        self.invalidate();
    }

    pub fn tool(&self) -> Option<AnnotateTool> {
        self.ivars().tool.get()
    }
    pub fn set_tool(&self, tool: Option<AnnotateTool>) {
        self.ivars().tool.set(tool);
        self.commit_text_editor();
        if let Some(w) = self.window() {
            w.invalidateCursorRectsForView(self.as_ns_view());
        }
        self.state_change();
    }

    pub fn color(&self) -> Retained<NSColor> {
        self.ivars().color.borrow().clone().expect("set at make")
    }
    pub fn set_color(&self, c: Retained<NSColor>) {
        *self.ivars().color.borrow_mut() = Some(c.clone());
        self.apply_to_selected(|a| a.color = c.clone());
        self.restyle_editor();
    }

    pub fn size(&self) -> StrokeSize {
        self.ivars().size.get()
    }
    pub fn set_size(&self, s: StrokeSize) {
        self.ivars().size.set(s);
        self.apply_to_selected(|a| a.size = s);
        self.restyle_editor();
    }

    pub fn annotations(&self) -> Vec<Annotation> {
        self.ivars().annotations.borrow().clone()
    }
    fn mutate_annotations<R>(&self, f: impl FnOnce(&mut Vec<Annotation>) -> R) -> R {
        let r = f(&mut self.ivars().annotations.borrow_mut());
        self.invalidate();
        self.state_change();
        r
    }

    pub fn set_annotations_and_select(&self, list: Vec<Annotation>, select: Option<usize>) {
        self.mutate_annotations(|v| {
            *v = list;
        });
        if let Some(i) = select {
            if i < self.annotations().len() {
                let id = self.annotations()[i].id;
                self.set_selected_id(Some(id));
            }
        }
    }

    pub fn selected_id(&self) -> Option<u64> {
        self.ivars().selected_id.get()
    }
    pub fn set_selected_id(&self, id: Option<u64>) {
        self.ivars().selected_id.set(id);
        self.invalidate();
        if let Some(w) = self.window() {
            w.invalidateCursorRectsForView(self.as_ns_view());
        }
        self.state_change();
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.selected_id()?;
        self.annotations().iter().position(|a| a.id == id)
    }
    pub fn selected(&self) -> Option<Annotation> {
        self.selected_index().map(|i| self.annotations()[i].clone())
    }
    pub fn active_kind(&self) -> Option<AnnotateTool> {
        self.selected().map(|a| a.tool).or_else(|| self.tool())
    }
    pub fn effective_color(&self) -> Retained<NSColor> {
        self.selected().map(|a| a.color.clone()).unwrap_or_else(|| self.color())
    }
    pub fn effective_size(&self) -> StrokeSize {
        self.selected().map(|a| a.size).unwrap_or_else(|| self.size())
    }
    pub fn can_undo(&self) -> bool {
        !self.annotations().is_empty()
    }

    pub fn undo(&self) {
        self.commit_text_editor();
        self.mutate_annotations(|v| {
            v.pop();
        });
        self.set_selected_id(None);
    }

    pub fn delete_selected(&self) {
        let Some(id) = self.selected_id() else { return };
        self.mutate_annotations(|v| v.retain(|a| a.id != id));
        self.set_selected_id(None);
    }

    fn apply_to_selected(&self, change: impl FnOnce(&mut Annotation)) {
        if let Some(i) = self.selected_index() {
            self.mutate_annotations(|v| {
                if let Some(a) = v.get_mut(i) {
                    change(a);
                }
            });
        } else {
            self.state_change();
        }
    }

    fn as_ns_view(&self) -> &NSView {
        unsafe { &*(self as *const Self as *const NSView) }
    }

    pub fn image(&self) -> CFRetained<CGImage> {
        self.ivars().image.borrow().clone().expect("set at make")
    }

    /// Flattened result (`renderedImage`).
    pub fn rendered_image(&self) -> CFRetained<CGImage> {
        self.commit_text_editor();
        let image = self.image();
        renderer::render(&image, &self.annotations(), self.bounds().size)
            .unwrap_or(image)
    }

    pub fn finish(&self) {
        if let Some(d) = self.ivars().delegate.borrow_mut().as_mut() {
            d.did_finish(self.rendered_image());
        }
    }

    pub fn cancel(&self) {
        if let Some(d) = self.ivars().delegate.borrow_mut().as_mut() {
            d.did_cancel();
        }
    }

    pub fn request_ocr(&self) {
        self.commit_text_editor();
        if let Some(d) = self.ivars().delegate.borrow_mut().as_mut() {
            d.request_ocr(self.image());
        }
    }

    pub fn request_record(&self) {
        self.commit_text_editor();
        if let Some(d) = self.ivars().delegate.borrow_mut().as_mut() {
            d.request_record();
        }
    }
}

// MARK: Region resize + text editor

impl AnnotateView {
    /// `replaceImage(_:frame:)` — region resized: new crop, new frame;
    /// annotations stay put on screen.
    pub fn replace_image(&self, img: CFRetained<CGImage>, new_frame: CGRect) {
        self.commit_text_editor();
        let old = self.frame();
        let d = CGPoint::new(
            old.origin.x - new_frame.origin.x,
            new_frame.origin.y + new_frame.size.height - (old.origin.y + old.size.height),
        );
        self.mutate_annotations(|v| {
            for a in v.iter_mut() {
                a.translate(d);
            }
        });
        let mtm = MainThreadMarker::new().expect("main thread");
        let ns_img = NSImage::initWithCGImage_size(mtm.alloc::<NSImage>(), &img, new_frame.size);
        *self.ivars().image.borrow_mut() = Some(img);
        *self.ivars().ns_image.borrow_mut() = Some(ns_img);
        self.setFrame(new_frame);
        self.invalidate();
    }

    fn editing_annotation(&self) -> Option<Annotation> {
        if self.ivars().editor.borrow().is_none() {
            return None;
        }
        let tv = self.ivars().editor.borrow().clone();
        let text = tv?.string().to_string();
        Some(Annotation {
            // A preview copy: ids ≥1 belong to committed annotations, so 0
            // never collides (Swift: a fresh Annotation with a fresh UUID).
            id: 0,
            tool: AnnotateTool::Text,
            color: self.color(),
            size: self.size(),
            points: vec![self.ivars().editor_anchor.get()],
            text,
            text_box_size: Some(*self.ivars().editor_box_size.borrow()),
            seed: 1,
        })
    }

    /// nil = "hug the text". A box that ran into the right edge wrapped
    /// there, so it keeps that width.
    fn committed_box_size(&self, text: &str) -> Option<CGSize> {
        if !self.ivars().editor_auto_width.get() {
            return Some(*self.ivars().editor_box_size.borrow());
        }
        let natural = crate::annotate::annotation::measure_hand_text(text, self.size()) + 6.0;
        if natural <= self.ivars().editor_box_size.borrow().width + 0.5 {
            None
        } else {
            Some(*self.ivars().editor_box_size.borrow())
        }
    }

    /// Width that just fits the longest line (or the placeholder), never
    /// past the right edge of the picture.
    fn auto_width(&self, text: &str) -> f64 {
        let measure = if text.is_empty() {
            crate::app::localization::l("输入文字")
        } else {
            text.to_string()
        };
        let measured = crate::annotate::annotation::measure_hand_text(&measure, self.size());
        (measured.ceil() + 6.0)
            .min(self.bounds().size.width - self.ivars().editor_anchor_for_sizing.get().x - 6.0)
            .max(32.0)
    }

    fn edit_text(&self, a: &Annotation) {
        let Some(p) = a.points.first().copied() else { return };
        self.set_color(a.color.clone());
        self.set_size(a.size);
        self.ivars().editing_id.set(Some(a.id));
        self.invalidate();
        self.begin_text_editor(p, &a.text, Some(a.id));
    }

    fn begin_text_editor(&self, p: CGPoint, text: &str, replacing: Option<u64>) {
        let existing = replacing.and_then(|id| {
            self.annotations().into_iter().find(|a| a.id == id)
        });
        let auto = existing.as_ref().map(|e| e.text_box_size.is_none()).unwrap_or(true);
        self.ivars().editor_auto_width.set(auto);
        let mut box_size = existing.map(|e| e.bounds().size).unwrap_or(CGSize::ZERO);
        self.ivars().editor_anchor_for_sizing.set(p);
        if auto {
            box_size = CGSize::new(self.auto_width(text), 0.0);
        }
        *self.ivars().editor_box_size.borrow_mut() = box_size;
        let frame = CGRect::new(p, CGSize::new(box_size.width.max(1.0), box_size.height.max(1.0)));
        let tv = make_text_view(frame);
        if let Some(c) = unsafe { tv.textContainer() } {
            c.setContainerSize(CGSize::new(box_size.width.max(1.0), f64::MAX / 2.0_f64.sqrt()));
        }
        tv.setString(&NSString::from_str(text));
        // The class registers the two selector shapes informally; cast the
        // reference into the protocol object (same QL pattern as the shelf).
        let proto = unsafe {
            &*(self as *const Self
                as *const objc2::runtime::ProtocolObject<dyn NSTextViewDelegate>)
        };
        tv.setDelegate(Some(proto));
        let canvas_ptr = self as *const Self as usize;
        tv.set_on_commit(Box::new(move || {
            if let Some(this) = unsafe { (canvas_ptr as *const Self).as_ref() } {
                this.commit_text_editor();
            }
        }));
        self.addSubview(&tv);
        *self.ivars().editor.borrow_mut() = Some(tv.clone());
        self.ivars().editor_anchor.set(p);
        self.ivars().editing_id.set(replacing);
        self.restyle_editor();
        if let Some(w) = self.window() {
            let resp: Option<&NSResponder> = Some(&*tv);
            w.makeFirstResponder(resp);
        }
        tv.setSelectedRange(objc2_foundation::NSRange {
            location: text.chars().map(|c| c.len_utf16()).sum(),
            length: 0,
        });
    }

    fn layout_editor(&self) {
        if self.ivars().editor_auto_width.get() {
            let text = self
                .ivars()
                .editor
                .borrow()
                .as_ref()
                .map(|tv| tv.string().to_string())
                .unwrap_or_default();
            let w = self.auto_width(&text);
            *self.ivars().editor_box_size.borrow_mut() = CGSize::new(w, 0.0);
            if let Some(tv) = self.ivars().editor.borrow().as_ref() {
                if let Some(c) = unsafe { tv.textContainer() } {
                    c.setContainerSize(CGSize::new(w.max(1.0), f64::MAX / 2.0_f64.sqrt()));
                }
            }
        }
        let Some(a) = self.editing_annotation() else { return };
        if let Some(tv) = self.ivars().editor.borrow().as_ref() {
            tv.setFrame(a.bounds());
            tv.setNeedsDisplay(true);
        }
        self.invalidate();
        tv_needs_announce(self);
    }

    fn restyle_editor(&self) {
        let Some(tv) = self.ivars().editor.borrow().clone() else { return };
        let Some(a) = self.editing_annotation() else { return };
        tv.setFont(Some(&crate::annotate::annotation::hand_font(a.size.font_size())));
        tv.setTextColor(Some(&a.color));
        tv.setInsertionPointColor(Some(&a.color));
        let attrs = crate::annotate::text_layout::hand_attrs(
            &crate::annotate::annotation::hand_font(a.size.font_size()),
            &a.color,
        );
        let para = attrs.objectForKey(unsafe { objc2_app_kit::NSParagraphStyleAttributeName });
        if let Some(para) = para {
            tv.setDefaultParagraphStyle(Some(unsafe {
                &*(&*para as *const objc2::runtime::AnyObject
                    as *const objc2_app_kit::NSParagraphStyle)
            }));
        }
        if let Some(ts) = unsafe { tv.textStorage() } {
            unsafe { ts.setAttributes_range(Some(&attrs), objc2_foundation::NSRange {
                location: 0,
                length: ts.string().length(),
            }) };
        }
        unsafe {
            tv.setTypingAttributes(&attrs);
        }
        self.layout_editor();
    }

    pub fn commit_text_editor(&self) {
        let Some(tv) = self.ivars().editor.borrow_mut().take() else { return };
        // Trailing line breaks go: Return is a new line now, and the habit
        // of pressing it before clicking away would leave a taller box.
        let raw = tv.string().to_string();
        let text = if raw.trim().is_empty() {
            String::new()
        } else {
            raw.trim_end_matches('\n').trim_end_matches('\r').to_string()
        };
        tv.removeFromSuperview();
        if let Some(w) = self.window() {
            let resp: Option<&NSResponder> = Some(unsafe {
                &*(self as *const Self as *const NSResponder)
            });
            w.makeFirstResponder(resp);
        }
        let replacing = self.ivars().editing_id.get();
        self.ivars().editing_id.set(None);
        if let Some(id) = replacing {
            if let Some(i) = self.annotations().iter().position(|a| a.id == id) {
                if text.is_empty() {
                    self.mutate_annotations(|v| {
                        v.remove(i);
                    });
                    self.set_selected_id(None);
                } else {
                    let box_size = self.committed_box_size(&text);
                    let anchor = self.ivars().editor_anchor.get();
                    self.mutate_annotations(|v| {
                        if let Some(a) = v.get_mut(i) {
                            a.text = text.clone();
                            a.points = vec![anchor];
                            a.text_box_size = box_size;
                        }
                    });
                }
                self.invalidate();
                return;
            }
        }
        if !text.is_empty() {
            let box_size = self.committed_box_size(&text);
            let mut a = Annotation::new(
                AnnotateTool::Text,
                self.color(),
                self.size(),
                vec![self.ivars().editor_anchor.get()],
            );
            a.text = text;
            a.text_box_size = box_size;
            let id = a.id;
            self.mutate_annotations(|v| v.push(a));
            self.set_selected_id(Some(id));
        } else {
            self.invalidate();
        }
    }

    fn text_do_command(
        &self,
        _text_view: &NSView,
        sel: objc2::runtime::Sel,
    ) -> objc2::runtime::Bool {
        let Some(tv) = self.ivars().editor.borrow().clone() else {
            return false.into();
        };
        use objc2_app_kit::NSTextInputClient;
        if tv.hasMarkedText() {
            return false.into();
        }
        if sel == objc2::sel!(cancelOperation:) {
            tv.setString(&NSString::from_str(""));
            self.ivars().editing_id.set(None);
            self.commit_text_editor();
            return true.into();
        }
        false.into()
    }
}

/// A helper that mirrors `window?.invalidateCursorRects(for:)` chat from the
/// editor layout path.
fn tv_needs_announce(view: &AnnotateView) {
    if let Some(w) = view.window() {
        w.invalidateCursorRectsForView(view.as_ns_view());
    }
}


// MARK: Mouse / keys

impl AnnotateView {
    fn window_point(&self, event: &NSEvent) -> CGPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }

    fn clamped_point(&self, event: &NSEvent) -> CGPoint {
        let b = self.bounds();
        let p = self.window_point(event);
        CGPoint::new(
            p.x.min(b.size.width).max(0.0),
            p.y.min(b.size.height).max(0.0),
        )
    }

    fn annotate_mouse_down(&self, event: &NSEvent) {
        if let Some(w) = self.window() {
            let resp: Option<&NSResponder> = Some(unsafe {
                &*(self as *const Self as *const NSResponder)
            });
            w.makeFirstResponder(resp);
        }
        let p = self.window_point(event);
        if let Some(a) = self.editing_annotation() {
            // A click outside an open text box only confirms it: the frame
            // goes away and nothing new starts.
            let on_chrome = Self::on_text_chrome(&a, p);
            self.commit_text_editor();
            if !on_chrome || self.selected().is_none() {
                self.set_selected_id(None);
                self.invalidate();
                return;
            }
        }
        self.ivars().moved.set(false);
        self.ivars().pending_edit.set(None);
        if self.record_mode() {
            if event.clickCount() == 2 {
                self.request_record();
                return;
            }
            // Dragging the picture slides the frame.
            *self.ivars().drag.borrow_mut() = Some(Drag::Region {
                last_window: event.locationInWindow(),
            });
            return;
        }

        // 1. Chrome of the current selection: delete bubble, handles.
        if let Some(i) = self.selected_index() {
            let sel = self.annotations()[i].clone();
            let c = renderer::delete_center(&sel);
            if hypot(p.x - c.x, p.y - c.y) <= DELETE_RADIUS + 2.0 {
                self.delete_selected();
                return;
            }
            let inside_text = sel.tool == AnnotateTool::Text
                && crate::app::coordinates::contains_pt(sel.bounds(), p);
            if !inside_text {
                if let Some(h) = renderer::selection_handles(&sel)
                    .iter()
                    .position(|hs| hypot(p.x - hs.x, p.y - hs.y) <= HANDLE_RADIUS + 4.0)
                {
                    let model_h = sel.handles()[h];
                    *self.ivars().drag.borrow_mut() = Some(Drag::Handle {
                        index: h,
                        original: sel,
                        offset: CGPoint::new(p.x - model_h.x, p.y - model_h.y),
                    });
                    return;
                }
            }
        }
        // 2. An existing element under the cursor: select it (and maybe edit
        // text).
        if let Some(hit) = self.annotations().iter().rposition(|a| a.hit(p)) {
            let a = self.annotations()[hit].clone();
            if a.tool == AnnotateTool::Text && (self.selected_id() == Some(a.id) || event.clickCount() == 2) {
                self.ivars().pending_edit.set(Some(a.id));
            }
            self.set_selected_id(Some(a.id));
            *self.ivars().drag.borrow_mut() = Some(Drag::Move { last: p });
            return;
        }
        // 3. Empty space: deselect; double-click finishes; otherwise start
        // drawing with the active tool.
        if self.selected_id().is_some() {
            self.set_selected_id(None);
            if event.clickCount() == 2 {
                return;
            }
        }
        if event.clickCount() == 2 && self.ivars().draft.borrow().is_none() {
            self.finish();
            return;
        }
        let Some(tool) = self.tool() else {
            // No tool: drag moves the selection itself.
            *self.ivars().drag.borrow_mut() = Some(Drag::Region {
                last_window: event.locationInWindow(),
            });
            return;
        };
        if tool == AnnotateTool::Text {
            self.begin_text_editor(p, "", None);
        } else {
            *self.ivars().draft.borrow_mut() = Some(Annotation::new(
                tool,
                self.color(),
                self.size(),
                vec![p, p],
            ));
        }
    }

    fn annotate_mouse_dragged(&self, event: &NSEvent) {
        let p = self.clamped_point(event);
        self.ivars().moved.set(true);
        if let Some(mut d) = self.ivars().draft.borrow_mut().take() {
            if d.tool == AnnotateTool::Pen {
                d.points.push(p);
            } else {
                d.points[1] = p;
            }
            *self.ivars().draft.borrow_mut() = Some(d);
            self.invalidate();
            return;
        }
        if let Some(Drag::Region { last_window: last }) = *self.ivars().drag.borrow() {
            let now = event.locationInWindow();
            let (dx, dy) = (now.x - last.x, now.y - last.y);
            *self.ivars().drag.borrow_mut() = Some(Drag::Region { last_window: now });
            if let Some(del) = self.ivars().delegate.borrow_mut().as_mut() {
                del.move_region(dx, dy);
            }
            return;
        }
        let Some(i) = self.selected_index() else { return };
        let Some(dragged) = self.ivars().drag.borrow().as_ref().map(|d| match d {
            Drag::Move { last } => Drag::Move { last: *last },
            Drag::Handle { index, original, offset } => Drag::Handle {
                index: *index,
                original: original.clone(),
                offset: *offset,
            },
            Drag::Region { last_window } => Drag::Region { last_window: *last_window },
        }) else { return };
        match dragged {
            Drag::Move { last } => {
                self.mutate_annotations(|v| {
                    if let Some(a) = v.get_mut(i) {
                        a.translate(CGPoint::new(p.x - last.x, p.y - last.y));
                    }
                });
                *self.ivars().drag.borrow_mut() = Some(Drag::Move { last: p });
            }
            Drag::Handle { index, original, offset } => {
                // Reflow can change the height; always resize from the
                // mouse-down geometry.
                let mut resized = original;
                resized.set_handle(index, CGPoint::new(p.x - offset.x, p.y - offset.y));
                self.mutate_annotations(|v| {
                    if let Some(a) = v.get_mut(i) {
                        *a = resized.clone();
                    }
                });
            }
            Drag::Region { .. } => {}
        }
        if let Some(w) = self.window() {
            w.invalidateCursorRectsForView(self.as_ns_view());
        }
    }

    fn annotate_mouse_up(&self, _event: &NSEvent) {
        self.ivars().drag.borrow_mut().take();
        let pending = self.ivars().pending_edit.get();
        self.ivars().pending_edit.set(None);
        if let Some(id) = pending {
            if !self.ivars().moved.get() {
                if let Some(a) = self.annotations().into_iter().find(|a| a.id == id) {
                    self.edit_text(&a);
                    return;
                }
            }
        }
        let Some(d) = self.ivars().draft.borrow_mut().take() else { return };
        let r = d.rect();
        let big = if d.tool == AnnotateTool::Pen {
            d.points.len() > 1
        } else {
            r.size.width > 2.0 || r.size.height > 2.0
        };
        if !big {
            self.invalidate();
            return;
        }
        let select_after = if d.tool == AnnotateTool::Pen { None } else { Some(d.id) };
        self.mutate_annotations(|v| v.push(d));
        self.set_selected_id(select_after);
    }

    fn annotate_key_down(&self, event: &NSEvent) {
        let cmd = event
            .modifierFlags()
            .contains(objc2_app_kit::NSEventModifierFlags::Command);
        match event.keyCode() {
            36 | 76 => {
                if self.record_mode() {
                    self.request_record();
                } else {
                    self.finish();
                }
                return;
            }
            53 => {
                if self.selected_id().is_some() {
                    self.set_selected_id(None);
                } else {
                    self.cancel();
                }
                return;
            }
            6 if cmd => {
                if !self.record_mode() {
                    self.undo();
                }
                return;
            }
            51 | 117 => {
                if !self.record_mode() {
                    self.delete_selected();
                }
                return;
            }
            _ => {}
        }
        if !self.record_mode() && !cmd {
            if let Some(chars) = event.charactersIgnoringModifiers() {
                if let Some(ch) = chars.to_string().chars().next().map(|c| c.to_ascii_lowercase()) {
                    if let Some(t) = AnnotateTool::from_key(ch) {
                        self.set_tool(Some(t));
                        return;
                    }
                }
            }
        }
        unsafe {
            let _: () = msg_send![super(self), keyDown: event];
        }
    }
}

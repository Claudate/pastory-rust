//! Port of `Annotate/AnnotationTextView.swift`.
//!
//! A multiline editor whose zero insets match AnnotationTextLayout exactly.
//! Return breaks the line (⌘Return or a click outside finishes the box);
//! an empty box shows the 输入文字 placeholder.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSFont, NSStringDrawing, NSTextInputClient, NSTextView,
    NSStandardKeyBindingResponding};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

use crate::annotate::annotation::StrokeSize;
use crate::app::localization::l;

/// `HandFont.font(size:)` — 翩翩体 covers Latin too (falls back to system).
pub fn hand_font(size: f64) -> Retained<NSFont> {
    for name in ["HanziPenSC-W5", "HannotateSC-W5"] {
        if let Some(f) = NSFont::fontWithName_size(&NSString::from_str(name), size) {
            return f;
        }
    }
    NSFont::systemFontOfSize_weight(size, unsafe { objc2_app_kit::NSFontWeightMedium })
}

pub struct AnnotationTextViewIvars {
    on_commit: RefCell<Option<Box<dyn Fn() + 'static>>>,
}

impl Default for AnnotationTextViewIvars {
    fn default() -> Self {
        Self { on_commit: RefCell::new(None) }
    }
}

define_class!(
    // SAFETY:
    // - NSTextView subclass, main-thread only.
    #[unsafe(super(NSTextView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AnnotationTextViewIvars]
    pub struct AnnotationTextView;

    unsafe impl NSObjectProtocol for AnnotationTextView {}

    impl AnnotationTextView {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            // Return breaks the line, as in every other screenshot tool;
            // ⌘Return (or a click outside) finishes the box.
            let key = event.keyCode();
            if (key == 36 || key == 76) && !self.hasMarkedText() {
                if event.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Command) {
                    if let Some(f) = self.ivars().on_commit.borrow().as_ref() {
                        f();
                    }
                } else {
                    let none: Option<&AnyObject> = None;
                    unsafe { self.insertNewlineIgnoringFieldEditor(none) };
                }
                return;
            }
            unsafe {
                let _: () = objc2::msg_send![super(self), keyDown: event];
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty_rect: CGRect) {
            unsafe {
                let _: () = objc2::msg_send![super(self), drawRect: dirty_rect];
            }
            if self.string().to_string().is_empty() {
                let font = self
                    .font()
                    .unwrap_or_else(|| crate::annotate::annotation::hand_font(StrokeSize::M.font_size()));
                let color = self
                    .textColor()
                    .unwrap_or_else(|| nscolor_label())
                    .colorWithAlphaComponent(0.35);
                let attrs = crate::shelf::card::attrs(&font, &color, None);
                let placeholder = NSString::from_str(&l("输入文字"));
                unsafe {
                    placeholder.drawAtPoint_withAttributes(CGPoint::ZERO, Some(&attrs));
                }
            }
        }
    }
);

fn nscolor_label() -> objc2::rc::Retained<objc2_app_kit::NSColor> {
    unsafe {
        let c: objc2::rc::Retained<objc2_app_kit::NSColor> =
            objc2::msg_send![objc2::class!(NSColor), labelColor];
        c
    }
}

impl AnnotationTextView {
    pub fn set_on_commit(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_commit.borrow_mut() = Some(f);
    }
}

/// `AnnotationTextView(frame:)` with all the zero-inset Swift settings.
pub fn make(frame: CGRect) -> Retained<AnnotationTextView> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let this = mtm.alloc::<AnnotationTextView>().set_ivars(AnnotationTextViewIvars::default());
    let tv: Retained<AnnotationTextView> = unsafe {
        objc2::msg_send![super(this), initWithFrame: frame]
    };
    tv.setRichText(false);
    tv.setImportsGraphics(false);
    tv.setAllowsUndo(true);
    tv.setDrawsBackground(false);
    tv.setTextContainerInset(CGSize::ZERO);
    tv.setHorizontallyResizable(false);
    tv.setVerticallyResizable(false);
    if let Some(c) = unsafe { tv.textContainer() } {
        c.setLineFragmentPadding(0.0);
        c.setWidthTracksTextView(true);
        c.setHeightTracksTextView(false);
        c.setContainerSize(CGSize::new(frame.size.width, f64::MAX / 2.0_f64.sqrt()));
    }
    tv
}

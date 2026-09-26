//! Port of `Annotate/AnnotationTextLayout.swift`.
//!
//! TextKit layout shared by annotation measurement and export, matching the
//! live text editor: NSTextStorage + NSLayoutManager + NSTextContainer,
//! zero line-fragment padding, word wrap, same hand font.

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2_app_kit::{NSColor, NSLayoutManager, NSTextContainer, NSTextStorage};
use objc2_core_foundation::{CGPoint, CGSize};

use crate::annotate::annotation::{hand_font, StrokeSize};

pub struct AnnotationTextLayout {
    storage: Retained<NSTextStorage>,
    manager: Retained<NSLayoutManager>,
    container: Retained<NSTextContainer>,
    font: Retained<objc2_app_kit::NSFont>,
}

impl AnnotationTextLayout {
    pub fn new(text: &str, size: StrokeSize, color: &NSColor, width: f64) -> Self {
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        let font = hand_font(size.font_size());
        // textAttributes: hand font + color + word-wrap paragraph style.
        let attrs = hand_attrs(&font, color);
        let storage: Retained<NSTextStorage> = unsafe {
            // -initWithString:attributes: is not bound; msg_send the same
            // selector on the allocated object.
            objc2::msg_send![
                mtm.alloc::<NSTextStorage>(),
                initWithString: &*objc2_foundation::NSString::from_str(text),
                attributes: Some(&*attrs)
            ]
        };
        let manager = NSLayoutManager::new();
        let container = NSTextContainer::initWithContainerSize(
            mtm.alloc(),
            CGSize::new(width.max(1.0), f64::MAX / 2.0_f64.sqrt()),
        );
        container.setLineFragmentPadding(0.0);
        container.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping);
        storage.addLayoutManager(&manager);
        manager.addTextContainer(&container);
        manager.ensureLayoutForTextContainer(&container);
        Self { storage, manager, container, font }
    }

    /// `height` — usedRect + the extra line fragment for a trailing return,
    /// at least one default line (empty text still has room for the caret).
    pub fn height(&self) -> f64 {
        let used = self.manager.usedRectForTextContainer(&self.container);
        let extra = self
            .manager
            .extraLineFragmentTextContainer()
            .map(|c| {
                if c.isEqual(Some(&*self.container)) {
                    self.manager.extraLineFragmentRect().max().y
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0);
        let line = self.manager.defaultLineHeightForFont(&self.font);
        used.max().y.max(extra).max(line).ceil()
    }

    pub fn draw_at(&self, point: CGPoint) {
        let range = self.manager.glyphRangeForTextContainer(&self.container);
        self.manager.drawBackgroundForGlyphRange_atPoint(range, point);
        self.manager.drawGlyphsForGlyphRange_atPoint(range, point);
    }
}

/// `textAttributes`: hand font + color + word-wrap paragraph style
/// (Annotation.textAttributes).
pub fn hand_attrs(
    font: &objc2_app_kit::NSFont,
    color: &NSColor,
) -> Retained<objc2_foundation::NSDictionary<objc2_foundation::NSAttributedStringKey, objc2::runtime::AnyObject>> {
    use objc2_app_kit::{
        NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName,
    };
    use objc2_foundation::NSDictionary;
    let para = objc2_app_kit::NSMutableParagraphStyle::new();
    para.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping);
    // SAFETY: attribute-name statics are immutable; upcasts stay in-class.
    unsafe {
        let font_obj = &*(font as *const objc2_app_kit::NSFont as *const objc2::runtime::AnyObject);
        let color_obj = &*(color as *const NSColor as *const objc2::runtime::AnyObject);
        let para_obj = &*(objc2::rc::Retained::as_ptr(&para) as *const objc2::runtime::AnyObject);
        NSDictionary::from_slices(
            &[
                NSFontAttributeName,
                NSForegroundColorAttributeName,
                NSParagraphStyleAttributeName,
            ],
            &[font_obj, color_obj, para_obj],
        )
    }
}

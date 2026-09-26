//! Port of `Annotate/AnnotateToolbar.swift`.
//!
//! Two-layer toolbar, Feishu-style, on the brown desk with a light-blue
//! active state:
//!   Main bar:  ▢ ○ ╱ ↗ ✎ A ▦ │ 识别文字 │ ↶ │ ✕ · [✓ 复制]
//!   Sub bar:   sizes (dots, or 小/中/大 for text) · colors (rounded squares
//!              with a check; none for mosaic), hanging under the active
//!              tool with a pointer.
//!
//! No stack views / no AutoLayout: fixed geometry, matching the Swift
//! `fittingSize` exactly.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAttributedStringNSStringDrawing, NSBezierPath, NSButton, NSColor, NSImage, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace};
use objc2_foundation::NSString;

use crate::annotate::annotation::{palette, AnnotateTool, StrokeSize, ALL_SIZES, ALL_TOOLS};
use crate::annotate::annotate_view::AnnotateView;
use crate::app::{localization::l, theme};

// MARK: Icon helpers (ToolIcons)

/// Draw an 18-pt image in a 2× bitmap and wrap it in a template NSImage —
/// same pixels AppKit would hand the drawing closure, without needing the
/// (unbound) NSImage(size:flipped:drawingHandler:).
fn draw_template_18(f: impl FnOnce(&NSBezierPath)) -> Retained<NSImage> {
    let (w, h) = (36usize, 36usize);
    let space = CGColorSpace::new_device_rgb();
    let Some(ctx) = (unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, space.as_deref(), 1)
    }) else {
        return NSImage::new();
    };
    let gc = objc2_app_kit::NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, true);
    let prev = objc2_app_kit::NSGraphicsContext::currentContext();
    objc2_app_kit::NSGraphicsContext::setCurrentContext(Some(&gc));
    objc2_core_graphics::CGContext::scale_ctm(Some(&ctx), 2.0, 2.0);
    NSColor::blackColor().setStroke();
    NSColor::blackColor().setFill();
    f(&NSBezierPath::new());
    objc2_app_kit::NSGraphicsContext::setCurrentContext(prev.as_deref());
    let Some(cg) = CGBitmapContextCreateImage(Some(&ctx)) else {
        return NSImage::new();
    };
    let mtm = MainThreadMarker::new().expect("main thread");
    let img = NSImage::initWithCGImage_size(mtm.alloc(), &cg, CGSize::new(18.0, 18.0));
    img.setTemplate(true);
    img
}

/// `ToolIcons.image(for:)` — thin-line 18 pt template icons drawn in code.
fn tool_icon(tool: AnnotateTool) -> Retained<NSImage> {
    let bezier = |setup: &dyn Fn(&NSBezierPath)| -> Retained<NSImage> {
        draw_template_18(|p| {
            p.setLineWidth(1.6);
            p.setLineCapStyle(objc2_app_kit::NSLineCapStyle::Round);
            p.setLineJoinStyle(objc2_app_kit::NSLineJoinStyle::Round);
            setup(p);
            p.stroke();
        })
    };
    match tool {
        AnnotateTool::Rect => bezier(&|p| {
            p.appendBezierPathWithRoundedRect_xRadius_yRadius(
                CGRect::new(CGPoint::new(2.5, 3.0), CGSize::new(13.0, 12.0)),
                1.5,
                1.5,
            );
        }),
        AnnotateTool::Ellipse => bezier(&|p| {
            p.appendBezierPathWithOvalInRect(CGRect::new(
                CGPoint::new(2.5, 2.5),
                CGSize::new(13.0, 13.0),
            ));
        }),
        AnnotateTool::Arrow => bezier(&|p| {
            p.moveToPoint(CGPoint::new(3.0, 15.0));
            p.lineToPoint(CGPoint::new(15.0, 3.0));
            p.moveToPoint(CGPoint::new(8.5, 3.0));
            p.lineToPoint(CGPoint::new(15.0, 3.0));
            p.lineToPoint(CGPoint::new(15.0, 9.5));
        }),
        AnnotateTool::Line => bezier(&|p| {
            p.moveToPoint(CGPoint::new(3.0, 15.0));
            p.lineToPoint(CGPoint::new(15.0, 3.0));
        }),
        AnnotateTool::Pen => bezier(&|p| {
            // Pen nib with a squiggle under it.
            p.moveToPoint(CGPoint::new(11.5, 2.5));
            p.lineToPoint(CGPoint::new(15.5, 6.5));
            p.lineToPoint(CGPoint::new(7.5, 14.5));
            p.lineToPoint(CGPoint::new(3.5, 15.5));
            p.lineToPoint(CGPoint::new(4.5, 11.5));
            p.closePath();
            p.moveToPoint(CGPoint::new(9.5, 4.5));
            p.lineToPoint(CGPoint::new(13.5, 8.5));
        }),
        AnnotateTool::Text => bezier(&|p| {
            p.moveToPoint(CGPoint::new(3.5, 4.0));
            p.lineToPoint(CGPoint::new(14.5, 4.0));
            p.moveToPoint(CGPoint::new(3.5, 3.0));
            p.lineToPoint(CGPoint::new(3.5, 6.0));
            p.moveToPoint(CGPoint::new(14.5, 3.0));
            p.lineToPoint(CGPoint::new(14.5, 6.0));
            p.moveToPoint(CGPoint::new(9.0, 4.0));
            p.lineToPoint(CGPoint::new(9.0, 15.5));
            p.moveToPoint(CGPoint::new(6.5, 15.5));
            p.lineToPoint(CGPoint::new(11.5, 15.5));
        }),
        AnnotateTool::Mosaic => draw_template_18(|p| {
            p.setLineWidth(1.6);
            // Checker squares: black even cells, 35% black odd cells.
            for r in 0..3 {
                for c in 0..3 {
                    if (r + c) % 2 == 0 {
                        p.appendBezierPathWithRect(CGRect::new(
                            CGPoint::new(3.0 + c as f64 * 4.2, 3.0 + r as f64 * 4.2),
                            CGSize::new(3.8, 3.8),
                        ));
                    }
                }
            }
            p.fill();
            let q = NSBezierPath::new();
            for r in 0..3 {
                for c in 0..3 {
                    if (r + c) % 2 == 1 {
                        q.appendBezierPathWithRect(CGRect::new(
                            CGPoint::new(3.0 + c as f64 * 4.2, 3.0 + r as f64 * 4.2),
                            CGSize::new(3.8, 3.8),
                        ));
                    }
                }
            }
            NSColor::blackColor().colorWithAlphaComponent(0.35).setFill();
            q.fill();
        }),
    }
}

/// The 14×14 check used on the color buttons (`SubBar.check(on:)`).
fn check_image(on_color: &NSColor) -> Retained<NSImage> {
    let (w, h) = (28usize, 28usize);
    let space = CGColorSpace::new_device_rgb();
    let Some(ctx) = (unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, space.as_deref(), 1)
    }) else {
        return NSImage::new();
    };
    let gc = objc2_app_kit::NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, true);
    let prev = objc2_app_kit::NSGraphicsContext::currentContext();
    objc2_app_kit::NSGraphicsContext::setCurrentContext(Some(&gc));
    objc2_core_graphics::CGContext::scale_ctm(Some(&ctx), 2.0, 2.0);
    let p = NSBezierPath::new();
    p.setLineWidth(2.0);
    p.setLineCapStyle(objc2_app_kit::NSLineCapStyle::Round);
    p.setLineJoinStyle(objc2_app_kit::NSLineJoinStyle::Round);
    p.moveToPoint(CGPoint::new(3.0, 7.0));
    p.lineToPoint(CGPoint::new(6.0, 4.0));
    p.lineToPoint(CGPoint::new(11.5, 10.5));
    if crate::annotate::annotation::is_light(on_color) {
        NSColor::blackColor().setStroke();
    } else {
        NSColor::whiteColor().setStroke();
    }
    p.stroke();
    objc2_app_kit::NSGraphicsContext::setCurrentContext(prev.as_deref());
    let Some(cg) = CGBitmapContextCreateImage(Some(&ctx)) else {
        return NSImage::new();
    };
    let mtm = MainThreadMarker::new().expect("main thread");
    NSImage::initWithCGImage_size(mtm.alloc(), &cg, CGSize::new(14.0, 14.0))
}

/// A 14×14 filled dot in `tint` (`SubBar` size dots).
fn dot_image(diameter: f64, tint: &NSColor) -> Retained<NSImage> {
    let (w, h) = (28usize, 28usize);
    let space = CGColorSpace::new_device_rgb();
    let Some(ctx) = (unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, space.as_deref(), 1)
    }) else {
        return NSImage::new();
    };
    let gc = objc2_app_kit::NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, true);
    let prev = objc2_app_kit::NSGraphicsContext::currentContext();
    objc2_app_kit::NSGraphicsContext::setCurrentContext(Some(&gc));
    objc2_core_graphics::CGContext::scale_ctm(Some(&ctx), 2.0, 2.0);
    tint.setFill();
    NSBezierPath::bezierPathWithOvalInRect(CGRect::new(
        CGPoint::new((14.0 - diameter) / 2.0, (14.0 - diameter) / 2.0),
        CGSize::new(diameter, diameter),
    ))
    .fill();
    objc2_app_kit::NSGraphicsContext::setCurrentContext(prev.as_deref());
    let Some(cg) = CGBitmapContextCreateImage(Some(&ctx)) else {
        return NSImage::new();
    };
    let mtm = MainThreadMarker::new().expect("main thread");
    NSImage::initWithCGImage_size(mtm.alloc(), &cg, CGSize::new(14.0, 14.0))
}

/// An SF symbol at a point size (识别文字 / ↶ / ✕ / ✓ icons).
fn symbol_image(name: &str, point: f64, weight: f64) -> Option<Retained<NSImage>> {
    let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
    let cfg = objc2_app_kit::NSImageSymbolConfiguration::configurationWithPointSize_weight(
        point, weight,
    );
    img.imageWithSymbolConfiguration(&cfg)
}

// MARK: AnnotateToolbar

fn bar_ink() -> Retained<NSColor> {
    theme::on_brown()
}
fn bar_selected_bg() -> Retained<NSColor> {
    theme::paper_blue()
}
fn bar_selected_ink() -> Retained<NSColor> {
    theme::ink()
}

/// divider between groups (`AnnotateToolbar.divider()` = deskDivider 26).
fn divider_26() -> Retained<NSView> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let v = NSView::new(mtm);
    v.setWantsLayer(true);
    if let Some(layer) = v.layer() {
        let c = theme::on_brown().colorWithAlphaComponent(0.25).CGColor();
        unsafe {
            let _: () = msg_send![&*layer, setBackgroundColor: &*c];
        }
    }
    v.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(1.0, 26.0)));
    v
}

pub struct AnnotateToolbarIvars {
    canvas: RefCell<Option<Retained<AnnotateView>>>,
    tool_buttons: RefCell<Vec<(AnnotateTool, Retained<NSButton>)>>,
    undo_button: RefCell<Option<Retained<NSButton>>>,
    sub_bar: RefCell<Option<Retained<SubBar>>>,
    width: Cell<f64>,
    drag_origin: Cell<Option<CGPoint>>,
}

impl Default for AnnotateToolbarIvars {
    fn default() -> Self {
        Self {
            canvas: RefCell::new(None),
            tool_buttons: RefCell::new(Vec::new()),
            undo_button: RefCell::new(None),
            sub_bar: RefCell::new(None),
            width: Cell::new(0.0),
            drag_origin: Cell::new(None),
        }
    }
}

define_class!(
    // SAFETY:
    // - Plain NSView with hand-placed children (no AutoLayout, like M2).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AnnotateToolbarIvars]
    pub struct AnnotateToolbar;

    unsafe impl NSObjectProtocol for AnnotateToolbar {}

    impl AnnotateToolbar {
        #[unsafe(method(pickTool:))]
        fn pick_tool(&self, sender: &AnyObject) {
            let Some(canvas) = self.ivars().canvas.borrow().clone() else { return };
            let tag: isize = unsafe { msg_send![sender, tag] };
            let Some(t) = ALL_TOOLS.get(tag as usize).copied() else { return };
            // Second click deactivates.
            canvas.set_tool(if canvas.tool() == Some(t) { None } else { Some(t) });
        }

        #[unsafe(method(ocrTapped:))]
        fn ocr_tapped(&self, _sender: &AnyObject) {
            if let Some(canvas) = self.ivars().canvas.borrow().clone() {
                canvas.request_ocr();
            }
        }

        #[unsafe(method(undoTapped:))]
        fn undo_tapped(&self, _sender: &AnyObject) {
            if let Some(canvas) = self.ivars().canvas.borrow().clone() {
                canvas.undo();
            }
        }

        #[unsafe(method(cancelTapped:))]
        fn cancel_tapped(&self, _sender: &AnyObject) {
            if let Some(canvas) = self.ivars().canvas.borrow().clone() {
                canvas.cancel();
            }
        }

        #[unsafe(method(doneTapped:))]
        fn done_tapped(&self, _sender: &AnyObject) {
            if let Some(canvas) = self.ivars().canvas.borrow().clone() {
                canvas.finish();
            }
        }

        #[unsafe(method(drawRect:))]
        fn t_draw_rect(&self, _dirty: CGRect) {
            theme::draw_desk(
                &NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    crate::app::coordinates::inset_rect(self.bounds(), 0.5, 0.5),
                    6.0,
                    6.0,
                ),
            );
        }

        // Drag the bar anywhere on its ground; the sub bar follows.
        #[unsafe(method(mouseDown:))]
        fn t_mouse_down(&self, event: &objc2_app_kit::NSEvent) {
            self.ivars()
                .drag_origin
                .set(Some(self.convertPoint_fromView(event.locationInWindow(), None)));
        }

        #[unsafe(method(mouseDragged:))]
        fn t_mouse_dragged(&self, event: &objc2_app_kit::NSEvent) {
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
            self.refresh();
        }

        #[unsafe(method(mouseUp:))]
        fn t_mouse_up(&self, _event: &objc2_app_kit::NSEvent) {
            self.ivars().drag_origin.set(None);
        }
    }
);

impl AnnotateToolbar {
    /// Icon button: 38×38, image-only, template tint, corner 9.
    fn icon_button(
        image: &NSImage,
        tip: &str,
        target: Option<&AnyObject>,
        action: objc2::runtime::Sel,
    ) -> Retained<NSButton> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let b = unsafe { NSButton::buttonWithImage_target_action(image, target, Some(action), mtm) };
        b.setBordered(false);
        b.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
        b.setImageScaling(objc2_app_kit::NSImageScaling::ScaleNone);
        b.setToolTip(Some(&NSString::from_str(tip)));
        b.setContentTintColor(Some(&bar_ink()));
        b.setWantsLayer(true);
        if let Some(layer) = b.layer() {
            unsafe {
                let _: () = msg_send![&*layer, setCornerRadius: 9.0f64];
            }
        }
        b.setFrameSize(CGSize::new(38.0, 38.0));
        b
    }


    /// `init(canvas:doneTitle:)` — hand-placed main bar (stack edge insets
    /// 8, spacing 4; height 56).
    pub fn make(canvas: &Retained<AnnotateView>, done_title: &str) -> Retained<AnnotateToolbar> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<AnnotateToolbar>().set_ivars(AnnotateToolbarIvars::default());
        let bar: Retained<AnnotateToolbar> = unsafe { msg_send![super(this), init] };
        *bar.ivars().canvas.borrow_mut() = Some(canvas.clone());
        theme::paper_sheet(&bar, 6.0);
        // SAFETY: the bar outlives its buttons.
        let bar_obj: &AnyObject =
            unsafe { &*(&*bar as *const AnnotateToolbar as *const AnyObject) };
        let target = Some(bar_obj);

        let mut x = 8.0;
        for t in ALL_TOOLS {
            let icon = tool_icon(t);
            let b = Self::icon_button(&icon, &t.tip(), target, sel!(pickTool:));
            b.setTag(ALL_TOOLS.iter().position(|u| *u == t).unwrap() as isize);
            b.setFrameOrigin(CGPoint::new(x, (56.0 - 38.0) / 2.0));
            bar.addSubview(&b);
            bar.ivars().tool_buttons.borrow_mut().push((t, b));
            x += 38.0 + 4.0;
        }
        let d1 = divider_26();
        d1.setFrameOrigin(CGPoint::new(x, (56.0 - 26.0) / 2.0));
        bar.addSubview(&d1);
        x += 1.0 + 4.0;

        // 识别文字: icon + label (attributed serif 15, width = title + 44).
        let ocr_title = format!(" {}", l("识别文字"));
        let serif15 = theme::serif(15.0, false);
        let dict = crate::shelf::card::attrs(&serif15, &bar_ink(), None);
        let ocr_attr = unsafe {
            objc2_foundation::NSAttributedString::initWithString_attributes(
                objc2::AllocAnyThread::alloc(),
                &NSString::from_str(&ocr_title),
                Some(&dict),
            )
        };
        let ocr_w = (ocr_attr.size().width + 44.0).ceil();
        let ocr_btn = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(&ocr_title),
                target,
                Some(sel!(ocrTapped:)),
                mtm,
            )
        };
        ocr_btn.setBordered(false);
        ocr_btn.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageLeading);
        ocr_btn.setImageHugsTitle(true);
        if let Some(img) = symbol_image("text.viewfinder", 15.0, unsafe {
            objc2_app_kit::NSFontWeightRegular
        }) {
            ocr_btn.setImage(Some(&img));
        }
        ocr_btn.setContentTintColor(Some(&bar_ink()));
        ocr_btn.setAttributedTitle(&ocr_attr);
        ocr_btn.setWantsLayer(true);
        if let Some(layer) = ocr_btn.layer() {
            unsafe {
                let _: () = msg_send![&*layer, setCornerRadius: 9.0f64];
            }
        }
        ocr_btn.setFrame(CGRect::new(
            CGPoint::new(x, (56.0 - 38.0) / 2.0),
            CGSize::new(ocr_w, 38.0),
        ));
        bar.addSubview(&ocr_btn);
        x += ocr_w + 4.0;

        let d2 = divider_26();
        d2.setFrameOrigin(CGPoint::new(x, (56.0 - 26.0) / 2.0));
        bar.addSubview(&d2);
        x += 1.0 + 4.0;

        // 撤销 · 取消 · 完成 — plain ink glyphs; 完成 in the deep blue.
        let done_tip = if done_title == l("复制") {
            l("完成 ⏎ · 复制到剪贴板")
        } else {
            format!("{done_title} ⏎")
        };
        let undo_img = symbol_image("arrow.uturn.backward", 16.0, unsafe {
            objc2_app_kit::NSFontWeightMedium
        })
        .expect("symbol");
        let undo = Self::icon_button(&undo_img, &l("撤销 ⌘Z"), target, sel!(undoTapped:));
        undo.setContentTintColor(Some(&theme::on_brown()));
        undo.setFrameOrigin(CGPoint::new(x, (56.0 - 38.0) / 2.0));
        bar.addSubview(&undo);
        *bar.ivars().undo_button.borrow_mut() = Some(undo);
        x += 38.0 + 4.0;

        let cancel_img = symbol_image("xmark", 17.0, unsafe {
            objc2_app_kit::NSFontWeightSemibold
        })
        .expect("symbol");
        let cancel = Self::icon_button(&cancel_img, &l("取消 ⎋"), target, sel!(cancelTapped:));
        cancel.setContentTintColor(Some(&theme::on_brown()));
        cancel.setFrameOrigin(CGPoint::new(x, (56.0 - 38.0) / 2.0));
        bar.addSubview(&cancel);
        x += 38.0 + 4.0;

        let done_img = symbol_image("checkmark", 17.0, unsafe { objc2_app_kit::NSFontWeightBold })
        .expect("symbol");
        let done = Self::icon_button(&done_img, &done_tip, target, sel!(doneTapped:));
        done.setContentTintColor(Some(&theme::paper_blue()));
        done.setFrameOrigin(CGPoint::new(x, (56.0 - 38.0) / 2.0));
        bar.addSubview(&done);
        x += 38.0;

        let width = (x + 8.0).ceil();
        bar.ivars().width.set(width);
        bar.setFrameSize(CGSize::new(width, 56.0));

        let sub = SubBar::make(canvas);
        *bar.ivars().sub_bar.borrow_mut() = Some(sub);

        // canvas state → refresh (the bar is alive whenever the canvas
        // fires: siblings inside the same overlay window).
        let bar_raw = &*bar as *const AnnotateToolbar as usize;
        canvas.set_on_state_change(Box::new(move || {
            if let Some(b) = unsafe { (bar_raw as *const AnnotateToolbar).as_ref() } {
                b.refresh();
            }
        }));
        bar.refresh();
        bar
    }

    /// `fittingSize` — (bar width, 56).
    pub fn fitting(&self) -> CGSize {
        CGSize::new(self.ivars().width.get(), 56.0)
    }

    /// Called once the overlay has placed the main bar (`didLayout`).
    pub fn did_layout(&self) {
        self.refresh();
    }

    /// `subBar.isHidden = true` (record mode hides the size/color chip too).
    pub fn hide_sub_bar(&self) {
        if let Some(sub) = self.ivars().sub_bar.borrow().as_ref() {
            sub.setHidden(true);
        }
    }

    /// Tear down for the overlay's release: sub bar first, then ourselves.
    pub fn remove_self_and_sub(&self) {
        if let Some(sub) = self.ivars().sub_bar.borrow().as_ref() {
            sub.removeFromSuperview();
        }
        self.removeFromSuperview();
    }

    /// `refresh()` — undo state, active chip, sub bar for the active kind.
    pub fn refresh(&self) {
        let Some(canvas) = self.ivars().canvas.borrow().clone() else { return };
        let can_undo = canvas.can_undo();
        if let Some(undo) = self.ivars().undo_button.borrow().as_ref() {
            undo.setEnabled(can_undo);
            undo.setAlphaValue(if can_undo { 1.0 } else { 0.35 });
        }
        let active = canvas.active_kind();
        for (t, b) in self.ivars().tool_buttons.borrow().iter() {
            let on = Some(*t) == active;
            if let Some(layer) = b.layer() {
                unsafe {
                    if on {
                        let c = bar_selected_bg().CGColor();
                        let _: () = msg_send![&*layer, setBackgroundColor: &*c];
                    } else {
                        let none: Option<&objc2_core_graphics::CGColor> = None;
                        let _: () = msg_send![&*layer, setBackgroundColor: none];
                    }
                }
            }
            let tint = if on { &bar_selected_ink() } else { &bar_ink() };
            let tint = tint.clone();
            b.setContentTintColor(Some(&tint));
        }
        self.layout_sub_bar(active);
    }

    /// `layoutSubBar(for:)` — pointer chip hanging under the active kind.
    fn layout_sub_bar(&self, kind: Option<AnnotateTool>) {
        let Some(kind) = kind else {
            if let Some(sub) = self.ivars().sub_bar.borrow().as_ref() {
                sub.removeFromSuperview();
            }
            return;
        };
        let sub = self.ivars().sub_bar.borrow().clone().expect("set at make");
        sub.configure(kind);
        sub.setHidden(false);
        if let Some(host) = unsafe { self.superview() } {
            if (unsafe { sub.superview() }).map(|s| s != host).unwrap_or(true) {
                host.addSubview(&sub);
            }
            let button_vec = self.ivars().tool_buttons.borrow().clone();
            let Some((_, button)) = button_vec.iter().find(|(t, _)| *t == kind) else {
                return;
            };
            let size = sub.fitting();
            let gap = 8.0;
            let my = self.frame();
            let host_b = host.bounds();
            let below = my.origin.y - gap - size.height >= host_b.min().y + 4.0;
            let y = if below {
                my.origin.y - gap - size.height
            } else {
                my.origin.y + my.size.height + gap
            };
            let anchor_rect = button.convertRect_toView(button.bounds(), Some(&host));
            let anchor_x = anchor_rect.origin.x + anchor_rect.size.width / 2.0;
            let mut sx = anchor_x - size.width / 2.0;
            sx = sx.max(host_b.min().x + 4.0).min(host_b.max().x - size.width - 4.0);
            sub.set_pointer_x(anchor_x - sx);
            sub.set_points_up(below);
            sub.setFrame(CGRect::new(
                CGPoint::new(sx.round(), y.round()),
                size,
            ));
            sub.setNeedsDisplay(true);
        }
    }
}

// MARK: SubBar

const POINTER_H: f64 = 7.0;

pub struct SubBarIvars {
    canvas: RefCell<Option<Retained<AnnotateView>>>,
    size_buttons: RefCell<Vec<(StrokeSize, Retained<NSButton>)>>,
    color_buttons: RefCell<Vec<Retained<NSButton>>>,
    color_divider: RefCell<Option<Retained<NSView>>>,
    pointer_x: Cell<f64>,
    points_up: Cell<bool>,
}

impl Default for SubBarIvars {
    fn default() -> Self {
        Self {
            canvas: RefCell::new(None),
            size_buttons: RefCell::new(Vec::new()),
            color_buttons: RefCell::new(Vec::new()),
            color_divider: RefCell::new(None),
            pointer_x: Cell::new(40.0),
            points_up: Cell::new(true),
        }
    }
}

define_class!(
    // SAFETY:
    // - Plain NSView with hand-placed children; main thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SubBarIvars]
    pub struct SubBar;

    unsafe impl NSObjectProtocol for SubBar {}

    impl SubBar {
        #[unsafe(method(pickSize:))]
        fn pick_size(&self, sender: &AnyObject) {
            let Some(canvas) = self.ivars().canvas.borrow().clone() else { return };
            let tag: isize = unsafe { msg_send![sender, tag] };
            canvas.set_size(StrokeSize::from_raw(tag as i64));
        }

        #[unsafe(method(pickColor:))]
        fn pick_color(&self, sender: &AnyObject) {
            let Some(canvas) = self.ivars().canvas.borrow().clone() else { return };
            let tag: isize = unsafe { msg_send![sender, tag] };
            let colors = palette();
            if let Some(c) = colors.get(tag as usize) {
                canvas.set_color(c.clone());
            }
        }

        #[unsafe(method(mouseDown:))]
        fn sb_mouse_down(&self, _event: &objc2_app_kit::NSEvent) {
            // Absorb: clicks on the slip are not canvas clicks.
        }

        #[unsafe(method(drawRect:))]
        fn sb_draw_rect(&self, _dirty: CGRect) {
            // Paper slip with a little pointer toward the main bar.
            let bounds = self.bounds();
            let body = if self.ivars().points_up.get() {
                CGRect::new(
                    CGPoint::ZERO,
                    CGSize::new(bounds.size.width, bounds.size.height - POINTER_H),
                )
            } else {
                CGRect::new(
                    CGPoint::new(0.0, POINTER_H),
                    CGSize::new(bounds.size.width, bounds.size.height - POINTER_H),
                )
            };
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(body, 12.0, 12.0);
            let px = self
                .ivars()
                .pointer_x
                .get()
                .max(14.0)
                .min(bounds.size.width - 14.0);
            if self.ivars().points_up.get() {
                let top = body.origin.y + body.size.height;
                path.moveToPoint(CGPoint::new(px - 7.0, top));
                path.lineToPoint(CGPoint::new(px, top + POINTER_H));
                path.lineToPoint(CGPoint::new(px + 7.0, top));
            } else {
                path.moveToPoint(CGPoint::new(px - 7.0, body.origin.y));
                path.lineToPoint(CGPoint::new(px, body.origin.y - POINTER_H));
                path.lineToPoint(CGPoint::new(px + 7.0, body.origin.y));
            }
            path.closePath();
            theme::draw_desk(&path);
        }
    }
);

impl SubBar {
    /// `SubBar.init(canvas:)` — sizes (dots, or 小/中/大 for text) │ colors.
    pub fn make(canvas: &Retained<AnnotateView>) -> Retained<SubBar> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<SubBar>().set_ivars(SubBarIvars::default());
        let bar: Retained<SubBar> = unsafe { msg_send![super(this), init] };
        *bar.ivars().canvas.borrow_mut() = Some(canvas.clone());
        theme::paper_sheet(&bar, 6.0);
        // SAFETY: the sub bar outlives its buttons.
        let bar_obj: &AnyObject = unsafe { &*(&*bar as *const SubBar as *const AnyObject) };
        let target = Some(bar_obj);

        let mut x = 12.0;
        for s in ALL_SIZES {
            let b = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(""),
                    target,
                    Some(sel!(pickSize:)),
                    mtm,
                )
            };
            b.setBordered(false);
            b.setTag(s.raw() as isize);
            b.setWantsLayer(true);
            if let Some(layer) = b.layer() {
                unsafe {
                    let _: () = msg_send![&*layer, setCornerRadius: 6.0f64];
                }
            }
            b.setFrame(CGRect::new(CGPoint::new(x, 11.0), CGSize::new(24.0, 24.0)));
            bar.addSubview(&b);
            bar.ivars().size_buttons.borrow_mut().push((s, b));
            x += 24.0 + 6.0;
        }
        // The color group may hide entirely (mosaic).
        let d = divider_26();
        d.setFrameOrigin(CGPoint::new(x, 10.0));
        bar.addSubview(&d);
        *bar.ivars().color_divider.borrow_mut() = Some(d);
        x += 1.0 + 6.0;
        for (i, c) in palette().iter().enumerate() {
            let b = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(""),
                    target,
                    Some(sel!(pickColor:)),
                    mtm,
                )
            };
            b.setBordered(false);
            b.setTag(i as isize);
            b.setWantsLayer(true);
            if let Some(layer) = b.layer() {
                let cg_color = c.CGColor();
                unsafe {
                    let _: () = msg_send![&*layer, setBackgroundColor: &*cg_color];
                    let _: () = msg_send![&*layer, setCornerRadius: 6.0f64];
                    let _: () = msg_send![&*layer, setBorderWidth: 1.0f64];
                    let clear = NSColor::clearColor().CGColor();
                    let _: () = msg_send![&*layer, setBorderColor: &*clear];
                }
            }
            b.setFrame(CGRect::new(CGPoint::new(x, 10.0), CGSize::new(26.0, 26.0)));
            bar.addSubview(&b);
            bar.ivars().color_buttons.borrow_mut().push(b);
            x += 26.0 + 6.0;
        }
        let width = 12.0 + 84.0 + 6.0 + 1.0 + 6.0 + 218.0 + 16.0;
        bar.setFrameSize(CGSize::new(width, 46.0 + POINTER_H));
        bar
    }

    /// `fittingSize` — (343, 46 + pointer).
    pub fn fitting(&self) -> CGSize {
        CGSize::new(12.0 + 84.0 + 6.0 + 1.0 + 6.0 + 218.0 + 16.0, 46.0 + POINTER_H)
    }

    fn set_pointer_x(&self, x: f64) {
        self.ivars().pointer_x.set(x);
    }

    fn set_points_up(&self, up: bool) {
        self.ivars().points_up.set(up);
        // The stack is centred in the body, not the pointer strip (`layout`).
        let dy = if up { 0.0 } else { POINTER_H };
        for (_, b) in self.ivars().size_buttons.borrow().iter() {
            let mut f = b.frame();
            f.origin.y = 11.0 + dy;
            b.setFrame(f);
        }
        if let Some(d) = self.ivars().color_divider.borrow().as_ref() {
            let mut f = d.frame();
            f.origin.y = 10.0 + dy;
            d.setFrame(f);
        }
        for b in self.ivars().color_buttons.borrow().iter() {
            let mut f = b.frame();
            f.origin.y = 10.0 + dy;
            b.setFrame(f);
        }
    }

    /// `configure(kind:)` — sizes · colors (none for mosaic).
    pub fn configure(&self, kind: AnnotateTool) {
        let Some(canvas) = self.ivars().canvas.borrow().clone() else { return };
        let colored = kind != AnnotateTool::Mosaic;
        if let Some(d) = self.ivars().color_divider.borrow().as_ref() {
            d.setHidden(!colored);
        }
        for b in self.ivars().color_buttons.borrow().iter() {
            b.setHidden(!colored);
        }
        let sel_size = canvas.effective_size();
        for (s, b) in self.ivars().size_buttons.borrow().iter() {
            let on = *s == sel_size;
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
            if kind == AnnotateTool::Text {
                b.setImage(None);
                let label = [l("小"), l("中"), l("大")][s.raw() as usize - 1].clone();
                let dict = crate::shelf::card::attrs(&theme::serif(13.0, on), &tint, None);
                let attr = unsafe {
                    objc2_foundation::NSAttributedString::initWithString_attributes(
                        objc2::AllocAnyThread::alloc(),
                        &NSString::from_str(&label),
                        Some(&dict),
                    )
                };
                b.setAttributedTitle(&attr);
            } else {
                let empty = objc2_foundation::NSAttributedString::new();
                b.setAttributedTitle(&empty);
                let img = dot_image(s.dot_diameter(), &tint);
                b.setImage(Some(&img));
                b.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
            }
        }
        let sel_color = canvas.effective_color();
        for (i, b) in self.ivars().color_buttons.borrow().iter().enumerate() {
            let c = palette()[i].clone();
            let on = crate::annotate::annotation::same_color(&c, &sel_color);
            if on {
                let img = check_image(&c);
                b.setImage(Some(&img));
            } else {
                b.setImage(None);
            }
            b.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
            if let Some(layer) = b.layer() {
                unsafe {
                    if on {
                        let c = theme::paper().CGColor();
                        let _: () = msg_send![&*layer, setBorderColor: &*c];
                        let _: () = msg_send![&*layer, setBorderWidth: 2.0f64];
                    } else {
                        let c = NSColor::clearColor().CGColor();
                        let _: () = msg_send![&*layer, setBorderColor: &*c];
                        let _: () = msg_send![&*layer, setBorderWidth: 1.0f64];
                    }
                }
            }
        }
    }
}

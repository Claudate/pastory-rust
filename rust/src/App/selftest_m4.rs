//! Port of the M4 render/text selftests (`App/SelfTest.swift` annotate +
//! ocr/ocrpanel, `App/AnnotationTextSelfTest.swift`).

use objc2::rc::Retained;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;

use crate::annotate::annotate_view::AnnotateView;
use objc2::runtime::NSObjectProtocol as _;
use crate::annotate::annotation::{palette, AnnotateTool, Annotation, StrokeSize};
use crate::annotate::ocr;
use crate::app::theme;
use crate::capture::screenshotter;

/// `--selftest ocr [in.png]` (SelfTest.ocr).
pub fn ocr(path: Option<&str>) -> bool {
    let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
    let _app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let (image, expect): (objc2_core_foundation::CFRetained<CGImage>, Vec<&str>) =
        if let Some(path) = path {
            let Some(img) = screenshotter::image_from_file(std::path::Path::new(path)) else {
                println!("cannot read {path}");
                return false;
            };
            (img, vec![])
        } else {
            let Some(img) = crate::app::selftest_m2::render_sample() else {
                println!("sample render failed");
                return false;
            };
            (img, vec!["Pastory", "截图工具", "2026"])
        };
    let t0 = std::time::Instant::now();
    match ocr::recognize(&image) {
        Ok(text) => {
            println!(
                "--- OCR ({} ms) ---\n{}\n---",
                (t0.elapsed().as_secs_f64() * 1000.0) as i64,
                text
            );
            let missing: Vec<&&str> = expect.iter().filter(|e| !text.contains(**e)).collect();
            if !missing.is_empty() {
                println!("missing: {missing:?}");
                return false;
            }
            true
        }
        Err(e) => {
            println!("ocr failed: {e}");
            false
        }
    }
}

fn seg(x1: f64, y1: f64, x2: f64, y2: f64) -> Vec<CGPoint> {
    vec![CGPoint::new(x1, y1), CGPoint::new(x2, y2)]
}

/// `screen.frame` of the main screen (OverlayView mask sizing).
fn main_screen_frame() -> CGRect {
    let mtm = objc2::MainThreadMarker::new().expect("main thread");
    objc2_app_kit::NSScreen::mainScreen(mtm)
        .map(|s| s.frame())
        .unwrap_or(CGRect::ZERO)
}

/// Stage backdrop blue (SelfTest's desktop stand-in).
fn slate_color() -> Retained<objc2_app_kit::NSColor> {
    objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(0.35, 0.45, 0.55, 1.0)
}

/// `--selftest annotate [out.png]` (SelfTest.renderAnnotate): canvas +
/// toolbar + mask render, idle/selected/arrow frames, plus the flattened
/// export.
pub fn annotate(out: &str) -> bool {
    let Some(img) = crate::app::selftest_m2::render_sample() else {
        return false;
    };
    let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let scale = 2.0f64;
    let rect = CGRect::new(
        CGPoint::new(220.0, 150.0),
        CGSize::new(
            CGImage::width(Some(&img)) as f64 / scale,
            CGImage::height(Some(&img)) as f64 / scale,
        ),
    );
    let stage = objc2_app_kit::NSView::new(mtm);
    stage.setWantsLayer(true);
    stage.setFrame(CGRect::new(
        CGPoint::ZERO,
        CGSize::new((rect.size.width + 80.0).max(900.0), rect.size.height + 320.0),
    ));
    if let Some(layer) = stage.layer() {
        let c = slate_color().CGColor();
        unsafe {
            let _: () = objc2::msg_send![&*layer, setBackgroundColor: &*c];
        }
    }
    // The frozen mask behind the frame (OverlayView "held" state).
    let mask = crate::capture::selection_overlay::debug_overlay_view(
        stage.frame().size,
        main_screen_frame(),
    );
    mask.set_held(true);
    mask.set_held_rect(Some(rect));
    stage.addSubview(&mask);
    let canvas = AnnotateView::make(rect, img);
    stage.addSubview(&canvas);
    let top = crate::capture::selection_overlay::debug_top_bar();
    let ts = top.frame().size;
    top.setFrameOrigin(CGPoint::new(
        ((stage.bounds().min().x + stage.bounds().size.width / 2.0) - ts.width / 2.0).round(),
        stage.bounds().max().y - 16.0 - ts.height,
    ));
    stage.addSubview(&top);
    let bar = crate::annotate::toolbar::AnnotateToolbar::make(
        &canvas,
        &crate::app::localization::l("复制"),
    );
    let bf = bar.fitting();
    bar.setFrame(CGRect::new(
        CGPoint::new(
            8.0f64.max((rect.origin.x + rect.size.width / 2.0 - bf.width / 2.0).round()),
            rect.origin.y - 68.0,
        ),
        bf,
    ));
    stage.addSubview(&bar);
    bar.did_layout();

    let window = unsafe {
        objc2_app_kit::NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<objc2_app_kit::NSWindow>(),
            stage.frame(),
            objc2_app_kit::NSWindowStyleMask::Borderless,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setContentView(Some(&stage));
    unsafe {
        window.setReleasedWhenClosed(false);
    }

    let colors = palette();
    let (ink, red, blue, orange) = (
        colors[5].clone(),
        colors[1].clone(),
        colors[4].clone(),
        colors[2].clone(),
    );
    let pen_pts: Vec<CGPoint> = (0..40)
        .map(|i| {
            let x = i as f64 * 3.0;
            CGPoint::new(280.0 + x, 96.0 + 9.0 * (x / 7.0).sin())
        })
        .collect();
    let text = Annotation {
        text: "你好，今天天气怎么样？".into(),
        ..Annotation::new(
            AnnotateTool::Text,
            blue.clone(),
            StrokeSize::M,
            vec![CGPoint::new(262.0, 6.0)],
        )
    };
    let text2 = Annotation {
        text: "Hello Pastory".into(),
        ..Annotation::new(
            AnnotateTool::Text,
            ink.clone(),
            StrokeSize::S,
            vec![CGPoint::new(300.0, 32.0)],
        )
    };
    canvas.set_annotations_and_select(
        vec![
            Annotation::new(AnnotateTool::Rect, red.clone(), StrokeSize::M, seg(12.0, 8.0, 250.0, 44.0)),
            Annotation::new(AnnotateTool::Arrow, blue.clone(), StrokeSize::L, seg(300.0, 118.0, 215.0, 52.0)),
            Annotation::new(AnnotateTool::Ellipse, orange.clone(), StrokeSize::S, seg(20.0, 56.0, 180.0, 104.0)),
            Annotation::new(AnnotateTool::Line, ink.clone(), StrokeSize::S, seg(260.0, 60.0, 430.0, 60.0)),
            Annotation::new(AnnotateTool::Pen, red.clone(), StrokeSize::M, pen_pts),
            Annotation::new(AnnotateTool::Mosaic, red.clone(), StrokeSize::M, seg(20.0, 72.0, 200.0, 108.0)),
            text,
            text2,
        ],
        None,
    );
    let base = std::path::Path::new(out);
    let stem = base.with_extension("");
    let idle_path = format!("{}.idle.png", stem.display());
    if !crate::app::selftest_m2::snapshot(&stage, &idle_path) {
        return false;
    }
    canvas.set_annotations_and_select(canvas.annotations(), Some(0));
    if !crate::app::selftest_m2::snapshot(&stage, out) {
        return false;
    }
    canvas.set_annotations_and_select(canvas.annotations(), Some(1));
    let arrow_path = format!("{}.arrow.png", stem.display());
    let _ = crate::app::selftest_m2::snapshot(&stage, &arrow_path);
    let flat = canvas.rendered_image();
    let flat_path = format!("{}.flat.png", stem.display());
    if let Some(png) = screenshotter::png_data(&flat) {
        let _ = std::fs::write(&flat_path, &png);
    }
    let cs = CGImage::color_space(Some(&flat))
        .and_then(|s| objc2_core_graphics::CGColorSpace::name(Some(&s)))
        .map(|n| n.to_string())
        .unwrap_or_else(|| "nil".into());
    println!(
        "flattened {}×{} colorSpace={} → {}",
        CGImage::width(Some(&flat)),
        CGImage::height(Some(&flat)),
        cs,
        flat_path
    );
    true
}

/// `--selftest ocrpanel [out.png]` (SelfTest:126).
pub fn ocrpanel(out: &str) -> bool {
    let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let sample = "Pastory 是一个截图工具\n所有复制过的内容都留在剪贴板里\nMade in 2026 · 中英混排 OK";
    let view = crate::annotate::ocr_panel::controller().debug_view(sample);
    crate::app::selftest_m2::snapshot(&view, out)
}

// MARK: annotationtext (AnnotationTextSelfTest.swift)

struct Checks(bool);
impl Checks {
    fn check(&mut self, label: &str, cond: bool) {
        println!("{} {}", if cond { "ok  " } else { "FAIL" }, label);
        self.0 = self.0 && cond;
    }
}

/// `--selftest annotationtext [out.png]` (AnnotationTextSelfTest).
pub fn annotationtext(out: &str) -> bool {
    let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let mut ok = Checks(true);

    let sample = if crate::app::localization::is_english() {
        "Resize this text box to wrap words automatically.\nKeep the font size, and all the text visible. 👋🏽"
    } else {
        "拖动文字框的边或四角，文字会随宽度自动换行。\n字号保持不变，完整内容不会被截掉。👋🏽"
    };
    let original = {
        let mut a = Annotation::new(
            AnnotateTool::Text,
            theme::purple(),
            StrokeSize::M,
            vec![CGPoint::new(80.0, 80.0)],
        );
        a.text = sample.into();
        a.text_box_size = Some(CGSize::new(420.0, 220.0));
        a
    };
    let mut narrow = original.clone();
    narrow.set_handle(5, CGPoint::new(280.0, original.bounds().mid().y));
    ok.check(
        "narrowing wraps text without changing its content or font",
        narrow.text_layout().height() > original.text_layout().height()
            && narrow.size == original.size
            && narrow.text == original.text,
    );
    ok.check("text has corner and edge handles", narrow.handles().len() == 8);
    {
        let b = narrow.bounds();
        let before = original.bounds();
        ok.check(
            "right edge preserves top-left anchor",
            b.origin == before.origin && (b.size.width - 200.0).abs() < 1e-9,
        );
    }
    narrow.set_handle(7, CGPoint::new(narrow.bounds().mid().x, 600.0));
    ok.check(
        "height can be extended independently",
        (narrow.bounds().size.height - 520.0).abs() < 1e-9
            && (narrow.bounds().size.width - 200.0).abs() < 1e-9,
    );
    narrow.set_handle(7, CGPoint::new(narrow.bounds().mid().x, 81.0));
    ok.check(
        "height cannot clip wrapped text",
        (narrow.bounds().size.height - narrow.text_layout().height()).abs() < 0.5,
    );
    for handle in 0..8 {
        let mut a = original.clone();
        let p = a.handles()[handle];
        a.set_handle(handle, CGPoint::new(p.x + 24.0, p.y + 12.0));
        let (r, before) = (a.bounds(), original.bounds());
        let horizontal = if [0usize, 2, 4].contains(&handle) {
            r.max().x == before.max().x
        } else {
            r.min().x == before.min().x
        };
        let vertical = if [0usize, 1, 6].contains(&handle) {
            r.max().y == before.max().y
        } else {
            r.min().y == before.min().y
        };
        ok.check(&format!("handle {handle} preserves opposite edges"), horizontal && vertical);
    }
    let mut crossed = original.clone();
    crossed.set_handle(0, CGPoint::new(900.0, 900.0));
    ok.check(
        "dragging past the opposite corner keeps a usable box",
        (crossed.bounds().size.width - 32.0).abs() < 1e-9
            && crossed.bounds().size.height >= crossed.text_layout().height(),
    );
    {
        let empty = crate::annotate::text_layout::AnnotationTextLayout::new(
            "",
            StrokeSize::M,
            &theme::purple(),
            200.0,
        );
        ok.check("empty text still has a line for the caret", empty.height() > 0.0);
    }
    {
        let trailing = crate::annotate::text_layout::AnnotationTextLayout::new(
            "Hello\n",
            StrokeSize::M,
            &theme::purple(),
            200.0,
        );
        let single = crate::annotate::text_layout::AnnotationTextLayout::new(
            "Hello",
            StrokeSize::M,
            &theme::purple(),
            200.0,
        );
        ok.check(
            "explicit trailing newline has room for the caret",
            trailing.height() > single.height(),
        );
    }
    annotationtext_live(&mut ok, out, &original, sample)
}

/// The mouse/key-driven half (events fabricated through NSEvent — the Swift
/// test's exact route through `mouseDown:`/`keyDown:`).
fn annotationtext_live(ok: &mut Checks, out: &str, original: &Annotation, sample: &str) -> bool {
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSTextInputClient, NSView, NSWindow};
    use objc2_foundation::NSRange;

    let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
    let (w, h) = (1440usize, 1040usize);
    let space = unsafe {
        objc2_core_graphics::CGColorSpace::with_name(Some(objc2_core_graphics::kCGColorSpaceSRGB))
    };
    let Some(ctx) = (unsafe {
        objc2_core_graphics::CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, space.as_deref(), 1)
    }) else {
        return false;
    };
    objc2_core_graphics::CGContext::set_fill_color_with_color(Some(&ctx), Some(&theme::paper().CGColor()));
    objc2_core_graphics::CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64)));
    let Some(image) = objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx)) else {
        return false;
    };
    let canvas = AnnotateView::make(CGRect::new(CGPoint::ZERO, CGSize::new(720.0, 520.0)), image.clone());
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            canvas.frame(),
            objc2_app_kit::NSWindowStyleMask::Borderless,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    window.setContentView(Some(&canvas));
    let window_number = window.windowNumber();

    let mouse = |kind: NSEventType, point: CGPoint, clicks: isize| -> Retained<NSEvent> {
        let loc = canvas.convertPoint_toView(point, None);
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            loc,
            NSEventModifierFlags::empty(),
            0.0,
            window_number,
            None,
            0,
            clicks,
            0.0,
        )
        .expect("synthetic mouse event")
    };
    let canvas_obj: &AnyObject = unsafe { &*(&*canvas as *const AnnotateView as *const AnyObject) };
    let click = |point: CGPoint| unsafe {
        let down = mouse(NSEventType::LeftMouseDown, point, 1);
        let up = mouse(NSEventType::LeftMouseUp, point, 1);
        let _: () = objc2::msg_send![canvas_obj, mouseDown: &*down];
        let _: () = objc2::msg_send![canvas_obj, mouseUp: &*up];
    };
    let two_step_drag = |from: CGPoint, to: CGPoint| unsafe {
        let down = mouse(NSEventType::LeftMouseDown, from, 1);
        let dragged_a = mouse(NSEventType::LeftMouseDragged, CGPoint::new(to.x + 20.0, to.y), 1);
        let dragged = mouse(NSEventType::LeftMouseDragged, to, 1);
        let up = mouse(NSEventType::LeftMouseUp, to, 1);
        let _: () = objc2::msg_send![canvas_obj, mouseDown: &*down];
        let _: () = objc2::msg_send![canvas_obj, mouseDragged: &*dragged_a];
        let _: () = objc2::msg_send![canvas_obj, mouseDragged: &*dragged];
        let _: () = objc2::msg_send![canvas_obj, mouseUp: &*up];
    };
    let plain_drag = |from: CGPoint, to: CGPoint| unsafe {
        let down = mouse(NSEventType::LeftMouseDown, from, 1);
        let dragged = mouse(NSEventType::LeftMouseDragged, to, 1);
        let up = mouse(NSEventType::LeftMouseUp, to, 1);
        let _: () = objc2::msg_send![canvas_obj, mouseDown: &*down];
        let _: () = objc2::msg_send![canvas_obj, mouseDragged: &*dragged];
        let _: () = objc2::msg_send![canvas_obj, mouseUp: &*up];
    };
    let return_key = |modifiers: NSEventModifierFlags| -> Retained<NSEvent> {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            CGPoint::ZERO,
            modifiers,
            0.0,
            window_number,
            None,
            &objc2_foundation::NSString::from_str("\r"),
            &objc2_foundation::NSString::from_str("\r"),
            false,
            36,
        )
        .expect("synthetic key event")
    };

    use crate::annotate::text_view::AnnotationTextView;
    use objc2::ClassType as _;

    fn active_editor(canvas: &AnnotateView) -> Option<Retained<AnnotationTextView>> {
        for v in canvas.subviews().iter() {
            if v.isKindOfClass(AnnotationTextView::class()) {
                return unsafe {
                    Retained::retain(&*v as *const NSView as *const AnyObject as *mut AnnotationTextView)
                };
            }
        }
        None
    }
    let _ = two_step_drag;
    let _ = plain_drag;

    canvas.set_tool(Some(AnnotateTool::Text));
    canvas.set_size(StrokeSize::M);

    // 1. Click: a multiline editor opens; typing grows it to the wrapped
    // content, hugging text up to the right edge of the picture.
    click(CGPoint::new(80.0, 80.0));
    let Some(editor) = active_editor(&canvas) else {
        ok.check("opens a multiline editor", false);
        return false;
    };
    ok.check("opens a multiline editor", true);
    {
        let ns = objc2_foundation::NSString::from_str(sample);
        let any: &AnyObject = unsafe { &*(&*ns as *const objc2_foundation::NSString as *const AnyObject) };
        unsafe {
            editor.insertText_replacementRange(any, editor.selectedRange());
        }
    }
    let natural = crate::annotate::annotation::measure_hand_text(sample, canvas.size()) + 6.0;
    let room = canvas.bounds().size.width - 80.0 - 6.0;
    let mut expected = original.clone();
    expected.text_box_size = if natural <= room {
        None
    } else {
        Some(CGSize::new(room, 0.0))
    };
    ok.check(
        "typing grows the editor to fit the wrapped content",
        editor.frame() == expected.bounds(),
    );
    if let (Some(lm), Some(container)) = (unsafe { editor.layoutManager() }, unsafe { editor.textContainer() }) {
        let used_h = lm.usedRectForTextContainer(&container).max().y.ceil();
        ok.check(
            "live editor and export have the same line layout",
            used_h == expected.text_layout().height(),
        );
    } else {
        ok.check("live editor and export have the same line layout", false);
    }
    let url = std::path::Path::new(out).with_extension("");
    let editing_path = format!("{}.editing.png", url.display());
    ok.check(
        "editing snapshot",
        crate::app::selftest_m2::snapshot(&canvas, &editing_path),
    );
    // The editor lets the canvas receive resize handles (window coords in
    // = canvas coords when the canvas is the window's content view).
    let edge = crate::annotate::renderer::selection_handles(&expected)[5];
    {
        let loc = canvas.convertPoint_toView(edge, None);
        let hit: *mut NSView = unsafe { objc2::msg_send![canvas_obj, hitTest: loc] };
        let canvas_raw: *mut NSView = &*canvas as *const AnnotateView as *mut NSView;
        ok.check("editor lets the canvas receive resize handles", hit == canvas_raw);
    }
    // 2. Drag the right edge grip: commits, resizes to 180, anchor keeps.
    {
        let down = mouse(NSEventType::LeftMouseDown, edge, 1);
        let dragged_a = mouse(
            NSEventType::LeftMouseDragged,
            CGPoint::new(80.0 + 180.0 + crate::annotate::renderer::TEXT_INSET + 20.0, edge.y),
            1,
        );
        let dragged = mouse(
            NSEventType::LeftMouseDragged,
            CGPoint::new(80.0 + 180.0 + crate::annotate::renderer::TEXT_INSET, edge.y),
            1,
        );
        let up = mouse(
            NSEventType::LeftMouseUp,
            CGPoint::new(80.0 + 180.0 + crate::annotate::renderer::TEXT_INSET, edge.y),
            1,
        );
        unsafe {
            let _: () = objc2::msg_send![canvas_obj, mouseDown: &*down];
            let _: () = objc2::msg_send![canvas_obj, mouseDragged: &*dragged_a];
            let _: () = objc2::msg_send![canvas_obj, mouseDragged: &*dragged];
            let _: () = objc2::msg_send![canvas_obj, mouseUp: &*up];
        }
    }
    let Some(resized) = canvas.selected() else {
        ok.check("resize commits the edited text", false);
        return false;
    };
    ok.check(
        "dragging an active editor commits and resizes it",
        active_editor(&canvas).is_none()
            && resized.text == sample
            && (resized.bounds().size.width - 180.0).abs() < 1e-9,
    );
    ok.check(
        "successive drag events keep the original anchor",
        resized.bounds().origin == expected.bounds().origin,
    );
    ok.check(
        "selected text snapshot",
        crate::app::selftest_m2::snapshot(&canvas, out),
    );

    // 3. Reopen: box dimensions + caret position preserved.
    click(CGPoint::new(100.0, 100.0));
    let Some(reopened) = active_editor(&canvas) else {
        ok.check("reopens resized text", false);
        return false;
    };
    ok.check(
        "reopening preserves box dimensions and Unicode caret position",
        reopened.frame() == resized.bounds()
            && reopened.selectedRange().location
                == sample.chars().map(|c| c.len_utf16()).sum::<usize>(),
    );
    // IME confirmation belongs to the input method, not the canvas's Return.
    {
        use objc2_app_kit::NSTextInputClient;
        let mark = objc2_foundation::NSString::from_str("候选");
        let mark_obj: &AnyObject = unsafe { &*(&*mark as *const objc2_foundation::NSString as *const AnyObject) };
        unsafe {
            reopened.setMarkedText_selectedRange_replacementRange(
                mark_obj,
                NSRange { location: 2, length: 0 },
                reopened.selectedRange(),
            );
        }
        let handled: objc2::runtime::Bool = unsafe {
            objc2::msg_send![canvas_obj, textView: &*reopened, doCommandBy: objc2::sel!(insertNewline:)]
        };
        ok.check(
            "Return does not commit an IME candidate",
            !handled.as_bool() && active_editor(&canvas).is_some(),
        );
        reopened.unmarkText();
    }
    // ⎋ restores the existing text and box geometry.
    {
        let _: objc2::runtime::Bool = unsafe {
            objc2::msg_send![canvas_obj, textView: &*reopened, doCommandBy: objc2::sel!(cancelOperation:)]
        };
        let sel = canvas.selected();
        ok.check(
            "Escape restores existing text and box geometry",
            sel.as_ref().map(|a| a.text.as_str()) == Some(sample)
                && sel.as_ref().map(|a| a.bounds()) == Some(resized.bounds()),
        );
    }
    // 4. Reopen + ⌘Return commits multiline editing.
    click(CGPoint::new(100.0, 100.0));
    let Some(final_editor) = active_editor(&canvas) else {
        return false;
    };
    {
        let ns = objc2_foundation::NSString::from_str(" ✓");
        let any: &AnyObject = unsafe { &*(&*ns as *const objc2_foundation::NSString as *const AnyObject) };
        unsafe {
            final_editor.insertText_replacementRange(any, final_editor.selectedRange());
        }
        let cmd_key = return_key(NSEventModifierFlags::Command);
        unsafe {
            let _: () = objc2::msg_send![&*final_editor, keyDown: &*cmd_key];
        }
    }
    ok.check("⌘Return commits multiline editing", active_editor(&canvas).is_none());
    {
        let sel = canvas.selected();
        ok.check(
            "editing preserves the resized width",
            sel.as_ref().map(|a| (a.bounds().size.width - 180.0).abs() < 1e-9) == Some(true)
                && sel.as_ref().map(|a| a.text.clone()) == Some(format!("{sample} ✓")),
        );
    }
    // 5. Return inside the editor is a line break, through window routing too.
    click(CGPoint::new(100.0, 100.0));
    let Some(line_editor) = active_editor(&canvas) else {
        return false;
    };
    let plain_key = return_key(NSEventModifierFlags::empty());
    unsafe {
        let _: () = objc2::msg_send![&*line_editor, keyDown: &*plain_key];
    }
    ok.check(
        "Return inserts a line break and keeps editing",
        active_editor(&canvas).is_some()
            && line_editor.string().to_string() == format!("{sample} ✓\n"),
    );
    window.sendEvent(&plain_key);
    ok.check(
        "Return routed through the window still reaches the text box",
        active_editor(&canvas).is_some()
            && line_editor.string().to_string() == format!("{sample} ✓\n\n"),
    );
    {
        let who = window.firstResponder().map(|r| {
            let name: Retained<objc2_foundation::NSString> = unsafe { objc2::msg_send![&*r, className] };
            name.to_string()
        });
        println!("     first responder: {}", who.unwrap_or_else(|| "nil".into()));
    }
    // 6. Click-outside-confirm + start-nothing-new.
    let before = canvas.annotations().len();
    click(CGPoint::new(600.0, 480.0));
    ok.check(
        "a click outside confirms the box and starts nothing new",
        active_editor(&canvas).is_none()
            && canvas.annotations().len() == before
            && canvas.selected_id().is_none(),
    );
    click(CGPoint::new(600.0, 480.0));
    ok.check("the next click starts a new box", active_editor(&canvas).is_some());
    canvas.commit_text_editor();

    // 7. Flattened export at full pixel size.
    let flat = canvas.rendered_image();
    ok.check(
        "export keeps the original image resolution",
        CGImage::width(Some(&flat)) == 1440 && CGImage::height(Some(&flat)) == 1040,
    );
    let flat_path = format!("{}.flat.png", url.display());
    if let Some(png) = screenshotter::png_data(&flat) {
        if std::fs::write(&flat_path, &png).is_err() {
            ok.check("writes flattened output", false);
        }
    } else {
        ok.check("writes flattened output", false);
    }
    // 8. Corner and delete-chip chrome on a text box.
    canvas.set_annotations_and_select(vec![original.clone()], Some(0));
    let corner = crate::annotate::renderer::selection_handles(original)[3];
    {
        let down = mouse(NSEventType::LeftMouseDown, corner, 1);
        let dragged = mouse(NSEventType::LeftMouseDragged, CGPoint::new(corner.x + 20.0, corner.y), 1);
        let up = mouse(NSEventType::LeftMouseUp, CGPoint::new(corner.x + 20.0, corner.y), 1);
        unsafe {
            let _: () = objc2::msg_send![canvas_obj, mouseDown: &*down];
            let _: () = objc2::msg_send![canvas_obj, mouseDragged: &*dragged];
            let _: () = objc2::msg_send![canvas_obj, mouseUp: &*up];
        }
    }
    ok.check(
        "a corner grip resizes the box",
        canvas.annotations().len() == 1
            && canvas
                .selected()
                .map(|a| (a.bounds().size.width - 440.0).abs() < 1e-9)
                .unwrap_or(false),
    );
    canvas.set_annotations_and_select(vec![original.clone()], Some(0));
    click(crate::annotate::renderer::delete_center(original));
    ok.check(
        "the chip on the top-right corner deletes",
        canvas.annotations().is_empty(),
    );
    ok.0
}

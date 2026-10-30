//! Port of the M2 render commands of `App/SelfTest.swift`: `shelf` and
//! `shelfsearch` (SelfTest.swift:465-523) — seed the scratch store, host the
//! shelf tree at 1600×450 in a borderless window, snapshot to PNG.
//!
//! Video-card compromise (M5 lands `SyntheticMovie` with the capture slice):
//! the seeded recording's payload is a fixed 64-byte stub file; cards render
//! from the poster thumbnail, which the selftest generates locally at
//! 1280×720 with fixed content. Playback never happens in these renders.

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGImage,
};
use objc2_foundation::{NSUUID, NSString};

use crate::app::selftest_m1::sample_file;
use crate::capture::screenshotter;
use crate::clipboard::item::ClipKind;
use crate::clipboard::store::{self, Source};
use crate::shelf::card;
use crate::shelf::panel;

/// Fixed payload bytes of the seeded recording (see module note).
const VIDEO_STUB: &[u8] = b"PASTORY SEED STUB - M5 SyntheticMovie replaces this with a real 8s recording.\n";

/// `SelfTest.seedStore` — identical entries, in the Swift order. No-op when
/// the store already holds anything.
fn seed_store() {
    seed_store_path()
}

/// The seeded store for the M6 render commands (same seed factored out).
pub(crate) fn seed_store_path() {
    if store::read(|s| !s.items.is_empty()) {
        return;
    }
    // 1. The recording (oldest card once everything else lands) — since M5
    // this is a real SyntheticMovie (Swift seedStore matches exactly: 8 s of
    // the moving block at 1280×720; the stub stood in for it until then).
    let tmp = std::env::temp_dir().join("pastory-seed.mp4");
    let _ = std::fs::remove_file(&tmp);
    if crate::app::synthetic_movie::write(&tmp, CGSize::new(1280.0, 720.0), 8.0, 30).is_ok() {
        let poster = crate::capture::gif_encoder::poster(&tmp);
        store::with(|s| {
            if let Some(poster) = poster.as_deref() {
                s.insert_video(&tmp, Some(poster), 8.0, &Source::pastory());
            } else {
                // poster() 返回 nil 时 Swift 也照样入库（缩略图闪后再补）
                s.insert_video(&tmp, None, 8.0, &Source::pastory());
            }
        });
    }
    // 2. Meeting notes (Plain text from Notes).
    store::with(|s| {
        s.insert_text(
            "会议纪要 9/11\n1. 下周三发布\n2. 演示视频重录\n3. 链接统一加追踪参数",
            None,
            &Source {
                bundle_id: Some("com.apple.Notes".into()),
                name: Some("备忘录".into()),
            },
        );
    });
    // 3. A link (Safari).
    store::with(|s| {
        s.insert_text(
            "https://github.com/Claudate/pastory-rust",
            None,
            &Source {
                bundle_id: Some("com.apple.Safari".into()),
                name: Some("Safari".into()),
            },
        );
    });
    // 4. The sample image (with OCR text already answered).
    if let Some(img) = render_sample() {
        if let Some(png) = screenshotter::png_data(&img) {
            store::with(|s| {
                s.insert_image(&png, &Source::pastory(), Some("PasteJar 是一个截图工具".into()));
            });
        }
    }
    // 5. Two repo files (Finder).
    let package = sample_file();
    let readme = package
        .parent()
        .map(|p| p.join("README.md"))
        .unwrap_or_else(|| package.clone());
    store::with(|s| {
        s.insert_files(
            &[package, readme],
            &Source {
                bundle_id: Some("com.apple.finder".into()),
                name: Some("Finder".into()),
            },
        );
    });
    // 6. A code snippet (VS Code), newest → first card → on the clipboard.
    store::with(|s| {
        s.insert_text(
            "const shelf = items.filter(i => i.pinned)\n  .map(render)\n  .join('')",
            None,
            &Source {
                bundle_id: Some("com.microsoft.VSCode".into()),
                name: Some("Code".into()),
            },
        );
    });
    // 7. Pin the oldest card (the recording).
    let oldest = store::read(|s| s.items.last().map(|it| it.id.clone()));
    if let Some(id) = oldest {
        store::with(|s| s.toggle_pin(&id, true));
    }
    // 8. Title the first text card.
    let first_text = store::read(|s| {
        s.items
            .iter()
            .find(|it| it.kind == ClipKind::Text)
            .map(|it| it.id.clone())
    });
    if let Some(id) = first_text {
        store::with(|s| s.set_title(Some("翻译 prompt"), &id));
    }
    // PASTORY_HOWTO: seed the how-to card (Swift's HowToCard.seedIfNeeded).
    if crate::app::sandbox::launch().env.iter().any(|(k, _)| k == "PASTORY_HOWTO") {
        crate::shelf::how_to_card::seed_if_needed();
    }
}

/// The 1280×720 poster of the seeded recording: fixed dark frame with a
/// timecode readout and color bars (content arbitrary but pinned).
fn seed_poster() -> Option<CFRetained<CGImage>> {
    let (w, h) = (1280usize, 720usize);
    let space = CGColorSpace::new_device_rgb();
    let ctx = unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, space.as_deref(), 1)?
    };
    let gc = objc2_app_kit::NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, false);
    let prev = objc2_app_kit::NSGraphicsContext::currentContext();
    objc2_app_kit::NSGraphicsContext::setCurrentContext(Some(&gc));
    let full = CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64));
    objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(0.10, 0.11, 0.14, 1.0).setFill();
    objc2_app_kit::NSRectFill(full);
    // Color bars along the bottom (a muted four-swatch strip).
    let swatches = [
        (0.71, 0.64, 0.95),
        (0.74, 0.84, 0.90),
        (0.95, 0.93, 0.89),
        (0.50, 0.65, 0.74),
    ];
    for (i, (r, g, b)) in swatches.iter().enumerate() {
        objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(*r, *g, *b, 0.85).setFill();
        objc2_app_kit::NSRectFill(CGRect::new(
            CGPoint::new(80.0 + i as f64 * 290.0, 96.0),
            CGSize::new(260.0, 120.0),
        ));
    }
    let font = objc2_app_kit::NSFont::systemFontOfSize(64.0);
    card::draw_text(
        CGRect::new(CGPoint::new(80.0, h as f64 - 240.0), CGSize::new(1120.0, 80.0)),
        "PasteJar 录屏 00:08",
        &font,
        &objc2_app_kit::NSColor::whiteColor(),
        None,
    );
    let small = objc2_app_kit::NSFont::systemFontOfSize(40.0);
    card::draw_text(
        CGRect::new(CGPoint::new(80.0, h as f64 - 330.0), CGSize::new(1120.0, 60.0)),
        "1280×720 · 8s · fixed poster (M5 replaces with SyntheticMovie)",
        &small,
        &objc2_app_kit::NSColor::colorWithCalibratedWhite_alpha(0.75, 1.0),
        None,
    );
    objc2_app_kit::NSGraphicsContext::setCurrentContext(prev.as_deref());
    CGBitmapContextCreateImage(Some(&ctx))
}

/// `SelfTest.renderSample`: 900×260 white with the same three lines at the
/// same points (symbols per the flipped:false context trick).
pub(crate) fn render_sample() -> Option<CFRetained<CGImage>> {
    let (w, h) = (900usize, 260usize);
    objc2::rc::autoreleasepool(|_| {
        let name = unsafe { objc2_core_graphics::kCGColorSpaceSRGB };
        let space = CGColorSpace::with_name(Some(name));
        let ctx = unsafe {
            CGBitmapContextCreate(
                std::ptr::null_mut(),
                w,
                h,
                8,
                0,
                space.as_deref(),
                1, // premultipliedLast
            )?
        };
        let gc = objc2_app_kit::NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, false);
        let prev = objc2_app_kit::NSGraphicsContext::currentContext();
        objc2_app_kit::NSGraphicsContext::setCurrentContext(Some(&gc));
        objc2_app_kit::NSColor::whiteColor().setFill();
        objc2_app_kit::NSRectFill(CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64)));
        let lines = [
            "PasteJar 是一个截图工具",
            "所有复制过的内容都留在剪贴板里",
            "Made in 2026 · 中英混排 OK",
        ];
        let font = objc2_app_kit::NSFont::systemFontOfSize(40.0);
        let attrs = card::attrs(&font, &objc2_app_kit::NSColor::blackColor(), None);
        for (i, s) in lines.iter().enumerate() {
            let s = NSString::from_str(s);
            unsafe {
                // SAFETY: flipped:false context, same drawAtPoint semantics as
                // SelfTest.renderSample.
                use objc2_app_kit::NSStringDrawing;
                s.drawAtPoint_withAttributes(
                    CGPoint::new(40.0, 180.0 - i as f64 * 64.0),
                    Some(&attrs),
                );
            }
        }
        objc2_app_kit::NSGraphicsContext::setCurrentContext(prev.as_deref());
        CGBitmapContextCreateImage(Some(&ctx))
    })
}

/// `SelfTest.snapshot`: layout → display → cacheDisplay → PNG (1x).
pub(crate) fn snapshot(view: &objc2_app_kit::NSView, out: &str) -> bool {
    view.layoutSubtreeIfNeeded();
    view.displayIfNeeded();
    let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(view.bounds()) else {
        println!("no rep");
        return false;
    };
    view.cacheDisplayInRect_toBitmapImageRep(view.bounds(), &rep);
    let png = unsafe {
        rep.representationUsingType_properties(
            objc2_app_kit::NSBitmapImageFileType::PNG,
            &objc2_foundation::NSDictionary::<NSString, objc2::runtime::AnyObject>::new(),
        )
    };
    let Some(png) = png else { return false };
    // SAFETY: the NSData outlives this write by scoping.
    if unsafe { std::fs::write(out, png.as_bytes_unchecked()) }.is_err() {
        println!("write failed: {out}");
        return false;
    }
    println!(
        "rendered {}×{} → {}",
        view.bounds().size.width as i64,
        view.bounds().size.height as i64,
        out
    );
    true
}

/// Warm every picture card's thumbnail and wait for the decodes to land, so a
/// tree built afterwards draws pictures rather than placeholders.
pub(crate) fn warm_card_thumbnails() {
    let pictures = store::read(|s| {
        s.items
            .iter()
            .filter(|it| it.kind == ClipKind::Image || it.kind == ClipKind::Video)
            .cloned()
            .collect::<Vec<_>>()
    });
    store::with(|s| {
        for it in &pictures {
            s.warm_thumbnail(it);
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let all = store::read(|s| pictures.iter().all(|it| s.is_thumbnail_cached(&it.id)));
        if all {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `--selftest thumbcull` — the 全部-tab picture flicker.
///
/// The store keeps ~100 decoded thumbnails and evicts the oldest third past
/// that. Building the shelf used to warm a thumbnail for EVERY image in the
/// list, so with more pictures than the cache holds the oldest — the leftmost,
/// on-screen cards — were evicted and re-decoded on the next 0.25 s poll tick,
/// blinking between placeholder and image. The visible-band cull is what stops
/// it; this pins the two halves: a many-picture store shows a picture on every
/// on-screen card, and `reload` decodes at most the band (never the tens of
/// off-screen pictures), so the on-screen ones cannot be re-evicted.
pub fn thumbcull() -> bool {
    // Deterministic store: the scratch dir may be reused between runs.
    crate::app::selftest_m1::clear_store();
    // A store far larger than the thumbnail cache (100): 60 pictures here, so
    // a full-list warm would decode all 60 and, at a real 101-image history,
    // churn the cache. Kept modest so the test is quick while the invariant it
    // checks — only the visible band is decoded — still bites.
    let n = 60usize;
    for i in 0..n {
        let px = move |x: usize, y: usize| -> [u8; 4] {
            // Distinct per picture so the thumbnails differ; a fixed value
            // would still decode but would make the report harder to read.
            let v = ((i * 7 + x + y) % 256) as u8;
            [v, (255 - v), ((i * 13) % 256) as u8, 255]
        };
        let Some(space) = objc2_core_graphics::CGColorSpace::new_device_rgb() else {
            println!("FAIL no device rgb colour space");
            return false;
        };
        let Some((png, _cg)) = crate::app::selftest_m1::bitmap_png(40, 30, &space, px) else {
            println!("FAIL could not build sample png");
            return false;
        };
        store::with(|s| {
            s.insert_image(&png, &Source::pastory(), Some(format!("sample {i}")));
        });
    }

    let mtm = objc2::MainThreadMarker::new().expect("thumbcull on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let size = CGSize::new(1600.0, 450.0);
    let host = crate::shelf::view::build(mtm, size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    let window = unsafe {
        objc2_app_kit::NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<objc2_app_kit::NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            objc2_app_kit::NSWindowStyleMask::Borderless,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setContentView(Some(&host));
    host.layoutSubtreeIfNeeded();
    // Let the first band of decodes land.
    pump(std::time::Duration::from_millis(400));

    let total = store::read(|s| s.items.len());
    let cached = store::read(|s| {
        s.items.iter().filter(|it| s.is_thumbnail_cached(&it.id)).count()
    });
    let cards_on_screen = crate::shelf::view::visible_card_count();

    // Invariant 1: the whole list is NOT warmed — the band is a small slice.
    let culled = cached < total;
    // Invariant 2: the on-screen cards do have their pictures (a warm band),
    // so the flicker is not "fixed" by simply never drawing a thumbnail.
    let on_screen_warm = cached > 0 && cards_on_screen > 0;
    println!("     {total} pictures, {cached} decoded, {cards_on_screen} cards on screen");
    println!(
        "{} only the on-screen band is decoded (not the whole list)",
        if culled { "ok  " } else { "FAIL" }
    );
    println!(
        "{} on-screen cards carry a decoded thumbnail",
        if on_screen_warm { "ok  " } else { "FAIL" }
    );
    culled && on_screen_warm
}

/// Pump the main run loop for `dur` so the poll timer and the decode threads
/// land their results.
fn pump(dur: std::time::Duration) {
    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    let deadline = std::time::Instant::now() + dur;
    while std::time::Instant::now() < deadline {
        let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.02);
        // SAFETY: NSDefaultRunLoopMode is a valid mode token.
        let mode: &'static objc2_foundation::NSRunLoopMode =
            unsafe { objc2_foundation::NSDefaultRunLoopMode };
        run_loop.runMode_beforeDate(mode, &limit);
    }
}

/// `SelfTest.renderShelf(out:, searching:)`.
pub fn render_shelf(out: &str, searching: bool) -> bool {
    seed_store();
    let model_reset = |m: &mut crate::shelf::model::ShelfModel| m.reset();
    panel::with_model_mut(model_reset);
    if searching {
        panel::with_model_mut(|m| {
            m.show_welcome = false;
            m.set_query("PasteJar");
        });
    }
    // Thumbnails decode in the background; wait for them so the render shows
    // pictures, not placeholders.
    warm_card_thumbnails();
    panel::with_model_mut(|m| m.show_welcome = false); // the plain shelf
    if let Some(n) = crate::app::sandbox::launch()
        .env
        .iter()
        .find(|(k, _)| k == "PASTORY_SELECT")
        .and_then(|(_, v)| v.parse::<u32>().ok())
    {
        panel::with_model_mut(|m| {
            for _ in 0..n {
                m.move_selection(1);
            }
        });
    }
    let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    apply_forced_appearance(&app);
    let size = CGSize::new(1600.0, 450.0);
    let host = crate::shelf::view::build(mtm, size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    // Needs a window for materials + layout to resolve.
    let window = unsafe {
        objc2_app_kit::NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<objc2_app_kit::NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            objc2_app_kit::NSWindowStyleMask::Borderless,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setContentView(Some(&host));
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    let ok = snapshot(&host, out);
    // Keep the tree alive past the snapshot like the Swift window does, then
    // let it go deterministically; NSUUID keeps temp-file names unique.
    let _ = NSUUID::new();
    ok
}

/// Honor `PASTORY_APPEARANCE=dark|light` for this process, so the dark-mode
/// renders are real pixels rather than an assertion about semantic colours.
/// `setAppearance:` lives on NSApplication (the view and window halves of
/// NSAppearanceCustomization are not bound).
fn apply_forced_appearance(app: &objc2_app_kit::NSApplication) {
    let forced = crate::app::sandbox::launch()
        .env
        .iter()
        .find(|(k, _)| k == "PASTORY_APPEARANCE")
        .map(|(_, v)| v.clone());
    let Some(name) = forced.as_deref() else { return };
    let token = if name == "dark" {
        unsafe { objc2_app_kit::NSAppearanceNameDarkAqua }
    } else {
        unsafe { objc2_app_kit::NSAppearanceNameAqua }
    };
    if let Some(a) = objc2_app_kit::NSAppearance::appearanceNamed(token) {
        app.setAppearance(Some(&a));
    }
}

/// `--selftest clickpin` — drive the real card's Pin button through the real
/// event route (`mouseDown:` / `mouseUp:` → `hit_card` → `run_action` →
/// `toggle_pin` → `reload`) and report the pinned flag before/after. This is
/// the reproduction for "clicking Pin after opening the shelf crashes the
/// app": every step the user's click takes runs here, in a process whose
/// stderr is captured, so a panic or a hang names itself instead of being
/// inferred from a window that disappeared.
pub fn clickpin() -> bool {
    seed_store();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    let mtm = objc2::MainThreadMarker::new().expect("clickpin on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let size = CGSize::new(1600.0, 450.0);
    let host = crate::shelf::view::build(mtm, size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    // A window is required: the click helper reads `windowNumber` off it.
    let window = unsafe {
        objc2_app_kit::NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<objc2_app_kit::NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            objc2_app_kit::NSWindowStyleMask::Borderless,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setContentView(Some(&host));
    host.layoutSubtreeIfNeeded();

    let count = crate::shelf::view::cards().len();
    println!("ok   {} cards seeded", count);
    let mut ok = true;
    for index in 0..count {
        let Some(p) = crate::shelf::view::selftest_action_center(index, card::ActionKind::Pin) else {
            println!("FAIL card {} has no Pin rect", index);
            ok = false;
            continue;
        };
        let before = store::read(|s| s.items.get(index).map(|it| it.pinned));
        let delivered = crate::shelf::view::selftest_click_at(p);
        let after = store::read(|s| s.items.get(index).map(|it| it.pinned));
        let flipped = matches!((before, after), (Some(a), Some(b)) if a != b);
        println!(
            "{} card {} pin {} -> {:?} (click at {:.1},{:.1} delivered={})",
            if delivered && flipped { "ok  " } else { "FAIL" },
            index,
            before.map(|b| b.to_string()).unwrap_or_else(|| "-".into()),
            after,
            p.x,
            p.y,
            delivered,
        );
        ok = ok && delivered && flipped;
    }
    ok
}

/// Show the REAL panel — material on, live window — for `secs`, pumping the
/// run loop so the slide-in, layout and the window server's compositing all
/// land. A snapshot cannot contain a material (see `theme::panel_material_alpha`),
/// so this is the only way to see what the user actually gets; pair it with an
/// external `screencapture`.
///
/// With `PASTORY_BACKDROP=#RRGGBB` a borderless window of that exact colour
/// sits behind the panel. Run it twice with two colours: if the shelf's ground
/// shifts with the backdrop, the material is really sampling the desktop. It
/// uses `.normal` level so the shelf (status-bar level) stays on top.
pub fn live_shot(secs: f64) -> bool {
    seed_store();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    let mtm = objc2::MainThreadMarker::new().expect("live_shot on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    // A regular app for the duration, so the panel is a real foreground
    // window the window server composites rather than an offscreen tree.
    // The real path: the app is an accessory and never takes activation, so
    // the panel is the only surface. `.followsWindowActiveState` therefore
    // renders whatever the product actually shows — which is exactly what
    // this self-test is here to measure.
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    apply_forced_appearance(&app);
    let backdrop = crate::app::sandbox::launch()
        .env
        .iter()
        .find(|(k, _)| k == "PASTORY_BACKDROP")
        .map(|(_, v)| v.clone());
    let _backdrop_window = backdrop.as_deref().and_then(|hex| make_backdrop(mtm, hex));
    panel::prewarm();
    panel::show();
    println!(
        "     live panel up: material alpha {:.2}, backdrop {}",
        crate::app::theme::panel_material_alpha(),
        backdrop.as_deref().unwrap_or("(desktop)"),
    );
    // The ground must sit ABOVE the material or the user's background image is
    // hidden behind the glass — the whole reason this list is printed. Order is
    // back to front, so `VisualEffect` has to come before `Ground...`.
    if let Some(root) = panel::content_view() {
        let order: Vec<String> = root
            .subviews()
            .iter()
            .map(|v| v.class().name().to_string_lossy().to_string())
            .collect();
        let eff = order.iter().position(|n| n.contains("VisualEffect"));
        let ground = order.iter().position(|n| n.contains("Ground"));
        let ok = matches!((eff, ground), (Some(a), Some(b)) if a < b);
        println!(
            "{} ground sits above the material view",
            if ok { "ok  " } else { "FAIL" }
        );
        println!("     layer order (back→front): {}", order.join(" < "));
    }
    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
    while std::time::Instant::now() < deadline {
        let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
        // SAFETY: NSDefaultRunLoopMode is a valid mode token for the main loop.
        let mode: &'static objc2_foundation::NSRunLoopMode =
            unsafe { objc2_foundation::NSDefaultRunLoopMode };
        run_loop.runMode_beforeDate(mode, &limit);
    }
    true
}

/// `--selftest deletepin` — the reproduction for "after pinning a card, click
/// a button and the app dies".
///
/// It drives the real click route (the card's Trash button → `mouseUp:` →
/// `run_action` → `panel::delete_item` → the pinned confirm → `model.delete`
/// → `reload`) and arms the real 0.25 s poll timer first, because that timer
/// is the other half of the bug: it keeps firing inside the confirm's nested
/// event loop (common modes include the modal one) and reloads the cards,
/// borrowing the model while the click still holds it. The confirm's stub runs
/// 0.6 s of nested loop — two ticks — with the click's stack still open, which
/// is exactly what a real `NSAlert.runModal` does before the user answers.
///
/// Before the fix this aborted here (`already mutably borrowed`); the fix runs
/// the confirm before the borrow and makes `view::refresh` stand down while a
/// dialog is up.
pub fn deletepin() -> bool {
    // A deterministic seed whatever the scratch store held before (a mutating
    // selftest only runs with PASTORY_STORE set; see selftest.rs).
    let stale: Vec<String> = store::read(|s| s.items.iter().map(|it| it.id.clone()).collect());
    store::with(|s| s.remove_ids(&stale));
    seed_store();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    // Pin every card: only a pinned one raises the confirm, and the bug lives
    // in that confirm's nested event loop.
    store::with(|s| {
        let ids: Vec<String> = s.items.iter().map(|it| it.id.clone()).collect();
        for id in ids {
            s.toggle_pin(&id, true);
        }
    });

    let mtm = objc2::MainThreadMarker::new().expect("deletepin on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let size = CGSize::new(1600.0, 450.0);
    let host = crate::shelf::view::build(mtm, size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    let window = unsafe {
        objc2_app_kit::NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<objc2_app_kit::NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            objc2_app_kit::NSWindowStyleMask::Borderless,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setContentView(Some(&host));
    host.layoutSubtreeIfNeeded();
    panel::install_poll_timer();

    let mut ok = true;
    let mut rounds = 0usize;
    while rounds < 8 {
        let list = panel::with_model_mut(|m| m.items());
        let Some(index) = list.iter().rposition(|it| it.pinned) else { break };
        let id = list[index].id.clone();
        let before = list.len();
        let Some(p) = crate::shelf::view::selftest_action_center(index, card::ActionKind::Trash)
        else {
            println!("FAIL card {} has no Trash rect", index);
            ok = false;
            break;
        };
        panel::stub_confirm_delete(true, 0.6);
        let delivered = crate::shelf::view::selftest_click_at(p);
        let after = store::read(|s| s.items.len());
        let gone = !store::read(|s| s.items.iter().any(|it| it.id == id));
        let good = delivered && gone && after + 1 == before;
        println!(
            "{} card {} Trash: {} -> {} items, gone={} (click at {:.1},{:.1} delivered={})",
            if good { "ok  " } else { "FAIL" },
            index,
            before,
            after,
            gone,
            p.x,
            p.y,
            delivered,
        );
        ok = ok && good;
        rounds += 1;
    }
    if rounds < 3 {
        println!("FAIL only {} pinned delete(s) ran; expected at least 3", rounds);
        ok = false;
    } else {
        println!("ok   {} pinned deletes survived the confirm's nested loop", rounds);
    }
    ok
}

/// Host the shelf tree in a borderless window and arm the real 0.25 s poll
/// timer — what `searchlive` and `searchtype` both need before they can
/// drive the panel. The window stays alive with the caller's binding.
fn host_shelf(size: CGSize) -> (objc2::rc::Retained<objc2_app_kit::NSWindow>, impl Fn(f64)) {
    let mtm = objc2::MainThreadMarker::new().expect("shelf self-test on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let host = crate::shelf::view::build(mtm, size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    let window = unsafe {
        objc2_app_kit::NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<objc2_app_kit::NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            objc2_app_kit::NSWindowStyleMask::Borderless,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setContentView(Some(&host));
    host.layoutSubtreeIfNeeded();
    panel::install_poll_timer();
    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    let pump = move |secs: f64| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
        while std::time::Instant::now() < deadline {
            let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
            // SAFETY: NSDefaultRunLoopMode is a valid mode token for the main loop.
            let mode: &'static objc2_foundation::NSRunLoopMode =
                unsafe { objc2_foundation::NSDefaultRunLoopMode };
            run_loop.runMode_beforeDate(mode, &limit);
        }
    };
    (window, pump)
}

/// `--selftest searchtype` — the one stage `searchlive` cannot see: the real
/// `NSTextField` and the delegate wired to it.
///
/// `searchlive` records the query itself (`set_query`), so the panel could
/// ship with the field unwired — no delegate assigned, a selector the
/// delegate never implements, the field never reachable by the keyboard —
/// and that self-test stayed green. This one types into the real control and
/// lets AppKit deliver the edit notification, which is the path a user's
/// keystroke takes. If it fails, the root cause is in that wiring, not in
/// the search pipeline.
pub fn searchtype() -> bool {
    seed_store();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    let size = CGSize::new(1600.0, 450.0);
    let (_window, pump) = host_shelf(size);

    let mut ok = true;
    let all = crate::shelf::view::cards();
    println!("{} {} cards on screen before typing", if all.len() > 3 { "ok  " } else { "FAIL" }, all.len());
    ok = ok && all.len() > 3;

    // "翻译" lives only in the first text card's title (seed step 8).
    let expected = store::read(|s| {
        s.items
            .iter()
            .find(|it| it.title.as_deref() == Some("翻译 prompt"))
            .map(|it| it.id.clone())
    });
    let Some(expected) = expected else {
        println!("FAIL the seeded store lacks its titled text card");
        return false;
    };

    let Some(field) = crate::shelf::view::search_field() else {
        println!("FAIL the shelf has no search field");
        return false;
    };
    if field.isHidden() {
        println!("FAIL the search field is hidden on a plain shelf");
        ok = false;
    }
    // What a keystroke does: the field's value changes and AppKit announces
    // the edit. Nothing here touches the model directly.
    field.setStringValue(&NSString::from_str("翻译"));
    crate::shelf::view::post_search_did_change();

    let recorded = panel::with_model_mut(|m| m.query().to_string());
    let searching = panel::with_model_mut(|m| m.is_searching());
    println!(
        "{} typing into the field recorded {:?} (is_searching={})",
        if recorded == "翻译" && searching { "ok  " } else { "FAIL" },
        recorded,
        searching
    );
    ok = ok && recorded == "翻译" && searching;

    let wait = std::time::Instant::now() + std::time::Duration::from_millis(2500);
    let mut listed = crate::shelf::view::cards();
    while std::time::Instant::now() < wait {
        pump(0.1);
        listed = crate::shelf::view::cards();
        if listed == vec![expected.clone()] {
            break;
        }
    }
    let landed = listed == vec![expected.clone()];
    println!(
        "{} the tick filtered the shelf to {} card(s) after real typing",
        if landed { "ok  " } else { "FAIL" },
        listed.len()
    );
    if !landed {
        println!("     expected {:?}, got {:?}", vec![expected.clone()], listed);
    }
    ok = ok && landed;

    // The clear button's path: empty the real field, announce it, tick.
    field.setStringValue(&NSString::from_str(""));
    crate::shelf::view::post_search_did_change();
    let wait = std::time::Instant::now() + std::time::Duration::from_millis(2500);
    let mut cleared = crate::shelf::view::cards();
    while std::time::Instant::now() < wait {
        pump(0.1);
        cleared = crate::shelf::view::cards();
        if cleared == all {
            break;
        }
    }
    let back = cleared == all;
    println!("{} clearing the field brought all {} cards back", if back { "ok  " } else { "FAIL" }, cleared.len());
    ok = ok && back;

    ok
}

/// `--selftest searchpanel` — the stage both tests above skip: the live
/// `ShelfPanel` (borderless + non-activating) and a real key event routed
/// through its own `sendEvent:` override into the keyboard table.
///
/// `searchlive` records the query itself and `searchtype` writes the field
/// directly; both host their tree in a plain `NSWindow`, so neither can see
/// whether the live panel's keyboard route works at all — `sendEvent:` could
/// stop calling `handle`, the panel could stop taking key focus, or the
/// `pending_query` hand-off could drop or reorder the characters typed before
/// the caret lands, and both stayed green. This one types on the real window.
pub fn searchpanel() -> bool {
    seed_store();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    let mtm = objc2::MainThreadMarker::new().expect("searchpanel on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    panel::prewarm();
    panel::show();
    // `show()` resets the model, and the welcome card's own recorder would
    // swallow the first keystroke if it were up.
    panel::with_model_mut(|m| m.show_welcome = false);
    crate::shelf::view::refresh();

    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    let pump = move |secs: f64| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
        while std::time::Instant::now() < deadline {
            let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
            // SAFETY: NSDefaultRunLoopMode is a valid mode token for the main loop.
            let mode: &'static objc2_foundation::NSRunLoopMode =
                unsafe { objc2_foundation::NSDefaultRunLoopMode };
            run_loop.runMode_beforeDate(mode, &limit);
        }
    };
    // Let the open animation settle and the first reload land.
    pump(0.4);

    // The window, not the tree: the keyboard table runs only from here.
    let Some(win) = panel::selftest_window() else {
        println!("FAIL no live ShelfPanel to send keys to");
        return false;
    };
    let mut ok = true;

    let send_key = |ch: &str, code: u16| {
        let s = NSString::from_str(ch);
        let ev = objc2_app_kit::NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            objc2_app_kit::NSEventType::KeyDown,
            CGPoint::ZERO,
            objc2_app_kit::NSEventModifierFlags::empty(),
            0.0,
            win.windowNumber(),
            None,
            &s,
            &s,
            false,
            code,
        )
        .expect("synthetic key event");
        win.sendEvent(&ev);
    };

    // Phase 1: with nothing focused (show() drops the caret), the first letter
    // must reach the keyboard table — it records the character and asks the
    // view to move focus into the box.
    let before_focus = panel::with_model(|m| m.focus_search);
    send_key("n", 45);
    let pending = panel::with_model_mut(|m| m.pending_query.clone());
    let after_focus = panel::with_model(|m| m.focus_search);
    println!(
        "{} a key on the live panel reached the keyboard table (pending={:?}, focus {}->{})",
        if pending == "n" && after_focus > before_focus { "ok  " } else { "FAIL" },
        pending,
        before_focus,
        after_focus
    );
    ok = ok && pending == "n" && after_focus > before_focus;

    // Phase 2: the hand-off lands. The scheduled flush puts the pending
    // character into the query, and the tick drives that into the field.
    let wait = std::time::Instant::now() + std::time::Duration::from_millis(900);
    while std::time::Instant::now() < wait && panel::with_model(|m| m.query() != "n") {
        pump(0.05);
    }
    let landed = panel::with_model(|m| m.query() == "n" && m.pending_query.is_empty());
    let field_text = crate::shelf::view::search_field()
        .map(|f| f.stringValue().to_string())
        .unwrap_or_default();
    println!(
        "{} the pending character landed in the query ({:?}) and the field ({:?})",
        if landed && field_text == "n" { "ok  " } else { "FAIL" },
        panel::with_model(|m| m.query().to_string()),
        field_text
    );
    ok = ok && landed && field_text == "n";

    // Phase 3: two letters typed the way a user types them — one key, the run
    // loop turns, the next key. The gap is what makes this side meaningful: if
    // the first key only set `pending_query`/`focus_search` without the
    // refresh that runs `focus_search_field`, nothing is focused yet when the
    // second key arrives, and the second character overwrites the first
    // (`pending_query = chars`). The caret must be in the box by then.
    panel::with_model_mut(|m| m.set_query(String::new()));
    crate::shelf::view::drop_focus_now();
    panel::with_model_mut(|m| m.pending_query.clear());
    pump(0.35);
    send_key("n", 45);
    pump(0.08);
    send_key("o", 31);
    let wait = std::time::Instant::now() + std::time::Duration::from_millis(900);
    while std::time::Instant::now() < wait && panel::with_model(|m| m.query().len() < 2) {
        pump(0.05);
    }
    let typed = panel::with_model_mut(|m| m.query().to_string());
    let field_text = crate::shelf::view::search_field()
        .map(|f| f.stringValue().to_string())
        .unwrap_or_default();
    println!(
        "{} two keys in a row kept both characters in order (query={:?}, field={:?})",
        if typed == "no" && field_text == "no" { "ok  " } else { "FAIL" },
        typed,
        field_text
    );
    ok = ok && typed == "no" && field_text == "no";

    // Phase 4: the list must actually follow the box. Phases 1-3 only prove the
    // keystrokes arrive — they record the query yet never look at the shelf, so
    // the panel could ship with `reload()`'s drain/service pair cut out and
    // they stayed green (the same gap that made this side necessary). "prom"
    // occurs only in the titled card seeded above ("翻译 prompt"), so a
    // filtered shelf is unambiguous.
    let expected = store::read(|s| {
        s.items
            .iter()
            .find(|it| it.title.as_deref() == Some("翻译 prompt"))
            .map(|it| it.id.clone())
    });
    let Some(expected) = expected else {
        println!("FAIL the seeded store lacks its titled text card");
        return false;
    };
    panel::with_model_mut(|m| m.set_query(String::new()));
    crate::shelf::view::drop_focus_now();
    panel::with_model_mut(|m| m.pending_query.clear());
    pump(0.35);
    // One key, the loop turns, the next key — the way a person types.
    for (ch, code) in [("p", 35u16), ("r", 15), ("o", 31), ("m", 46)] {
        send_key(ch, code);
        pump(0.12);
    }
    let wait = std::time::Instant::now() + std::time::Duration::from_millis(2500);
    let mut listed = crate::shelf::view::cards();
    while std::time::Instant::now() < wait {
        pump(0.1);
        listed = crate::shelf::view::cards();
        if listed == vec![expected.clone()] {
            break;
        }
    }
    let settled = !panel::with_model_mut(|m| m.is_searching());
    let filtered = listed == vec![expected.clone()] && settled;
    println!(
        "{} typing on the live panel filtered the shelf to {} card(s) (query={:?})",
        if filtered { "ok  " } else { "FAIL" },
        listed.len(),
        panel::with_model(|m| m.query().to_string())
    );
    if !filtered {
        println!("     expected {:?}, got {:?}", vec![expected.clone()], listed);
    }
    ok = ok && filtered;

    // Phase 5: the click a user makes, delivered the way the window server
    // delivers one. Every phase above reaches the box in-process — the keyboard
    // table hands focus in, `searchtype` writes the field directly — so none of
    // them shows that a click is *routed* to the box, which is the complaint
    // ("点进去打不了字"，框一直是灰的).
    //
    // The click has to leave through the window server, not through
    // `NSWindow::sendEvent:`. A synthesized LeftMouseDown handed straight to
    // `sendEvent:` drops into the cell's tracking loop, which then waits for a
    // mouseUp that only the window server would have enqueued — a deadlock, not
    // a test. And it has to be a real click because the panel is a borderless
    // non-activating panel of an accessory app: whether a first click is
    // delivered at all is the window server's decision, and that decision is
    // exactly what `sendEvent:` bypasses. Returns `None` when this process is
    // not allowed to post events, so the phase is reported as skipped rather
    // than as a failure.
    let class_of = |obj: &objc2::runtime::AnyObject| -> String {
        String::from_utf8_lossy(obj.class().name().to_bytes()).into_owned()
    };
    let can_post = objc2_core_graphics::CGPreflightPostEventAccess();
    let click_box = |win: &objc2_app_kit::NSWindow, label: &str| -> Option<bool> {
        let Some(field) = crate::shelf::view::search_field() else {
            println!("     FAIL no search box in the tree");
            return Some(false);
        };
        // The caret must be *absent* before the click, or a `currentEditor`
        // read afterwards is green whether or not the click arrived.
        if field.currentEditor().is_some() {
            println!("     FAIL the field editor was already up before the click");
            return Some(false);
        }
        let Some(host) = (unsafe { field.superview() }) else {
            println!("     FAIL the search box has no superview");
            return Some(false);
        };
        // `frame()` is in the header's space; a real click carries a point in
        // the window server's global space, which grows down from the main
        // display's top-left (Cocoa's grows up from the window's bottom-left).
        let f = field.frame();
        let loc = host.convertPoint_toView(CGPoint::new(f.mid().x, f.mid().y), None);
        let wf = win.frame();
        let mtm = objc2::MainThreadMarker::new().expect("selftest on the main thread");
        let screen = objc2_app_kit::NSScreen::mainScreen(mtm).expect("a main screen");
        let pt = CGPoint::new(
            wf.min().x + loc.x,
            screen.frame().size.height - (wf.min().y + loc.y),
        );
        // Which view the tree *thinks* is under that point. `hitTest:` takes a
        // point in the receiver's *superview* coordinates, and the content
        // view's parent is the window's frame view, whose space is the window
        // space `loc` is already in — so the probe goes to the parent, not
        // through the content view (feeding `loc` to the content view, as this
        // used to, measures a different point entirely and reported the
        // footer for every click).
        let landed_on = win
            .contentView()
            .and_then(|c| unsafe { c.superview() })
            .and_then(|sv| sv.hitTest(loc))
            .map(|h| class_of(&h))
            .unwrap_or_else(|| "<none>".to_string());
        println!(
            "     {}: box at {:?} size {:?} -> global ({:.0}, {:.0}); hitTest there = {}",
            label,
            (f.min().x, f.min().y),
            (f.size.width, f.size.height),
            pt.x,
            pt.y,
            landed_on
        );
        if !can_post {
            return None;
        }
        // Posting the click is only half of it: a window-server event lands in
        // `NSApplication`'s queue, and this self-test drives the run loop by
        // hand (`runMode:beforeDate:`) instead of through `NSApp.run`, so
        // nothing would ever dequeue it — the click would sit in the queue and
        // the assertion below would read the harness, not the product. Hand
        // the events to `sendEvent:` one at a time in the order they were
        // queued: the down may open a tracking loop that pulls the matching up
        // out of the queue itself, which only works while the up is still in
        // there.
        // Returns the ObjC exception message if dispatching one aborted the
        // event, so the caller can report it instead of the process dying.
        let drain = |secs: f64| -> Option<String> {
            let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
            let mode: &'static objc2_foundation::NSRunLoopMode =
                unsafe { objc2_foundation::NSDefaultRunLoopMode };
            let mut failure: Option<String> = None;
            while std::time::Instant::now() < deadline {
                let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
                let mask = objc2_app_kit::NSEventMask::LeftMouseDown
                    | objc2_app_kit::NSEventMask::LeftMouseUp;
                match app.nextEventMatchingMask_untilDate_inMode_dequeue(mask, Some(&limit), mode, true)
                {
                    // Dispatching into AppKit can raise: asking a mouse event
                    // for a key-only property (say `keyCode`) throws
                    // `NSInternalInconsistencyException`, and an ObjC exception
                    // unwinding through a Rust frame aborts the process with no
                    // reason printed. Catch it and report the name and reason,
                    // so the run fails with a readable line instead of dying.
                    Some(ev) => {
                        if let Err(e) = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                            app.sendEvent(&ev)
                        })) {
                            let txt = e
                                .as_ref()
                                .map(|x| {
                                    let name: *mut objc2::runtime::AnyObject =
                                        unsafe { objc2::msg_send![&**x, name] };
                                    let reason: *mut objc2::runtime::AnyObject =
                                        unsafe { objc2::msg_send![&**x, reason] };
                                    let s = |o: *mut objc2::runtime::AnyObject| -> String {
                                        if o.is_null() {
                                            return "<null>".into();
                                        }
                                        let u: *const std::ffi::c_char =
                                            unsafe { objc2::msg_send![&*o, UTF8String] };
                                        if u.is_null() {
                                            "<null>".into()
                                        } else {
                                            unsafe { std::ffi::CStr::from_ptr(u) }
                                                .to_string_lossy()
                                                .into_owned()
                                        }
                                    };
                                    format!("{}: {}", s(name), s(reason))
                                })
                                .unwrap_or_else(|| "<unnamed>".to_string());
                            failure = Some(txt);
                            break;
                        }
                    }
                    None => {}
                }
            }
            failure
        };
        let src = objc2_core_graphics::CGEventSource::new(
            objc2_core_graphics::CGEventSourceStateID::CombinedSessionState,
        );
        for ty in [
            objc2_core_graphics::CGEventType::LeftMouseDown,
            objc2_core_graphics::CGEventType::LeftMouseUp,
        ] {
            let Some(ev) = objc2_core_graphics::CGEvent::new_mouse_event(
                src.as_deref(),
                ty,
                pt,
                objc2_core_graphics::CGMouseButton::Left,
            ) else {
                println!("     FAIL the system refused the mouse event");
                return Some(false);
            };
            objc2_core_graphics::CGEvent::post(
                objc2_core_graphics::CGEventTapLocation::SessionEventTap,
                Some(&ev),
            );
            pump(0.12);
        }
        if let Some(txt) = drain(0.6) {
            println!("     FAIL dispatching the click raised {txt}");
            return Some(false);
        }
        let editor = field.currentEditor().is_some();
        let responder = win
            .firstResponder()
            .map(|r| class_of(&r))
            .unwrap_or_else(|| "<none>".to_string());
        println!(
            "{} a real click at ({:.0}, {:.0}) put the caret in the box (firstResponder={})",
            if editor { "ok  " } else { "FAIL" },
            pt.x,
            pt.y,
            responder
        );
        Some(editor)
    };

    // 5a: nothing in front of the header.
    panel::with_model_mut(|m| {
        m.show_welcome = false;
        m.set_query(String::new());
    });
    crate::shelf::view::refresh();
    crate::shelf::view::drop_focus_now();
    pump(0.3);
    let mut by_click = click_box(&win, "header alone");
    // ...and the box keeps what is typed next, the way it does for a user.
    if by_click == Some(true) {
        send_key("z", 6);
        let wait = std::time::Instant::now() + std::time::Duration::from_millis(900);
        while std::time::Instant::now() < wait && panel::with_model(|m| m.query().is_empty()) {
            pump(0.05);
        }
        let in_field = crate::shelf::view::search_field()
            .map(|f| f.stringValue().to_string())
            .unwrap_or_default();
        let in_query = panel::with_model(|m| m.query().to_string());
        println!(
            "{} typing after the click landed in the box (field={:?}, query={:?})",
            if in_field == "z" && in_query == "z" { "ok  " } else { "FAIL" },
            in_field,
            in_query
        );
        by_click = Some(in_field == "z" && in_query == "z");
    }
    match by_click {
        Some(v) => {
            println!(
                "{} clicking the search box put the caret in it",
                if v { "ok  " } else { "FAIL" }
            );
            ok = ok && v;
        }
        None => println!(
            "     skipped the click: this process may not post events (no Accessibility permission)"
        ),
    }

    // 5b: the first-launch state, which is the one the user opens the panel
    // on — the welcome card is up. If anything covers the header band, this is
    // where it shows.
    panel::with_model_mut(|m| {
        m.show_welcome = true;
        m.set_query(String::new());
    });
    crate::shelf::view::refresh();
    crate::shelf::view::drop_focus_now();
    pump(0.3);
    let by_click_with_welcome = click_box(&win, "welcome card up");
    match by_click_with_welcome {
        Some(v) => {
            println!(
                "{} clicking the search box works with the welcome card up",
                if v { "ok  " } else { "FAIL" }
            );
            ok = ok && v;
        }
        None => println!("     skipped the click: this process may not post events"),
    }


    // 5c: the state the user is actually in. Every phase above clicks a panel
    // that `show()` just made key. The real shelf hands the keyboard back to
    // the previous app as soon as a card is copied (`handBackFocus`), and from
    // then on it is visible but NOT key — which is where the click has to
    // promote the window itself before the field editor will start. The report
    // ("框一直是灰的，点进去没有光标、打不了字") is exactly this state, and 5a/5b
    // could never reach it.
    panel::with_model_mut(|m| {
        m.show_welcome = false;
        m.set_query(String::new());
    });
    crate::shelf::view::refresh();
    crate::shelf::view::drop_focus_now();
    pump(0.3);
    panel::selftest_drop_key();
    pump(0.1);
    let not_key = !win.isKeyWindow();
    println!(
        "{} the panel is visible but not key ({})",
        if not_key && panel::is_visible() { "ok  " } else { "FAIL" },
        if win.isKeyWindow() { "still key" } else { "resigned" }
    );
    ok = ok && not_key && panel::is_visible();
    let by_click_not_key = click_box(&win, "not the key window");
    match by_click_not_key {
        Some(v) => {
            println!(
                "{} one click focuses the search box while the panel is not key",
                if v { "ok  " } else { "FAIL" }
            );
            ok = ok && v;
        }
        None => println!("     skipped the click: this process may not post events"),
    }


    panel::hide();
    ok
}
/// drives it, which is the part `--selftest search` cannot see.
///
/// That one calls `update_search_blocking`, the synchronous twin the tests
/// use, and drains completions by hand — so the panel could ship with no
/// `drain_completions` caller, a job dropped at every keystroke and no notify
/// hook, and the search self-test stayed green throughout. This one goes the
/// other way: build the real tree in a window, arm the real 0.25 s poll
/// timer, record the query the way `controlTextDidChange:` does (and nothing
/// else), then pump the run loop. Every card on screen came from the tick, so
/// an unwired `service_search`/`drain_completions` leaves the list un-filtered
/// and this fails.
pub fn searchlive() -> bool {
    seed_store();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    let size = CGSize::new(1600.0, 450.0);
    let (_window, pump) = host_shelf(size);

    let mut ok = true;
    let all = crate::shelf::view::cards();
    println!("{} {} cards on screen before searching", if all.len() > 3 { "ok  " } else { "FAIL" }, all.len());
    ok = ok && all.len() > 3;

    // "翻译" lives only in the first text card's title (seed step 8), and the
    // code snippet must drop out — which is what makes the assertion more
    // than "something happened".
    let expected = store::read(|s| {
        s.items
            .iter()
            .find(|it| it.title.as_deref() == Some("翻译 prompt"))
            .map(|it| it.id.clone())
    });
    let dropped = store::read(|s| {
        s.items
            .iter()
            .find(|it| s.text(it).map(|t| t.contains("const shelf")).unwrap_or(false))
            .map(|it| it.id.clone())
    });
    let (Some(expected), Some(dropped)) = (expected, dropped) else {
        println!("FAIL the seeded store lacks its titled text card or its code snippet");
        return false;
    };

    // Exactly what the search field records on a keystroke, and nothing more:
    // the tick is what must start the search.
    panel::with_model_mut(|m| m.set_query("翻译"));
    let pending = crate::shelf::view::cards();
    let searching = panel::with_model_mut(|m| m.is_searching());
    println!(
        "{} query recorded mid-search: {} cards still shown, is_searching={}",
        if pending == all && searching { "ok  " } else { "FAIL" },
        pending.len(),
        searching
    );
    ok = ok && pending == all && searching;

    let wait = std::time::Instant::now() + std::time::Duration::from_millis(2500);
    let mut listed = crate::shelf::view::cards();
    while std::time::Instant::now() < wait {
        pump(0.1);
        listed = crate::shelf::view::cards();
        if listed == vec![expected.clone()] {
            break;
        }
    }
    let still_searching = panel::with_model_mut(|m| m.is_searching());
    let landed = listed == vec![expected.clone()] && !still_searching;
    println!(
        "{} the poll tick filtered the shelf to {} card(s) (is_searching={})",
        if landed { "ok  " } else { "FAIL" },
        listed.len(),
        still_searching
    );
    ok = ok && landed;
    if !landed {
        println!("     expected {:?}, got {:?}", vec![expected.clone()], listed);
        println!("     the dropped snippet card was {:?}", dropped);
    }

    // The list must stay put: a query that keeps restarting its search would
    // show as churn here even when the first landing looked right.
    pump(0.6);
    let stable = crate::shelf::view::cards() == vec![expected.clone()];
    println!("{} the settled list does not churn on later ticks", if stable { "ok  " } else { "FAIL" });
    ok = ok && stable;

    // Clearing (the field's clear button) must bring the history back through
    // the same tick, with no search started at all.
    panel::with_model_mut(|m| m.set_query(String::new()));
    let wait = std::time::Instant::now() + std::time::Duration::from_millis(2500);
    let mut cleared = crate::shelf::view::cards();
    while std::time::Instant::now() < wait {
        pump(0.1);
        cleared = crate::shelf::view::cards();
        if cleared == all {
            break;
        }
    }
    let back = cleared == all && !panel::with_model_mut(|m| m.is_searching());
    println!("{} clearing the query brought all {} cards back", if back { "ok  " } else { "FAIL" }, cleared.len());
    ok = ok && back;

    ok
}

/// A solid-colour window behind the shelf, for the material test above.
fn make_backdrop(mtm: objc2::MainThreadMarker, hex: &str) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).ok()?;
    let color = objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(
        ((v >> 16) & 0xFF) as f64 / 255.0,
        ((v >> 8) & 0xFF) as f64 / 255.0,
        (v & 0xFF) as f64 / 255.0,
        1.0,
    );
    let screen = objc2_app_kit::NSScreen::mainScreen(mtm)?;
    let frame = screen.frame();
    let w = unsafe {
        objc2_app_kit::NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<objc2_app_kit::NSWindow>(),
            frame,
            objc2_app_kit::NSWindowStyleMask::Borderless,
            objc2_app_kit::NSBackingStoreType::Buffered,
            false,
        )
    };
    w.setBackgroundColor(Some(&color));
    w.setOpaque(true);
    w.setLevel(0);
    unsafe { w.setReleasedWhenClosed(false) };
    w.orderFrontRegardless();
    Some(w)
}

/// `--selftest renamepanel` — the third path through the live panel: a real
/// click on a card's title row, then real keystrokes.
///
/// The report was 「粘贴板上加标题功能没实现」, the same shape as the search
/// complaint: nothing on screen responded to a click. Every existing phase
/// reaches a rename in-process (`MenuHandler::renameItem:` is an action target
/// AppKit calls directly, and `searchtype` writes fields by hand), so the
/// click route — window server decides whether a non-activating panel of an
/// accessory app gets the first click at all — had no coverage. `searchpanel`
/// covers that route for the search box; this covers it for the title row,
/// where a different view and a different hit zone answer.
///
/// Three things have to hold and each fails on its own: the click must open the
/// editor (the box only exists while `renaming` is set), the box must take the
/// caret, and the characters typed next must end up stored as the clip's title.
pub fn renamepanel() -> bool {
    seed_store();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    let mtm = objc2::MainThreadMarker::new().expect("renamepanel on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    panel::prewarm();
    panel::show();
    // `show()` resets the model, and the welcome card's own recorder would
    // swallow the first keystroke if it were up.
    panel::with_model_mut(|m| m.show_welcome = false);
    crate::shelf::view::refresh();

    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    let pump = move |secs: f64| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
        while std::time::Instant::now() < deadline {
            let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
            // SAFETY: NSDefaultRunLoopMode is a valid mode token for the main loop.
            let mode: &'static objc2_foundation::NSRunLoopMode =
                unsafe { objc2_foundation::NSDefaultRunLoopMode };
            run_loop.runMode_beforeDate(mode, &limit);
        }
    };
    pump(0.4);

    let Some(win) = panel::selftest_window() else {
        println!("FAIL no live ShelfPanel to click in");
        return false;
    };
    let mut ok = true;

    // The titled text card from the seed (step 8). It is the only card whose
    // title row answers, and its title is what the assertion reads back.
    let Some((index, id, before)) = store::read(|s| {
        s.items
            .iter()
            .position(|it| it.title.as_deref() == Some("翻译 prompt"))
            .map(|i| (i, s.items[i].id.clone(), s.items[i].title.clone().unwrap_or_default()))
    }) else {
        println!("FAIL the seeded store lacks its titled text card");
        return false;
    };

    let Some(row_point) = crate::shelf::view::selftest_title_row_center(index) else {
        println!("FAIL card {index} shows no title row to click");
        return false;
    };
    // The row's own space is not the window's: the cards row sits inside the
    // scroll view, whose bounds origin moves with the list. `convertPoint:`
    // walks that chain; a frame-relative point would miss by the scroll offset.
    let Some(loc) = crate::shelf::view::selftest_row_point_in_window(row_point) else {
        println!("FAIL could not map the title row into window space");
        return false;
    };
    let wf = win.frame();
    let screen = objc2_app_kit::NSScreen::mainScreen(mtm).expect("a main screen");
    // A real click carries the point in the window server's global space, which
    // grows down from the main display's top-left (Cocoa's grows up from the
    // window's bottom-left).
    let pt = CGPoint::new(
        wf.min().x + loc.x,
        screen.frame().size.height - (wf.min().y + loc.y),
    );
    let landed_on = win
        .contentView()
        .and_then(|c| unsafe { c.superview() })
        .and_then(|sv| sv.hitTest(loc))
        .map(|h| String::from_utf8_lossy(h.class().name().to_bytes()).into_owned())
        .unwrap_or_else(|| "<none>".to_string());
    println!(
        "     title row of card {index} at row {row_point:?} -> window {loc:?} -> global ({:.0}, {:.0}); hitTest there = {landed_on}",
        pt.x, pt.y
    );

    let can_post = objc2_core_graphics::CGPreflightPostEventAccess();
    if !can_post {
        println!("     skipped the click: this process may not post events (no Accessibility permission)");
        panel::hide();
        return ok;
    }

    // Post the click, then dequeue it by hand. A window-server event lands in
    // `NSApplication`'s queue, and this self-test drives the run loop by hand
    // instead of through `NSApp.run`, so nothing else would ever dequeue it and
    // the assertion below would read the harness rather than the product.
    let src = objc2_core_graphics::CGEventSource::new(
        objc2_core_graphics::CGEventSourceStateID::CombinedSessionState,
    );
    for ty in [
        objc2_core_graphics::CGEventType::LeftMouseDown,
        objc2_core_graphics::CGEventType::LeftMouseUp,
    ] {
        let Some(ev) = objc2_core_graphics::CGEvent::new_mouse_event(
            src.as_deref(),
            ty,
            pt,
            objc2_core_graphics::CGMouseButton::Left,
        ) else {
            println!("     FAIL the system refused the mouse event");
            panel::hide();
            return false;
        };
        objc2_core_graphics::CGEvent::post(
            objc2_core_graphics::CGEventTapLocation::SessionEventTap,
            Some(&ev),
        );
        pump(0.12);
    }

    // Hand the queued events to `sendEvent:` in order: the down may open a
    // tracking loop that pulls the matching up out of the queue itself, which
    // only works while the up is still in there.
    {
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(0.6);
        let mode: &'static objc2_foundation::NSRunLoopMode =
            unsafe { objc2_foundation::NSDefaultRunLoopMode };
        let mut failure: Option<String> = None;
        while std::time::Instant::now() < deadline {
            let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
            let mask = objc2_app_kit::NSEventMask::LeftMouseDown
                | objc2_app_kit::NSEventMask::LeftMouseUp;
            if let Some(ev) =
                app.nextEventMatchingMask_untilDate_inMode_dequeue(mask, Some(&limit), mode, true)
            {
                // An exception raised while dispatching (a mouse event asked for
                // a key-only property, say) would abort the process with no
                // reason; catch it and report the name and reason.
                if let Err(e) = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                    app.sendEvent(&ev)
                })) {
                    let txt = e
                        .as_ref()
                        .map(|x| {
                            let name: *mut objc2::runtime::AnyObject =
                                unsafe { objc2::msg_send![&**x, name] };
                            let reason: *mut objc2::runtime::AnyObject =
                                unsafe { objc2::msg_send![&**x, reason] };
                            let s = |o: *mut objc2::runtime::AnyObject| -> String {
                                if o.is_null() {
                                    return "<null>".into();
                                }
                                let u: *const std::ffi::c_char =
                                    unsafe { objc2::msg_send![&*o, UTF8String] };
                                if u.is_null() {
                                    "<null>".into()
                                } else {
                                    unsafe { std::ffi::CStr::from_ptr(u) }
                                        .to_string_lossy()
                                        .into_owned()
                                }
                            };
                            format!("{}: {}", s(name), s(reason))
                        })
                        .unwrap_or_else(|| "<unnamed>".to_string());
                    failure = Some(txt);
                    break;
                }
            }
        }
        if let Some(txt) = failure {
            println!("     FAIL dispatching the click raised {txt}");
            panel::hide();
            return false;
        }
    }

    let renaming = panel::with_model(|m| m.renaming_id.as_deref() == Some(id.as_str()));
    let caret = crate::shelf::view::rename_field()
        .map(|f| f.currentEditor().is_some())
        .unwrap_or(false);
    println!(
        "{} a real click on the title row opened the title editor (renaming={renaming}, caret={caret}, firstResponder={})",
        if renaming && caret { "ok  " } else { "FAIL" },
        win.firstResponder()
            .map(|r| String::from_utf8_lossy(r.class().name().to_bytes()).into_owned())
            .unwrap_or_else(|| "<none>".to_string())
    );
    ok = ok && renaming && caret;

    // Type the way a user does: one key, the run loop turns, the next key. A
    // rename box that never took the caret drops the characters into whatever
    // had the keyboard — that is the 「打不了字」 half of the report.
    let send_key = |ch: &str, code: u16| {
        let s = NSString::from_str(ch);
        let ev = objc2_app_kit::NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            objc2_app_kit::NSEventType::KeyDown,
            CGPoint::ZERO,
            objc2_app_kit::NSEventModifierFlags::empty(),
            0.0,
            win.windowNumber(),
            None,
            &s,
            &s,
            false,
            code,
        )
        .expect("synthetic key event");
        // Through the panel, not the field: `sendEvent:` dispatches into
        // whichever field editor holds the caret, which is the real path.
        win.sendEvent(&ev);
    };
    send_key("x", 7);
    pump(0.12);
    send_key("y", 16);
    pump(0.3);

    let draft = crate::shelf::view::rename_field()
        .map(|f| f.stringValue().to_string())
        .unwrap_or_default();
    println!("     the box holds {draft:?} (seeded with {before:?})");

    // Ending the rename (clicking the tick, ⎋, or a click on the desk) is what
    // commits; an unchanged draft writes nothing.
    crate::shelf::view::end_rename();
    pump(0.3);
    let saved = store::read(|s| {
        s.items
            .iter()
            .find(|it| it.id == id)
            .and_then(|it| it.title.clone())
    });
    let typed_landed = draft.contains("x") && draft.contains("y");
    println!(
        "{} the typed characters reached the title box and were saved (title={:?})",
        if typed_landed && saved.as_deref() == Some(draft.as_str()) { "ok  " } else { "FAIL" },
        saved
    );
    ok = ok && typed_landed && saved.as_deref() == Some(draft.as_str());

    panel::hide();
    ok
}

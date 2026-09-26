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
            "https://github.com/nothingbutcici/pastory",
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
                s.insert_image(&png, &Source::pastory(), Some("Pastory 是一个截图工具".into()));
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
        "Pastory 录屏 00:08",
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
            "Pastory 是一个截图工具",
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

/// `SelfTest.renderShelf(out:, searching:)`.
pub fn render_shelf(out: &str, searching: bool) -> bool {
    seed_store();
    let model_reset = |m: &mut crate::shelf::model::ShelfModel| m.reset();
    panel::with_model_mut(model_reset);
    if searching {
        panel::with_model_mut(|m| {
            m.show_welcome = false;
            m.set_query("Pastory");
        });
    }
    // Thumbnails decode in the background; give them a moment so the render
    // shows pictures, not placeholders.
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

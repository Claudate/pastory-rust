//! Port of the M6 render/command selftests (`App/SelfTest.swift`):
//! `settings` · `contact` · `welcome` · `note` · `editors` · `updatewin` ·
//! `download` · `updater` · `import` · `openpanel`.
//!
//! Render commands build the same views the live path uses, hosted in a
//! borderless window at the Swift frames, and snap to PNG through the
//! shared `selftest_m2::snapshot`.


use objc2::MainThreadMarker;
use objc2_app_kit::{NSBackingStoreType, NSOpenPanel, NSWindow, NSWindowStyleMask};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSData, NSDate, NSRunLoop, NSString, NSURL};

use crate::app::{sandbox, selftest_m2};
use crate::clipboard::item::ClipKind;
use crate::clipboard::store::{self};
use crate::shelf::panel;
use crate::shelf::settings_pane::Choice;

/// The shelf host pipeline every pane render shares (SelfTest's settings /
/// contact / welcome blocks all build the same tree and snap).
fn render_shelf_host(lean_reset: impl FnOnce(&'_ mut crate::shelf::model::ShelfModel), height: f64, out: &str) -> bool {
    selftest_m2::seed_store_path();
    panel::with_model_mut(lean_reset);
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    // `PASTORY_WIDTH` mirrors the Swift render command's host width override.
    let width = sandbox::launch()
        .env
        .iter()
        .find(|(k, _)| k == "PASTORY_WIDTH")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(1600.0);
    let size = CGSize::new(width, height);
    let host = crate::shelf::view::build(mtm, size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setContentView(Some(&host));
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    selftest_m2::snapshot(&host, out)
}

/// `PASTORY_HEIGHT` = the host window's height, keeping step with the Swift render commands.
fn env_height(default: f64) -> f64 {
    sandbox::launch()
        .env
        .iter()
        .find(|(k, _)| k == "PASTORY_HEIGHT")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(default)
}

pub fn settings(out: &str) -> bool {
    render_shelf_host(|m| {
        m.reset();
        m.show_settings = true;
    }, env_height(450.0), out)
}

/// `--selftest skin <dir>` — the 设置 pane once per skin, so a skin can be
/// eyeballed (and pixel-diffed) without clicking through the picker. The host
/// is taller than the real panel so the whole 外观 sheet is in one frame.
pub fn skins(dir: &str) -> bool {
    let dir = std::path::PathBuf::from(dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        println!("skin: cannot create {}: {e}", dir.display());
        return false;
    }
    let mut ok = true;
    for skin in crate::app::theme_skin::all() {
        crate::app::theme_skin::select(skin.id);
        let out = dir.join(format!("skin-{}.png", skin.id));
        println!("skin: {} ({})", skin.id, skin.name);
        ok &= render_shelf_host(
            |m| {
                m.reset();
                m.show_settings = true;
            },
            env_height(700.0),
            &out.to_string_lossy(),
        );
    }
    ok
}

/// `--selftest skinlive` — a skin switch on the live panel, which is the one
/// thing the offscreen `skin` renders cannot show.
///
/// The picker's own route is `theme_skin::select` → `theme_skin::store` →
/// `panel::retheme`, and it runs with the panel already on screen, so two
/// things have to hold afterwards that no still render can prove: the switch
/// is visible immediately (the panel's own draw, not a cache), and the panel
/// has not moved. The frame comes from `panel::frame_on_screen()` — the
/// product's own visible-window accessor, so this is not a second copy of the
/// geometry that could agree with itself — and the colour is the panel's own
/// content view drawn through `cacheDisplayInRect`, i.e. pixels.
///
/// Every skin must differ from the one before it: a picker that selects but
/// never repaints would otherwise pass on the frame check alone.
pub fn skinlive() -> bool {
    selftest_m2::seed_store_path();
    // The opening state: no settings sheet, no welcome card standing in front
    // of the shelf, so the sample below is the ground the skins actually move.
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    panel::prewarm();
    panel::show();
    // `show` slides the content up over 0.22 s; a snapshot inside that window
    // would compare the animation rather than the skin.
    spin(0.35);
    let Some(before) = panel::frame_on_screen() else {
        println!("skinlive: the panel never came up");
        return false;
    };
    println!(
        "     live panel up at {:.0},{:.0} {:.0}×{:.0}",
        before.origin.x, before.origin.y, before.size.width, before.size.height
    );

    let mut ok = true;
    // The average colour of the panel as drawn, carried across the loop so
    // each skin is compared with its predecessor.
    let mut previous: Option<(&str, (f64, f64, f64))> = None;
    for skin in crate::app::theme_skin::all() {
        crate::app::theme_skin::select(skin.id);
        crate::app::theme_skin::store(skin.id);
        panel::retheme();
        spin(0.12);

        let took = crate::app::theme_skin::current_id() == skin.id;
        let moved = panel::frame_on_screen();
        let same_frame = moved
            .map(|f| {
                (f.origin.x - before.origin.x).abs() < 0.5
                    && (f.origin.y - before.origin.y).abs() < 0.5
                    && (f.size.width - before.size.width).abs() < 0.5
                    && (f.size.height - before.size.height).abs() < 0.5
            })
            .unwrap_or(false);
        let color = panel::content_view().and_then(|v| mean_color(&v));

        let (repainted, delta) = match (color, previous) {
            (Some(c), Some((_, p))) => (rgb_distance(c, p) >= 2.0, Some(rgb_distance(c, p))),
            // The first skin is whatever the process started in; the one
            // before it is the one the picker was showing, which is not
            // necessarily the same.
            (Some(_), None) => (true, None),
            (None, _) => (false, None),
        };
        let good = took && same_frame && repainted;
        ok &= good;
        println!(
            "{} skinlive: {:<9} {} · frame {} · ground {}{}",
            if good { "ok  " } else { "FAIL" },
            skin.id,
            if took { "selected" } else { "NOT SELECTED" },
            if same_frame { "unmoved" } else { "MOVED — the panel jumped" },
            match color {
                Some(c) => format!("rgb({:.0}, {:.0}, {:.0})", c.0, c.1, c.2),
                None => "not drawn".to_string(),
            },
            match delta {
                Some(d) if d < 2.0 => format!(" — unchanged from {} (Δ{d:.1})", previous.map(|p| p.0).unwrap_or("-")),
                Some(d) => format!(" — Δ{d:.1} from {}", previous.map(|p| p.0).unwrap_or("-")),
                None => String::new(),
            }
        );
        previous = color.map(|c| (skin.id, c));
    }
    panel::hide();
    ok
}

/// The average colour of what a view draws, on a coarse grid. Averaging (not
/// one corner pixel) keeps this about the ground as a whole: cards and text
/// shift by a few pixels between skins without that meaning the ground did
/// not change. `None` when the rep or the view is too small to sample.
fn mean_color(view: &objc2_app_kit::NSView) -> Option<(f64, f64, f64)> {
    let bounds = view.bounds();
    if bounds.size.width < 3.0 || bounds.size.height < 3.0 {
        return None;
    }
    let rep = view.bitmapImageRepForCachingDisplayInRect(bounds)?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    let w = rep.pixelsWide() as isize;
    let h = rep.pixelsHigh() as isize;
    let stride = rep.bytesPerRow() as isize;
    let spp = rep.samplesPerPixel() as isize;
    if w < 3 || h < 3 || spp < 3 {
        return None;
    }
    let base = rep.bitmapData();
    const GRID: isize = 12;
    let mut sum = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..GRID {
        for j in 0..GRID {
            let x = (w * (2 * i + 1)) / (2 * GRID);
            let y = (h * (2 * j + 1)) / (2 * GRID);
            let off = y * stride + x * spp;
            // SAFETY: both indices are inside the rep's own bitmap, and the
            // step is the rep's samples-per-pixel, so the three bytes read are
            // one pixel's components.
            let rgb = unsafe { std::slice::from_raw_parts(base.offset(off), 3) };
            sum.0 += rgb[0] as f64;
            sum.1 += rgb[1] as f64;
            sum.2 += rgb[2] as f64;
        }
    }
    let n = (GRID * GRID) as f64;
    Some((sum.0 / n, sum.1 / n, sum.2 / n))
}

/// How far apart two colours are, per channel in 0-255: a threshold of 2
/// ignores the card shuffle a rebuild causes and still catches a ground that
/// only tinted.
fn rgb_distance(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (dr, dg, db) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    ((dr * dr + dg * dg + db * db) / 3.0).sqrt()
}

/// `--selftest viewer <out.png>` — a real image card's picture opened in the
/// 查看图片 window. Goes through the same click route the live card uses
/// (`hit_card` → `HitZone::Thumb` → `image_viewer::open`), so what the render
/// shows is what a click produces, not a view built for the test; the render
/// itself is the viewer window's own content view, not the shelf host.
pub fn viewer(out: &str) -> bool {
    selftest_m2::seed_store_path();
    // The thumbnails decode in the background; without waiting for them the
    // cards carry no picture and the click below has nothing to land on.
    selftest_m2::warm_card_thumbnails();
    let size = CGSize::new(1600.0, 450.0);
    let host = crate::shelf::view::build(MainThreadMarker::new().expect("main thread"), size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            MainThreadMarker::new().expect("main thread").alloc::<NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setContentView(Some(&host));
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    // Wait for the image card's thumbnail, then click it exactly where the
    // picture is drawn (the row's own coordinates).
    let mut clicked = false;
    for _ in 0..120 {
        clicked = crate::shelf::view::debug_click_first_thumbnail();
        if clicked {
            break;
        }
        spin(0.05);
    }
    if !clicked {
        println!("viewer: no image card with a decoded thumbnail");
        return false;
    }
    // The picture decodes off the main thread; give the hop its turn.
    for _ in 0..40 {
        if crate::shelf::image_viewer::is_ready() {
            break;
        }
        spin(0.05);
    }
    let state = crate::shelf::image_viewer::debug_state();
    // Render the viewer's own window content (the shelf host only exists to
    // drive the click route; the picture now lives in a separate window).
    let ok = match crate::shelf::image_viewer::debug_content_view() {
        Some(v) => selftest_m2::snapshot(&v, out),
        None => {
            println!("viewer: the 查看图片 window has no content view");
            false
        }
    };
    // What has to be true for this render to mean anything: a picture really
    // is decoded and drawn, and the band it covers is a picture rather than
    // the loading placeholder. The colour count comes through the view's own
    // draw pass, so it is pixels, not intent — and it is what makes this
    // checkable without looking at the PNG.
    let painted = match state {
        Some((w, h, photo, corner, inside)) => {
            println!(
                "viewer: {w}×{h} decoded, {}, scrim rgb{corner:?}, {inside}/81 samples inside the picture differ from it",
                if photo { "filling the panel" } else { "at its own size" }
            );
            // A drawn picture covers its frame; the scrim only shows through
            // where there is no picture. Nearly all of the inside grid coming
            // back as the scrim's colour means the frame was never painted.
            inside >= 45
        }
        None => {
            println!("viewer: nothing on screen after the click");
            false
        }
    };
    crate::shelf::image_viewer::close();
    ok && painted
}

/// `--selftest previewvideo [out.png]` — a video card's 「查看」, the click the
/// app used to abort on.
///
/// The sequence is the live one, not a shortcut: build the shelf tree, run the
/// video card's `Preview` action, and only then ask whether Quick Look came up.
/// The old code called `preview_selected()` from inside a model borrow; that
/// entry point is gone, so this test fails loudly (with a render to look at)
/// instead of taking the process down with it.
///
/// Optional `out.png` writes a render of the shelf with the preview open.
pub fn preview_video(out: Option<&str>) -> bool {
    selftest_m2::seed_store_path();
    // The video's poster decodes in the background; the card needs it before
    // there is anything to preview.
    selftest_m2::warm_card_thumbnails();
    let mtm = MainThreadMarker::new().expect("main thread");
    let size = CGSize::new(1600.0, 450.0);
    let host = crate::shelf::view::build(mtm, size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setContentView(Some(&host));
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    // `build` already walked the model into the card list, so the action has
    // cards to find. Reaching the line after it is half the assertion: the
    // path this replaces aborted right here.
    if !crate::shelf::view::debug_preview_first_video() {
        println!("previewvideo: no video card in the store");
        return false;
    }
    // Quick Look orders its panel in on the same turn; give it a beat.
    let mut visible = false;
    for _ in 0..40 {
        visible = panel::debug_quick_look_visible();
        if visible {
            break;
        }
        spin(0.05);
    }
    let rendered = match out {
        Some(path) => selftest_m2::snapshot(&host, path),
        None => true,
    };
    if visible {
        println!("previewvideo: video card preview action ran, Quick Look panel is up");
    } else {
        println!("previewvideo: action ran but no Quick Look panel appeared");
    }
    // Leave no preview behind for the next command.
    crate::shelf::panel::toggle_quick_look();
    rendered && visible
}

/// `--selftest background <dir>` — the panel background's memory: the half of
/// the feature no render can show. The `skin` renders prove the picture gets
/// painted; what has to hold here is that the file the picker remembers is
/// still there on the next launch.
///
/// `set_panel_background` writes, the panel's own `panel_background_path()`
/// reads back, the bookmark alone still resolves once the plain path is gone —
/// that is the route a sandboxed relaunch takes — and `clear_panel_background`
/// removes both. The reader is the product's own accessor, so this is not a
/// second copy of the keys agreeing with itself.
pub fn background(dir: &str) -> bool {
    use crate::app::background;
    use crate::app::preferences::{ns_string_id, Preferences};

    // Both overrides short-circuit the stored value, so with either set this
    // would read the override back and prove nothing. Refuse, rather than pass
    // for the wrong reason.
    if sandbox::self_test_var("PASTORY_BACKGROUND").is_some()
        || sandbox::self_test_var("PASTORY_BACKGROUND_DIM").is_some()
    {
        println!(
            "background: PASTORY_BACKGROUND / _DIM is set — this test reads the stored value, not the override"
        );
        return false;
    }

    // The test picture is a copy of the shipped logo inside the scratch
    // folder, so nothing depends on where the process was started from and the
    // user's own files are never touched.
    let src = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../Resources/Logo.png"));
    let dir = std::path::PathBuf::from(dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        println!("background: cannot create {}: {e}", dir.display());
        return false;
    }
    let file = dir.join("pastory-background-test.png");
    if let Err(e) = std::fs::copy(src, &file) {
        println!("background: no test image at {}: {e}", src.display());
        return false;
    }
    let path = file.to_string_lossy().to_string();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path));
    let prefs = Preferences::shared();
    let mut ok = true;

    // 1. A clean start. This is also the first touch of the preferences, which
    //    is where the self-test suite domain gets wiped, so it must come before
    //    anything is written.
    prefs.clear_panel_background();
    ok &= bg_check(
        "a cleared background is not remembered",
        prefs.panel_background_path().is_none() && !background::has_image(),
    );
    ok &= render_shelf_host(
        |m| {
            m.reset();
            m.show_settings = true;
        },
        env_height(700.0),
        &dir.join("background-none.png").to_string_lossy(),
    );

    // 2. The pick: what went in has to be what comes back out.
    prefs.set_panel_background(&url);
    let read_back = prefs.panel_background_path().unwrap_or_default();
    ok &= bg_check(
        "the picked file is remembered",
        same_file(&read_back, &path),
    );
    ok &= bg_check("the remembered picture decodes", background::has_image());
    ok &= bg_check(
        "the settings row names the file",
        background::display_name() == "pastory-background-test.png",
    );
    ok &= render_shelf_host(
        |m| {
            m.reset();
            m.show_settings = true;
        },
        env_height(700.0),
        &dir.join("background-set.png").to_string_lossy(),
    );

    // 3. The opacity: the value the panel draws with, and the chip the row lights.
    //    The strip is 淡/中/浓/全亮 (3/6/8/10 tenths); 0.9 sits between 浓 and 全亮
    //    and lands on the nearer chip, 浓. 1.0 is the fresh-install default.
    prefs.set_panel_background_dim(0.9);
    ok &= bg_check(
        "an opacity of 0.9 reads back at the 浓 step",
        (background::dim() - 0.9).abs() < 1e-9 && background::dim_step() == 8,
    );
    prefs.set_panel_background_dim(1.0);
    ok &= bg_check(
        "an opacity of 1.0 reads back at the 全亮 step",
        (background::dim() - 1.0).abs() < 1e-9 && background::dim_step() == 10,
    );
    // The mutual exclusion: a fresh pick turns the photo on, and the paint
    // path agrees (the render above already snapped with the photo in force).
    ok &= bg_check(
        "a fresh pick turns the photo on",
        background::enabled() && background::active(),
    );

    // 4. The sandboxed relaunch: with the plain path gone the bookmark is all
    //    that is left, and it still has to resolve to the same file.
    prefs
        .defaults()
        .removeObjectForKey(ns_string_id(background::PATH_KEY));
    let via_bookmark = prefs.panel_background_path().unwrap_or_default();
    ok &= bg_check(
        "the bookmark alone still resolves the file",
        same_file(&via_bookmark, &path),
    );

    // 5. The clear: both keys gone, the switch falls off with the file (the
    //    mutual exclusion's other half), and the panel falls back to the
    //    skin's own ground rather than holding a picture nothing points at.
    prefs.clear_panel_background();
    ok &= bg_check(
        "clearing forgets the file and its picture",
        prefs.panel_background_path().is_none() && !background::has_image(),
    );
    ok &= bg_check(
        "clearing also pauses the photo",
        !background::enabled() && !background::active(),
    );
    ok &= bg_check(
        "the settings row goes back to empty",
        background::display_name().is_empty(),
    );

    // Leave the veil where a fresh install would have it.
    prefs.set_panel_background_dim(background::DEFAULT_DIM);
    ok
}

/// One line of `--selftest background`'s output, in the shape the other
/// selftests print.
fn bg_check(what: &str, ok: bool) -> bool {
    println!("{} background: {what}", if ok { "ok  " } else { "FAIL" });
    ok
}

/// Whether two paths name the same file. `/tmp` is a symlink to `/private/tmp`
/// on macOS, so a stored path and a canonicalized one differ as strings while
/// pointing at exactly the same thing.
fn same_file(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Let the main queue run for `seconds` (the viewer decodes off-thread).
fn spin(seconds: f64) {
    let limit = NSDate::dateWithTimeIntervalSinceNow(seconds);
    let mode: &'static objc2_foundation::NSRunLoopMode =
        unsafe { objc2_foundation::NSDefaultRunLoopMode };
    let _ = NSRunLoop::mainRunLoop().runMode_beforeDate(mode, &limit);
}

/// `--selftest settingsclick` — a real click inside the settings pane.
///
/// The offscreen `settings`/`skin` renders build the tree and snap it, and
/// `skinlive` switches the skin from the top level: neither presses a control
/// the pane itself owns, which is where this port keeps its worst failure
/// modes (a re-entered model borrow aborts the process; a rebuild from inside
/// the pane's own `mouseUp:` releases the view mid-dispatch).
///
/// Every tab is opened, and each of its controls is then pressed through the
/// real `mouseUp:` route, so the whole path runs exactly as it does for the
/// user. Controls that would block in a modal system dialog (the open panels,
/// the alerts, the login item, the permission panes) are listed in
/// `selftest_blocks` and skipped — they cannot be driven non-interactively.
///
/// Returns false if the process would have died: a control that panicked or
/// aborted never reaches the next line.
pub fn settingsclick() -> bool {
    selftest_m2::seed_store_path();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    panel::prewarm();
    panel::show();
    // `show()` resets the model itself, so the pane is opened after it.
    panel::with_model_mut(|m| m.show_settings = true);
    crate::shelf::view::refresh();
    spin(0.35);
    if crate::shelf::view::settings_view().is_none() {
        println!("FAIL settingsclick: the pane never came up");
        return false;
    }

    let mut ok = true;
    let tabs = [
        "appearance",
        "clipboard",
        "capture",
        "location",
        "import",
        "updates",
        "shortcuts",
        "cleanup",
        "system",
    ];
    for tab in tabs {
        // Open the tab. The pane is re-read every pass: a rebuild may have
        // replaced it (and `skinlive`'s skin change is not what this test is
        // about, so only the first pass runs against the live panel).
        let Some(view) = crate::shelf::view::settings_view() else {
            println!("FAIL settingsclick: the pane went away on {tab}");
            return false;
        };
        // The rail's hotspots are recorded by `drawRect:` too — forced through
        // the bitmap path: an occluded panel window skips every display pass.
        crate::app::selftest_draw::force_draw(&view);
        let Some(center) =
            crate::shelf::settings_pane::selftest_hotspot_center(&view, Choice::Tab(tab))
        else {
            println!("FAIL settingsclick: no {tab} tab in the rail");
            ok = false;
            continue;
        };
        crate::shelf::settings_pane::selftest_click_at(&view, center);
        spin(0.05);
        let Some(view) = crate::shelf::view::settings_view() else {
            println!("FAIL settingsclick: the pane went away after opening {tab}");
            return false;
        };
        let reached = crate::shelf::settings_pane::selftest_tab(&view) == tab;
        if !reached {
            println!("FAIL settingsclick: the {tab} tab click did not land");
            ok = false;
            continue;
        }

        // Press every control on it. The hotspots are recorded by `drawRect:`,
        // so a pane built since the last pass has none until it draws once —
        // forced through the bitmap path (an occluded window skips display
        // passes, `displayIfNeeded` included).
        crate::app::selftest_draw::force_draw(&view);
        let hotspots = crate::shelf::settings_pane::selftest_hotspots(&view);
        let mut pressed = 0usize;
        let mut skipped = 0usize;
        for (choice, center) in hotspots {
            if crate::shelf::settings_pane::selftest_blocks(choice) {
                skipped += 1;
                continue;
            }
            // A rebuilding choice (the skin picker, the language rows) is
            // driven first: everything after it runs against the fresh tree.
            crate::shelf::settings_pane::selftest_click_at(&view, center);
            spin(0.05);
            pressed += 1;
            let Some(now) = crate::shelf::view::settings_view() else {
                println!("FAIL settingsclick: {tab} · {choice:?} took the pane down");
                return false;
            };
            // Back on the tab under test: a switch resets to the first page
            // only when the tree was rebuilt, which is expected for a skin.
            let back = crate::shelf::settings_pane::selftest_tab(&now) == tab;
            if !back && !matches!(choice, Choice::Skin(_) | Choice::Language(_)) {
                // Re-open and carry on; the control itself still ran.
                if let Some(c) =
                    crate::shelf::settings_pane::selftest_hotspot_center(&now, Choice::Tab(tab))
                {
                    crate::shelf::settings_pane::selftest_click_at(&now, c);
                    spin(0.05);
                }
            }
        }
        println!(
            "ok   settingsclick: {tab:<11} {pressed} control(s) pressed · {skipped} skipped (modal/user state)"
        );
    }

    crate::shelf::panel::hide();
    println!(
        "{} settingsclick: the pane survived every control on every tab",
        if ok { "ok  " } else { "FAIL" }
    );
    ok
}

pub fn contact(out: &str) -> bool {
    render_shelf_host(|m| {
        m.reset();
        m.show_contact = true;
    }, 560.0, out)
}

/// `--selftest contactclick` — a real click on the sidebar's 联系我 row.
///
/// The offscreen `contact` render builds the pane and snaps it, but never
/// takes the route the user's crash did: a live click on the sidebar of the
/// real panel while the shelf's own controls run (the search field holding a
/// field editor, the 0.25 s poll timer firing under the pane). This opens
/// the real panel and presses the sidebar row through the real
/// `mouseDown:`/`mouseUp:` route — once plainly, once with the search field
/// holding first responder (the order a ⌘F-then-click produces) — lets the
/// poll timer run with the pane up, then presses ⎋ to back out.
///
/// Returns false if the process would have died: an abort never reaches the
/// next line, the panic lands on stderr and the runner sees a non-zero exit.
pub fn contactclick() -> bool {
    selftest_m2::seed_store_path();
    panel::with_model_mut(|m| m.reset());
    panel::with_model_mut(|m| m.show_welcome = false);
    panel::prewarm();
    panel::show();
    spin(0.35);

    let mut ok = true;
    let Some(pt) = crate::shelf::view::selftest_sidebar_row_center(2) else {
        println!("FAIL contactclick: the sidebar never came up");
        panel::hide();
        return false;
    };

    // Pass 1: the plain click.
    if !crate::shelf::view::selftest_sidebar_click_at(pt) {
        println!("FAIL contactclick: the sidebar click did not dispatch");
        panel::hide();
        return false;
    }
    spin(0.3);
    let up = panel::with_model(|m| m.show_contact) && crate::shelf::view::contact_view().is_some();
    println!(
        "{} contactclick: the sidebar row {} the contact pane",
        if up { "ok  " } else { "FAIL" },
        if up { "opened" } else { "did not open" }
    );
    ok &= up;

    // Pass 2: the poll timer with the pane up — the re-entrancy shape the
    // pin-delete confirm used to hit, now against the contact pane.
    spin(1.2);
    let alive = crate::shelf::view::contact_view().is_some();
    println!(
        "{} contactclick: the pane survived 1.2s of poll ticks",
        if alive { "ok  " } else { "FAIL" }
    );
    ok &= alive;

    // Pass 3: the user's real order — ⌘F puts the caret in the search box,
    // then the click hides it. The field editor's resign must not re-enter.
    panel::with_model_mut(|m| m.show_contact = false);
    crate::shelf::view::refresh();
    spin(0.2);
    crate::shelf::view::focus_search_now();
    spin(0.2);
    if !crate::shelf::view::selftest_sidebar_click_at(pt) {
        println!("FAIL contactclick: the second sidebar click did not dispatch");
        panel::hide();
        return false;
    }
    spin(0.3);
    let up2 = panel::with_model(|m| m.show_contact) && crate::shelf::view::contact_view().is_some();
    println!(
        "{} contactclick: the click with the search box focused {} the pane",
        if up2 { "ok  " } else { "FAIL" },
        if up2 { "opened" } else { "did not open" }
    );
    ok &= up2;

    // Pass 4: back out with ⎋ — the only way the pane closes now that the
    // header button is gone, sent through the real `sendEvent:` route.
    let Some(win) = panel::selftest_window() else {
        println!("FAIL contactclick: no live panel to send ⎋ to");
        panel::hide();
        return false;
    };
    let esc = objc2_app_kit::NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        objc2_app_kit::NSEventType::KeyDown,
        CGPoint::ZERO,
        objc2_app_kit::NSEventModifierFlags::empty(),
        0.0,
        win.windowNumber(),
        None,
        &NSString::from_str("\u{1b}"),
        &NSString::from_str("\u{1b}"),
        false,
        53, // VK_ESCAPE
    )
    .expect("synthetic ⎋");
    win.sendEvent(&esc);
    spin(0.3);
    let down = !panel::with_model(|m| m.show_contact);
    println!(
        "{} contactclick: ⎋ {} the pane",
        if down { "closed" } else { "FAIL" },
        if down { "closed" } else { "did not close" }
    );
    ok &= down;

    panel::hide();
    println!(
        "{} contactclick: the contact pane survived a real click cycle",
        if ok { "ok  " } else { "FAIL" }
    );
    ok
}

pub fn welcome(out: &str) -> bool {
    render_shelf_host(|m| {
        m.reset();
        m.show_welcome = true;
    }, env_height(450.0), out)
}

/// `SelfTest.note` — the first text card rendered as a desktop note
/// (natural size branch against the store's own content).
pub fn note(out: &str) -> bool {
    selftest_m2::seed_store_path();
    let Some(item) = store::read(|s| s.items.iter().find(|it| it.kind == ClipKind::Text).cloned()) else {
        println!("note: no text item");
        return false;
    };
    crate::shelf::desktop_notes::place(&item.id, Some(CGPoint::new(80.0, 200.0)));
    let Some(view) = crate::shelf::desktop_notes::any_note_view() else {
        println!("note: no view");
        return false;
    };
    let h = crate::shelf::desktop_note_view::NoteView::natural_height(&item) + crate::shelf::desktop_note_view::NOTE_MARGIN * 2.0;
    let w = crate::shelf::desktop_note_view::NOTE_W + crate::shelf::desktop_note_view::NOTE_MARGIN * 2.0;
    view.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(w, h)));
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            CGRect::new(CGPoint::ZERO, CGSize::new(w, h)),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setContentView(Some(&view));
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    let ok = selftest_m2::snapshot(&view, out);
    crate::shelf::desktop_notes::close(&item.id);
    ok
}

/// `SelfTest.editors` — three renders: titled text, untitled text, image.
pub fn editors(out: &str) -> bool {
    selftest_m2::seed_store_path();
    let base = out.strip_suffix(".png").unwrap_or(out).to_string();
    let mut ok = true;
    let titled = store::read(|s| s.items.iter().find(|it| it.kind == ClipKind::Text && it.title.is_some()).cloned());
    if let Some(t) = titled {
        match crate::shelf::text_editor::TextEditorWindow::debug_view(&t) {
            Some(v) => { ok = selftest_m2::snapshot(&v, &format!("{base}.text.png")) && ok; }
            None => ok = false,
        }
    }
    let untitled = store::read(|s| s.items.iter().find(|it| it.kind == ClipKind::Text && it.title.is_none()).cloned());
    if let Some(t) = untitled {
        match crate::shelf::text_editor::TextEditorWindow::debug_view(&t) {
            Some(v) => { ok = selftest_m2::snapshot(&v, &format!("{base}.text-untitled.png")) && ok; }
            None => ok = false,
        }
    }
    let image = store::read(|s| s.items.iter().find(|it| it.kind == ClipKind::Image).cloned());
    if let Some(im) = &image {
        match crate::shelf::image_editor::ImageEditorWindow::debug_view(im) {
            Some(v) => { ok = selftest_m2::snapshot(&v, &format!("{base}.image.png")) && ok; }
            None => ok = false,
        }
    }

    // A render only shows the layout `make` produced at the window's own
    // default size; what this round is about is the window following a resize.
    // Drive the resize path directly and check every placed frame, at a large
    // size and at the 420×300 minimum.
    if let Some(t) = store::read(|s| {
        s.items
            .iter()
            .find(|it| it.kind == ClipKind::Text && it.title.is_some())
            .cloned()
    }) {
        // The same window at its 420×300 minimum, drawn rather than only
        // measured — the render is what shows the footer still fits.
        match crate::shelf::text_editor::TextEditorWindow::debug_view_sized(
            &t,
            Some(CGSize::new(420.0, 300.0)),
        ) {
            Some(v) => { ok = selftest_m2::snapshot(&v, &format!("{base}.text-min.png")) && ok; }
            None => ok = false,
        }
        for (label, size) in [
            ("900×620", CGSize::new(900.0, 620.0)),
            ("420×300", CGSize::new(420.0, 300.0)),
        ] {
            let frames = crate::shelf::text_editor::TextEditorWindow::debug_layout_at(&t, size);
            let get = |n: &str| frames.iter().find(|f| f.0 == n).copied();
            let all_inside = frames.iter().all(|f| {
                f.1 >= -0.5
                    && f.2 >= -0.5
                    && f.1 + f.3 <= size.width + 0.5
                    && f.2 + f.4 <= size.height + 0.5
            });
            let inner = size.width - 32.0;
            let width_tracks = get("sheet").map(|f| (f.3 - inner).abs() < 1.0).unwrap_or(false)
                && get("scroll").map(|f| (f.3 - inner).abs() < 1.0).unwrap_or(false);
            // Footer: the count on the left, cancel then save hard against
            // the right margin, none of them overlapping.
            let footer_ok = match (get("count"), get("cancel"), get("save")) {
                (Some(c), Some(x), Some(s)) => {
                    c.1 + c.3 <= x.1 + 0.5
                        && x.1 + x.3 <= s.1 + 0.5
                        && (s.1 + s.3 - (size.width - 16.0)).abs() < 1.0
                        && s.2 + s.4 <= size.height + 0.5
                        && s.2 >= 0.0
                }
                _ => false,
            };
            println!(
                "     text {label}: {} frames, all inside={all_inside}, width tracks={width_tracks}, footer ok={footer_ok}",
                frames.len()
            );
            for f in &frames {
                println!("       {:14} x={:7.1} y={:7.1} w={:7.1} h={:6.1}", f.0, f.1, f.2, f.3, f.4);
            }
            ok = all_inside && width_tracks && footer_ok && frames.len() == 6 && ok;
        }
    }

    if let Some(im) = &image {
        // The window `make` actually built for this card.
        match crate::shelf::image_editor::ImageEditorWindow::debug_geometry(im) {
            Some((cw, ch, cx, cy, cwd, cht, bx, by, bw, bh)) => {
                let canvas_ok =
                    cx >= 0.0 && cy >= 44.0 && cx + cwd <= cw + 0.5 && cy + cht <= ch + 0.5;
                let bar_ok = bx >= 0.0
                    && by >= 0.0
                    && bx + bw <= cw + 0.5
                    && by + bh <= cy + 0.5;
                println!(
                    "     image window {cw:.0}×{ch:.0}: canvas ({cx:.0},{cy:.0}) {cwd:.0}×{cht:.0}, toolbar ({bx:.0},{by:.0}) {bw:.0}×{bh:.0}"
                );
                ok = canvas_ok && bar_ok && ok;
            }
            None => ok = false,
        }
        // The clamp itself, on a picture no screen can show whole: the window
        // must come back no larger than the visible frame, with the picture
        // still inside it — the case that used to build a window taller than
        // the display.
        let vf = CGRect::new(CGPoint::ZERO, CGSize::new(1470.0, 920.0));
        let tall = CGSize::new(1179.0, 5000.0);
        let (tw, tc) = crate::shelf::image_editor::editor_geometry(tall, vf);
        let clamp_ok = tw.height <= vf.size.height - 40.0 + 0.5
            && tw.width <= vf.size.width - 40.0 + 0.5
            && tc.min().y >= 44.0
            && tc.min().y + tc.size.height <= tw.height + 0.5;
        println!(
            "     clamp: {:.0}pt-tall picture on a {:.0}×{:.0} screen → window {:.0}×{:.0}, canvas y {:.0}..{:.0}",
            tall.height,
            vf.size.width,
            vf.size.height,
            tw.width,
            tw.height,
            tc.min().y,
            tc.min().y + tc.size.height
        );
        ok = clamp_ok && ok;

        // The image window is resizable now: drive `layout` for a big box and
        // for the 600×360 minimum (the toolbar is ~554pt wide, so narrower
        // than that it would clip), and check the canvas and toolbar actually
        // moved with it (before this the frames stayed at their `make` origin).
        for (label, size) in [
            ("1200×820", CGSize::new(1200.0, 820.0)),
            ("600×360", CGSize::new(600.0, 360.0)),
        ] {
            match crate::shelf::image_editor::ImageEditorWindow::debug_layout_at(im, size) {
                Some((cx, cy, cwd, cht, bx, by, bw, bh)) => {
                    let inside = cx >= -0.5
                        && cy >= -0.5
                        && cx + cwd <= size.width + 0.5
                        && cy + cht <= size.height + 0.5
                        && bx >= -0.5
                        && by >= -0.5
                        && bx + bw <= size.width + 0.5
                        && by + bh <= size.height + 0.5;
                    println!(
                        "     image {label}: canvas ({cx:.0},{cy:.0}) {cwd:.0}×{cht:.0}, toolbar ({bx:.0},{by:.0}) {bw:.0}×{bh:.0}, inside={inside}"
                    );
                    ok = inside && ok;
                }
                None => ok = false,
            }
        }
    }
    ok
}

#[cfg(not(feature = "mas"))]
/// `SelfTest.updatewin` — the progress panel at 1.9 / 4.86 MB (Swift:
/// `UpdateProgressWindow(version:)` + `update(done:total:)`). Resizable now;
/// the render is at the title-dependent width `new` settles on.
pub fn updatewin(out: &str) -> bool {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let w = crate::app::update_progress::Window::new("1.0.3".into());
    w.update(1_900_000, 4_860_000);
    let content = w.content_view();
    content.setFrame(CGRect::new(CGPoint::ZERO, content.frame().size));
    content.layoutSubtreeIfNeeded();
    selftest_m2::snapshot(&content, out)
}

#[cfg(not(feature = "mas"))]
/// `SelfTest.download <url> [out]` — the downloader exercised for real.
pub fn download(rest: &[String]) -> bool {
    let url_s = rest.first().map(String::as_str).unwrap_or("");
    let dest = rest
        .get(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("pastory-dl-test"));
    let Some(url) = NSURL::URLWithString(&NSString::from_str(url_s)) else {
        println!("download: bad url {url_s}");
        return false;
    };
    match crate::app::update_progress::fetch_blocking(&url, &dest) {
        Ok(bytes) => {
            println!("ok   {bytes} bytes");
            true
        }
        Err(e) => {
            println!("failed: {}", e.describe());
            false
        }
    }
}

/// `SelfTest.updater` — fixture parse + version compare + notes strip +
/// the ad-hoc self-install guard (Swift's checks table).
pub fn updater() -> bool {
    let fixture = r#"{"tag_name":"v1.2","html_url":"https://github.com/Claudate/pastory-rust/releases/tag/v1.2","body":"- faster shelf\n- HEIC option",
  "assets":[{"name":"PasteJar-1.2.zip","browser_download_url":"https://github.com/Claudate/pastory-rust/releases/download/v1.2/PasteJar-1.2.zip"}]}"#;
    let r = crate::app::updater::parse_release(fixture.as_bytes()).ok();
    if let Some(rel) = &r {
        println!(
            "parsed: {} {} notes={}",
            rel.version,
            rel.zip_url.as_deref().and_then(|u| u.rsplit('/').next()).unwrap_or("-"),
            rel.notes.len()
        );
    }
    // `mut` is for the direct build only: the self-install guard below is
    // appended under `not(mas)`.
    #[cfg_attr(feature = "mas", allow(unused_mut))]
    let mut checks: Vec<(&str, bool)> = vec![
        ("parses tag/notes/asset", r.as_ref().map(|rel| {
            rel.version == "1.2" && rel.zip_url.as_deref().and_then(|u| u.rsplit('/').next()) == Some("PasteJar-1.2.zip") && rel.notes.contains("HEIC")
        }).unwrap_or(false)),
        ("1.2 > 1.0", crate::app::updater::is_newer("1.2", "1.0")),
        ("1.0.1 > 1.0", crate::app::updater::is_newer("1.0.1", "1.0")),
        ("1.0 !> 1.0", !crate::app::updater::is_newer("1.0", "1.0")),
        ("1.10 > 1.9", crate::app::updater::is_newer("1.10", "1.9")),
        ("notes: zh half, no markdown", {
            let t = crate::app::updater::notes_for_display(
                "PasteJar 1.2\n\n**新功能**\n- 一\n- 二\n\n---\n\nPasteJar 1.2\n\n**New**\n- one",
                "1.2",
                false,
            );
            t == "新功能\n•  一\n•  二"
        }),
        ("notes: en half", crate::app::updater::notes_for_display("中文\n---\n**New**\n- one", "1.2", true) == "New\n•  one"),
        ("notes: legacy brand title stripped", crate::app::updater::notes_for_display("Pastory Rust 1.3\n\n- old", "1.3", true) == "•  old"),
        ("notes: no divider keeps all", crate::app::updater::notes_for_display("- a\n- b", "1.2", true) == "•  a\n•  b"),
        ("feed schema: ext keys parse; unknown host refused", {
            // Cloudflare feed doc (docs/MAS_PLAN.md §1.1): sha256/mas extension
            // keys must not break parsing, and the zip host whitelist is what
            // gates the download — only GitHub's hosts plus the compiled-in
            // feed host pass.
            let feed = r#"{"tag_name":"v1.3","html_url":"https://github.com/Claudate/pastory-rust/releases/tag/v1.3","body":"- feed notes",
  "assets":[{"name":"PasteJar-1.3.zip","browser_download_url":"https://feed.example.com/files/PasteJar-1.3.zip","sha256":"aa"},{"name":"notes.txt","browser_download_url":"https://feed.example.com/files/notes.txt"}],"mas":{"store_url":"macrod://x"}}"#;
            match crate::app::updater::parse_release(feed.as_bytes()) {
                Ok(rel) => {
                    let host_is_feed = crate::app::updater::FEED_HOST == Some("feed.example.com");
                    rel.version == "1.3"
                        && rel.notes.contains("feed notes")
                        && rel.zip_url.is_some() == host_is_feed
                        && rel.sha256.is_some() == host_is_feed
                }
                Err(_) => false,
            }
        }),
        ("sha256: digest matches the system reference", {
            // Cross-checked against `shasum -a 256` for the same inputs, so a
            // backend change in sha2 cannot silently alter the digest, and an
            // unreadable file must never pass the quick-check.
            crate::app::updater::sha256_hex(b"")
                == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                && crate::app::updater::sha256_hex(b"pastejar")
                    == "5fd29975e157bb8984b8f946eef5d1959dc08d294fe87d8f5286191cc4562a25"
                && !crate::app::updater::verify_sha256(
                    std::path::Path::new("/nonexistent-pastejar-selftest"),
                    "e3b0c44",
                )
        }),
    ];
    #[cfg(not(feature = "mas"))]
    checks.push((
        "ad-hoc build must not self-install",
        // The dev build of this binary is ad-hoc-signed: it must never
        // replace itself (Swift checks teamIdentifier==nil → canSelfInstall false).
        {
            let bundle = crate::app::updater::bundle_path();
            let team = crate::app::updater::team_identifier(&bundle);
            team.is_none() && !crate::app::updater::can_self_install() || team.is_some()
        },
    ));
    let mut ok = true;
    for (name, good) in &checks {
        println!("{} {}", if *good { "ok  " } else { "FAIL" }, name);
        ok &= *good;
    }
    ok
}

/// `--selftest exportdir` — the remembered 「保存到本地」 folder.
///
/// Under the sandbox the grant does not survive a launch, so the memory is a
/// security-scoped bookmark (docs/MAS_PLAN.md §3): it must round-trip, the
/// plain path must stay readable for the settings pane, and a memory this
/// launch cannot use must be dropped rather than block the next save.
pub fn exportdir() -> bool {
    use crate::app::preferences::{ns_string_id, Preferences};

    let prefs = Preferences::shared();
    let tmp = std::env::temp_dir().join(format!("pastejar-exportdir-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    // The bookmark hands back the real path, and /var is a symlink to
    // /private/var — normalise before comparing, or the test fails on macOS's
    // own plumbing rather than on the code.
    let tmp = std::fs::canonicalize(&tmp).expect("canonicalize scratch dir");
    let url = NSURL::fileURLWithPath(&NSString::from_str(&tmp.to_string_lossy()));

    prefs.set_custom_export_dir(&url);
    let path_shown = prefs.custom_export_dir().as_deref() == Some(&*tmp.to_string_lossy());
    // Drop the plain path: what comes back now can only be the bookmark.
    prefs.defaults().removeObjectForKey(ns_string_id("exportDir"));
    let via_bookmark = prefs.export_directory() == tmp;

    // A blob that cannot be resolved (a folder on a volume that is gone) must
    // fall back to ~/Downloads and forget it, never keep handing it back.
    unsafe {
        prefs.defaults().setObject_forKey(
            Some(&*NSData::with_bytes(b"not a bookmark")),
            ns_string_id("exportDirBookmark"),
        );
    }
    crate::app::preferences_export::forget_open_export_dir();
    let fallback = prefs.export_directory();
    let home = std::env::var_os("HOME").expect("HOME");
    let downloads = std::path::PathBuf::from(home).join("Downloads");
    let reset = fallback == downloads && prefs.custom_export_dir().is_none();

    let _ = std::fs::remove_dir_all(&tmp);
    prefs.defaults().removeObjectForKey(ns_string_id("exportDirBookmark"));
    prefs.defaults().removeObjectForKey(ns_string_id("exportDir"));
    crate::app::preferences_export::forget_open_export_dir();

    let checks: [(&str, bool); 3] = [
        ("bookmark: plain path kept for the pane", path_shown),
        ("bookmark: resolves after the path key is gone", via_bookmark),
        ("bookmark: unusable memory reset to ~/Downloads", reset),
    ];
    let mut ok = true;
    for (name, good) in &checks {
        println!("{} {}", if *good { "ok  " } else { "FAIL" }, name);
        ok &= *good;
    }
    ok
}

/// `SelfTest.import <db>` — scan a foreign SQLite and pull it into the
/// sandbox store (prints what it found; asserted n > 0 like Swift).
pub fn import(rest: &[String]) -> bool {
    let Some(db_path) = rest.first() else {
        println!("import needs a db path");
        return false;
    };
    let path = std::path::PathBuf::from(db_path);
    match crate::clipboard::importer::scan(&path) {
        Ok(scan) => {
            println!(
                "scan: {} texts, {} images from tables {:?}",
                scan.texts(),
                scan.images(),
                scan.tables
            );
            for e in scan.entries.iter().take(12) {
                match &e.payload {
                    crate::clipboard::store::ImportPayload::Text(t) => println!(
                        "  text  {} pin={} {}",
                        e.created_at,
                        e.pinned,
                        t.chars().take(40).collect::<String>().replace('\n', "⏎")
                    ),
                    crate::clipboard::store::ImportPayload::Image(d) => println!(
                        "  image {} pin={} {} bytes",
                        e.created_at,
                        e.pinned,
                        d.len()
                    ),
                }
            }
            let n = store::with(|s| s.import_entries(scan.entries));
            println!("imported {n}; store now {}", store::read(|s| s.items.len()));
            n > 0
        }
        Err(f) => {
            println!("import failed: {}", f.message());
            false
        }
    }
}

/// `SelfTest.openpanel` — how long until the system open panel is actually
/// on screen (Swift's first-show timing).
pub fn openpanel() -> bool {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let t0 = std::time::Instant::now();
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(true);
    panel.setCanChooseFiles(true);
    let home = std::env::var_os("HOME").expect("HOME");
    let url = NSURL::fileURLWithPath(&NSString::from_str(&home.to_string_lossy()));
    panel.setDirectoryURL(Some(&url));
    unsafe {
        let _: () = objc2::msg_send![&app, activate];
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let block = block2::RcBlock::new(|_response: isize| {});
    unsafe {
        let _: () = objc2::msg_send![&*panel, beginWithCompletionHandler: &*block];
    }
    let mut waited = 0.0f64;
    let rl = NSRunLoop::mainRunLoop();
    let visible = loop {
        if panel.isVisible() { break true; }
        if waited >= 30.0 { break panel.isVisible(); }
        let limit = NSDate::dateWithTimeIntervalSinceNow(0.05);
        let mode: &'static objc2_foundation::NSRunLoopMode = unsafe { objc2_foundation::NSDefaultRunLoopMode };
        let _ = rl.runMode_beforeDate(mode, &limit);
        waited += 0.05;
    };
    println!(
        "open panel visible after {:.2} s (visible={})",
        t0.elapsed().as_secs_f64(),
        if visible { "yes" } else { "no" }
    );
    // Close the sheet (`panel.cancel(nil)`); raw msg_send sidesteps the
    // generated binding's typing of the sender.
    unsafe {
        let nil_sender: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
        let _: () = objc2::msg_send![&*panel, cancel: nil_sender];
    }
    visible || waited < 30.0
}

/// Keeps the host alive one window beat, like the Swift tests' windows.
#[allow(dead_code)]
fn host_keep(_: &NSWindow) {}

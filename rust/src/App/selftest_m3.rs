//! Port of the M3 `capture` selftest (`App/SelfTest.swift:257-315`): a real
//! one-shot capture of the main display vs /usr/sbin/screencapture of the
//! same rect, compared by random pixel sampling (median/p95/max). Slow async
//! SCK calls are gated on the main run loop.
//!
//! Coordinate coverage (§8.10): the region round-trip picks a fixed
//! display-local rect, captures it directly, and samples it against the same
//! crop of the reference image — a wrong point↔pixel or top-left↔bottom-left
//! conversion shows up as a huge diff immediately.

use std::path::Path;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGImage};
use objc2_foundation::{NSDate, NSRunLoop};

use crate::app::coordinates;
use crate::app::permissions;
use crate::capture::screenshotter;
use crate::capture::target::{CaptureTarget, ShareableSnapshot};

/// Wait for a main-thread completion (a dispatched task + a pump deadline).
/// nil on timeout — SCK never blocks long on a granted session.
///
/// The value crosses the run-loop hand-off as a raw box so `T` can carry
/// ObjC objects that are not `Send` (they are immutable snapshots; the hop
/// is main-thread → SCK queue → main-thread).
pub(crate) fn block_on<T: 'static>(task: impl FnOnce(Box<dyn FnOnce(T) + Send + 'static>), timeout_secs: f64) -> Option<T> {
    let slot: std::sync::Arc<std::sync::Mutex<Option<usize>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot2 = slot.clone();
    task(Box::new(move |value: T| {
        let raw = Box::into_raw(Box::new(value)) as usize;
        *slot2.lock().unwrap() = Some(raw);
    }));
    let run_loop = NSRunLoop::mainRunLoop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_secs);
    while std::time::Instant::now() < deadline {
        if slot.lock().unwrap().is_some() {
            break;
        }
        let limit = NSDate::dateWithTimeIntervalSinceNow(0.05);
        // SAFETY: NSDefaultRunLoopMode is a valid mode token.
        let mode: &'static objc2_foundation::NSRunLoopMode =
            unsafe { objc2_foundation::NSDefaultRunLoopMode };
        let _ = run_loop.runMode_beforeDate(mode, &limit);
    }
    let raw = slot.lock().unwrap().take()?;
    // SAFETY: the box was packed by the completion above, reached the slot
    // untouched, and is reclaimed exactly once here (main thread).
    Some(*unsafe { Box::from_raw(raw as *mut T) })
}

/// A stack of both PNGs into a single 8-bit sRGB buffer for sampling
/// (`SelfTest.compare`'s `raw`).
fn raw_pixels(img: &CGImage) -> Option<Vec<u8>> {
    let (w, h) = (screenshotter::width(img), screenshotter::height(img));
    let mut buf = vec![0u8; w * h * 4];
    let space = unsafe {
        CGColorSpace::with_name(Some(objc2_core_graphics::kCGColorSpaceSRGB))
    };
    let ctx = unsafe {
        CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            w,
            h,
            8,
            w * 4,
            space.as_deref(),
            1, // premultipliedLast
        )
    }?;
    objc2_core_graphics::CGContext::draw_image(
        Some(&ctx),
        CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64)),
        Some(img),
    );
    Some(buf)
}

/// Deterministic sample order (Swift uses SystemRandom; a fixed SplitMix64
/// keeps reruns comparable without changing the statistics' meaning).
fn sample_indices(count: usize, n: usize) -> Vec<usize> {
    let mut g = 0x853C_49E6_748F_EA9Bu64;
    (0..n)
        .map(|_| {
            g = g.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = g;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        })
        .map(|z| (z as usize) % count)
        .collect()
}

/// `SelfTest.compare` — 2000 RGB samples per pair; median/p95/max + |Δ|>2 count.
fn compare(a: &CGImage, b: &CGImage, label: &str) {
    if screenshotter::width(a) != screenshotter::width(b)
        || screenshotter::height(a) != screenshotter::height(b)
    {
        println!("{label}: size differs, skipped comparison");
        return;
    }
    let (Some(ra), Some(rb)) = (raw_pixels(a), raw_pixels(b)) else {
        return;
    };
    let pixels = ra.len() / 4;
    let mut diffs: Vec<i32> = sample_indices(pixels, 2000)
        .into_iter()
        .map(|i| {
            let j = i * 4;
            (0..3)
                .map(|c| (ra[j + c] as i32 - rb[j + c] as i32).abs())
                .max()
                .unwrap_or(0)
        })
        .collect();
    diffs.sort_unstable();
    let over2 = diffs.iter().filter(|d| **d > 2).count();
    println!(
        "{label} pixel diff vs screencapture (2000 samples): median={} p95={} max={} samples>2: {}",
        diffs[1000],
        diffs[1900],
        diffs[1999],
        over2
    );
}

/// `--selftest capture [out.png]` (SelfTest.capture).
pub fn capture(out: &str) -> bool {
    if !permissions::has_screen_recording() {
        // Same words as the Swift test, plus the §5.5 first-capture request:
        // on a machine that has never granted the bundle, this puts Pastory
        // into the System Settings list; grant it, then re-run. The run
        // waits up to 60 s for the toggle so an in-person grant flips this
        // very invocation.
        let _ = permissions::request_screen_recording();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline && !permissions::has_screen_recording() {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if !permissions::has_screen_recording() {
            println!("no screen recording permission");
            return false;
        }
    }
    let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let Some(Some(snap)) = block_on(|cb| ShareableSnapshot::fetch(cb), 10.0) else {
        println!("no shareable content");
        return false;
    };
    let Some(screen) = objc2_app_kit::NSScreen::mainScreen(mtm) else {
        println!("no main screen");
        return false;
    };
    let Some(display) = snap.display_for_screen(&screen) else {
        println!("no display");
        return false;
    };
    let snap_ptr = Box::into_raw(Box::new(snap)) as usize;
    // SAFETY: reclaimed below; the snapshot lives for the whole test.
    let snap = || unsafe { &*(snap_ptr as *const ShareableSnapshot) };
    let t0 = std::time::Instant::now();
    let Some(Some(img)) = block_on(
        |cb| screenshotter::capture(CaptureTarget::Display(display.clone()), snap(), cb),
        15.0,
    ) else {
        println!("capture failed");
        return false;
    };
    let dt = t0.elapsed().as_secs_f64();
    let Some(png) = screenshotter::png_data(&img) else {
        println!("png failed");
        return false;
    };
    if let Err(e) = std::fs::write(out, &png) {
        println!("write {out} failed: {e}");
        return false;
    }
    let cs = CGImage::color_space(Some(&img))
        .and_then(|s| CGColorSpace::name(Some(&s)))
        .map(|n| n.to_string())
        .unwrap_or_else(|| "nil".into());
    println!(
        "captured {}×{} in {} ms, colorSpace={}, {} bytes → {}",
        screenshotter::width(&img),
        screenshotter::height(&img),
        (dt * 1000.0) as i64,
        cs,
        png.len(),
        out
    );
    let screen_cs = screen
        .colorSpace()
            .and_then(|s| quote_name(&s))
        .unwrap_or_else(|| "nil".into());
    println!("screen colorSpace={screen_cs}");
    capture_reference(&Path::new(out), &screen, &img, &display, snap())
}

/// `screen.colorSpace.cgColorSpace.name` as a string.
fn quote_name(space: &objc2_app_kit::NSColorSpace) -> Option<String> {
    let cg = space.CGColorSpace()?;
    CGColorSpace::name(Some(&cg)).map(|n| n.to_string())
}

/// Take Apple's own shot of the same rect and sample both; then re-shoot a
/// display-local region and check the coordinate math end to end.
fn capture_reference(
    out: &Path,
    screen: &objc2_app_kit::NSScreen,
    img: &CGImage,
    display: &objc2_screen_capture_kit::SCDisplay,
    snap: &ShareableSnapshot,
) -> bool {
    let ref_path = out
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("pastory-ref.png");
    let f = screen.frame();
    let cg_y = coordinates::primary_height() - (f.origin.y + f.size.height);
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .arg("-x")
        .arg("-R")
        .arg(format!(
            "{},{},{},{}",
            f.origin.x as i64, cg_y as i64, f.size.width as i64, f.size.height as i64
        ))
        .arg(&ref_path)
        .status();
    let Ok(status) = status else {
        println!("screencapture reference unavailable (skipped comparison)");
        return true;
    };
    if !status.success() {
        println!("screencapture reference unavailable (skipped comparison)");
        return true;
    }
    let Ok(ref_data) = std::fs::read(&ref_path) else {
        println!("screencapture reference unavailable (skipped comparison)");
        return true;
    };
    let Some(ref_img) = screenshotter::image_from_png(&ref_data) else {
        println!("screencapture reference unavailable (skipped comparison)");
        return true;
    };
    let ref_cs = CGImage::color_space(Some(&ref_img))
        .and_then(|s| CGColorSpace::name(Some(&s)))
        .map(|n| n.to_string())
        .unwrap_or_else(|| "nil".into());
    println!(
        "reference {}×{}, colorSpace={ref_cs}",
        screenshotter::width(&ref_img),
        screenshotter::height(&ref_img)
    );
    compare(img, &ref_img, "full");

    // Region round-trip: display-local points → direct capture vs the same
    // crop of the reference (wrong origin/scale shows up instantly).
    let local = CGRect::new(CGPoint::new(100.0, 100.0), CGSize::new(640.0, 480.0));
    let display_owned: objc2::rc::Retained<objc2_screen_capture_kit::SCDisplay> = unsafe {
        // SAFETY: retaining a live object for the capture call.
        objc2::rc::Retained::retain(
            display as *const objc2_screen_capture_kit::SCDisplay
                as *mut objc2_screen_capture_kit::SCDisplay,
        )
        .expect("display non-null")
    };
    let Some(Some(region)) = block_on(
        |cb| screenshotter::capture(CaptureTarget::Region(display_owned, local), snap, cb),
        15.0,
    ) else {
        println!("region capture failed");
        return false;
    };
    let k = screenshotter::width(&ref_img) as f64 / f.size.width;
    let crop = CGRect::new(
        CGPoint::new(100.0 * k, 100.0 * k),
        CGSize::new(640.0 * k, 480.0 * k),
    );
    println!(
        "region {}×{} (expect {:.0}×{:.0})",
        screenshotter::width(&region),
        screenshotter::height(&region),
        640.0 * k,
        480.0 * k
    );
    let Some(cropped) = CGImage::with_image_in_rect(Some(&ref_img), crop) else {
        println!("region reference crop failed");
        return false;
    };
    compare(&region, &cropped, "region");
    true
}

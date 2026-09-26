//! Port of `App/SelfTest.swift` M5 branches (`writer` codec modes, `gif`,
//! `preview`).

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_media::CMTime;

use crate::app::synthetic_movie;
use crate::capture::{gif_encoder, screen_recorder};

/// Page of "text" (fine random pattern) that scrolls, like SelfTest's.
const W: usize = 3200;
const H: usize = 1640;

fn make_page() -> Vec<u8> {
    let mut page = vec![0u8; W * (H + 800) * 4];
    let mut g = crate::app::theme::Seeded(7);
    for row in 0..(H + 800) {
        if row % 28 >= 16 {
            continue;
        }
        // text lines with leading
        let mut x = 120usize;
        while x < W - 120 {
            if (x / 9) % 3 != 2 {
                let v: u8 = if g.next() % 5 == 0 { 30 } else { 235 };
                let o = (row * W + x) * 4;
                page[o] = v;
                page[o + 1] = v;
                page[o + 2] = v;
                page[o + 3] = 255;
            }
            x += 1;
        }
    }
    page
}

/// Feed `frames` synthesized frames through the writer parts (Swift's inner loop).
fn pump(
    parts: &mut screen_recorder::WriterParts,
    page: &[u8],
    frames: usize,
    still: bool,
) -> bool {
    unsafe {
        parts.writer.startSessionAtSourceTime(CMTime {
            value: 0,
            timescale: screen_recorder::FPS,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        })
    };
    for i in 0..frames {
        while !unsafe { parts.input.isReadyForMoreMediaData() } {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Direct pool + memcpy port of the Swift self-test.
        let pool = match unsafe { parts.adaptor.pixelBufferPool() } {
            Some(p) => p,
            None => return false,
        };
        let mut raw: *mut objc2_core_video::CVPixelBuffer = std::ptr::null_mut();
        #[allow(deprecated)]
        let status = unsafe {
            objc2_core_video::CVPixelBufferPoolCreatePixelBuffer(
                None,
                &pool,
                std::ptr::NonNull::new(&mut raw).expect("out ptr"),
            )
        };
        if status != 0 || raw.is_null() {
            return false;
        }
        let pb = unsafe {
            objc2_core_foundation::CFRetained::from_raw(std::ptr::NonNull::new_unchecked(raw))
        };
        unsafe {
            objc2_core_video::CVPixelBufferLockBaseAddress(&pb, objc2_core_video::CVPixelBufferLockFlags::empty());
        }
        let base = objc2_core_video::CVPixelBufferGetBaseAddress(&pb) as *mut u8;
        let stride = objc2_core_video::CVPixelBufferGetBytesPerRow(&pb);
        let scroll = if still { (i / 15) * 12 * W * 4 } else { i * 12 * W * 4 };
        for row in 0..H {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    page.as_ptr().add(scroll + row * W * 4),
                    base.add(row * stride),
                    W * 4,
                );
            }
        }
        unsafe {
            objc2_core_video::CVPixelBufferUnlockBaseAddress(&pb, objc2_core_video::CVPixelBufferLockFlags::empty());
        }
        unsafe {
            parts.adaptor.appendPixelBuffer_withPresentationTime(&pb, CMTime {
                value: i as i64,
                timescale: screen_recorder::FPS,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            });
        }
    }
    true
}

/// SelfTest's `writer` movie half: settings valid for all four codec modes,
/// the file plays back with the right duration, bitrate lands where the
/// formula says.
pub fn writer_modes() -> bool {
    let page = make_page();
    let mut ok_all = true;
    let tmp = std::env::temp_dir();
    for (hevc, cq, still) in [
        (false, false, false),
        (true, false, false),
        (false, false, true),
        (false, true, false),
    ] {
        let name = format!(
            "pastory-writer-{}-{}.mp4",
            if hevc { "hevc" } else { "h264" },
            if cq { "cq" } else { "abr" }
        );
        let url = tmp.join(&name);
        match screen_recorder::make_writer(&url, W, H, hevc, cq) {
            Ok(mut parts) => {
                let good = pump(&mut parts, &page, 60, still)
                    && {
                        unsafe { parts.input.markAsFinished() };
                        synthetic_movie::finish_writer(&parts.writer)
                    };
                let _ = &parts;
                let dur = gif_encoder::duration(&url);
                let bytes = std::fs::metadata(&url).map(|m| m.len()).unwrap_or(0);
                let good = good && (dur - 2.0).abs() < 0.2 && bytes > 0;
                let kbps = bytes * 8 / 2 / 1000;
                println!(
                    "{} {} {}: duration={:.2}s size={}K ≈ {} kbps {}",
                    if good { "ok  " } else { "FAIL" },
                    if hevc { "hevc" } else { "h264" },
                    if cq {
                        format!("quality {}", screen_recorder::QUALITY)
                    } else {
                        format!("abr {}kbps", screen_recorder::bitrate(W, H, hevc) / 1000)
                    },
                    dur,
                    bytes / 1024,
                    kbps,
                    if still { "mostly still" } else { "while scrolling" }
                );
                ok_all = ok_all && good;
            }
            Err(e) => {
                println!("FAIL {} {}: {}", if hevc { "hevc" } else { "h264" }, if cq { "cq" } else { "abr" }, e);
                ok_all = false;
            }
        }
    }
    ok_all
}


/// `--selftest gif` (SelfTest.gif): 2 s synthetic MP4 → GIF, frame count
/// lands at 2 s × 10 fps = 20 ± 2, poster + duration come out.
pub fn gif() -> bool {
    let dir = std::env::temp_dir();
    let mov = dir.join("pastory-selftest.mp4");
    let gif_path = dir.join("pastory-selftest.gif");
    if let Err(e) = crate::app::synthetic_movie::write(&mov, CGSize::new(640.0, 360.0), 2.0, 30) {
        println!("synthetic movie failed: {e}");
        return false;
    }
    let t0 = std::time::Instant::now();
    if let Err(e) = crate::capture::gif_encoder::encode(&mov, &gif_path, None) {
        println!("gif selftest failed: {e}");
        return false;
    }
    let ms = t0.elapsed().as_millis();
    let Some(cg) = crate::capture::screenshotter::image_from_file(&gif_path) else {
        println!("gif unreadable");
        return false;
    };
    let bytes = std::fs::metadata(&gif_path).map(|m| m.len()).unwrap_or(0);
    let n = {
        let src_path = objc2_core_foundation::CFString::from_str(&gif_path.to_string_lossy());
        let url2 = objc2_core_foundation::CFURL::with_file_system_path(None, Some(&src_path), objc2_core_foundation::CFURLPathStyle::CFURLPOSIXPathStyle, false);
        url2.map(|u| {
            let src = unsafe { objc2_image_io::CGImageSource::with_url(&u, None) }?;
            Some(unsafe { src.count() })
        }).flatten().unwrap_or(0)
    };
    let first_w = crate::capture::screenshotter::width(&cg);
    let first_h = crate::capture::screenshotter::height(&cg);
    println!("gif: {} frames, {}×{}, {} KB, encoded in {} ms → {}",
        n, first_w, first_h, bytes / 1024, ms, gif_path.display());
    let poster = crate::capture::gif_encoder::poster(&mov);
    println!(
        "poster: {}, duration {:.2}s",
        poster.as_ref().map(|p| format!("{}×{}", crate::capture::screenshotter::width(p), crate::capture::screenshotter::height(p))).unwrap_or_else(|| "nil".into()),
        crate::capture::gif_encoder::duration(&mov)
    );
    n >= 18 && n <= 22 && poster.is_some()
}


/// `--selftest preview [out.png]` (SelfTest:49-55): 6 s synthetic movie in
/// the preview window, seeked to 2.5 s.
pub fn preview(out: &str) -> bool {
    let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let mov = std::env::temp_dir().join("pastory-preview.mp4");
    let _ = std::fs::remove_file(&mov);
    if let Err(e) = crate::app::synthetic_movie::write(&mov, CGSize::new(1280.0, 720.0), 6.0, 30) {
        println!("synthetic movie failed: {e}");
        return false;
    }
    let w = crate::capture::recording_preview::RecordingPreviewWindow::new(
        &mov,
        6.0,
        CGSize::new(1280.0, 720.0),
        CGRect::new(CGPoint::new(200.0, 200.0), CGSize::new(640.0, 360.0)),
    );
    w.debug_seek(2.5);
    match w.debug_content_view() {
        Some(v) => crate::app::selftest_m2::snapshot(&v, out),
        None => false,
    }
}

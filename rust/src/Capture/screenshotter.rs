//! Port of `Capture/Screenshotter.swift`.
//!
//! Codec half (M1): PNG / HEIC / TIFF through ImageIO (profiles kept), magic-
//! byte sniffing, high-quality downscale. Capture half (M3): one-shot shots
//! through ScreenCaptureKit — the image keeps the display's own color space
//! (Display P3 on most Macs) so nothing is re-encoded to sRGB. The async
//! `SCScreenshotManager` API is driven by completion handler and the result
//! hops to the main queue (§8.7: channels/runloop, no tokio).

use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFMutableData, CFNumber, CFRetained, CFString, CFType, CFURL,
    CFURLPathStyle,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage,
    CGInterpolationQuality,
};
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImageDestinationLossyCompressionQuality,
    kCGImageSourceShouldCacheImmediately,
};

/// `CGImageAlphaInfo.premultipliedLast` (the Swift contexts' bitmapInfo).
const PREMULTIPLIED_LAST: u32 = 1;

/// The ImageIO options dictionaries take `CFDictionary` with erased generics;
/// build typed and reinterpret. SAFETY: a CFDictionary is a CFDictionary —
/// the generic parameters are compile-time markers only.
fn to_opaque(d: CFRetained<CFDictionary<CFType, CFType>>) -> CFRetained<CFDictionary> {
    unsafe { CFRetained::cast_unchecked(d) }
}

/// PNG with the image's ICC profile embedded.
pub fn png_data(image: &CGImage) -> Option<Vec<u8>> {
    encode(image, "public.png", None)
}

/// HEIC at `quality` with the image's own color space kept (ImageIO embeds the
/// profile; sips would flatten it to sRGB).
pub fn heic_data(image: &CGImage, quality: f64) -> Option<Vec<u8>> {
    encode(image, "public.heic", Some(quality))
}

pub fn tiff_data(image: &CGImage) -> Option<Vec<u8>> {
    encode(image, "public.tiff", None)
}

fn encode(image: &CGImage, uti: &str, quality: Option<f64>) -> Option<Vec<u8>> {
    // SAFETY: plain ImageIO; the destination writes into `data`, which we copy
    // out before returning.
    unsafe {
        let data = CFMutableData::new(None, 0)?;
        let type_str = CFString::from_str(uti);
        let dest = CGImageDestination::with_data(&data, &type_str, 1, None)?;
        let opts = quality.map(|q| {
            let q = CFNumber::new_f64(q);
            let d = CFDictionary::<CFType, CFType>::from_slices(
                &[kCGImageDestinationLossyCompressionQuality.as_ref()],
                &[q.as_ref()],
            );
            to_opaque(d)
        });
        dest.add_image(image, opts.as_deref());
        if !dest.finalize() {
            return None;
        }
        let len = data.length() as usize;
        let ptr = data.byte_ptr();
        if ptr.is_null() || len == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr, len).to_vec())
    }
}

/// Any encoded image bytes (PNG / JPEG / TIFF / GIF / HEIC…) → PNG bytes; nil
/// when the bytes are not a picture.
pub fn png_data_from_image_bytes(d: &[u8]) -> Option<Vec<u8>> {
    if d.len() <= 16 {
        return None;
    }
    if d.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return Some(d.to_vec());
    }
    let b = &d[..12];
    let jpg = b[0] == 0xFF && b[1] == 0xD8;
    let tiff = (b[0] == 0x49 && b[1] == 0x49 && b[2] == 0x2A)
        || (b[0] == 0x4D && b[1] == 0x4D && b[2] == 0x00 && b[3] == 0x2A);
    let gif = b[0] == 0x47 && b[1] == 0x49 && b[2] == 0x46;
    let heic = b[4] == 0x66 && b[5] == 0x74 && b[6] == 0x79 && b[7] == 0x70;
    if !(jpg || tiff || gif || heic) {
        return None;
    }
    // Decode whatever it is, re-encode as PNG (same pixels, same profile).
    let cg = decode_any(d)?;
    png_data(&cg)
}

/// Decode any ImageIO-supported format to a CGImage.
pub fn decode_any(d: &[u8]) -> Option<CFRetained<CGImage>> {
    let data = CFData::from_bytes(d);
    // SAFETY: plain ImageIO.
    unsafe {
        let src = CGImageSource::with_data(&data, None)?;
        src.image_at_index(0, None)
    }
}

/// `Screenshotter.image(fromPNG:)` — the pasteboard/export decode path.
pub fn image_from_png(d: &[u8]) -> Option<CFRetained<CGImage>> {
    decode_any(d)
}

/// Decode from a file with `kCGImageSourceShouldCacheImmediately` (the
/// thumbnail warm path).
pub fn image_from_file(path: &std::path::Path) -> Option<CFRetained<CGImage>> {
    // SAFETY: plain ImageIO over a CFURL.
    unsafe {
        let cf_path = CFString::from_str(&path.to_string_lossy());
        let url = CFURL::with_file_system_path(
            None,
            Some(&cf_path),
            CFURLPathStyle::CFURLPOSIXPathStyle,
            false,
        )?;
        let src = CGImageSource::with_url(&url, None)?;
        let cache = CFBoolean::new(true);
        let opts = CFDictionary::<CFType, CFType>::from_slices(
            &[kCGImageSourceShouldCacheImmediately.as_ref()],
            &[cache.as_ref()],
        );
        let opts = to_opaque(opts);
        src.image_at_index(0, Some(&opts))
    }
}

/// Downscale for shelf thumbnails; keeps the color space.
pub fn thumbnail(image: &CGImage, max_pixels: i64) -> Option<CFRetained<CGImage>> {
    let w = CGImage::width(Some(image)) as i64;
    let h = CGImage::height(Some(image)) as i64;
    let k = (max_pixels as f64 / (w.max(h)) as f64).min(1.0);
    if k >= 1.0 {
        // Swift returns the same image; callers write it out unchanged.
        return Some(unsafe {
            CFRetained::retain(std::ptr::NonNull::new_unchecked(
                image as *const CGImage as *mut CGImage,
            ))
        });
    }
    let tw = (((w as f64) * k) as usize).max(1);
    let th = (((h as f64) * k) as usize).max(1);
    let space = CGImage::color_space(Some(image)).or_else(|| {
        // SAFETY: kCGColorSpaceSRGB is a stable extern constant.
        CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceSRGB }))
    })?;
    // SAFETY: `data` is null (the context allocates its own buffer).
    let ctx = unsafe { CGBitmapContextCreate(std::ptr::null_mut(), tw, th, 8, 0, Some(&space), PREMULTIPLIED_LAST) }?;
    CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::High);
    CGContext::draw_image(
        Some(&ctx),
        objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::new(0.0, 0.0),
            objc2_core_foundation::CGSize::new(tw as f64, th as f64),
        ),
        Some(image),
    );
    CGBitmapContextCreateImage(Some(&ctx))
}

/// Bytes to keep on disk for a screenshot, per the storage setting: (data, extension).
pub fn stored_image(png: &[u8], cg: &CGImage, store_heic: bool) -> (Vec<u8>, String) {
    if store_heic {
        if let Some(heic) = heic_data(cg, 0.9) {
            return (heic, "heic".into());
        }
    }
    (png.to_vec(), "png".into())
}

/// `CGImage::width` / `height` as one-call helpers for hot paths.
pub fn width(image: &CGImage) -> usize {
    CGImage::width(Some(image))
}

pub fn height(image: &CGImage) -> usize {
    CGImage::height(Some(image))
}

// MARK: ScreenCaptureKit (M3)

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_app_kit::NSScreen;
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCDisplay, SCScreenshotManager,
    SCStreamConfiguration, SCWindow,
};

use crate::capture::target::{CaptureTarget, ShareableSnapshot};

/// `kCVPixelFormatType_32BGRA` ('BGRA').
/// `kCVPixelFormatType_32BGRA` ('BGRA'), shared with the recorder/stream.
pub(crate) const PIXEL_FORMAT_BGRA_32: u32 = 0x4247_5241;

/// Pixels per point for the output size (`pixelScale`). The screen's backing
/// scale is what the user sees; `pointPixelScale` alone has come back as 1 on
/// some displays and produced half-resolution pictures (Swift comment kept).
pub fn pixel_scale(point_pixel_scale: f32, backing_scale: Option<f64>) -> f64 {
    (point_pixel_scale as f64).max(backing_scale.unwrap_or(1.0)).max(1.0)
}

/// The screen a target is measured against (`Screenshotter.screen(for:)`):
/// direct for display/region, max overlap for a window.
fn screen_for_target(target: &CaptureTarget, snapshot: &ShareableSnapshot) -> Option<Retained<NSScreen>> {
    match target {
        CaptureTarget::Display(d) | CaptureTarget::Region(d, _) => snapshot.screen_for_display(d),
        CaptureTarget::Window(w) => {
            let mtm = objc2::MainThreadMarker::new().expect("main thread");
            let r = crate::app::coordinates::cocoa_rect_from_cg(unsafe { w.frame() });
            let mut best: Option<Retained<NSScreen>> = None;
            let mut best_area = 0.0;
            for s in NSScreen::screens(mtm).iter() {
                let area = crate::app::coordinates::intersection_area(s.frame(), r);
                if area > best_area || best.is_none() {
                    best_area = area;
                    best = Some(s);
                }
            }
            best
        }
    }
}

/// `screen.colorSpace.cgColorSpace.name` — the display profile token handed
/// to SCStreamConfiguration so captures stay in the screen's own space.
pub fn screen_color_space_name(screen: &NSScreen) -> Option<CFRetained<CFString>> {
    let space = screen.colorSpace()?;
    let cg = space.CGColorSpace()?;
    CGColorSpace::name(Some(&cg))
}

/// Run `SCScreenshotManager.captureImage` and deliver the image on the main
/// thread (`completion`); `None` is Swift's thrown error path.
fn run_capture(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
    completion: Box<dyn FnOnce(Option<CFRetained<CGImage>>) + Send + 'static>,
) {
    let completion = std::sync::Arc::new(std::sync::Mutex::new(Some(completion)));
    let block = block2::RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        if !error.is_null() {
            let err = unsafe { &*error };
            eprintln!(
                "capture failed ({}): {}",
                err.code(),
                err.localizedDescription()
            );
        }
        let image = (!image.is_null()).then(|| unsafe {
            // SAFETY: non-null completion parameter, retained immediately.
            CFRetained::retain(std::ptr::NonNull::new_unchecked(image))
        });
        let cb = completion.lock().unwrap().take().expect("fires once");
        // SAFETY: the boxed image is reclaimed exactly once, on the main
        // thread; CGImage is immutable and safe to pass across the queue.
        let raw = Box::into_raw(Box::new(image)) as usize;
        crate::app::delegate::dispatch_main_async(Box::new(move || {
            let image = unsafe { Box::from_raw(raw as *mut Option<CFRetained<CGImage>>) };
            cb(*image);
        }));
    });
    // SAFETY: block is copied by SCK; the generated signature is matched.
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            config,
            Some(&*block),
        );
    }
}

/// `Screenshotter.capture(_:snapshot:)` — one-shot capture of a display /
/// display-region / single window. Completion fires on the main thread.
pub fn capture(
    target: CaptureTarget,
    snapshot: &ShareableSnapshot,
    completion: Box<dyn FnOnce(Option<CFRetained<CGImage>>) + Send + 'static>,
) {
    let filter: Retained<SCContentFilter> = match &target {
        CaptureTarget::Display(d) | CaptureTarget::Region(d, _) => unsafe {
            // SAFETY: plain SCK construction.
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                d,
                &NSArray::from_retained_slice(&snapshot.own_windows),
            )
        },
        CaptureTarget::Window(w) => unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), w)
        },
    };
    let cfg = unsafe { SCStreamConfiguration::new() };
    let screen = screen_for_target(&target, snapshot);
    let backing = screen.as_ref().map(|s| s.backingScaleFactor());
    let scale = pixel_scale(unsafe { filter.pointPixelScale() }, backing);
    let point_size = match &target {
        CaptureTarget::Region(_, r) => {
            unsafe {
                cfg.setSourceRect(*r);
            }
            r.size
        }
        _ => unsafe { filter.contentRect() }.size,
    };
    unsafe {
        cfg.setPixelFormat(PIXEL_FORMAT_BGRA_32);
        cfg.setCaptureResolution(SCCaptureResolutionType::Best);
        cfg.setShowsCursor(false);
        cfg.setIgnoreShadowsSingleWindow(true);
        cfg.setScalesToFit(false);
        cfg.setWidth((point_size.width * scale).round() as usize);
        cfg.setHeight((point_size.height * scale).round() as usize);
        if let Some(cg) = screen.as_ref().and_then(|s| screen_color_space_name(s)) {
            cfg.setColorSpaceName(&cg);
        }
    }
    run_capture(&filter, &cfg, completion);
}

/// `Screenshotter.captureDisplay(_:excluding:backingScale:colorSpaceName:)` —
/// the whole display, excluding just the given window ids (the picker's mask
/// windows), fetched fresh. Other Pastory windows, the shelf included, stay
/// in the picture; region/window shots are crops of this.
pub fn capture_display(
    display: &SCDisplay,
    excluding: &[u32],
    backing_scale: Option<f64>,
    color_space_name: Option<&CFString>,
    completion: Box<dyn FnOnce(Option<CFRetained<CGImage>>) + Send + 'static>,
) {
    // SAFETY: plain retain of a live object for the async hop.
    let display: Retained<SCDisplay> = unsafe {
        Retained::retain(display as *const SCDisplay as *mut SCDisplay)
            .expect("display is non-null")
    };
    let excluding: Vec<u32> = excluding.to_vec();
    let cs_name = color_space_name.map(|s| s.to_string());
    // The fetch completion must be `Send`: bundle the ObjC payload as a raw
    // box and reclaim it on the main thread (SC snapshots are immutable).
    type Ctx = (
        Retained<SCDisplay>,
        Vec<u32>,
        Option<String>,
        Option<f64>,
        Box<dyn FnOnce(Option<CFRetained<CGImage>>) + Send + 'static>,
    );
    let ctx = Box::into_raw(Box::new((display, excluding, cs_name, backing_scale, completion)))
        as usize;
    ShareableSnapshot::fetch(Box::new(move |snap| {
        let (display, excluding, cs_name, backing_scale, completion) =
            *unsafe { Box::from_raw(ctx as *mut Ctx) };
        let Some(snap) = snap else {
            completion(None);
            return;
        };
        let own: Vec<Retained<SCWindow>> = unsafe { snap.content.windows() }
            .iter()
            .filter(|w| excluding.contains(&unsafe { w.windowID() }))
            .collect();
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::from_retained_slice(&own),
            )
        };
        let cfg = unsafe { SCStreamConfiguration::new() };
        let scale = pixel_scale(unsafe { filter.pointPixelScale() }, backing_scale);
        let content = unsafe { filter.contentRect() };
        unsafe {
            cfg.setPixelFormat(PIXEL_FORMAT_BGRA_32);
            cfg.setCaptureResolution(SCCaptureResolutionType::Best);
            cfg.setShowsCursor(false);
            cfg.setScalesToFit(false);
            cfg.setWidth((content.size.width * scale).round() as usize);
            cfg.setHeight((content.size.height * scale).round() as usize);
            if let Some(name) = &cs_name {
                cfg.setColorSpaceName(&CFString::from_str(name));
            }
        }
        run_capture(&filter, &cfg, completion);
    }));
}

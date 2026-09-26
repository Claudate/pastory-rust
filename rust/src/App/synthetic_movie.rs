//! Port of `App/SyntheticMovie.swift`.
//!
//! Test helper: a short H.264 movie of a moving block, so GIF encoding (and
//! the MP4 writer selftest) can be checked without real screen recording.

use objc2::rc::Retained;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor,
    AVFileTypeMPEG4, AVVideoCodecKey, AVVideoCodecTypeH264, AVVideoHeightKey,
    AVVideoWidthKey,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGColor, CGColorSpace, CGContext};
use objc2_core_media::CMTime;
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

/// premultipliedFirst | byteOrder32Little (sRGB BGRA frame bitmaps).
pub(crate) const BITMAP_INFO: u32 = 2 | (2 << 12);

/// `kCVPixelFormatType_32BGRA` ('BGRA').
const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241;

fn num(i: isize) -> Retained<NSNumber> {
    NSNumber::numberWithInteger(i)
}

pub fn color_srgb(r: f64, g: f64, b: f64) -> objc2_core_foundation::CFRetained<CGColor> {
    CGColor::new_generic_rgb(r, g, b, 1.0)
}

/// MP4 writer defaults: the "media type video" settings + adaptor attrs.
pub fn video_input(
    mtm: objc2::MainThreadMarker,
    codec: &objc2_foundation::NSString,
    width: usize,
    height: usize,
) -> Retained<AVAssetWriterInput> {
    let codec_obj: &objc2::runtime::AnyObject =
        unsafe { &*(codec as *const NSString as *const objc2::runtime::AnyObject) };
    let wn = num(width as isize);
    let hn = num(height as isize);
    let w_obj: &objc2::runtime::AnyObject =
        unsafe { &*(objc2::rc::Retained::as_ptr(&wn) as *const objc2::runtime::AnyObject) };
    let h_obj: &objc2::runtime::AnyObject =
        unsafe { &*(objc2::rc::Retained::as_ptr(&hn) as *const objc2::runtime::AnyObject) };
    let settings = NSDictionary::<NSString, objc2::runtime::AnyObject>::from_slices(
        &[
            unsafe { AVVideoCodecKey }.expect("codec key"),
            unsafe { AVVideoWidthKey }.expect("width key"),
            unsafe { AVVideoHeightKey }.expect("height key"),
        ],
        &[codec_obj, w_obj, h_obj],
    );
    unsafe {
        AVAssetWriterInput::initWithMediaType_outputSettings(
            mtm.alloc(),
            objc2_av_foundation::AVMediaTypeVideo.expect("video media type"),
            Some(&settings),
        )
    }
}

/// BGRA attributes for the pixel buffer adaptor (`sourcePixelBufferAttributes`).
pub fn adaptor_attrs(
    width: usize,
    height: usize,
) -> Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> {
    let fmt = num(PIXEL_FORMAT_BGRA as isize);
    let w = num(width as isize);
    let h = num(height as isize);
    // kCVPixelBuffer* keys are CFString-typed; NSString is toll-free — the
    // references are upcast, not converted.
    let keys: [&NSString; 3] = [
        unsafe { &*(kCVPixelBufferPixelFormatTypeKey as *const objc2_core_foundation::CFString as *const NSString) },
        unsafe { &*(kCVPixelBufferWidthKey as *const objc2_core_foundation::CFString as *const NSString) },
        unsafe { &*(kCVPixelBufferHeightKey as *const objc2_core_foundation::CFString as *const NSString) },
    ];
    NSDictionary::from_slices(&keys, &[
        unsafe { &*(objc2::rc::Retained::as_ptr(&fmt) as *const objc2::runtime::AnyObject) },
        unsafe { &*(objc2::rc::Retained::as_ptr(&w) as *const objc2::runtime::AnyObject) },
        unsafe { &*(objc2::rc::Retained::as_ptr(&h) as *const objc2::runtime::AnyObject) },
    ])
}

/// A writer input + adaptor pair already configured for BGRA output.
pub fn make_adaptor(
    mtm: objc2::MainThreadMarker,
    writer: &AVAssetWriter,
    codec: &objc2_foundation::NSString,
    width: usize,
    height: usize,
) -> Option<(Retained<AVAssetWriterInput>, Retained<AVAssetWriterInputPixelBufferAdaptor>)> {
    let input = video_input(mtm, codec, width, height);
    unsafe {
        input.setExpectsMediaDataInRealTime(true);
    }
    if !unsafe { writer.canAddInput(&input) } {
        return None;
    }
    unsafe { writer.addInput(&input) };
    let attrs = adaptor_attrs(width, height);
    // SAFETY: same dictionary; only the value type is re-marked.
    let opaque: Retained<NSDictionary<NSString>> = unsafe { Retained::cast_unchecked(attrs) };
    let adaptor = unsafe {
        AVAssetWriterInputPixelBufferAdaptor::initWithAssetWriterInput_sourcePixelBufferAttributes(
            mtm.alloc::<AVAssetWriterInputPixelBufferAdaptor>(),
            &input,
            Some(&opaque),
        )
    };
    Some((input, adaptor))
}

/// One BGRA frame from the adaptor's pool, painted by `f`.
pub fn paint_frame(
    adaptor: &AVAssetWriterInputPixelBufferAdaptor,
    width: usize,
    height: usize,
    f: impl FnOnce(&CGContext, usize, usize),
) -> Option<objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>> {
    let pool = unsafe { adaptor.pixelBufferPool() }?;
    let mut raw: *mut objc2_core_video::CVPixelBuffer = std::ptr::null_mut();
    #[allow(deprecated)]
    let status = unsafe { objc2_core_video::CVPixelBufferPoolCreatePixelBuffer(
        None,
        &pool,
        std::ptr::NonNull::new(&mut raw).expect("out pointer"),
    ) };
    if status != 0 || raw.is_null() {
        return None;
    }
    // SAFETY: the pool's Create rule makes this buffer +1 owned.
    let pb: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer> = unsafe {
        objc2_core_foundation::CFRetained::from_raw(std::ptr::NonNull::new_unchecked(raw))
    };
    unsafe {
        objc2_core_video::CVPixelBufferLockBaseAddress(&pb, objc2_core_video::CVPixelBufferLockFlags::empty());
    }
    let base = objc2_core_video::CVPixelBufferGetBaseAddress(&pb);
    let stride = objc2_core_video::CVPixelBufferGetBytesPerRow(&pb);
    // sRGB, premultipliedFirst | byteOrder32Little (BGRA).
    let space = CGColorSpace::with_name(Some(unsafe {
        objc2_core_graphics::kCGColorSpaceSRGB
    }))
    .expect("sRGB");
    let ctx = unsafe { CGBitmapContextCreate(base, width, height, 8, stride, Some(&space), BITMAP_INFO) };
    if let Some(ctx) = ctx {
        f(&ctx, width, height);
    }
    unsafe {
        objc2_core_video::CVPixelBufferUnlockBaseAddress(&pb, objc2_core_video::CVPixelBufferLockFlags::empty());
    }
    Some(pb)
}

/// `finishWritingWithCompletionHandler` on a runloop pump; true when the
/// writer is .completed + the completion callback fired.
pub fn finish_writer(w: &AVAssetWriter) -> bool {
    let slot: std::sync::Arc<std::sync::Mutex<bool>> =
        std::sync::Arc::new(std::sync::Mutex::new(false));
    let slot2 = slot.clone();
    let block = block2::RcBlock::new(move || {
        *slot2.lock().unwrap() = true;
    });
    unsafe {
        w.finishWritingWithCompletionHandler(&*block);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    while std::time::Instant::now() < deadline && !*slot.lock().unwrap() {
        let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.02);
        let mode: &'static objc2_foundation::NSRunLoopMode =
            unsafe { objc2_foundation::NSDefaultRunLoopMode };
        let _ = run_loop.runMode_beforeDate(mode, &limit);
    }
    unsafe {
        *slot.lock().unwrap() && w.status() == objc2_av_foundation::AVAssetWriterStatus::Completed
    }
}

/// `SyntheticMovie.write(to:size:seconds:fps:)`.
pub fn write(path: &std::path::Path, size: CGSize, seconds: f64, fps: i64) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let (w, h) = (size.width as usize, size.height as usize);
    let mtm = objc2::MainThreadMarker::new().expect("main thread");
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let Some(fty) = (unsafe { AVFileTypeMPEG4 }) else {
        return Err("no mp4 file type".into());
    };
    let writer = unsafe { AVAssetWriter::initWithURL_fileType_error(mtm.alloc(), &url, fty) }
        .map_err(|e| e.localizedDescription().to_string())?;
    let Some(codec) = (unsafe { AVVideoCodecTypeH264 }) else {
        return Err("no h264 codec".into());
    };
    let Some((input, adaptor)) = make_adaptor(mtm, &writer, codec, w, h) else {
        return Err("cannot add video input".into());
    };
    if !unsafe { writer.startWriting() } {
        return Err("startWriting failed".into());
    }
    unsafe {
        writer.startSessionAtSourceTime(CMTime {
            value: 0,
            timescale: fps as i32,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        })
    };
    let total = (seconds * fps as f64) as usize;
    for i in 0..total {
        while !unsafe { input.isReadyForMoreMediaData() } {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let Some(pb) = paint_frame(&adaptor, w, h, |ctx, w2, h2| {
            CGContext::set_fill_color_with_color(
                Some(ctx),
                Some(&color_srgb(0.95, 0.95, 0.97)),
            );
            CGContext::fill_rect(
                Some(ctx),
                CGRect::new(CGPoint::ZERO, CGSize::new(w2 as f64, h2 as f64)),
            );
            CGContext::set_fill_color_with_color(
                Some(ctx),
                Some(&color_srgb(0.56, 0.42, 1.0)),
            );
            let x = (i as f64 / total as f64) * (w2 as f64 - 80.0);
            CGContext::fill_rect(
                Some(ctx),
                CGRect::new(
                    CGPoint::new(x, h2 as f64 / 2.0 - 40.0),
                    CGSize::new(80.0, 80.0),
                ),
            );
        }) else {
            return Err("pool create failed".into());
        };
        let _ = unsafe { adaptor.appendPixelBuffer_withPresentationTime(&pb, CMTime {
            value: i as i64,
            timescale: fps as i32,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        }) };
        let _ = pb;
    }
    unsafe { input.markAsFinished() };
    if !finish_writer(&writer) {
        return Err("finishWriting failed".into());
    }
    Ok(())
}


/// A reader-only settings dict: just the pixel format (AVAssetReader
/// wants nothing else from us; size comes from the track).
pub fn pixel_format_only() -> Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> {
    let fmt = num(PIXEL_FORMAT_BGRA as isize);
    let key: &NSString =
        unsafe { &*(kCVPixelBufferPixelFormatTypeKey as *const objc2_core_foundation::CFString as *const NSString) };
    let val: &objc2::runtime::AnyObject =
        unsafe { &*(objc2::rc::Retained::as_ptr(&fmt) as *const objc2::runtime::AnyObject) };
    NSDictionary::from_slices(&[key], &[val])
}


/// Discard a never-started writer (`cancel` paths from stop(): no session,
/// nothing to finish). Swift: `writer.cancelWriting()`.
pub fn cancel_writer(w: &AVAssetWriter) {
    unsafe {
        let _: () = objc2::msg_send![w, cancelWriting];
    }
}

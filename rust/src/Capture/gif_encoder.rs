//! Port of `Capture/GIFEncoder.swift`.
//!
//! MP4 → GIF with a size budget that grows with length: start from the
//! recording's pixels (long edge ≤ 1280, 10 fps) and scale down / drop
//! frames until the estimate fits. Short clips stay sharp, long ones stay
//! sendable.
//!
//! Decode side keeps AVAssetReader (§3.2); encode side is the `gif` crate
//! with the budget ladder / retry loop one-for-one from Swift.

use objc2::rc::Retained;
use objc2_core_foundation::CGSize;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSString, NSURL};

/// ≤10 s: 6 MB · ≤30 s: 10 MB · ≤60 s: 15 MB · longer: 20 MB
/// (`budget(for:)`).
pub fn budget(duration: f64) -> f64 {
    let mb = if duration <= 10.0 {
        6.0
    } else if duration <= 30.0 {
        10.0
    } else if duration <= 60.0 {
        15.0
    } else {
        20.0
    };
    mb * 1024.0 * 1024.0
}

pub const BYTES_PER_PIXEL_FRAME: f64 = 0.15;
pub const MAX_EDGE: f64 = 1280.0;
pub const MIN_EDGE: f64 = 640.0;

pub struct Plan {
    pub size: CGSize,
    pub fps: f64,
}

/// `plan(natural:duration:shrink:)` — the down-stepping ladder.
pub fn plan(natural: CGSize, duration: f64, shrink: f64) -> Plan {
    let budget_bytes = budget(duration);
    let mut k = (MAX_EDGE / natural.width.max(natural.height)).min(1.0) * shrink;
    let mut fps = 10.0;
    let estimate = |fps: f64, kk: f64| -> f64 {
        duration * fps * (natural.width * kk) * (natural.height * kk) * BYTES_PER_PIXEL_FRAME
    };
    if estimate(fps, k) > budget_bytes {
        fps = 8.0;
    }
    if estimate(fps, k) > budget_bytes {
        k *= (budget_bytes / estimate(fps, k)).sqrt();
        let min_k = MIN_EDGE / natural.width.max(natural.height);
        k = k.min(1.0).max(min_k.min(1.0));
    }
    if estimate(fps, k) > budget_bytes {
        fps = 6.0;
    }
    Plan {
        size: CGSize::new(
            (natural.width * k).round(),
            (natural.height * k).round(),
        ),
        fps,
    }
}

/// `duration(movie:)` — AVURLAsset load(.duration).
pub fn duration(movie: &std::path::Path) -> f64 {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&movie.to_string_lossy()));
    let asset = unsafe { objc2_av_foundation::AVURLAsset::URLAssetWithURL_options(&url, None) };
    let d = unsafe { asset.duration() };
    if d.timescale == 0 {
        return 0.0;
    }
    (d.value as f64) / (d.timescale as f64)
}

/// First frame, for thumbnails (`poster(movie:)`).
pub fn poster(movie: &std::path::Path) -> Option<Retained<CGImage>> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&movie.to_string_lossy()));
    let asset = unsafe { objc2_av_foundation::AVURLAsset::URLAssetWithURL_options(&url, None) };
    let gen = unsafe { objc2_av_foundation::AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset) };
    unsafe {
        gen.setAppliesPreferredTrackTransform(true);
    }
    unsafe {
        gen.setRequestedTimeToleranceBefore(objc2_core_media::CMTime {
            value: 0,
            timescale: 600,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        });
    }
    unsafe {
        gen.setRequestedTimeToleranceAfter(objc2_core_media::CMTime {
            value: 300,
            timescale: 600,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        });
    }
    let mut actual = objc2_core_media::CMTime {
        value: 0,
        timescale: 600,
        flags: objc2_core_media::CMTimeFlags::Valid,
        epoch: 0,
    };
    #[allow(deprecated)]
    match unsafe {
        gen.copyCGImageAtTime_actualTime_error(
            objc2_core_media::CMTime {
                value: 60,
                timescale: 600,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            },
            &mut actual,
        )
    } {
        Ok(img) => Some(img),
        Err(_) => None,
    }
}
// MARK: encode

use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderTrackOutput, AVMediaTypeVideo,
};
use objc2_foundation::NSDictionary;


/// `encode(movie:to:progress:)` — encode once; if the file overshoots the
/// budget by more than a quarter, shrink and encode again.
pub fn encode(movie: &std::path::Path, to: &std::path::Path, progress: Option<&dyn Fn(f64)>) -> Result<(), String> {
    encode_pass(movie, to, 1.0, progress)?;
    let file_bytes = std::fs::metadata(to).map(|m| m.len()).unwrap_or(0) as f64;
    let budget_bytes = budget(duration(movie));
    if file_bytes > budget_bytes * 1.25 {
        let shrink = (budget_bytes / file_bytes).sqrt().max(0.4);
        encode_pass(movie, to, shrink, progress)?;
    }
    Ok(())
}

/// One pass of the encoder (`encodePass`).
fn encode_pass(movie: &std::path::Path, to: &std::path::Path, shrink: f64, progress: Option<&dyn Fn(f64)>) -> Result<(), String> {
    let _ = std::fs::remove_file(to);
    let mtm = objc2::MainThreadMarker::new().expect("main thread");
    let url = NSURL::fileURLWithPath(&NSString::from_str(&movie.to_string_lossy()));
    let asset = unsafe { objc2_av_foundation::AVURLAsset::URLAssetWithURL_options(&url, None) };
    let duration = duration(movie);
    #[allow(deprecated)]
    let tracks = unsafe { asset.tracksWithMediaType(AVMediaTypeVideo.expect("video media type")) };
    let Some(track) = tracks.firstObject() else {
        return Err("no video track (gif 1)".into());
    };
    let natural = unsafe { track.naturalSize() };
    let plan = plan(natural, duration, shrink);
    let (w_px, h_px) = (plan.size.width as usize, plan.size.height as usize);

    // Reader: outputSettings = just the pixel format (same as Swift).
    let out_settings: Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> =
        crate::app::synthetic_movie::pixel_format_only();
    let reader = match unsafe { AVAssetReader::initWithAsset_error(mtm.alloc(), &asset) } {
        Ok(r) => r,
        Err(e) => return Err(format!("reader init: {}", e.localizedDescription())),
    };
    let output = unsafe {
        AVAssetReaderTrackOutput::initWithTrack_outputSettings(mtm.alloc(), &track, Some(&out_settings))
    };
    unsafe { output.setAlwaysCopiesSampleData(false) };
    unsafe { reader.addOutput(&output) };
    if !unsafe { reader.startReading() } {
        let why = unsafe { reader.error() }
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| "reader failed (gif 2)".into());
        return Err(why);
    }

    // gif crate encoder (LZW, wait-then-palettize frames like ImageIO would
    // have emitted; budget ladder same as Swift).
    let mut out_buf: Vec<u8> = Vec::new();
    let count = 1usize.max((duration * plan.fps) as usize);
    let mut encoder = gif::Encoder::new(&mut out_buf, w_px as u16, h_px as u16, &[])
        .map_err(|e| format!("gif encoder: {e}"))?;
    encoder
        .set_repeat(gif::Repeat::Infinite)
        .map_err(|e| format!("gif repeat: {e}"))?;
    let delay_cs = ((100.0 / plan.fps).round() as u16).max(1);

    let mut next_time = 0.0f64;
    let mut written = 0usize;
    let mut last_rgba: Option<Vec<u8>> = None;
    while let Some(sb) = unsafe { output.copyNextSampleBuffer() } {
        let Some(pb) = (unsafe { sb.image_buffer() }) else { continue };
        let pts = seconds_of(unsafe { sb.presentation_time_stamp() });
        if pts + 1e-6 < next_time {
            continue;
        }
        let Some(rgba) = converted_rgba(&pb, w_px, h_px) else { continue };
        while next_time <= pts && written < count {
            let mut buffer = rgba.clone();
            let mut frame = gif::Frame::from_rgba_speed(w_px as u16, h_px as u16, &mut buffer, 10);
            frame.delay = delay_cs;
            encoder.write_frame(&frame).map_err(|e| format!("gif frame: {e}"))?;
            written += 1;
            next_time += 1.0 / plan.fps;
        }
        last_rgba = Some(rgba);
        if let Some(p) = progress {
            p(written as f64 / count as f64);
        }
        if written >= count {
            break;
        }
    }
    // A slow source (static screen) yields fewer frames than the clock wants;
    // repeat to keep timing.
    while written < count {
        let Some(mut buffer) = last_rgba.clone() else { break };
        let mut frame = gif::Frame::from_rgba_speed(w_px as u16, h_px as u16, &mut buffer, 10);
        frame.delay = delay_cs;
        encoder.write_frame(&frame).map_err(|e| format!("gif frame: {e}"))?;
        written += 1;
    }
    drop(encoder);
    std::fs::write(to, &out_buf).map_err(|e| format!("write gif: {e}"))?;
    Ok(())
}



fn seconds_of(t: objc2_core_media::CMTime) -> f64 {
    if t.timescale == 0 {
        0.0
    } else {
        t.value as f64 / t.timescale as f64
    }
}


/// One decoded frame as RGBA bytes at the target size (nearest sampling;
/// the screen steered to this format anyway — the loss on text-edge is what
/// the GIF codec budget is for).
fn converted_rgba(
    pb: &objc2_core_video::CVPixelBuffer,
    w_px: usize,
    h_px: usize,
) -> Option<Vec<u8>> {
    unsafe { objc2_core_video::CVPixelBufferLockBaseAddress(pb, objc2_core_video::CVPixelBufferLockFlags::ReadOnly) };
    let base = objc2_core_video::CVPixelBufferGetBaseAddress(pb) as *const u8;
    let stride = objc2_core_video::CVPixelBufferGetBytesPerRow(pb);
    let bw = objc2_core_video::CVPixelBufferGetWidth(pb);
    let bh = objc2_core_video::CVPixelBufferGetHeight(pb);
    let mut out = vec![0u8; w_px * h_px * 4];
    for y in 0..h_px {
        let sy = (y * bh) / h_px;
        for x in 0..w_px {
            let sx = (x * bw) / w_px;
            if !base.is_null() && sy < bh && sx < bw {
                let src = unsafe { base.add(sy * stride + sx * 4) } ;
                let dst = y * w_px * 4 + x * 4;
                unsafe {
                    out[dst + 0] = *src.add(2);
                    out[dst + 1] = *src.add(1);
                    out[dst + 2] = *src.add(0);
                    out[dst + 3] = *src.add(3);
                }
            }
        }
    }
    unsafe { objc2_core_video::CVPixelBufferUnlockBaseAddress(pb, objc2_core_video::CVPixelBufferLockFlags::ReadOnly) };
    Some(out)
}

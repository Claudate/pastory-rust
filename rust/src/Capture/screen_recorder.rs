//! Port of `Capture/ScreenRecorder.swift`.
//!
//! SCStream frames → our own AVAssetWriter, so the bitrate is decided while
//! recording (no second pass). Cursor included, no audio: this is "frame a
//! region, record a short demo, send it", not a production recorder.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2_core_foundation::CGSize;
use objc2_core_media::CMTime;
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

pub const FPS: i32 = 30;
/// Kept only for the self-test comparison; see `bitrate` for why it is not
/// the default.
pub const QUALITY: f64 = 0.72;

/// `ScreenRecorder.bitrate(width:height:hevc:)` — average-bitrate target:
/// bits per pixel per frame, 0.1 for H.264 / 0.065 for HEVC, clamped to
/// 3–30 Mbps.
pub fn bitrate(width: usize, height: usize, hevc: bool) -> i64 {
    let bpp = if hevc { 0.065 } else { 0.1 };
    let bps = (width * height) as f64 * FPS as f64 * bpp;
    bps.max(3_000_000.0).min(30_000_000.0) as i64
}

/// Even dimension for the encoder (`evened`): at least 2, odd → one down.
fn evened(v: f64) -> usize {
    let n = 2usize.max(v.round() as usize);
    if n % 2 == 0 {
        n
    } else {
        n - 1
    }
}

fn num(i: isize) -> Retained<NSNumber> {
    NSNumber::numberWithInteger(i)
}

fn obj_n(n: &NSNumber) -> &objc2::runtime::AnyObject {
    unsafe { &*(n as *const NSNumber as *const objc2::runtime::AnyObject) }
}

fn obj(o: &NSString) -> &objc2::runtime::AnyObject {
    unsafe { &*(o as *const NSString as *const objc2::runtime::AnyObject) }
}

/// The video-settings statics are `Option<&'static NSString>`.
#[inline]
fn k(s: Option<&'static NSString>) -> &'static NSString {
    s.expect("video settings static")
}

use objc2_av_foundation::{
    AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor,
    AVVideoAllowFrameReorderingKey, AVVideoAverageBitRateKey, AVVideoCodecKey,
    AVVideoCodecTypeH264, AVVideoCodecTypeHEVC, AVVideoCompressionPropertiesKey,
    AVVideoExpectedSourceFrameRateKey, AVVideoHeightKey,
    AVVideoMaxKeyFrameIntervalKey, AVVideoProfileLevelH264HighAutoLevel,
    AVVideoProfileLevelKey, AVVideoQualityKey, AVVideoWidthKey,
};

#[derive(Clone)]
pub struct WriterParts {
    pub writer: Retained<objc2_av_foundation::AVAssetWriter>,
    pub input: Retained<AVAssetWriterInput>,
    pub adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
}

/// `ScreenRecorder.makeWriter(url:width:height:hevc:constantQuality:)` —
/// writer + input + adaptor, already started.
pub fn make_writer(
    path: &std::path::Path,
    width: usize,
    height: usize,
    hevc: bool,
    constant_quality: bool,
) -> Result<WriterParts, String> {
    let _ = std::fs::remove_file(path);
    let mtm = objc2::MainThreadMarker::new().expect("main thread");
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let Some(fty) = (unsafe { objc2_av_foundation::AVFileTypeMPEG4 }) else {
        return Err("no mp4 file type".into());
    };
    let writer = unsafe {
        objc2_av_foundation::AVAssetWriter::initWithURL_fileType_error(mtm.alloc(), &url, fty)
    }
    .map_err(|e| e.localizedDescription().to_string())?;
    let codec = if hevc {
        unsafe { AVVideoCodecTypeHEVC }.expect("hevc codec")
    } else {
        unsafe { AVVideoCodecTypeH264 }.expect("h264 codec")
    };

    // Compression properties (codec settings from the Swift table).
    let reorder: Retained<objc2::runtime::AnyObject> = unsafe {
        objc2::msg_send![objc2::class!(NSNumber), numberWithBool: false]
    };
    let expected_rate = num(FPS as isize);
    let keyframes = num((FPS * 2) as isize);
    let props = if constant_quality {
        let q: Retained<objc2::runtime::AnyObject> = unsafe {
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: QUALITY]
        };
        NSDictionary::<NSString, objc2::runtime::AnyObject>::from_slices(
            &[
                k(unsafe { AVVideoExpectedSourceFrameRateKey }),
                k(unsafe { AVVideoMaxKeyFrameIntervalKey }),
                k(unsafe { AVVideoAllowFrameReorderingKey }),
                k(unsafe { AVVideoQualityKey }),
            ],
            &[obj_n(&expected_rate), obj_n(&keyframes), &*reorder, &*q],
        )
    } else {
        let rate = num(bitrate(width, height, hevc) as isize);
        let base = [
            k(unsafe { AVVideoExpectedSourceFrameRateKey }),
            k(unsafe { AVVideoMaxKeyFrameIntervalKey }),
            k(unsafe { AVVideoAllowFrameReorderingKey }),
            k(unsafe { AVVideoAverageBitRateKey }),
        ];
        let base_vals: Vec<&objc2::runtime::AnyObject> = vec![
            obj_n(&expected_rate),
            obj_n(&keyframes),
            &*reorder,
            obj_n(&rate),
        ];
        if !hevc {
            let profile_obj = obj(unsafe {
                AVVideoProfileLevelH264HighAutoLevel.expect("h264 profile")
            });
            let mut keys = base.to_vec();
            keys.push(k(unsafe { AVVideoProfileLevelKey }));
            let mut vals = base_vals.clone();
            vals.push(profile_obj);
            NSDictionary::<NSString, objc2::runtime::AnyObject>::from_slices(&keys, &vals)
        } else {
            NSDictionary::<NSString, objc2::runtime::AnyObject>::from_slices(&base, &base_vals)
        }
    };
    let props_val: &objc2::runtime::AnyObject = unsafe {
        &*(objc2::rc::Retained::as_ptr(&props) as *const objc2::runtime::AnyObject)
    };
    let settings = NSDictionary::<NSString, objc2::runtime::AnyObject>::from_slices(
        &[
            k(unsafe { AVVideoCodecKey }),
            k(unsafe { AVVideoWidthKey }),
            k(unsafe { AVVideoHeightKey }),
            k(unsafe { AVVideoCompressionPropertiesKey }),
        ],
        &[
            obj(codec),
            obj_n(&num(width as isize)),
            obj_n(&num(height as isize)),
            props_val,
        ],
    );
    let input = unsafe {
        AVAssetWriterInput::initWithMediaType_outputSettings(
            mtm.alloc(),
            objc2_av_foundation::AVMediaTypeVideo.expect("video media type"),
            Some(&settings),
        )
    };
    unsafe {
        input.setExpectsMediaDataInRealTime(true);
    }
    if !unsafe { writer.canAddInput(&input) } {
        if constant_quality {
            return make_writer(path, width, height, hevc, false);
        }
        return Err("cannot add video input".into());
    }
    unsafe { writer.addInput(&input) };
    let attrs = crate::app::synthetic_movie::adaptor_attrs(width, height);
    let opaque: Retained<NSDictionary<NSString>> = unsafe { Retained::cast_unchecked(attrs) };
    let adaptor = unsafe {
        AVAssetWriterInputPixelBufferAdaptor::initWithAssetWriterInput_sourcePixelBufferAttributes(
            mtm.alloc(),
            &input,
            Some(&opaque),
        )
    };
    if !unsafe { writer.startWriting() } {
        return Err("startWriting failed".into());
    }
    Ok(WriterParts {
        writer,
        input,
        adaptor,
    })
}

// MARK: the stream (SCStream → AVAssetWriterInputPixelBufferAdaptor)

use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_core_media::CMSampleBuffer;
use objc2_screen_capture_kit::{
    SCContentFilter, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamOutput, SCStreamOutputType, SCCaptureResolutionType,
    SCWindow,
};

use crate::capture::target::CaptureTarget;

/// Stream config for a target (`start(target:…)`-time config block).
pub fn make_stream_config(
    target: &crate::capture::target::CaptureTarget,
    excluding: &[Retained<SCWindow>],
    backing_scale: Option<f64>,
) -> Option<(Retained<SCContentFilter>, Retained<SCStreamConfiguration>, CGSize)> {
    let filter: Retained<SCContentFilter> = match target {
        crate::capture::target::CaptureTarget::Display(d)
        | crate::capture::target::CaptureTarget::Region(d, _) => unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                d,
                &objc2_foundation::NSArray::from_retained_slice(excluding),
            )
        },
        crate::capture::target::CaptureTarget::Window(w) => unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(
                SCContentFilter::alloc(),
                w,
            )
        },
    };
    let cfg = unsafe { SCStreamConfiguration::new() };
    let scale = crate::capture::screenshotter::pixel_scale(unsafe { filter.pointPixelScale() }, backing_scale);
    let point_size = match target {
        crate::capture::target::CaptureTarget::Region(_, r) => {
            unsafe {
                cfg.setSourceRect(*r);
            }
            r.size
        }
        _ => unsafe { filter.contentRect() }.size,
    };
    let (w, h) = (evened(point_size.width * scale), evened(point_size.height * scale));
    unsafe {
        cfg.setMinimumFrameInterval(CMTime {
            value: 1,
            timescale: FPS,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        });
        cfg.setPixelFormat(crate::capture::screenshotter::PIXEL_FORMAT_BGRA_32);
        cfg.setQueueDepth(6);
        cfg.setScalesToFit(false);
        cfg.setCaptureResolution(SCCaptureResolutionType::Best);
        cfg.setShowsCursor(true);
        cfg.setCapturesAudio(false);
        cfg.setWidth(w);
        cfg.setHeight(h);
    }
    Some((filter, cfg, CGSize::new(w as f64, h as f64)))
}

/// The recorder's whole lifecycle is main-thread; frames arrive on the
/// capture queue and each carries a raw-boxed CMTime + pooled buffer into the
/// writer (same shape as the Swift serial queue version).
pub struct ScreenRecorderIvars {
    stream: RefCell<Option<Retained<SCStream>>>,
    parts: RefCell<Option<WriterParts>>,
    session_started: Cell<bool>,
    last_pts_value: Cell<i64>,
    last_pts_timescale: Cell<i32>,
    last_frame: RefCell<Option<objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>>>,
    failed: Cell<bool>,
    on_failure: RefCell<Option<Box<dyn Fn(String) + 'static>>>,
    // The queue behind didOutputSampleBuffer.
    stopped: Cell<bool>,
    /// The capturer-measured output pixel size (recorder.pixelSize).
    pub pixel_size: Cell<CGSize>,
}

impl Default for ScreenRecorderIvars {
    fn default() -> Self {
        Self {
            stream: RefCell::new(None),
            parts: RefCell::new(None),
            session_started: Cell::new(false),
            last_pts_value: Cell::new(0),
            last_pts_timescale: Cell::new(FPS),
            last_frame: RefCell::new(None),
            failed: Cell::new(false),
            on_failure: RefCell::new(None),
            stopped: Cell::new(true),
            pixel_size: Cell::new(CGSize::ZERO),
        }
    }
}

define_class!(
    // SAFETY: NSObject with the two SCK protocol selectors registered
    // informally (they are both @optional in Objective-C).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ScreenRecorderIvars]
    pub struct ScreenRecorder;

    unsafe impl NSObjectProtocol for ScreenRecorder {}

    impl ScreenRecorder {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output_sample_buffer_of_type(
            &self,
            _stream: &AnyObject,
            sample_buffer: &CMSampleBuffer,
            out_type: SCStreamOutputType,
        ) {
            self.on_frame(sample_buffer, out_type);
        }

        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop_with_error(&self, _stream: &AnyObject, error: &AnyObject) {
            let desc: Retained<objc2_foundation::NSString> = unsafe {
                objc2::msg_send![error, localizedDescription]
            };
            if !self.ivars().failed.replace(true) {
                self.fail(&desc.to_string());
            }
        }
    }
);

impl ScreenRecorder {
    /// `start(target:excluding:backingScale:outputURL:)` — build filter +
    /// config + writer, attach the output, start capture (the completion is
    /// the async continue point; swift's `await s.startCapture()`).
    pub fn start(
        target: &CaptureTarget,
        excluding: &[Retained<SCWindow>],
        backing_scale: Option<f64>,
        output_path: &std::path::Path,
        on_failure: Box<dyn Fn(String) + 'static>,
        on_started: Box<dyn FnOnce(Result<(), String>) + 'static>,
    ) -> Result<Retained<ScreenRecorder>, String> {
        let recorder = ScreenRecorder::new_recorder();
        *recorder.ivars().on_failure.borrow_mut() = Some(on_failure);
        let Some((filter, cfg, pixel_size)) = make_stream_config(target, excluding, backing_scale) else {
            let on_started = on_started;
            on_started(Err("stream config failed".into()));
            return Err("stream config failed".into());
        };
        recorder.ivars().pixel_size.set(pixel_size);
        recorder.ivars().pixel_size.set(pixel_size);
        let parts = make_writer(
            output_path,
            pixel_size.width as usize,
            pixel_size.height as usize,
            crate::app::preferences::Preferences::shared().record_hevc(),
            false,
        )?;
        // SCStream(filter:configuration:delegate:) + the queue ours arrive on.
        let proto: &ProtocolObject<dyn SCStreamDelegate> =
            unsafe { &*(&*recorder as *const ScreenRecorder as *const ProtocolObject<dyn SCStreamDelegate>) };
        let proto_out: &ProtocolObject<dyn SCStreamOutput> =
            unsafe { &*(&*recorder as *const ScreenRecorder as *const ProtocolObject<dyn SCStreamOutput>) };
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                objc2::AllocAnyThread::alloc(),
                &filter,
                &cfg,
                Some(proto),
            )
        };
        let _ = unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                proto_out,
                objc2_screen_capture_kit::SCStreamOutputType::Screen,
                None,
            )
            .map_err(|e| e.localizedDescription().to_string())?
        };
        // (recorder, on_started) crosses the completion hop as one raw box
        // (Swift's `await s.startCapture()` continue point).
        type Hop = (Retained<ScreenRecorder>, Box<dyn FnOnce(Result<(), String>) + 'static>);
        let raw = Box::into_raw(Box::new((recorder.clone(), on_started))) as usize;
        let block = block2::RcBlock::new(move |error: *mut objc2_foundation::NSError| {
            let (recorder, on_started) = *unsafe { Box::from_raw(raw as *mut Hop) };
            if error.is_null() {
                recorder.ivars().stopped.set(false);
                on_started(Ok(()));
            } else {
                let err = unsafe { &*error };
                on_started(Err(err.localizedDescription().to_string()));
            }
        });
        *recorder.ivars().stream.borrow_mut() = Some(stream.clone());
        *recorder.ivars().parts.borrow_mut() = Some(parts);
        unsafe {
            stream.startCaptureWithCompletionHandler(Some(&*block));
        }
        Ok(recorder)
    }

    fn new_recorder() -> Retained<ScreenRecorder> {
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<ScreenRecorder>().set_ivars(ScreenRecorderIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    // MARK: Frames

    /// `stream(_:didOutputSampleBuffer:of:)` — Swift's handler verbatim:
    /// complete frames only, session on the first one, append-or-drop when
    /// the encoder lags.
    fn on_frame(&self, sb: &CMSampleBuffer, out_type: SCStreamOutputType) {
        if out_type != objc2_screen_capture_kit::SCStreamOutputType::Screen {
            return;
        }
        if !unsafe { sb.is_valid() } {
            return;
        }
        let parts_owned = self.ivars().parts.borrow().clone();
        let Some(parts) = parts_owned.as_ref() else {
            return;
        };
        // Only complete frames; SCK also delivers idle / blank ones.
        let swift_complete: Option<i64> = (|| {
            let arr = unsafe { sb.sample_attachments_array(false) }?;
            // The elements are opaque-typed in the binding; result of SCK is
            // always the frame-info dict, so re-mark the element type.
            let arr:
                objc2_core_foundation::CFRetained<objc2_core_foundation::CFArray<objc2_core_foundation::CFType>> =
                unsafe { objc2_core_foundation::CFRetained::cast_unchecked(arr) };
            let first = arr.get(0)?;
            let dict = unsafe {
                objc2_core_foundation::CFRetained::cast_unchecked::<
                    objc2_core_foundation::CFDictionary<
                        objc2_core_foundation::CFString,
                        objc2_core_foundation::CFType,
                    >,
                >(first)
            };
            let key = objc2_core_foundation::CFString::from_static_str("status");
            let value: objc2_core_foundation::CFRetained<objc2_core_foundation::CFType> = dict.get(&key)?;
            let number = unsafe {
                objc2_core_foundation::CFRetained::cast_unchecked::<objc2_core_foundation::CFNumber>(value)
            };
            number.as_i64()
        })();
        // Swift: `status != .complete` → drop.
        if let Some(st) = swift_complete {
            if st != objc2_screen_capture_kit::SCFrameStatus::Complete.0 as i64 {
                return;
            }
        } else {
            // No attachments on this buffer — Swift's `nil` case maps to the same continue.
            return;
        }
        let Some(pb) = (unsafe { sb.image_buffer() }) else { return };
        let pts = unsafe { sb.presentation_time_stamp() };
        if !self.ivars().session_started.replace(true) {
            unsafe {
                parts.writer.startSessionAtSourceTime(CMTime {
                    value: pts.value,
                    timescale: pts.timescale,
                    flags: objc2_core_media::CMTimeFlags::Valid,
                    epoch: 0,
                })
            };
        }
        if !unsafe { parts.input.isReadyForMoreMediaData() } {
            return; // encoder behind: drop, never block the capture queue
        }
        let ok = unsafe {
            parts.adaptor.appendPixelBuffer_withPresentationTime(&pb, pts)
        };
        if ok {
            // CVPixelBuffer comes back through CMSampleBuffer as CFRetained
            // in objc2 0.6; storage ivar keeps the same wrapper.
            *self.ivars().last_frame.borrow_mut() = Some(pb);
            self.ivars().last_pts_value.set(pts.value);
            self.ivars().last_pts_timescale.set(pts.timescale);
        } else if unsafe { parts.writer.status() } == objc2_av_foundation::AVAssetWriterStatus::Failed {
            self.fail_by_writers("append failed");
        }
    }

    fn fail_by_writers(&self, why: &str) {
        if self.ivars().failed.replace(true) {
            return;
        }
        let desc = self
            .ivars()
            .parts
            .borrow()
            .as_ref()
            .and_then(|p| unsafe { p.writer.error() })
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| why.to_string());
        self.fail(&desc);
    }

    fn fail(&self, desc: &str) {
        eprintln!("capture failed: {desc}");
        if let Some(f) = self.ivars().on_failure.borrow().as_ref() {
            f(desc.to_string());
        }
    }
    /// `stop()` — Swift's: stop capture, remove our output, pad the last
    /// frame if needed (a still ending would otherwise be cut off), then
    /// finish writing. Completion runs on the main queue.
    pub fn stop_no_finish(&self) {
        self.stop(Box::new(|| {}));
    }

    pub fn stop(&self, completion: Box<dyn Fn() + 'static>) {
        self.ivars().stopped.set(true);
        let Some(stream) = self.ivars().stream.borrow_mut().take() else {
            self.finish_writer_tail_boxed(completion);
            return;
        };
        let proto_out: &ProtocolObject<dyn SCStreamOutput> =
            unsafe { &*(self as *const Self as *const ProtocolObject<dyn SCStreamOutput>) };
        let _ = unsafe {
            stream.removeStreamOutput_type_error(
                proto_out,
                objc2_screen_capture_kit::SCStreamOutputType::Screen,
            )
        };
        let me_raw = self as *const Self as usize;
        // SAFETY: completion crosses the capture queue as a raw box; the
        // block reclaims it exactly once.
        let raw = Box::into_raw(Box::new(completion)) as usize;
        let block = block2::RcBlock::new(move |error: *mut objc2_foundation::NSError| {
            let me = unsafe { &*(me_raw as *const ScreenRecorder) };
            let completion = unsafe { Box::from_raw(raw as *mut Box<dyn Fn()>) };
            if !error.is_null() {
                let err = unsafe { &*error };
                if let Some(f) = me.ivars().on_failure.borrow().as_ref() {
                    f(err.localizedDescription().to_string());
                }
            }
            me.finish_writer_tail_boxed(*completion);
        });
        unsafe {
            stream.stopCaptureWithCompletionHandler(Some(&*block));
        }
    }

    /// Shared tail of `stop` (writer only; capture already released).
    fn finish_writer_tail_boxed(&self, completion: Box<dyn Fn()>) {
        let some_parts = self.ivars().parts.borrow_mut().take();
        let Some(parts) = some_parts else {
            completion();
            return;
        };
        // Padding: SCK only sends frames when something changed, so a still
        // ending would be cut off — hold the last picture up to now.
        let session_started = self.ivars().session_started.get();
        let now_i64;
        let now_ts;
        {
            // CMClockGetTime(CMClockGetHostTimeClock()) — approximated in
            // seconds×600 with an epoch captured on frame; whatever happens
            // is within half a frame of truth.
            now_i64 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| (d.as_secs_f64() * 600.0) as i64)
                .unwrap_or(0);
            now_ts = 600;
        }
        let last_v = self.ivars().last_pts_value.get();
        let last_ts = self.ivars().last_pts_timescale.get().max(1);
        let last_sec = last_v as f64 / last_ts as f64;
        let now_sec = now_i64 as f64 / now_ts as f64;
        if session_started && now_sec - last_sec > 0.15 {
            if let Some(pb) = self.ivars().last_frame.borrow().as_ref() {
                if unsafe { parts.input.isReadyForMoreMediaData() } {
                    let _ = unsafe {
                        parts.adaptor.appendPixelBuffer_withPresentationTime(&pb, CMTime {
                            value: now_i64,
                            timescale: now_ts,
                            flags: objc2_core_media::CMTimeFlags::Valid,
                            epoch: 0,
                        })
                    };
                }
            }
        }
        if session_started {
            unsafe { parts.input.markAsFinished() };
        }
        let finished = if session_started {
            crate::app::synthetic_movie::finish_writer(&parts.writer)
        } else {
            crate::app::synthetic_movie::cancel_writer(&parts.writer);
            true
        };
        if !finished { self.fail_by_writers("finish failed"); }
        completion();
    }
}

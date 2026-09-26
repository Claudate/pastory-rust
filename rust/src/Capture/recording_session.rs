//! Port of `Capture/RecordingSession.swift`.
//!
//! Region recording after the picker: a click-through frame around the
//! region, a small control bar, then "MP4 or GIF?" when you stop. Result
//! goes to the shelf and the pasteboard as a file.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSPanel, NSTextField, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;
use objc2_screen_capture_kit::SCWindow;

use crate::app::{localization::l, theme};
use crate::capture::recording_preview::RecordingPreviewWindow;
use crate::capture::screen_recorder::ScreenRecorder;
use crate::capture::target::{CaptureTarget, ShareableSnapshot};

/// 10 min hard cap; this is for short demos.
const MAX_SECONDS: i64 = 600;
/// The timer tick interval (0.5 s, like Swift).
const TICK: f64 = 0.5;

pub struct RecordingSession {
    target: RefCell<Option<CaptureTarget>>,
    region_screen_rect: Cell<CGRect>,
    recorder: RefCell<Option<Retained<ScreenRecorder>>>,
    pub(crate) frame: RefCell<Option<Retained<objc2_app_kit::NSWindow>>>,
    bar: RefCell<Option<Retained<NSPanel>>>,
    time_label: RefCell<Option<Retained<NSTextField>>>,
    dot: RefCell<Option<Retained<NSView>>>,
    timer_alive: Cell<bool>,
    started_seconds: Cell<i64>,
    tmp_path: RefCell<std::path::PathBuf>,
    pub(crate) is_recording: Cell<bool>,
    stopping: Cell<bool>,
    cancelled: Cell<bool>,
    failed: Cell<bool>,
    preview: RefCell<Option<Retained<RecordingPreviewWindow>>>,
    on_finish: RefCell<Option<Box<dyn Fn() + 'static>>>,
}

static CURRENT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// The active session (Swift's `CaptureCoordinator.recording`). The first
/// call before any recording returns a bare placeholder (`has_active` is
/// then false).
pub fn current() -> &'static RecordingSession {
    let ptr = *CURRENT.get_or_init(|| {
        Box::into_raw(Box::new(RecordingSession::bare())) as usize
    });
    // SAFETY: boxed for the process lifetime; everything here is main-thread.
    unsafe { &*(ptr as *const RecordingSession) }
}

fn set_current(s: &'static RecordingSession) {
    let ptr = s as *const RecordingSession as usize;
    let _ = CURRENT.set(ptr);
}

/// A session actually lives (Swift's `recording.isRecording || preview up`
/// path in the hotkey guard).
pub fn has_active() -> bool {
    let c = current();
    c.is_recording.get() || c.stopping.get() || c.preview.borrow().is_some()
}

impl RecordingSession {
    fn bare() -> Self {
        Self {
            target: RefCell::new(None),
            region_screen_rect: Cell::new(CGRect::ZERO),
            recorder: RefCell::new(None),
            frame: RefCell::new(None),
            bar: RefCell::new(None),
            time_label: RefCell::new(None),
            dot: RefCell::new(None),
            timer_alive: Cell::new(false),
            started_seconds: Cell::new(0),
            tmp_path: RefCell::new(std::path::PathBuf::new()),
            is_recording: Cell::new(false),
            stopping: Cell::new(false),
            cancelled: Cell::new(false),
            failed: Cell::new(false),
            preview: RefCell::new(None),
            on_finish: RefCell::new(None),
        }
    }

    /// `init(target:regionScreenRect:)` — Swift's constructor: the temp url.
    pub fn configure(&self, target: CaptureTarget, region_screen_rect: CGRect) {
        *self.target.borrow_mut() = Some(target);
        self.region_screen_rect.set(region_screen_rect);
        let id = objc2_foundation::NSUUID::UUID().UUIDString().to_string();
        *self.tmp_path.borrow_mut() = std::env::temp_dir().join(format!("pastory-{id}.mp4"));
        // The coordinator already holds ONE session: `current()` above is that
        // session; configure() resets it for the next recording.
        self.failed.set(false);
        self.cancelled.set(false);
        self.stopping.set(false);
    }

    pub fn set_on_finish(&self, f: Box<dyn Fn() + 'static>) {
        *self.on_finish.borrow_mut() = Some(f);
    }

    /// `start()` — show the frame + control bar, fetch the window ids to
    /// exclude (frame + bar), start capture.
    pub fn start(&self) {
        self.show_frame();
        self.show_bar();
        let frame_num = self.frame.borrow().as_ref().map(|w| w.windowNumber() as u32);
        let bar_num = self.bar.borrow().as_ref().map(|w| w.windowNumber() as u32);
        let own: Vec<u32> = [frame_num, bar_num].into_iter().flatten().collect();
        let session_raw = self as *const Self as usize;
        ShareableSnapshot::fetch(Box::new(move |snap| {
            let session = unsafe { &*(session_raw as *const RecordingSession) };
            let Some(snap) = snap else {
                session.fail(Some("unable to list shareable content".into()));
                return;
            };
            session.start_capture_with(&snap, &own);
        }));
    }

    /// Filter the list down to our two mask windows (frame + bar), then make
    /// the recorder and start the stream (Swift's async start).
    fn start_capture_with(&self, snap: &ShareableSnapshot, own: &[u32]) {
        let own_windows: Vec<Retained<SCWindow>> = unsafe { snap.content.windows() }
            .iter()
            .filter(|w| own.contains(&unsafe { w.windowID() }))
            .collect();
        let scale = crate::annotate::ocr_panel::screens_intersecting(
            MainThreadMarker::new().expect("main thread"),
            self.region_screen_rect.get(),
        )
        .map(|s| s.backingScaleFactor());
        let session_raw = self as *const Self as usize;
        let on_started: Box<dyn FnOnce(Result<(), String>) + 'static> =
            Box::new(move |r: Result<(), String>| {
                let session = unsafe { &*(session_raw as *const RecordingSession) };
                session.capture_started(r);
            });
        let on_failure: Box<dyn Fn(String) + 'static> = Box::new(move |desc: String| {
            let session = unsafe { &*(session_raw as *const RecordingSession) };
            session.fail(Some(desc));
        });
        let Some(target) = self.target.borrow().clone() else { return };
        let tmp = self.tmp_path.borrow().clone();
        match ScreenRecorder::start(&target, &own_windows, scale, &tmp, on_failure, on_started) {
            Ok(rec) => {
                *self.recorder.borrow_mut() = Some(rec);
            }
            Err(e) => {
                self.fail(Some(e));
            }
        }
    }

    /// Stream started (Swift's `await recorder.start()` continuation).
    fn capture_started(&self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                if self.cancelled.get() {
                    // 「丢弃」came while the stream was starting: stop it now,
                    // nothing to keep.
                    if let Some(rec) = self.recorder.borrow().as_ref() {
                        rec.stop_no_finish();
                    }
                    let _ = std::fs::remove_file(self.tmp_path.borrow().as_path());
                    return;
                }
                self.is_recording.set(true);
                self.started_seconds.set(Self::now_unix());
                self.timer_alive.set(true);
                self.tick();
            }
            Err(e) => {
                if !self.cancelled.get() {
                    self.fail(Some(e));
                }
            }
        }
    }

    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// 0.5 s cadence (Swift's timer): mm:ss label, blink, 10 min cap.
    fn tick(&self) {
        if !self.timer_alive.get() {
            return;
        }
        let s = (Self::now_unix() - self.started_seconds.get()).max(0);
        if let Some(label) = self.time_label.borrow().as_ref() {
            label.setStringValue(&NSString::from_str(&format!(
                "{:02}:{:02}",
                s / 60,
                s % 60
            )));
        }
        if let Some(d) = self.dot.borrow().as_ref() {
            d.setAlphaValue(if d.alphaValue() == 1.0 { 0.25 } else { 1.0 });
        }
        if s >= MAX_SECONDS {
            self.stop();
            return;
        }
        let raw = self as *const Self as usize;
        crate::app::delegate::dispatch_main_after(TICK, Box::new(move || {
            if let Some(s) = unsafe { (raw as *const RecordingSession).as_ref() } {
                s.tick();
            }
        }));
    }

    /// `stop()` — Swift's: guard isRecording && !stopping; then "保存中…" +
    /// recorder.stop + duration check → showFormatChoice.
    pub fn stop(&self) {
        if !self.is_recording.get() || self.stopping.get() {
            return;
        }
        self.stopping.set(true);
        self.timer_alive.set(false);
        if let Some(label) = self.time_label.borrow().as_ref() {
            label.setStringValue(&NSString::from_str(&l("保存中…")));
        }
        let raw = self as *const Self as usize;
        let tmp = self.tmp_path.borrow().clone();
        if let Some(rec) = self.recorder.borrow().as_ref() {
            rec.stop(Box::new(move || {
                let session = unsafe { &*(raw as *const RecordingSession) };
                session.recorder_stopped(&tmp);
            }));
            return;
        }
        self.recorder_stopped(&tmp);
    }

    /// After the writer is finished: duration > 0.2 + file exists → format
    /// choice; else 录屏失败 alert.
    fn recorder_stopped(&self, tmp: &std::path::Path) {
        self.is_recording.set(false);
        let dur = crate::capture::gif_encoder::duration(tmp);
        if dur > 0.2 && tmp.exists() {
            self.show_format_choice(dur);
            return;
        }
        self.fail(None);
    }

    /// `cancel()` — mid-start 丢弃 or teardown paths.
    pub fn cancel(&self) {
        // stop() is mid-flight; its continuation must not be raced.
        if self.stopping.get() && self.is_recording.get() {
            return;
        }
        self.cancelled.set(true);
        self.timer_alive.set(false);
        if self.is_recording.get() {
            if let Some(rec) = self.recorder.borrow().as_ref() {
                rec.stop_no_finish();
            }
        }
        self.is_recording.set(false);
        let _ = std::fs::remove_file(self.tmp_path.borrow().as_path());
        self.teardown();
    }

    /// `fail(_:)` — both stream delegates report the same failure once.
    fn fail(&self, error: Option<String>) {
        if self.failed.replace(true) {
            return;
        }
        self.timer_alive.set(false);
        self.is_recording.set(false);
        let _ = std::fs::remove_file(self.tmp_path.borrow().as_path());
        self.teardown();
        if let Some(desc) = error {
            let mtm = MainThreadMarker::new().expect("main thread");
            let alert = objc2_app_kit::NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(&l("录屏失败")));
            alert.setInformativeText(&NSString::from_str(&desc));
            {
                let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
                unsafe {
                    let _: () = objc2::msg_send![&app, activate];
                }
                #[allow(deprecated)]
                app.activateIgnoringOtherApps(true);
            }
            alert.runModal();
        }
    }

    /// `teardown()` — close everything, fire on_finish.
    fn teardown(&self) {
        if let Some(w) = self.frame.borrow_mut().take() {
            let sender: Option<&AnyObject> = None;
            w.orderOut(sender);
            w.close();
        }
        if let Some(b) = self.bar.borrow_mut().take() {
            let sender: Option<&AnyObject> = None;
            b.orderOut(sender);
            b.close();
        }
        let _ = self.recorder.borrow_mut().take();
        if let Some(p) = self.preview.borrow_mut().take() {
            p.close();
        }
        if let Some(f) = self.on_finish.borrow().as_ref() {
            f();
        }
    }

    // MARK: After stop

    /// `showFormatChoice(duration:)` — present the preview window.
    fn show_format_choice(&self, duration: f64) {
        if let Some(w) = self.frame.borrow_mut().take() {
            let sender: Option<&AnyObject> = None;
            w.orderOut(sender);
            w.close();
        }
        if let Some(b) = self.bar.borrow_mut().take() {
            let sender: Option<&AnyObject> = None;
            b.orderOut(sender);
            b.close();
        }
        let pixel = self.recorder_pixel_size();
        let w = RecordingPreviewWindow::new(&self.tmp_path.borrow(), duration, pixel, self.region_screen_rect.get());
        let raw = self as *const Self as usize;
        w.set_on_choose(Box::new(move |gif| {
            let s = unsafe { &*(raw as *const RecordingSession) };
            s.deliver(gif);
        }));
        w.set_on_discard(Box::new(move || {
            let s = unsafe { &*(raw as *const RecordingSession) };
            s.cancel();
        }));
        *self.preview.borrow_mut() = Some(w.clone());
        w.present();
    }

    /// The pixel size the writer really used (recorder.pixelSize).
    fn recorder_pixel_size(&self) -> CGSize {
        self.recorder
            .borrow()
            .as_ref()
            .map(|r| r.ivars().pixel_size.get())
            .unwrap_or(CGSize::ZERO)
    }

    /// `deliver(gif:)` — encode if asked, then insert + pasteboard.
    fn deliver(&self, gif: bool) {
        let src = self.tmp_path.borrow().clone();
        let busy_text = if gif { l("正在转 GIF…") } else { l("保存中…") };
        if let Some(p) = self.preview.borrow().as_ref() {
            p.set_busy(&busy_text);
        }
        let session_raw = self as *const Self as usize;
        let gif_path = src.with_extension("gif");
        if gif {
            {
                let base = std::env::temp_dir().join("pastory-encode-task");
                let _ = base;
            }
            let block_src = src.clone();
            let block_gif = gif_path.clone();
            let block_raw = session_raw;
            std::thread::spawn(move || {
                let _prog = crate::app::delegate::dispatch_main_after;
                let progress = move |progress: f64| {
                    crate::app::delegate::dispatch_main_async(Box::new(move || {
                        if let Some(p) = {
                            let session = unsafe { &*(block_raw as *const RecordingSession) };
                            session.preview.borrow().clone()
                        } {
                            p.set_busy(&l("正在转 GIF… %d%%").replacen(
                                "%d",
                                &format!("{}", (progress * 100.0) as i64),
                                1,
                            ));
                        }
                    }));
                };
                let result = crate::capture::gif_encoder::encode(&block_src, &block_gif, Some(&progress));
                let session_raw = block_raw;
                crate::app::delegate::dispatch_main_async(Box::new(move || {
                    let session = unsafe { &*(session_raw as *const RecordingSession) };
                    match result {
                        Ok(()) => session.deliver_paste(block_gif, true, &block_src),
                        Err(e) => {
                            let _ = std::fs::remove_file(&block_gif);
                            if let Some(p) = session.preview.borrow().as_ref() {
                                p.set_idle(&l("GIF 转换失败：%@，可以改选 MP4").replacen("%@", &e, 1));
                            }
                        }
                    }
                }));
            });
            return;
        }
        self.deliver_paste(src, gif, &self.tmp_path.borrow());
    }
}

// MARK: frame + control bar (Swift: showFrame/showBar)

impl RecordingSession {
    /// `showFrame` — click-through blue frame 3 pt around the region.
    fn show_frame(&self) {
        let pad = -3.0f64;
        let r = crate::app::coordinates::inset_rect(self.region_screen_rect.get(), pad, pad);
        let mtm = MainThreadMarker::new().expect("main thread");
        let w = unsafe {
            objc2_app_kit::NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                r,
                NSWindowStyleMask::Borderless,
                objc2_app_kit::NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe {
            w.setReleasedWhenClosed(false);
        }
        w.setOpaque(false);
        w.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
        w.setHasShadow(false);
        w.setIgnoresMouseEvents(true);
        w.setLevel(3); // floating
        w.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );
        let frame_view = FrameViewBuilder::new(CGRect::new(CGPoint::ZERO, r.size));
        w.setContentView(Some(&frame_view));
        w.orderFrontRegardless();
        *self.frame.borrow_mut() = Some(w);
    }

    /// `showBar` — desk strip: red dot, mm:ss, ■ 停止, 丢弃.
    fn show_bar(&self) {
        if let Some(old) = self.bar.borrow_mut().take() {
            let sender: Option<&AnyObject> = None;
            old.orderOut(sender);
            old.close();
        }
        let mtm = MainThreadMarker::new().expect("main thread");
        let p = {
            NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                CGRect::new(CGPoint::ZERO, CGSize::new(290.0, 52.0)),
                NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                objc2_app_kit::NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe {
            p.setReleasedWhenClosed(false);
        }
        p.setOpaque(false);
        p.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
        p.setHasShadow(true);
        p.setLevel(25); // .statusBar
        p.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );

        let v = DeskStripViewBuilder::new();
        // dot
        let d = NSView::new(mtm);
        d.setWantsLayer(true);
        if let Some(layer) = d.layer() {
            let c = crate::app::theme::srgb_octets(0xE0, 0x33, 0x33).CGColor();
            unsafe {
                let _: () = objc2::msg_send![&*layer, setBackgroundColor: &*c];
                let _: () = objc2::msg_send![&*layer, setCornerRadius: 5.0f64];
            }
        }
        d.setFrame(CGRect::new(CGPoint::new(16.0, (52.0 - 10.0) / 2.0), CGSize::new(10.0, 10.0)));
        v.addSubview(&d);
        *self.dot.borrow_mut() = Some(d);
        // label
        let label = NSTextField::labelWithString(&NSString::from_str("00:00"), mtm);
        label.setFont(Some(&objc2_app_kit::NSFont::monospacedDigitSystemFontOfSize_weight(
            15.0,
            unsafe { objc2_app_kit::NSFontWeightSemibold },
        )));
        label.setTextColor(Some(&theme::on_brown()));
        label.setFrame(CGRect::new(CGPoint::new(16.0 + 10.0 + 10.0, (52.0 - 20.0) / 2.0), CGSize::new(70.0, 20.0)));
        v.addSubview(&label);
        *self.time_label.borrow_mut() = Some(label);
        // SAFETY: the delegate object (this session) outlives the panel.
        let delegate_obj: &AnyObject = unsafe { &*(self as *const Self as *const AnyObject) };
        let stop_btn = crate::capture::selection_overlay::paper_button_export(
            &l("■ 停止"),
            true,
            false,
            Some(delegate_obj),
            objc2::sel!(stopTappedSession:),
        );
        let cancel_btn = crate::capture::selection_overlay::paper_button_export(
            &l("丢弃"),
            false,
            true,
            Some(delegate_obj),
            objc2::sel!(cancelTappedSession:),
        );
        let stop_w = stop_btn.frame().size.width;
        let cancel_w = cancel_btn.frame().size.width;
        stop_btn.setFrameOrigin(CGPoint::new(290.0 - 10.0 - stop_w, 9.0));
        cancel_btn.setFrameOrigin(CGPoint::new(290.0 - 10.0 - stop_w - 10.0 - cancel_w, 9.0));
        v.addSubview(&cancel_btn);
        v.addSubview(&stop_btn);

        self.place(&p, CGSize::new(290.0, 52.0));
        p.setContentView(Some(&v));
        p.orderFrontRegardless();
        *self.bar.borrow_mut() = Some(p);
    }

    /// `place(_:size:)` — bar position: below the region, else above, else inside.
    fn place(&self, p: &objc2_app_kit::NSWindow, size: CGSize) {
        let region = self.region_screen_rect.get();
        let screen = crate::annotate::ocr_panel::screens_intersecting(MainThreadMarker::new().expect("main thread"), region);
        let vf = screen.map(|s| s.visibleFrame()).unwrap_or(region);
        let mut origin = CGPoint::new(
            region.max().x - size.width,
            region.min().y - size.height - 10.0,
        );
        if origin.y < vf.min().y {
            origin.y = region.max().y + 10.0;
        }
        if origin.y + size.height > vf.max().y {
            origin.y = region.min().y + 10.0;
        }
        origin.x = origin
            .x
            .max(vf.min().x + 8.0)
            .min(vf.max().x - size.width - 8.0);
        p.setFrame_display(CGRect::new(origin, size), true);
    }

    /// Insert the delivery file into the shelf and onto the pasteboard
    /// (`deliver`'s tail).
    fn deliver_paste(&self, file: std::path::PathBuf, gif: bool, src: &std::path::Path) {
        let poster = crate::capture::gif_encoder::poster(src);
        let dur = crate::capture::gif_encoder::duration(src);
        let item = crate::clipboard::store::with(|s| {
            s.insert_video(&file, poster.as_deref(), dur, &crate::clipboard::store::Source::pastory())
        });
        if gif {
            let _ = std::fs::remove_file(src);
        }
        if let Some(item) = item {
            crate::clipboard::store::with(|s| {
                s.copy_to_pasteboard(&item);
            });
        }
        self.teardown();
    }
}

// MARK: FrameView + DeskStripView (one-time chrome; framed classes below)

pub struct FrameViewBuilderIvars;

objc2::define_class!(
    // SAFETY: click-through frame view.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = FrameViewBuilderIvars]
    pub struct FrameViewBuilder;

    unsafe impl NSObjectProtocol for FrameViewBuilder {}

    impl FrameViewBuilder {
        #[unsafe(method(drawRect:))]
        fn fb_draw_rect(&self, _dirty: CGRect) {
            let bounds = self.bounds();
            let accent = crate::annotate::annotation::palette_accent();
            accent.setStroke();
            let path = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                crate::app::coordinates::inset_rect(bounds, 1.5, 1.5),
                4.0,
                4.0,
            );
            path.setLineWidth(3.0);
            path.stroke();
        }
    }
);

impl FrameViewBuilder {
    fn new(frame: objc2_core_foundation::CGRect) -> objc2::rc::Retained<Self> {
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<Self>().set_ivars(FrameViewBuilderIvars);
        unsafe { objc2::msg_send![super(this), initWithFrame: frame] }
    }
}

/// The desk-coloured strip behind the control bar (`DeskStripView`).
pub struct DeskStripViewBuilderIvars;

objc2::define_class!(
    // SAFETY: borderless bar content view.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DeskStripViewBuilderIvars]
    pub struct DeskStripViewBuilder;

    unsafe impl NSObjectProtocol for DeskStripViewBuilder {}

    impl DeskStripViewBuilder {
        #[unsafe(method(drawRect:))]
        fn db_draw_rect(&self, _dirty: CGRect) {
            crate::app::theme::draw_desk(
                &objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    crate::app::coordinates::inset_rect(self.bounds(), 0.5, 0.5),
                    6.0,
                    6.0,
                ),
            );
        }
    }
);

impl DeskStripViewBuilder {
    fn new() -> objc2::rc::Retained<Self> {
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<Self>().set_ivars(DeskStripViewBuilderIvars);
        let v: objc2::rc::Retained<Self> = unsafe { objc2::msg_send![super(this), init] };
        crate::app::theme::paper_sheet(&v, 6.0);
        v.setFrame(objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::ZERO,
            objc2_core_foundation::CGSize::new(290.0, 52.0),
        ));
        v
    }
}

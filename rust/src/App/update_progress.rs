//! Port of `App/UpdateProgressWindow.swift` — the paper-style download panel
//! and `UpdateDownload`, the one-file NSURLSessionDownloadTask wrapper.
//!
//! Two entry shapes, both over NSURLSession:
//! - `download()` for the updater: delegate-driven progress → main queue and
//!   completion/moving/cancel through a small delegate object, so the
//!   downloaded temp file is copied out exactly when `didFinishDownloadingToURL:`
//!   fires (the Swift semantics — it vanishes when that callback returns).
//! - `fetch_blocking()` for `--selftest download`: the same request through a
//!   data task (the self-test only asserts byte counts and the failure path).

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex as StdMutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAppearanceCustomization, NSPanel, NSView, NSWindowStyleMask};
use objc2_foundation::{
    NSData, NSError, NSString, NSURL, NSURLSession, NSURLSessionConfiguration,
    NSURLSessionDownloadTask, NSURLSessionTask,
};

use crate::app::localization::l;
use crate::app::theme;

// MARK: Progress panel (selftest: updatewin)

/// A thin paper-blue bar on a dim track (`ProgressTrack`).
struct ProgressTrackIvars {
    fraction: std::cell::Cell<f64>,
}

define_class!(
    // SAFETY: plain NSView; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ProgressTrackIvars]
    struct ProgressTrack;

    unsafe impl NSObjectProtocol for ProgressTrack {}

    impl ProgressTrack {
        #[unsafe(method(drawRect:))]
        fn t_draw_rect(&self, _dirty: objc2_core_foundation::CGRect) {
            let bounds = self.bounds();
            let r = bounds.size.height / 2.0;
            theme::on_brown().colorWithAlphaComponent(0.18).setFill();
            objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, r, r).fill();
            let fraction = self.ivars().fraction.get();
            if fraction <= 0.0 {
                return;
            }
            let w = bounds.size.height.max(bounds.size.width * fraction.clamp(0.0, 1.0));
            theme::paper_blue().setFill();
            objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                objc2_core_foundation::CGRect::new(
                    bounds.min(),
                    objc2_core_foundation::CGSize::new(w, bounds.size.height),
                ),
                r,
                r,
            )
            .fill();
        }
    }
);

impl ProgressTrack {
    fn set_fraction(&self, f: f64) {
        self.ivars().fraction.set(f);
        self.setNeedsDisplay(true);
    }
}

/// Panel ivars: the five pieces of content + the cancel hook.
pub struct ProgressWindowIvars {
    title: std::cell::RefCell<Option<Retained<objc2_app_kit::NSTextField>>>,
    detail: std::cell::RefCell<Option<Retained<objc2_app_kit::NSTextField>>>,
    track: std::cell::RefCell<Option<Retained<ProgressTrack>>>,
    cancel_button: std::cell::RefCell<Option<Retained<objc2_app_kit::NSButton>>>,
    cancel_task: std::cell::RefCell<Option<Retained<NSURLSessionDownloadTask>>>,
}

define_class!(
    // SAFETY:
    // - NSPanel subclass + own action target; main-thread only.
    // - One instance per download; clones share the same underlying panel.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ProgressWindowIvars]
    pub struct ProgressWindow;

    unsafe impl NSObjectProtocol for ProgressWindow {}

    impl ProgressWindow {
        #[unsafe(method(cancelTapped:))]
        fn cancel_tapped(&self, _sender: &AnyObject) {
            if let Some(t) = self.ivars().cancel_task.borrow().as_ref() {
                t.cancel();
            }
        }
    }
);

/// Handle cloning (Swift's class semantics: the panel is the identity).
#[derive(Clone)]
pub struct Window {
    inner: Retained<ProgressWindow>,
}

fn label(s: &str, bold: bool, on_brown: bool) -> Retained<objc2_app_kit::NSTextField> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let f = objc2_app_kit::NSTextField::labelWithString(&NSString::from_str(s), mtm);
    f.setFont(Some(&theme::serif(if bold { 16.0 } else { 13.0 }, bold)));
    let color = if on_brown { theme::on_brown() } else { theme::on_brown_muted() };
    f.setTextColor(Some(&color));
    f
}

fn place(view: &impl objc2::Message, x: f64, y: f64, w: f64, h: f64) {
    unsafe {
        let _: () = msg_send![view, setFrame: objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::new(x, y),
            objc2_core_foundation::CGSize::new(w, h),
        )];
    }
}

impl Window {
    pub fn new(version: String) -> Window {
        let mtm = MainThreadMarker::new().expect("main thread");
        let rect = objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::ZERO,
            objc2_core_foundation::CGSize::new(380.0, 150.0),
        );
        let this = mtm.alloc::<ProgressWindow>().set_ivars(ProgressWindowIvars {
            title: std::cell::RefCell::new(None),
            detail: std::cell::RefCell::new(None),
            track: std::cell::RefCell::new(None),
            cancel_button: std::cell::RefCell::new(None),
            cancel_task: std::cell::RefCell::new(None),
        });
        let this: Retained<ProgressWindow> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Titled
                    | NSWindowStyleMask::FullSizeContentView
                    | NSWindowStyleMask::NonactivatingPanel,
                backing: objc2_app_kit::NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        this.setTitleVisibility(objc2_app_kit::NSWindowTitleVisibility::Hidden);
        this.setTitlebarAppearsTransparent(true);
        unsafe {
            let appearance = objc2_app_kit::NSAppearance::appearanceNamed(
                objc2_app_kit::NSAppearanceNameDarkAqua,
            );
            this.setAppearance(appearance.as_deref());
        }
        this.setBackgroundColor(Some(&theme::brown()));
        this.setLevel(3); // floating
        unsafe {
            this.setReleasedWhenClosed(false);
        }
        this.setHidesOnDeactivate(false);

        let content = crate::annotate::ocr_panel::ground_view_export(rect);
        this.setContentView(Some(&content));

        let w = rect.size.width;
        let h = rect.size.height;
        let title_field = label(&l("正在下载 Pastory %@…").replacen("%@", &version, 1), true, true);
        let detail_field = label(&l("正在连接 GitHub…"), false, false);
        let track_view: Retained<ProgressTrack> = {
            let t = mtm.alloc::<ProgressTrack>().set_ivars(ProgressTrackIvars {
                fraction: std::cell::Cell::new(0.0),
            });
            unsafe { msg_send![super(t), initWithFrame: rect] }
        };
        let cancel = crate::capture::selection_overlay::paper_button_export(
            &l("取消"),
            false,
            true,
            unsafe { Some(&*(Retained::as_ptr(&this) as *const AnyObject)) },
            sel!(cancelTapped:),
        );

        content.addSubview(&title_field);
        content.addSubview(&detail_field);
        content.addSubview(&track_view);
        content.addSubview(&cancel);

        place(&*title_field, 22.0, h - 30.0 - 22.0, w - 44.0, 22.0);
        place(&*track_view, 22.0, h - 30.0 - 22.0 - 14.0 - 8.0, w - 44.0, 8.0);
        let cb = cancel.frame().size;
        place(&*cancel, w - 18.0 - cb.width, 14.0, cb.width, cb.height);
        place(
            &*detail_field,
            22.0,
            14.0 + (cb.height - 20.0) / 2.0,
            w - 18.0 - cb.width - 12.0 - 22.0,
            20.0,
        );

        *this.ivars().title.borrow_mut() = Some(title_field);
        *this.ivars().detail.borrow_mut() = Some(detail_field);
        *this.ivars().track.borrow_mut() = Some(track_view);
        *this.ivars().cancel_button.borrow_mut() = Some(cancel);
        Window { inner: this }
    }

    pub fn show(&self) {
        self.inner.center();
        self.inner.orderFrontRegardless();
    }

    pub fn close(&self) {
        self.inner.orderOut(None);
    }

    /// `debugContentView` for the `updatewin` selftest: the panel's content.
    pub fn content_view(&self) -> Retained<NSView> {
        self.inner.contentView().expect("content")
    }

    /// `update(done:total:)` — file-style byte formatting, 8 pt track.
    pub fn update(&self, done: i64, total: i64) {
        if let Some(track) = self.ivars().track.borrow().as_ref() {
            track.set_fraction(if total > 0 { done as f64 / total as f64 } else { 0.0 });
        }
        if let Some(detail) = self.ivars().detail.borrow().as_ref() {
            let text = if total > 0 {
                format!("{} / {}", abbrev_bytes(done), abbrev_bytes(total))
            } else {
                abbrev_bytes(done)
            };
            detail.setStringValue(&NSString::from_str(&text));
        }
    }

    pub fn begin_installing(&self) {
        if let Some(title) = self.ivars().title.borrow().as_ref() {
            title.setStringValue(&NSString::from_str(&l("正在校验并安装…")));
        }
        if let Some(detail) = self.ivars().detail.borrow().as_ref() {
            detail.setStringValue(&NSString::from_str(""));
        }
        if let Some(track) = self.ivars().track.borrow().as_ref() {
            track.set_fraction(1.0);
        }
        if let Some(b) = self.ivars().cancel_button.borrow().as_ref() {
            b.setHidden(true);
        }
    }

    /// Wire the cancel button to the live download task (Swift's
    /// `download.cancel` on the same object).
    pub fn bind_cancel_task(&self, task: &NSURLSessionDownloadTask) {
        *self.inner.ivars().cancel_task.borrow_mut() = Some(task.into());
    }

    /// Raw handle for the cross-thread hop. The panel is a process-lifetime
    /// UI object driven only from the main queue; the raw form crosses the
    /// delegates' background hop and is rebuilt on main (`from_raw`).
    fn raw(&self) -> usize {
        Retained::as_ptr(&self.inner) as usize
    }

    /// Rebuild a live handle on the main thread.
    /// SAFETY: `raw` came from `Window::raw` on a live panel.
    unsafe fn from_raw(raw: usize) -> Window {
        Window {
            inner: Retained::retain(raw as *mut ProgressWindow).expect("panel alive"),
        }
    }

    /// The updater's install thread hops the handle the same way.
    pub fn raw_pub(&self) -> usize {
        self.raw()
    }

    /// `from_raw` for the updater module (same SAFETY contract).
    pub unsafe fn from_raw_pub(raw: usize) -> Window {
        Window::from_raw(raw)
    }

    fn ivars(&self) -> &ProgressWindowIvars {
        self.inner.ivars()
    }
}

/// `ByteCountFormatter.string(fromByteCount:countStyle:.file)` shape.
fn abbrev_bytes(n: i64) -> String {
    let n = n.max(0) as f64;
    if n >= 1_000_000_000.0 {
        format!("{:.1} GB", n / 1_000_000_000.0)
    } else if n >= 1_000_000.0 {
        format!("{:.1} MB", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1} KB", n / 1_000.0)
    } else {
        format!("{} bytes", n as i64)
    }
}

// MARK: Downloader

#[derive(Debug)]
pub enum DownloadError {
    Cancelled,
    Failed(String),
}

impl DownloadError {
    pub fn describe(&self) -> String {
        match self {
            DownloadError::Cancelled => "cancelled".into(),
            DownloadError::Failed(s) => s.clone(),
        }
    }
}

/// Session kept alive while a transfer is in flight; the session and task
/// handles are dropped (invalidated) right after the completion fires, like
/// Swift's `defer { session.finishTasksAndInvalidate() }`.
struct DownloadState {
    dest: PathBuf,
    /// Panel as a raw handle (the background delegates only move it along).
    window_raw: usize,
    done: StdMutex<Option<Box<dyn FnOnce(Result<PathBuf, DownloadError>) + Send>>>,
    moved: StdMutex<bool>,
    task: StdMutex<Option<Retained<NSURLSessionDownloadTask>>>,
    session: StdMutex<Option<Retained<NSURLSession>>>,
}

struct DownloadDelegateIvars {
    state: Arc<DownloadState>,
}

define_class!(
    // SAFETY:
    // - NSObject delegate; the session's delegate queue fires the methods on
    //   a background thread (Swift's `delegateQueue: nil`). Shared state is
    //   behind mutexes; the window reports hop to the main queue.
    // - Selectors registered directly on the class (M4 informal-protocol
    //   law): `unsafe impl NSURLSessionDownloadDelegate` would re-verify the
    //   parent protocol chain and its challenge methods lose their Sendable
    //   blocks.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DownloadDelegateIvars]
    struct DownloadDelegate;

    unsafe impl NSObjectProtocol for DownloadDelegate {}

    impl DownloadDelegate {
        #[unsafe(method(URLSession:downloadTask:didWriteData:totalBytesWritten:totalBytesExpectedToWrite:))]
        fn d_write(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionDownloadTask,
            _bytes_written: i64,
            total_written: i64,
            total_expected: i64,
        ) {
            let raw = self.ivars().state.window_raw;
            crate::app::delegate::dispatch_main_async(Box::new(move || {
                // SAFETY: the panel is alive for the transfer's lifetime.
                let window = unsafe { Window::from_raw(raw) };
                window.update(total_written, total_expected.max(0));
            }));
        }

        #[unsafe(method(URLSession:downloadTask:didFinishDownloadingToURL:))]
        fn d_finish(
            &self,
            _session: &NSURLSession,
            task: &NSURLSessionDownloadTask,
            location: &NSURL,
        ) {
            // The temp file is gone when this returns: copy it out now.
            let state = &self.ivars().state;
            let mut status_ok = true;
            unsafe {
                let response: Option<Retained<objc2_foundation::NSURLResponse>> =
                    msg_send![task, response];
                if let Some(r) = response {
                    let http: Retained<objc2_foundation::NSHTTPURLResponse> =
                        Retained::cast_unchecked(r);
                    status_ok = http.statusCode() == 200;
                }
            }
            let dest = state.dest.clone();
            let _ = std::fs::remove_file(&dest);
            let ok = status_ok
                && location
                    .path()
                    .map(|p| std::fs::copy(p.to_string(), &dest).is_ok())
                    .unwrap_or(false);
            // A failed move surfaces the same as a failed transfer (Swift's
            // `moveError`), checked at task completion before reporting.
            *state.moved.lock().expect("moved poisoned") = ok;
        }

        #[unsafe(method(URLSession:task:didCompleteWithError:))]
        fn d_complete(&self, _session: &NSURLSession, _task: &NSURLSessionTask, error: Option<&NSError>) {
            let state = &self.ivars().state;
            let outcome = match error {
                Some(e) => {
                    let _ = std::fs::remove_file(&state.dest);
                    if e.code() == -999 {
                        // NSURLErrorCancelled
                        Err(DownloadError::Cancelled)
                    } else {
                        Err(DownloadError::Failed(e.localizedDescription().to_string()))
                    }
                }
                None => {
                    if *state.moved.lock().expect("moved poisoned") {
                        Ok(state.dest.clone())
                    } else {
                        let _ = std::fs::remove_file(&state.dest);
                        Err(DownloadError::Failed("badServerResponse".into()))
                    }
                }
            };
            if let Some(done) = state.done.lock().expect("done poisoned").take() {
                crate::app::delegate::dispatch_main_async(Box::new(move || done(outcome)));
            }
            // The transfer is over; release session + task (Swift's
            // `session.finishTasksAndInvalidate()` in the defer).
            state.session.lock().expect("session poisoned").take();
            state.task.lock().expect("task poisoned").take();
        }
    }
);

impl DownloadDelegate {
    fn new(state: Arc<DownloadState>) -> Retained<Self> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm
            .alloc::<DownloadDelegate>()
            .set_ivars(DownloadDelegateIvars { state });
        unsafe { msg_send![super(this), init] }
    }
}

/// One file download with progress and cancel (`UpdateDownload.fetch`).
pub fn download(
    url: &NSURL,
    window: Window,
    done: impl FnOnce(Result<PathBuf, DownloadError>) + Send + 'static,
) {
    let dest = std::env::temp_dir().join(format!(
        "pastory-dl-{}.zip",
        objc2_foundation::NSUUID::new().UUIDString()
    ));
    let state = Arc::new(DownloadState {
        dest,
        window_raw: window.raw(),
        done: StdMutex::new(Some(Box::new(done))),
        moved: StdMutex::new(false),
        task: StdMutex::new(None),
        session: StdMutex::new(None),
    });
    let delegate = DownloadDelegate::new(state.clone());
    let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    config.setTimeoutIntervalForRequest(30.0);
    config.setTimeoutIntervalForResource(900.0);
    let delegate_obj: Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSURLSessionDelegate>> =
        unsafe { objc2::rc::Retained::cast_unchecked(delegate) };
    let session = unsafe {
        NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
            &config,
            Some(&delegate_obj),
            None,
        )
    };
    let task = session.downloadTaskWithURL(url);
    task.resume();
    *state.session.lock().expect("session poisoned") = Some(session);
    *state.task.lock().expect("task poisoned") = Some(task.clone());
    window.bind_cancel_task(&task);
    // The delegate is only retained by the session while the transfer runs;
    // forgetting it here matches the session's own lifetime.
    std::mem::forget(delegate_obj);
}

// MARK: Blocking variant for `--selftest download`

/// `--selftest download <url> [out]`: the downloader exercised for real —
/// the shared session's data task is enough here (bytes + failure path).
pub fn fetch_blocking(url: &NSURL, dest: &std::path::Path) -> Result<u64, DownloadError> {
    let _ = std::fs::remove_file(dest);
    let (tx, rx) = mpsc::sync_channel::<Result<u64, DownloadError>>(1);
    let dest = dest.to_path_buf();
    let block = RcBlock::new(
        move |data: *mut NSData, _resp: *mut objc2_foundation::NSURLResponse, error: *mut NSError| {
            let result = if !error.is_null() {
                Err(DownloadError::Failed(
                    unsafe { &*error }.localizedDescription().to_string(),
                ))
            } else if !data.is_null() {
                match std::fs::write(&dest, unsafe { (&*data).as_bytes_unchecked() }) {
                    Ok(()) => Ok(std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0)),
                    Err(e) => Err(DownloadError::Failed(e.to_string())),
                }
            } else {
                Err(DownloadError::Failed("no data".into()))
            };
            let _ = tx.send(result);
        },
    );
    let session = NSURLSession::sharedSession();
    let task = unsafe { session.dataTaskWithURL_completionHandler(url, &block) };
    task.resume();
    rx.recv().map_err(|e| DownloadError::Failed(e.to_string()))?
}

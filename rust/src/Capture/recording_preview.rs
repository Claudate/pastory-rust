//! Port of `Capture/RecordingPreviewWindow.swift`.
//!
//! Plays the fresh recording on loop so you can judge it before it goes
//! anywhere. Paper look: our own transport bar (play, time, blue progress,
//! duration), then 丢弃 · 复制为 GIF · 复制为 MP4.
//!
//! AVKitUI has no binding crate: `AVPlayerView` is hand-declared (setPlayer,
//! controlsStyle none, resizeAspect).

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
#[allow(deprecated)]
use objc2::{define_class, msg_send, msg_send_id, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSColor, NSEvent, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_av_foundation::AVPlayer;
use objc2_app_kit::NSStringDrawing;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSString, NSURL};

use crate::app::{localization::l, theme};
use crate::capture::selection_overlay::paper_button_export;

// MARK: Hand-declared AVPlayerView (QLPreviewPanel 同款 extern class)

objc2::extern_class!(
    /// AVPlayerView lives in AVKit, linked via the stub below.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[derive(Debug)]
    pub struct AVPlayerView;
);

type AVPlayerViewWeak = *mut AVPlayerView;


#[link(name = "AVKit", kind = "framework")]
unsafe extern "C-unwind" {}

fn av_player_view_new(frame: CGRect, player: &AVPlayer) -> Retained<NSView> {
    let alloc: *mut AVPlayerView = unsafe { msg_send![objc2::class!(AVPlayerView), alloc] };
    // SAFETY: AVPlayerView is created alive; msg_send_id is the only
    // spelling objc2 0.6 accepts for retained receivers on init selectors
    // here (plain msg_send! lacks the Encode hop for extern_class types).
    #[allow(deprecated)]
    let v: Retained<AVPlayerView> = unsafe {
        let this: *mut AVPlayerView = msg_send_id![alloc, initWithFrame: frame];
        objc2::rc::Retained::<AVPlayerView>::from_raw(this).expect("av player view non-null")
    };
    unsafe {
        let _: () = msg_send![&*v, setPlayer: player];
        // AVPlayerViewControlsStyleNone = 0
        let _: () = msg_send![&*v, setControlsStyle: 0_isize];
        let gravity = NSString::from_str("AVLayerVideoGravityResizeAspect");
        let _: () = msg_send![&*v, setVideoGravity: &*gravity];
        let _: () = msg_send![&*v, setWantsLayer: true];
        if let Some(layer) = v.layer() {
            let _: () = msg_send![&*layer, setCornerRadius: 4.0f64];
            let _: () = msg_send![&*layer, setMasksToBounds: true];
        }
    }
    unsafe {
        Retained::cast_unchecked(v)
    }
}

// MARK: TransportBar

const TRANSPORT_H: f64 = 52.0;

pub struct TransportBarIvars {
    duration: Cell<f64>,
    current: Cell<f64>,
    playing: Cell<bool>,
    on_toggle: RefCell<Option<Box<dyn Fn() + 'static>>>,
    on_seek: RefCell<Option<Box<dyn Fn(f64) + 'static>>>,
}

impl Default for TransportBarIvars {
    fn default() -> Self {
        Self {
            duration: Cell::new(0.01),
            current: Cell::new(0.0),
            playing: Cell::new(false),
            on_toggle: RefCell::new(None),
            on_seek: RefCell::new(None),
        }
    }
}

/// ▶ 00:01 ────●──── 00:11 — click or drag the track to seek.
pub struct TransportBar;

define_class!(
    // SAFETY: hand-drawn view, main thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TransportBarIvars]
    pub struct TransportBarImpl;

    unsafe impl NSObjectProtocol for TransportBarImpl {}

    impl TransportBarImpl {
        #[unsafe(method(drawRect:))]
        fn tb_draw_rect(&self, _dirty: CGRect) {
            self.draw_contents();
        }

        #[unsafe(method(mouseDown:))]
        fn tb_mouse_down(&self, event: &NSEvent) {
            self.handle_seek_event(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn tb_mouse_dragged(&self, event: &NSEvent) {
            self.handle_seek_event(event);
        }
    }
);

const BUTTON_SIZE: f64 = 36.0;

impl TransportBarImpl {
    fn track_rect(&self) -> CGRect {
        let b = self.bounds();
        CGRect::new(
            CGPoint::new(BUTTON_SIZE + 14.0 + 52.0, (b.min().y + b.size.height / 2.0) - 3.0),
            CGSize::new(b.size.width - (BUTTON_SIZE + 14.0 + 52.0) - 52.0, 6.0),
        )
    }

    fn clock(t: f64) -> String {
        let s = t as i64;
        format!("{:02}:{:02}", s / 60, s % 60)
    }

    pub fn make(duration: f64) -> Retained<Self> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<Self>().set_ivars(TransportBarIvars::default());
        let v: Retained<Self> = unsafe { msg_send![super(this), init] };
        v.ivars().duration.set(duration.max(0.01));
        v
    }

    pub fn set_on_toggle(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_toggle.borrow_mut() = Some(f);
    }

    pub fn set_on_seek(&self, f: Box<dyn Fn(f64) + 'static>) {
        *self.ivars().on_seek.borrow_mut() = Some(f);
    }

    pub fn set_current(&self, t: f64) {
        self.ivars().current.set(t);
        self.setNeedsDisplay(true);
    }

    pub fn set_playing(&self, on: bool) {
        self.ivars().playing.set(on);
        self.setNeedsDisplay(true);
    }

    pub fn playing(&self) -> bool {
        self.ivars().playing.get()
    }

    fn handle_seek_event(&self, event: &NSEvent) {
        let pt = self.convertPoint_fromView(event.locationInWindow(), None);
        if pt.x <= BUTTON_SIZE + 6.0 {
            if let Some(f) = self.ivars().on_toggle.borrow().as_ref() {
                f();
            }
            return;
        }
        self.seek_to(pt);
    }

    fn seek_to(&self, p: CGPoint) {
        let tr = self.track_rect();
        let inset = crate::app::coordinates::inset_rect(tr, -20.0, -14.0);
        let mid = tr.origin.y + tr.size.height / 2.0;
        if !crate::app::coordinates::contains_pt(inset, p) && (p.y - mid).abs() >= 20.0 {
            return;
        }
        let f = ((p.x - tr.origin.x) / tr.size.width).min(1.0).max(0.0);
        let t = f * self.ivars().duration.get();
        self.set_current(t);
        if let Some(cb) = self.ivars().on_seek.borrow().as_ref() {
            cb(t);
        }
    }

    fn draw_contents(&self) {
        let bounds = self.bounds();
        // Play / pause disc
        let disc = CGRect::new(
            CGPoint::new(0.0, bounds.min().y + bounds.size.height / 2.0 - BUTTON_SIZE / 2.0),
            CGSize::new(BUTTON_SIZE, BUTTON_SIZE),
        );
        theme::paper().setFill();
        objc2_app_kit::NSBezierPath::bezierPathWithOvalInRect(disc).fill();
        theme::ink().colorWithAlphaComponent(0.5).setStroke();
        objc2_app_kit::NSBezierPath::bezierPathWithOvalInRect(
            crate::app::coordinates::inset_rect(disc, 0.5, 0.5),
        )
        .stroke();
        // Play/pause glyph, tinted by theme ink
        let name = if self.playing() { "pause.fill" } else { "play.fill" };
        if let Some(icon) = symbol_icon(name, 14.0, unsafe { objc2_app_kit::NSFontWeightBold }) {
            let tinted = tint(icon, &theme::ink());
            let s = tinted.size();
            let off = if self.playing() { 0.0 } else { 1.0 };
            tinted.drawInRect(CGRect::new(
                CGPoint::new(
                    disc.origin.x + disc.size.width / 2.0 - s.width / 2.0 + off,
                    disc.origin.y + disc.size.height / 2.0 - s.height / 2.0,
                ),
                s,
            ));
        }
        // times
        let attrs = crate::shelf::card::attrs(&theme::serif(14.0, false), &theme::on_brown(), None);
        let current_ns = objc2_foundation::NSString::from_str(&Self::clock(self.ivars().current.get()));
        unsafe {
            current_ns.drawAtPoint_withAttributes(
                CGPoint::new(BUTTON_SIZE + 14.0, bounds.min().y + bounds.size.height / 2.0 - 8.0),
                Some(&attrs),
            );
        }
        let total_ns = objc2_foundation::NSString::from_str(&Self::clock(self.ivars().duration.get()));
        let ts = unsafe { total_ns.sizeWithAttributes(Some(&attrs)) };
        unsafe {
            total_ns.drawAtPoint_withAttributes(
                CGPoint::new(
                    bounds.max().x - ts.width,
                    bounds.min().y + bounds.size.height / 2.0 - 8.0,
                ),
                Some(&attrs),
            );
        }
        // Track
        let tr = self.track_rect();
        theme::on_brown().colorWithAlphaComponent(0.18).setFill();
        objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(tr, 3.0, 3.0).fill();
        let f = (self.ivars().current.get() / self.ivars().duration.get()).min(1.0).max(0.0);
        theme::paper_blue().setFill();
        objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            CGRect::new(tr.origin, CGSize::new(tr.size.width * f, tr.size.height)),
            3.0,
            3.0,
        )
        .fill();
        let knob = CGRect::new(
            CGPoint::new(tr.origin.x + tr.size.width * f - 7.0, (tr.origin.y + tr.size.height / 2.0) - 7.0),
            CGSize::new(14.0, 14.0),
        );
        theme::paper().setFill();
        objc2_app_kit::NSBezierPath::bezierPathWithOvalInRect(knob).fill();
        theme::ink().colorWithAlphaComponent(0.6).setStroke();
        objc2_app_kit::NSBezierPath::bezierPathWithOvalInRect(
            crate::app::coordinates::inset_rect(knob, 0.5, 0.5),
        )
        .stroke();
    }
}

fn symbol_icon(name: &str, point: f64, weight: f64) -> Option<Retained<objc2_app_kit::NSImage>> {
    let img = objc2_app_kit::NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &objc2_foundation::NSString::from_str(name),
        None,
    )?;
    let cfg = objc2_app_kit::NSImageSymbolConfiguration::configurationWithPointSize_weight(point, weight);
    img.imageWithSymbolConfiguration(&cfg)
}

/// `NSImage tinted` — sourceAtop color fill (draws the symbol in ink).
fn tint(img: Retained<objc2_app_kit::NSImage>, color: &NSColor) -> Retained<objc2_app_kit::NSImage> {
    let s = img.size();
    let (w, h) = (s.width as usize, s.height as usize);
    let (w2, h2) = (w * 2, h * 2);
    let space = objc2_core_graphics::CGColorSpace::new_device_rgb();
    let Some(ctx) = (unsafe {
        objc2_core_graphics::CGBitmapContextCreate(std::ptr::null_mut(), w2, h2, 8, 0, space.as_deref(), 1)
    }) else {
        return img;
    };
    let gc = objc2_app_kit::NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, true);
    let prev = objc2_app_kit::NSGraphicsContext::currentContext();
    objc2_app_kit::NSGraphicsContext::setCurrentContext(Some(&gc));
    let dst = CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64));
    objc2_core_graphics::CGContext::scale_ctm(Some(&ctx), 2.0, 2.0);
    unsafe { img.drawInRect_fromRect_operation_fraction_respectFlipped_hints(dst, CGRect::ZERO, objc2_app_kit::NSCompositingOperation::SourceOver, 1.0, true, None) };
    color.setFill();
    objc2_app_kit::NSRectFillUsingOperation(dst, objc2_app_kit::NSCompositingOperation::SourceAtop);
    objc2_app_kit::NSGraphicsContext::setCurrentContext(prev.as_deref());
    let mask_ctx = objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx));
    match mask_ctx {
        Some(cg) => {
            let mtm = MainThreadMarker::new().expect("main thread");
             objc2_app_kit::NSImage::initWithCGImage_size(mtm.alloc(), &cg, s)
        }
        None => img,
    }
}

// MARK: RecordingPreviewWindow

const PAD: f64 = 24.0;
const BUTTONS_H: f64 = 64.0;

pub struct RecordingPreviewWindowIvars {
    window: RefCell<Option<Retained<NSWindow>>>,
    player: RefCell<Option<Retained<AVPlayer>>>,
    looper: RefCell<Option<Retained<AnyObject>>>,
    time_observer: RefCell<Option<Retained<AnyObject>>>,
    info: RefCell<Option<Retained<objc2_app_kit::NSTextField>>>,
    buttons: RefCell<Vec<Retained<objc2_app_kit::NSButton>>>,
    transport: RefCell<Option<Retained<TransportBarImpl>>>,
    on_choose: RefCell<Option<Box<dyn Fn(bool) + 'static>>>,
    on_discard: RefCell<Option<Box<dyn Fn() + 'static>>>,
    decided: Cell<bool>,
}

impl Default for RecordingPreviewWindowIvars {
    fn default() -> Self {
        Self {
            window: RefCell::new(None),
            player: RefCell::new(None),
            looper: RefCell::new(None),
            time_observer: RefCell::new(None),
            info: RefCell::new(None),
            buttons: RefCell::new(Vec::new()),
            transport: RefCell::new(None),
            on_choose: RefCell::new(None),
            on_discard: RefCell::new(None),
            decided: Cell::new(false),
        }
    }
}

define_class!(
    // SAFETY: NSObject shell owning the window; main thread only.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RecordingPreviewWindowIvars]
    pub struct RecordingPreviewWindow;

    unsafe impl NSObjectProtocol for RecordingPreviewWindow {}

    impl RecordingPreviewWindow {
        #[unsafe(method(discardTapped:))]
        fn discard_tapped(&self, _sender: &AnyObject) {
            self.ivars().decided.set(true);
            if let Some(f) = self.ivars().on_discard.borrow().as_ref() {
                f();
            }
        }

        #[unsafe(method(gifTapped:))]
        fn gif_tapped(&self, _sender: &AnyObject) {
            self.ivars().decided.set(true);
            if let Some(f) = self.ivars().on_choose.borrow().as_ref() {
                f(true);
            }
        }

        #[unsafe(method(mp4Tapped:))]
        fn mp4_tapped(&self, _sender: &AnyObject) {
            self.ivars().decided.set(true);
            if let Some(f) = self.ivars().on_choose.borrow().as_ref() {
                f(false);
            }
        }
    }

    unsafe impl NSWindowDelegate for RecordingPreviewWindow {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &objc2_foundation::NSNotification) {
            // Closing the window with the red button counts as discarding.
            if !self.ivars().decided.replace(true) {
                if let Some(f) = self.ivars().on_discard.borrow().as_ref() {
                    f();
                }
            }
        }
    }
);

impl RecordingPreviewWindow {
    /// `init(movie:duration:pixelSize:near:)` — build the window exactly at
    /// Swift geometry.
    pub fn new(
        movie: &std::path::Path,
        duration: f64,
        pixel_size: CGSize,
        anchor: CGRect,
    ) -> Retained<RecordingPreviewWindow> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<RecordingPreviewWindow>().set_ivars(RecordingPreviewWindowIvars::default());
        let me: Retained<RecordingPreviewWindow> = unsafe { msg_send![super(this), init] };

        let url = NSURL::fileURLWithPath(&NSString::from_str(&movie.to_string_lossy()));
        let player = unsafe { AVPlayer::playerWithURL(&url, mtm) };
        unsafe {
            player.setMuted(true);
        }
        let transport = TransportBarImpl::make(duration);

        let screen = crate::annotate::ocr_panel::screens_intersecting(mtm, anchor);
        let vf = screen.as_ref()
            .map(|s| s.visibleFrame())
            .unwrap_or(CGRect::new(CGPoint::ZERO, CGSize::new(1440.0, 900.0)));
        let scale = screen.as_ref().map(|s| s.backingScaleFactor()).unwrap_or(2.0);
        let natural = CGSize::new(
            1.0f64.max(pixel_size.width / scale),
            1.0f64.max(pixel_size.height / scale),
        );
        let k = (((vf.size.width * 0.7 - 48.0) / natural.width)
            .min((vf.size.height * 0.7 - 160.0) / natural.height))
        .min(1.0);
        let video_size = CGSize::new(
            420.0f64.max((natural.width * k).round()),
            220.0f64.max((natural.height * k).round()),
        );
        let rect = CGRect::new(
            CGPoint::ZERO,
            CGSize::new(
                video_size.width + PAD * 2.0,
                44.0 + video_size.height + TRANSPORT_H + BUTTONS_H,
            ),
        );
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                rect,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::FullSizeContentView,
                objc2_app_kit::NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str(&l("录屏预览")));
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(objc2_app_kit::NSWindowTitleVisibility::Hidden);
        window.setAppearance(Some(&objc2_app_kit::NSAppearance::appearanceNamed(
            unsafe { NSAppearanceNameDarkAqua },
        )
        .expect("darkAqua")));
        window.setBackgroundColor(Some(&theme::brown()));
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        window.setLevel(3); // .floating
        {
            let proto: &ProtocolObject<dyn NSWindowDelegate> = ProtocolObject::from_ref(&*me);
            window.setDelegate(Some(proto));
        }
        *me.ivars().window.borrow_mut() = Some(window.clone());

        // Content + player view.
        let content = crate::annotate::ocr_panel::ground_view_export(rect);
        let pv = av_player_view_new(
            CGRect::new(
                CGPoint::new(PAD, TRANSPORT_H + BUTTONS_H),
                video_size,
            ),
            &player,
        );
        content.addSubview(&pv);

        transport.setFrame(CGRect::new(
            CGPoint::new(PAD, BUTTONS_H),
            CGSize::new(video_size.width, TRANSPORT_H),
        ));
        *me.ivars().transport.borrow_mut() = Some(transport.clone());
        {
            let tr_owned = me.ivars().transport.borrow().clone();
            let tr: &TransportBarImpl = &*tr_owned.as_ref().unwrap();
            let me_raw = &*me as *const RecordingPreviewWindow as usize;
            tr.set_on_toggle(Box::new(move || {
                if let Some(m) = unsafe { (me_raw as *const RecordingPreviewWindow).as_ref() } {
                    m.toggle_play();
                }
            }));
            tr.set_on_seek(Box::new(move |t| {
                if let Some(m) = unsafe { (me_raw as *const RecordingPreviewWindow).as_ref() } {
                    unsafe {
                        if let Some(p) = m.ivars().player.borrow().as_ref() {
                            p.seekToTime_toleranceBefore_toleranceAfter(
                                objc2_core_media::CMTime {
                                    value: (t * 600.0) as i64,
                                    timescale: 600,
                                    flags: objc2_core_media::CMTimeFlags::Valid,
                                    epoch: 0,
                                },
                                objc2_core_media::CMTime {
                                    value: 0,
                                    timescale: 600,
                                    flags: objc2_core_media::CMTimeFlags::Valid,
                                    epoch: 0,
                                },
                                objc2_core_media::CMTime {
                                    value: 0,
                                    timescale: 600,
                                    flags: objc2_core_media::CMTimeFlags::Valid,
                                    epoch: 0,
                                },
                            );
                        }
                    }
                }
            }));
            content.addSubview(tr);
        }
        *me.ivars().player.borrow_mut() = Some(player.clone());

        // Bottom row: info (left), 丢弃 · 复制为 GIF · 复制为 MP4 (right).
        let info = objc2_app_kit::NSTextField::labelWithString(&NSString::from_str(""), mtm);
        info.setFont(Some(&theme::serif(13.0, false)));
        info.setTextColor(Some(&theme::on_brown_muted()));
        info.setFrame(CGRect::new(
            CGPoint::new(PAD, 20.0),
            CGSize::new((rect.size.width - PAD * 2.0) / 2.0, 20.0),
        ));
        content.addSubview(&info);
        *me.ivars().info.borrow_mut() = Some(info);

        // SAFETY: no-memory-share issue here; buttons hold the delegate target as needed.
        {
            let me_obj: &AnyObject = unsafe { &*(&*me as *const RecordingPreviewWindow as *const AnyObject) };
            let w = rect.size.width;
            let mut rx = w - PAD;
            for (title, primary, ground, selector) in [
                (l("丢弃"), false, true, sel!(discardTapped:)),
                (
                    if duration > 20.0 {
                        l("复制为 GIF（会糊，建议 MP4）")
                    } else {
                        l("复制为 GIF")
                    },
                    false,
                    true,
                    sel!(gifTapped:),
                ),
                (l("复制为 MP4"), true, false, sel!(mp4Tapped:)),
            ]
            .iter()
            .rev()
            {
                let b = paper_button_export(title, *primary, *ground, Some(me_obj), *selector);
                let bw = b.frame().size.width;
                rx -= bw;
                b.setFrameOrigin(CGPoint::new(rx, 15.0));
                rx -= 8.0;
                content.addSubview(&b);
                me.ivars().buttons.borrow_mut().push(b);
            }
            window.contentView().map(|_c| ());
        }

        // Buttons laid out; content in.
        content.setFrame(rect);
        window.setContentView(Some(&content));
        // Place over the anchor: mid, clamped to the screen.
        {
            let mut origin = CGPoint::new(
                (anchor.origin.x + anchor.size.width / 2.0) - rect.size.width / 2.0,
                (anchor.origin.y + anchor.size.height / 2.0) - rect.size.height / 2.0,
            );
            origin.x = origin.x.max(vf.min().x + 12.0).min(vf.max().x - rect.size.width - 12.0);
            origin.y = origin.y.max(vf.min().y + 12.0).min(vf.max().y - rect.size.height - 12.0);
            window.setFrameOrigin(origin);
        }

        // Loop on end; transport follows the clock.
        {
            let me_raw = &*me as *const RecordingPreviewWindow as usize;
            let center = objc2_foundation::NSNotificationCenter::defaultCenter();
            let looper = unsafe {
                let name = objc2_av_foundation::AVPlayerItemDidPlayToEndTimeNotification;
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    player.currentItem().map(|i| {
                        &*(objc2::rc::Retained::as_ptr(&i) as *const AnyObject)
                    }),
                    None,
                    &block2::RcBlock::new(move |_notification: std::ptr::NonNull<objc2_foundation::NSNotification>| {
                        if let Some(m) = (me_raw as *const RecordingPreviewWindow).as_ref() {
                            if let Some(p) = m.ivars().player.borrow().as_ref() {
                                p.seekToTime(objc2_core_media::kCMTimeZero);
                                p.play();
                            }
                        }
                    }),
                )
            };
            *me.ivars().looper.borrow_mut() = Some(unsafe { Retained::cast_unchecked(looper) });
            let tr_raw = objc2::rc::Retained::as_ptr(me.ivars().transport.borrow().as_ref().unwrap()) as *const TransportBarImpl as usize;
            let observer = unsafe {
                me.ivars().player.borrow().as_ref().unwrap().addPeriodicTimeObserverForInterval_queue_usingBlock(
                    objc2_core_media::CMTime {
                        value: 30,
                        timescale: 600,
                        flags: objc2_core_media::CMTimeFlags::Valid,
                        epoch: 0,
                    },
                    None,
                    &block2::RcBlock::new(move |t: objc2_core_media::CMTime| {
                        if t.timescale != 0 {
                            let secs = t.value as f64 / t.timescale as f64;
                            if let Some(tr) = (tr_raw as *const TransportBarImpl).as_ref() {
                                tr.set_current(secs);
                            }
                        }
                    }),
                )
            };
            *me.ivars().time_observer.borrow_mut() = Some(observer);
        }
        me
    }

    /// `present()`.
    pub fn present(&self) {
        let Some(window) = self.ivars().window.borrow().clone() else { return };
        {
            let mtm = MainThreadMarker::new().expect("main thread");
            let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            unsafe {
                // SAFETY: modern activation first; the legacy call is the fallback.
                let _: () = msg_send![&app, activate];
            }
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
        let sender: Option<&AnyObject> = None;
        window.makeKeyAndOrderFront(sender);
        if let Some(p) = self.ivars().player.borrow().as_ref() {
            unsafe {
                p.play();
            }
        }
        if let Some(tr) = self.ivars().transport.borrow().as_ref() {
            tr.set_playing(true);
        }
        // Loop the player around when the end is reached.
    }

    fn toggle_play(&self) {
        let Some(p) = self.ivars().player.borrow().clone() else { return };
        let status = unsafe { p.timeControlStatus() };
        if status == objc2_av_foundation::AVPlayerTimeControlStatus::Playing {
            unsafe { p.pause() }
            if let Some(tr) = self.ivars().transport.borrow().as_ref() {
                tr.set_playing(false);
            }
        } else {
            unsafe { p.play() }
            if let Some(tr) = self.ivars().transport.borrow().as_ref() {
                tr.set_playing(true);
            }
        }
    }

    /// `setBusy(_:)` — buttons dim, the line says what is happening.
    pub fn set_busy(&self, text: &str) {
        if let Some(info) = self.ivars().info.borrow().as_ref() {
            info.setStringValue(&NSString::from_str(text));
        }
        for b in self.ivars().buttons.borrow().iter() {
            b.setEnabled(false);
            b.setAlphaValue(0.4);
        }
    }

    /// Back to choosing (`setIdle`); the recording is still there.
    pub fn set_idle(&self, text: &str) {
        if let Some(info) = self.ivars().info.borrow().as_ref() {
            info.setStringValue(&NSString::from_str(text));
        }
        for b in self.ivars().buttons.borrow().iter() {
            b.setEnabled(true);
            b.setAlphaValue(1.0);
        }
    }

    /// `close()` — teardown (Swift's recording_session closes it too).
    pub fn close(&self) {
        self.ivars().decided.set(true);
        if let Some(p) = self.ivars().player.borrow().as_ref() {
            unsafe {
                p.pause();
            }
        }
        if let Some(looper) = self.ivars().looper.borrow_mut().take() {
            unsafe {
                objc2_foundation::NSNotificationCenter::defaultCenter().removeObserver(&*looper);
            }
        }
        if let Some(to) = self.ivars().time_observer.borrow_mut().take() {
            unsafe {
                self.ivars().player.borrow().as_ref().unwrap()
                    .removeTimeObserver(&to);
            }
        }
        if let Some(window) = self.ivars().window.borrow().as_ref() {
            let sender: Option<&AnyObject> = None;
            window.orderOut(sender);
            window.close();
        }
    }

    /// Self-test only (`debugContentView`).
    pub fn debug_content_view(&self) -> Option<Retained<NSView>> {
        self.ivars().window.borrow().as_ref().and_then(|w| w.contentView())
    }

    /// Self-test only (`debugSeek`).
    pub fn debug_seek(&self, t: f64) {
        if let Some(tr) = self.ivars().transport.borrow().as_ref() {
            tr.set_current(t);
        }
    }
}

impl RecordingPreviewWindow {
    /// `onChoose(gif:)` — Swift assigns and teardown triggers it.
    pub fn set_on_choose(&self, f: Box<dyn Fn(bool) + 'static>) {
        *self.ivars().on_choose.borrow_mut() = Some(f);
    }

    /// `onDiscard` — Swift's recording-cancellation path.
    pub fn set_on_discard(&self, f: Box<dyn Fn() + 'static>) {
        *self.ivars().on_discard.borrow_mut() = Some(f);
    }
}

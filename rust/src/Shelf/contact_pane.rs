//! Port of `Shelf/ContactPane.swift` — 联系我: four paper sheets —
//! feedback group, GitHub star, X, buy me a coffee.
//!
//! Same hand-drawn shape as the settings pane: one view, hit rects in ivars.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBezierPath, NSEvent, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSString, NSURL};

use crate::app::localization::l;
use crate::app::theme;

/// 商店版保留这一格（唯一合规的自有入口）；直发版已按用户要求去掉开源地址。
#[cfg(feature = "mas")]
pub const REPO_URL: &str = "https://github.com/Claudate/pastory-rust";
/// 商店版不出现在任何渠道入口（提审约束），见下方 `draw_sheets`。
#[cfg(not(feature = "mas"))]
pub const X_URL: &str = "https://x.com/smithjacky521";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Link {
    Close,
    #[cfg(feature = "mas")]
    Repo,
    #[cfg(not(feature = "mas"))]
    X,
}

pub struct ContactViewIvars {
    hits: RefCell<Vec<(Link, CGRect)>>,
}

define_class!(
    // SAFETY: plain NSView drawing the four sheets; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ContactViewIvars]
    pub struct ContactView;

    unsafe impl NSObjectProtocol for ContactView {}

    impl ContactView {
        #[unsafe(method(isFlipped))]
        fn cv_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn cv_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn cv_draw_rect(&self, _dirty: CGRect) {
            draw(self);
        }

        #[unsafe(method(mouseUp:))]
        fn cv_mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            for (link, rect) in self.ivars().hits.borrow().iter() {
                if contains(rect, p) {
                    run_link(*link);
                    return;
                }
            }
        }
    }
);

impl ContactView {
    pub fn make(frame: CGRect) -> Retained<ContactView> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<ContactView>().set_ivars(ContactViewIvars {
            hits: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

fn contains(r: &CGRect, p: CGPoint) -> bool {
    p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y
}

fn run_link(link: Link) {
    match link {
        Link::Close => crate::shelf::panel::hide(),
        #[cfg(feature = "mas")]
        Link::Repo => open_url(REPO_URL),
        #[cfg(not(feature = "mas"))]
        Link::X => open_url(X_URL),
    }
}

fn open_url(s: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(s)) {
        objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

// MARK: Draw

fn draw(view: &ContactView) {
    let b = view.bounds();
    // Same fix as the settings pane: the view's frame already is the content
    // column, so Swift's `.padding(.horizontal, 24)` lands at x=4 within it.
    // Sheets start at the leading edge — they do not stretch
    // (Swift's ContactPane frames every sheet at a fixed 230pt, leading-aligned).
    let cx = 4.0;
    let cw = (b.size.width - 8.0).max(0.0);
    view.ivars().hits.borrow_mut().clear();
    // Header
    let title_font = theme::serif(22.0, true);
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(cx, 18.0), CGSize::new(400.0, crate::shelf::card::line_height(&title_font))),
        &l("联系我"),
        &title_font,
        &theme::label(),
        None,
    );
    header_button(view, CGRect::new(CGPoint::new(cx + cw - 40.0, 18.0), CGSize::new(40.0, 36.0)), "✕", Link::Close, false);
    let top = 18.0 + 44.0 + 14.0;
    draw_sheets(view, cx, top);
}

/// 联系格子。直发版四栏（微信群 / 个人微信 / X / 买咖啡，开源地址已去掉）；
/// 商店版（`mas`）只留自有 GitHub 一栏——微信 / X / 买咖啡是第三方或
/// 个人渠道，提审时不出现（见 docs/MAS_PLAN.md §0）。
fn draw_sheets(view: &ContactView, cx: f64, top: f64) {
    #[cfg(feature = "mas")]
    {
        sheet(view, cx, top, &l("GitHub 点个 Star 吧"), &l("开源产品，喜欢就请助力一下"), SheetArt::Link("Claudate/pastory-rust", Link::Repo));
    }

    #[cfg(not(feature = "mas"))]
    {
        let mut x = cx;
        sheet(view, x, top, &l("反馈 bug & 提功能"), &l("PasteJar 微信小小群，交个朋友"), SheetArt::Qr(theme::resource_named("WeChatGroup.png")));
        x += 246.0;
        sheet(view, x, top, &l("加我个人微信"), &l("直接找我聊，备注一下 PasteJar"), SheetArt::Qr(theme::resource_named("WeChatPersonal.png")));
        x += 246.0;
        sheet(view, x, top, &l("关注我的 X"), &l("新版本和碎碎念都在这。"), SheetArt::Link("@smithjacky521", Link::X));
        x += 246.0;
        sheet(view, x, top, "Buy me a coffee", &l("觉得好用，请我喝一杯。"), SheetArt::Qr(theme::resource_named("Coffee.png")));
    }
}

fn header_button(view: &ContactView, rect: CGRect, s: &str, link: Link, wide: bool) {
    let font = if wide { theme::serif(13.0, false) } else { theme::serif(16.0, false) };
    let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 18.0, 18.0);
    theme::label().colorWithAlphaComponent(0.4).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    let h = crate::shelf::card::line_height(&font);
    let tw = crate::shelf::card::measure(s, &font, 400.0, None).width;
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(rect.min().x + (rect.size.width - tw) / 2.0, rect.min().y + (rect.size.height - h) / 2.0), CGSize::new(tw + 1.0, h)),
        s,
        &font,
        &theme::label(),
        None,
    );
    view.ivars().hits.borrow_mut().push((link, rect));
}

enum SheetArt {
    Qr(Option<Retained<objc2_app_kit::NSImage>>),
    Link(&'static str, Link),
}

/// One equal-size sheet: torn brown label, two-line hint, the slot.
fn sheet(view: &ContactView, x: f64, y: f64, title: &str, line: &str, art: SheetArt) {
    let w = 230.0;
    let h = 260.0;
    let rect = CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));
    {
        objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
        crate::shelf::card::shadow(&objc2_app_kit::NSColor::blackColor().colorWithAlphaComponent(0.35), 8.0, 1.0, 4.0).set();
        theme::surface().setFill();
        let r = theme::radius_card();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, r, r).fill();
        objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
    }
    // Section title: plain secondary type, nothing drawn behind it.
    let font = theme::semibold(13.0);
    let tw = crate::shelf::card::measure(title, &font, 400.0, None).width;
    let label = CGRect::new(
        CGPoint::new(rect.min().x + 16.0, rect.min().y + 14.0),
        CGSize::new(tw + 20.0, crate::shelf::card::line_height(&font) + 6.0),
    );
    crate::shelf::card::draw_line_centered(title, &font, &theme::label_secondary(), label.min().x, label.mid().y, tw + 1.0);
    // One-line hint (fixed 44pt).
    crate::shelf::card::draw_text(
        CGRect::new(
            CGPoint::new(rect.min().x + 16.0, label.max().y + 8.0),
            CGSize::new(w - 32.0, 36.0),
        ),
        line,
        &theme::serif(14.0, false),
        &theme::label(),
        Some(1.0),
    );
    // The slot (QR or the link capsule).
    let slot_top = label.max().y + 14.0 + 36.0;
    let slot = CGRect::new(
        CGPoint::new(rect.min().x + 16.0, slot_top),
        CGSize::new(w - 32.0, (rect.max().y - 8.0) - slot_top - 8.0),
    );
    match art {
        SheetArt::Qr(Some(img)) => {
            // Swift's `aspectRatio(.fit).frame(120×120)`: scale INTO the slot;
            // `draw_centered` keeps native size and blows past it.
            let s = 120.0f64.min(slot.size.width).min(slot.size.height);
            let is = img.size();
            let fit = (s / is.width).min(s / is.height);
            let fw = is.width * fit;
            let fh = is.height * fit;
            let q = CGRect::new(
                CGPoint::new(slot.mid().x - fw / 2.0, slot.min().y + (slot.size.height - fh) / 2.0),
                CGSize::new(fw, fh),
            );
            let white = CGRect::new(
                CGPoint::new(q.min().x - 6.0, q.min().y - 6.0),
                CGSize::new(q.size.width + 12.0, q.size.height + 12.0),
            );
            objc2_app_kit::NSColor::whiteColor().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(white, 10.0, 10.0).fill();
            crate::shelf::card::draw_image(
                &img,
                q,
                objc2_app_kit::NSCompositingOperation::SourceOver,
                1.0,
            );
        }
        SheetArt::Qr(None) => {}
        SheetArt::Link(label, link) => {
            let font = objc2_app_kit::NSFont::systemFontOfSize(13.0);
            let text = format!("  ↗ {}", label);
            let tw = crate::shelf::card::measure(&text, &font, 300.0, None).width;
            let cell = CGRect::new(
                CGPoint::new(slot.mid().x - (tw + 28.0) / 2.0, slot.min().y + (slot.size.height - 28.0) / 2.0),
                CGSize::new(tw + 28.0, 28.0),
            );
            objc2_app_kit::NSColor::whiteColor().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(cell, 14.0, 14.0).fill();
            theme::label().colorWithAlphaComponent(0.35).setStroke();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(cell, 14.0, 14.0).stroke();
            crate::shelf::card::draw_line_centered(&text, &font, &theme::label(), cell.min().x + 14.0, cell.mid().y, tw + 1.0);
            view.ivars().hits.borrow_mut().push((link, cell));
        }
    }
}

/// Self-test hook: why a hotspot is (not) there. Reports what the
/// display cycle checks — window visibility, hidden ancestors, the real
/// frame (bounds alone hide an off-window origin) — so a pane that never
/// draws tells us which check failed.
pub fn selftest_diag(view: &ContactView) -> String {
    let mut chain = String::new();
    let mut v = unsafe { view.superview() };
    while let Some(sup) = v {
        chain.push_str(&format!(
            " › {}{}",
            sup.class().name().to_string_lossy(),
            if sup.isHidden() { "(hidden)" } else { "" }
        ));
        // SAFETY: main-thread self-test diagnostics on a live view; the
        // returned parent (if any) is valid for the loop's next read.
        v = unsafe { sup.superview() };
    }
    let win_state = view
        .window()
        .map(|w| {
            format!(
                "visible={} occlusion={:?} viewsNeedDisplay={:?} num={} frame={:?}",
                w.isVisible(),
                w.occlusionState(),
                w.viewsNeedDisplay(),
                w.windowNumber(),
                w.frame()
            )
        })
        .unwrap_or_else(|| "no-window".into());
    format!(
        "hidden={} hidden-ancestor={} win=({}) chain=[{} ] frame={:?} needs={} bounds={:?} hits={}",
        view.isHidden(),
        view.isHiddenOrHasHiddenAncestor(),
        win_state,
        chain,
        view.frame(),
        view.needsDisplay(),
        view.bounds(),
        view.ivars().hits.borrow().len(),
    )
}

/// Self-test hook: deliver a click at `p` (this view's coordinates) through
/// the real `mouseUp:` route — hit-testing, `run_link` and whatever that
/// schedules all run exactly as they do for a user's click.
pub fn selftest_click_at(view: &ContactView, p: CGPoint) -> bool {
    let Some(window) = view.window() else {
        return false;
    };
    let loc = view.convertPoint_toView(p, None);
    let number = window.windowNumber();
    let event = |kind| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            loc,
            objc2_app_kit::NSEventModifierFlags::empty(),
            0.0,
            number,
            None,
            0,
            1,
            0.0,
        )
        .expect("synthetic mouse event")
    };
    // SAFETY: `ContactView` is an NSView subclass; this is the same dispatch
    // AppKit performs for a real click in the pane.
    let obj: &AnyObject = unsafe { &*(view as *const ContactView as *const AnyObject) };
    unsafe {
        let up = event(objc2_app_kit::NSEventType::LeftMouseUp);
        let _: () = msg_send![obj, mouseUp: &*up];
    }
    true
}

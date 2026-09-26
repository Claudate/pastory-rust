//! Port of `Shelf/ContactPane.swift` — 联系我: four paper sheets —
//! feedback group, GitHub star, X, buy me a coffee.
//!
//! Same hand-drawn shape as the settings pane: one view, hit rects in ivars.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBezierPath, NSEvent, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSString, NSURL};

use crate::app::localization::l;
use crate::app::theme;

pub const REPO_URL: &str = "https://github.com/nothingbutcici/pastory";
pub const X_URL: &str = "https://x.com/nothingbutcici";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Link {
    Back,
    Close,
    Repo,
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
        Link::Back => {
            crate::shelf::panel::with_model_mut(|m| m.show_contact = false);
            crate::shelf::view::refresh();
        }
        Link::Close => crate::shelf::panel::hide(),
        Link::Repo => open_url(REPO_URL),
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
        &theme::on_brown(),
        None,
    );
    header_button(view, CGRect::new(CGPoint::new(cx + cw - 200.0, 18.0), CGSize::new(160.0, 36.0)), &format!("‹ {}", l("返回剪贴板")), Link::Back, true);
    header_button(view, CGRect::new(CGPoint::new(cx + cw - 40.0, 18.0), CGSize::new(40.0, 36.0)), "✕", Link::Close, false);
    let top = 18.0 + 44.0 + 14.0;
    let mut x = cx;
    sheet(view, x, top, &l("反馈 bug & 提功能"), &l("Pastory 微信小小群，交个朋友"), SheetArt::Qr(theme::resource_named("WeChatGroup.png")));
    x += 246.0;
    sheet(view, x, top, &l("GitHub 点个 Star 吧"), &l("开源产品，喜欢就请助力一下"), SheetArt::Link("nothingbutcici/pastory", Link::Repo));
    x += 246.0;
    sheet(view, x, top, &l("关注我的 X"), &l("新版本和碎碎念都在这。"), SheetArt::Link("@nothingbutcici", Link::X));
    x += 246.0;
    sheet(view, x, top, "Buy me a coffee", &l("觉得好用，请我喝一杯。"), SheetArt::Qr(theme::resource_named("Coffee.png")));
}

fn header_button(view: &ContactView, rect: CGRect, s: &str, link: Link, wide: bool) {
    let font = if wide { theme::serif(13.0, false) } else { theme::serif(16.0, false) };
    let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 18.0, 18.0);
    theme::on_brown().colorWithAlphaComponent(0.4).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    let h = crate::shelf::card::line_height(&font);
    let tw = crate::shelf::card::measure(s, &font, 400.0, None).width;
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(rect.min().x + (rect.size.width - tw) / 2.0, rect.min().y + (rect.size.height - h) / 2.0), CGSize::new(tw + 1.0, h)),
        s,
        &font,
        &theme::on_brown(),
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
        theme::paper().setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 6.0, 6.0).fill();
        objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
    }
    // Torn brown label.
    let font = theme::serif(13.0, true);
    let tw = crate::shelf::card::measure(title, &font, 400.0, None).width;
    let label = CGRect::new(
        CGPoint::new(rect.min().x + 16.0, rect.min().y + 14.0),
        CGSize::new(tw + 20.0, crate::shelf::card::line_height(&font) + 6.0),
    );
    theme::brown().setFill();
    theme::torn_paper_path(label, true, true, true, true, 21, 1.4, 6.0).fill();
    crate::shelf::card::draw_line_centered(title, &font, &theme::on_brown(), label.min().x + 10.0, label.mid().y, tw + 1.0);
    // One-line hint (fixed 44pt).
    crate::shelf::card::draw_text(
        CGRect::new(
            CGPoint::new(rect.min().x + 16.0, label.max().y + 8.0),
            CGSize::new(w - 32.0, 36.0),
        ),
        line,
        &theme::serif(14.0, false),
        &theme::ink(),
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
            theme::ink().colorWithAlphaComponent(0.35).setStroke();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(cell, 14.0, 14.0).stroke();
            crate::shelf::card::draw_line_centered(&text, &font, &theme::ink(), cell.min().x + 14.0, cell.mid().y, tw + 1.0);
            view.ivars().hits.borrow_mut().push((link, cell));
        }
    }
}

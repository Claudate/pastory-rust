//! One photo on the 晾衣绳: a glass frame, a clip, the thumbnail, the video
//! badge and the 已复制 check. Drawing only — the rope and the row live in
//! `line_view`, the window and the hit rects in `line_panel`.
//!
//! The metrics are this form's own, the way `card.rs` owns the ticket's: a
//! 150pt photo, a 4pt glass inset, a 16pt frame radius, a 9×26 clip. They are
//! deliberately *not* the skin's radii or the card's 288pt width — the two
//! forms are different products, and `layout_skin` is the axis that decides
//! which one runs, not a shared geometry.
//!
//! The frame is painted translucent (glass) rather than given an
//! `NSVisualEffectView`: an offscreen render cannot composite a material, so a
//! view-backed glass would be invisible in every self-test. The live window
//! carries the material instead (see `line_panel`), and this translucent fill
//! reads as the frosted frame on top of it — the same split the panel form
//! uses (`theme::panel_material_alpha`).

use objc2::AnyThread;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSGradient, NSGraphicsContext, NSImage,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;

use crate::app::theme;
use crate::clipboard::item::{ClipItem, ClipKind};
use crate::shelf::card::{self, SymbolWeight};

/// Base frame width (`cardWidth` 150) — the 100% card size.
pub const CARD_W: f64 = 150.0;
/// Base picture box inside the frame (`photoMaxHeight` 104).
pub const PHOTO_H: f64 = 104.0;
/// Glass inset around the picture (`inset` 4).
pub const INSET: f64 = 4.0;
/// Frame corner radius (`radius` 16).
pub const RADIUS: f64 = 16.0;
/// The clip's own box (`12×34`): the pin's proportions, big enough to read as
/// a clothes pin on a 150pt photo.
pub const CLIP_W: f64 = 12.0;
pub const CLIP_H: f64 = 34.0;
/// How much of the pin sits above the rope — the spring collar it hangs by.
/// `ROPE_TOP` is 10 and the rope never runs higher, so 9 is the most that
/// fits without the pin's head leaving the strip.
pub const CLIP_ABOVE: f64 = 9.0;
/// Base frame height at 100%: the picture box plus the inset on both sides.
pub const FRAME_H: f64 = PHOTO_H + INSET * 2.0;
/// Base gap between cards on the rope (`cardWidth + 24`).
pub const SPACING: f64 = CARD_W + 24.0;

/// The 晾衣绳's own display choices (设置 › 外观 › 晾衣绳): card size and the gap
/// between cards. Resolved once per process from Preferences, like the skin and
/// the form — so the drawing, the hit test, the drag frame and the strip's own
/// height all read the *same* numbers and cannot drift apart (a mismatch there
/// is what made the first `--selftest linehit` fail). The settings pane calls
/// [`reset`] after a change so a live strip adopts it on its next reload.
pub mod metrics {
    use std::sync::RwLock;

    use crate::app::preferences::{ns_string_id, Preferences};

    /// Card size as a percentage of the base (100 / 120 / 150).
    pub const SIZE_STEPS: &[isize] = &[100, 120, 150];
    /// Gap between cards, in points (24 / 48 / 80).
    pub const GAP_STEPS: &[isize] = &[24, 48, 80];

    const KEY_SIZE: &str = "lineCardScale";
    const KEY_GAP: &str = "lineCardGap";

    #[derive(Clone, Copy)]
    struct M {
        scale: f64,
        gap: f64,
    }

    static M: RwLock<Option<M>> = RwLock::new(None);

    fn read() -> M {
        let d = Preferences::shared().defaults();
        let s = d.integerForKey(ns_string_id(KEY_SIZE));
        let g = d.integerForKey(ns_string_id(KEY_GAP));
        let scale = if SIZE_STEPS.contains(&s) { s as f64 } else { 100.0 } / 100.0;
        let gap = if GAP_STEPS.contains(&g) { g as f64 } else { 24.0 };
        M { scale, gap }
    }

    fn m() -> M {
        if let Some(v) = *M.read().unwrap() {
            return v;
        }
        let v = read();
        *M.write().unwrap() = Some(v);
        v
    }

    /// Forget the cached values so the next read re-resolves them (the settings
    /// pane calls this after storing a change).
    pub fn reset() {
        *M.write().unwrap() = None;
    }

    /// The value the pane draws as selected, for the storage write.
    pub fn stored_size() -> isize {
        let s = Preferences::shared().defaults().integerForKey(ns_string_id(KEY_SIZE));
        if SIZE_STEPS.contains(&s) { s } else { 100 }
    }

    pub fn stored_gap() -> isize {
        let g = Preferences::shared().defaults().integerForKey(ns_string_id(KEY_GAP));
        if GAP_STEPS.contains(&g) { g } else { 24 }
    }

    /// Persist a size / gap choice.
    pub fn store_size(step: isize) {
        Preferences::shared()
            .defaults()
            .setInteger_forKey(step, ns_string_id(KEY_SIZE));
        reset();
    }

    /// Write the size preference *without* dropping the cache.
    ///
    /// `store_size` drops it itself, so a self-test driving only `store_size`
    /// cannot tell whether the settings pane's change handler drops it too —
    /// both runs read the new value. This writes behind the cache's back so
    /// that the handler is the only thing left that can invalidate it.
    pub fn debug_write_size(step: isize) {
        Preferences::shared()
            .defaults()
            .setInteger_forKey(step, ns_string_id(KEY_SIZE));
    }

    pub fn store_gap(step: isize) {
        Preferences::shared()
            .defaults()
            .setInteger_forKey(step, ns_string_id(KEY_GAP));
        reset();
    }

    pub fn scale() -> f64 {
        m().scale
    }

    pub fn gap() -> f64 {
        m().gap
    }
}

/// The frame width the strip is drawn (and hit-tested) at.
pub fn card_w() -> f64 {
    CARD_W * metrics::scale()
}
/// The picture box height at the chosen card size.
pub fn photo_h() -> f64 {
    PHOTO_H * metrics::scale()
}
/// The frame height at the chosen card size.
pub fn frame_h() -> f64 {
    photo_h() + INSET * 2.0
}
/// The gap between cards' attach points at the chosen size and gap.
pub fn spacing() -> f64 {
    card_w() + metrics::gap()
}

/// One hanging photo's drawable state. `Clone` because the view draws from a
/// snapshot the controller hands out (`line_panel::current_cards`), so a tick
/// never holds the model lock while it paints.
#[derive(Clone)]
pub struct LineCardData {
    pub item: ClipItem,
    /// The model's `selectedID` (the card the next action applies to). Drawn as
    /// an accent ring — *not* the same thing as [`Self::on_clipboard`], which is
    /// the green check on whatever is at the top of the shelf and never moves
    /// when the user picks another card.
    pub selected: bool,
    pub on_clipboard: bool,
    /// Decoded thumbnail (image / video), warmed by the reload.
    pub thumb: Option<objc2_core_foundation::CFRetained<CGImage>>,
    /// The card's breeze seed (an id hash); see `motion::wind_angle`.
    pub seed: u64,
}

/// A stable seed from a clip id: the same card always gets the same breeze,
/// and a self-test can reproduce a frame exactly.
pub fn seed_for(id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Where the frame and the clip sit for a card whose attach point (where the
/// clip grips the rope, top centre) is `attach`. Both rects are in the
/// unrotated line space; the caller rotates around `attach`.
///
/// The pin hangs *on* the rope: [`CLIP_ABOVE`] of its length is above the
/// line, the rest reaches down past it. The old 9×26 capsule sat at
/// `attach.y - 3.0`, so 23 of its 26 points were under the rope where the
/// frame covered them — that grey stump is what read as 「只剩半截」.
pub fn clip_rect(attach: CGPoint) -> CGRect {
    CGRect::new(
        CGPoint::new(attach.x - CLIP_W / 2.0, attach.y - CLIP_ABOVE),
        CGSize::new(CLIP_W, CLIP_H),
    )
}

/// The glass frame: hangs from just under the clip, overlapping it so the clip
/// reads as gripping the frame's top edge.
pub fn frame_rect(attach: CGPoint) -> CGRect {
    CGRect::new(
        CGPoint::new(attach.x - card_w() / 2.0, attach.y + CLIP_H - 10.0),
        CGSize::new(card_w(), frame_h()),
    )
}

/// The picture's box inside the frame.
pub fn photo_rect(attach: CGPoint) -> CGRect {
    let f = frame_rect(attach);
    CGRect::new(
        CGPoint::new(f.min().x + INSET, f.min().y + INSET),
        CGSize::new(card_w() - 2.0 * INSET, photo_h()),
    )
}

/// The attach point for a card centred at `cx` on a rope whose y at `cx` is
/// `rope_y` (`line_view::rope_y`).
pub fn attach_point(cx: f64, rope_y: f64) -> CGPoint {
    CGPoint::new(cx, rope_y)
}

/// Draw one card, rotated by `angle_deg` around its attach point.
pub fn draw(attach: CGPoint, angle_deg: f64, d: &LineCardData) {
    let gc = NSGraphicsContext::currentContext();
    let rotated = gc.is_some() && angle_deg.abs() > 1e-6;
    if let Some(gc) = &gc {
        NSGraphicsContext::saveGraphicsState_class();
        if rotated {
            let cg = gc.CGContext();
            objc2_core_graphics::CGContext::translate_ctm(Some(&cg), attach.x, attach.y);
            objc2_core_graphics::CGContext::rotate_ctm(Some(&cg), angle_deg.to_radians());
            objc2_core_graphics::CGContext::translate_ctm(Some(&cg), -attach.x, -attach.y);
        }
    }
    let frame = frame_rect(attach);
    glass(frame, d);
    clip(attach, d);
    NSGraphicsContext::restoreGraphicsState_class();
}

/// The glass frame: a soft shadow, a translucent fill, the picture clipped
/// into it, and a 1pt inner highlight along the top edge so it reads as glass.
fn glass(frame: CGRect, d: &LineCardData) {
    let path = theme::rounded_rect(frame, RADIUS);
    NSGraphicsContext::saveGraphicsState_class();
    card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.28), 12.0, 0.0, 5.0).set();
    // Frosted fill: the skin's surface, kept translucent so the material
    // behind the live window shows through.
    let fill = theme::surface().colorWithAlphaComponent(0.72);
    fill.setFill();
    path.fill();
    NSGraphicsContext::restoreGraphicsState_class();

    NSGraphicsContext::saveGraphicsState_class();
    path.addClip();
    // A faint top-to-bottom sheen: brighter at the top like real glass.
    if let Some(g) = NSGradient::initWithStartingColor_endingColor(
        NSGradient::alloc(),
        &NSColor::whiteColor().colorWithAlphaComponent(0.16),
        &NSColor::whiteColor().colorWithAlphaComponent(0.0),
    ) {
        g.drawInRect_angle(frame, -90.0);
    }
    // The picture.
    let photo = CGRect::new(
        CGPoint::new(frame.min().x + INSET, frame.min().y + INSET),
        CGSize::new(card_w() - 2.0 * INSET, photo_h()),
    );
    let well = theme::rounded_rect(photo, RADIUS - INSET);
    NSGraphicsContext::saveGraphicsState_class();
    well.addClip();
    theme::surface_alt().setFill();
    objc2_app_kit::NSRectFill(photo);
    if let Some(img) = &d.thumb {
        let px_w = CGImage::width(Some(img)) as f64;
        let px_h = CGImage::height(Some(img)) as f64;
        if px_w > 0.0 && px_h > 0.0 {
            let scale = (photo.size.width / px_w).min(photo.size.height / px_h);
            let iw = px_w * scale;
            let ih = px_h * scale;
            let at = CGRect::new(
                CGPoint::new(photo.mid().x - iw / 2.0, photo.mid().y - ih / 2.0),
                CGSize::new(iw, ih),
            );
            let nsimg = NSImage::initWithCGImage_size(NSImage::alloc(), img, CGSize::new(px_w, px_h));
            card::draw_image(&nsimg, at, objc2_app_kit::NSCompositingOperation::SourceOver, 1.0);
        }
    } else if d.item.kind == ClipKind::Image || d.item.kind == ClipKind::Video {
        // Decoding, or a missing thumbnail: a quiet glyph on the empty well.
        if crate::shelf::panel::thumbnail_missing(&d.item.id) {
            let name = if d.item.kind == ClipKind::Video { "film" } else { "photo" };
            if let Some(img) = card::symbol(
                name,
                30.0,
                SymbolWeight::Regular,
                &theme::label_tertiary(),
            ) {
                card::draw_centered(&img, photo, 1.0);
            }
        }
    } else {
        // Text / link / files have no picture: a small kind glyph and a line
        // of their own text, so an all-text rope is not a row of blank wells.
        non_picture(photo, d);
    }
    NSGraphicsContext::restoreGraphicsState_class();

    // Title caption: a titled clip shows its title along the frame's bottom, so
    // a named item is recognisable on the rope (the panel shows the title row).
    if let Some(t) = d.item.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let bar_h = 19.0;
        let bar = CGRect::new(
            CGPoint::new(frame.min().x + 4.0, frame.max().y - bar_h - 4.0),
            CGSize::new(frame.size.width - 8.0, bar_h),
        );
        NSColor::blackColor().colorWithAlphaComponent(0.36).setFill();
        theme::rounded_rect(bar, 6.0).fill();
        card::draw_text(
            CGRect::new(
                CGPoint::new(bar.min().x + 6.0, bar.min().y + 2.5),
                CGSize::new(bar.size.width - 12.0, 14.0),
            ),
            t,
            &theme::serif(11.0, false),
            &NSColor::whiteColor(),
            Some(1.0),
        );
    }

    // Pin badge: a dark disc at the frame's top-left, so a pinned clip (which
    // survives retention) is visible on the rope the way the panel's filled pin
    // glyph is.
    if d.item.pinned {
        let disc = CGRect::new(
            CGPoint::new(frame.min().x + 4.0, frame.min().y - 6.0),
            CGSize::new(20.0, 20.0),
        );
        NSColor::blackColor().colorWithAlphaComponent(0.42).setFill();
        NSBezierPath::bezierPathWithOvalInRect(disc).fill();
        if let Some(img) = card::symbol("pin.fill", 11.0, SymbolWeight::Semibold, &NSColor::whiteColor())
        {
            card::draw_centered(&img, disc, 1.0);
        }
    }

    // Video badge.
    if d.item.kind == ClipKind::Video {
        let center = CGPoint::new(photo.mid().x, photo.mid().y);
        let badge = CGRect::new(
            CGPoint::new(center.x - 18.0, center.y - 18.0),
            CGSize::new(36.0, 36.0),
        );
        NSColor::blackColor().colorWithAlphaComponent(0.42).setFill();
        NSBezierPath::bezierPathWithOvalInRect(badge).fill();
        if let Some(img) = card::symbol("play.fill", 15.0, SymbolWeight::Regular, &NSColor::whiteColor()) {
            let at = CGRect::new(
                CGPoint::new(center.x - 18.0 + 2.0, center.y - 18.0),
                CGSize::new(36.0, 36.0),
            );
            card::draw_centered(&img, at, 1.0);
        }
    }

    // 已复制 check: a small filled disc at the frame's top-right corner.
    if d.on_clipboard {
        let disc = CGRect::new(
            CGPoint::new(frame.max().x - 24.0, frame.min().y - 6.0),
            CGSize::new(20.0, 20.0),
        );
        theme::accent().setFill();
        NSBezierPath::bezierPathWithOvalInRect(disc).fill();
        if let Some(img) = card::symbol("checkmark", 11.0, SymbolWeight::Semibold, &theme::text_on_accent()) {
            card::draw_centered(&img, disc, 1.0);
        }
    }
    NSGraphicsContext::restoreGraphicsState_class();

    // 1pt inner highlight along the top edge (the glass lip).
    NSGraphicsContext::saveGraphicsState_class();
    path.addClip();
    NSColor::whiteColor().colorWithAlphaComponent(0.28).setFill();
    objc2_app_kit::NSRectFill(CGRect::new(
        CGPoint::new(frame.min().x + RADIUS * 0.4, frame.min().y + 0.5),
        CGSize::new(frame.size.width - RADIUS * 0.8, 1.0),
    ));
    NSGraphicsContext::restoreGraphicsState_class();
    // Hairline edge.
    theme::separator().setStroke();
    path.setLineWidth(1.0);
    path.stroke();

    // Selected: an accent ring just inside the hairline. Hover is drawn as a
    // *brightness* change elsewhere (a fill, not an outline), so the ring and
    // the hover highlight can both be true at once and still read as two
    // different things — 「这张是当前操作对象」 versus 「指针在这张上」.
    if d.selected {
        let ring = theme::rounded_rect(
            CGRect::new(
                CGPoint::new(frame.min().x + 1.5, frame.min().y + 1.5),
                CGSize::new(frame.size.width - 3.0, frame.size.height - 3.0),
            ),
            RADIUS - 1.5,
        );
        theme::accent().setStroke();
        ring.setLineWidth(2.0);
        ring.stroke();
    }
}

/// The well's content when the clip has no picture: an upright kind glyph and
/// a one-line preview, in the skin's own content colour.
fn non_picture(well: CGRect, d: &LineCardData) {
    let glyph = match d.item.kind {
        ClipKind::Url => "link",
        ClipKind::Files => "folder",
        _ => "text.alignleft",
    };
    let tint = match d.item.kind {
        ClipKind::Url => theme::accent(),
        _ => theme::label_secondary(),
    };
    let mut top = well.min().y + 12.0;
    if let Some(img) = card::symbol(glyph, 14.0, SymbolWeight::Regular, &tint) {
        let size = img.size();
        card::draw_image(
            &img,
            CGRect::new(CGPoint::new(well.min().x + 12.0, top), size),
            objc2_app_kit::NSCompositingOperation::SourceOver,
            1.0,
        );
        top += size.height + 8.0;
    }
    let text = preview_text(&d.item);
    if text.is_empty() {
        return;
    }
    let font = theme::serif(12.0, false);
    let color = match d.item.kind {
        ClipKind::Url => theme::accent(),
        _ => theme::label(),
    };
    card::draw_text(
        CGRect::new(
            CGPoint::new(well.min().x + 12.0, top),
            CGSize::new(
                (well.size.width - 24.0).max(0.0),
                (well.max().y - top - 10.0).max(0.0),
            ),
        ),
        &text,
        &font,
        &color,
        Some(2.0),
    );
}

/// The one-line preview a hanging card shows: a link's host, else the clip's
/// first non-empty line, clipped so it can never overflow the well.
fn preview_text(item: &ClipItem) -> String {
    let raw = match item.kind {
        ClipKind::Url => url_host(&item.snippet).unwrap_or_else(|| item.snippet.clone()),
        _ => item.snippet.clone(),
    };
    let line = raw
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(46).collect();
    if line.chars().count() > 46 {
        out.push('…');
    }
    out
}

/// The host of a URL snippet (`github.com`), for the link card's preview.
fn url_host(s: &str) -> Option<String> {
    let t = s.trim();
    let rest = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(t);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// The clip: the open-source cartoon clothes pin (Resources/ClothesPin.png —
/// game-icons' `clothespin` by Lorc, CC BY 3.0), hung over the rope with its
/// mouth closing on the frame's top edge.
///
/// The whole pin is drawn inside `clip_rect` and sits high enough that the
/// frame never swallows it; the strip's height leaves room for it at the
/// rope's trough (`line_view::content_height`).
fn clip(attach: CGPoint, _d: &LineCardData) {
    let r = clip_rect(attach);
    if let Some(img) = theme::clothes_pin() {
        NSGraphicsContext::saveGraphicsState_class();
        card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.30), 4.0, 0.0, 2.0).set();
        card::draw_image(
            &img,
            r,
            objc2_app_kit::NSCompositingOperation::SourceOver,
            1.0,
        );
        NSGraphicsContext::restoreGraphicsState_class();
        return;
    }
    // No artwork on disk (a bare `cargo run` from a tree without Resources):
    // a plain wooden peg, so the card still reads as hung rather than floating.
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 3.0, 3.0);
    NSColor::colorWithCalibratedRed_green_blue_alpha(0.85, 0.69, 0.51, 1.0).setFill();
    path.fill();
    theme::separator().setStroke();
    path.setLineWidth(1.0);
    path.stroke();
}

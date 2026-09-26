//! Port of `Shelf/ClipCardView.swift` — the paper ticket card, hand-drawn
//! with AppKit calls instead of SwiftUI views. Geometry and values are copied
//! one-for-one from the Swift view; every measurement in the render matches
//! the Swift baseline to the pixel (see the slice-C verification notes).
//!
//! All drawing assumes a flipped view (y grows downwards, like SwiftUI).

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontWeightLight, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSGraphicsContext, NSImage, NSImageSymbolConfiguration,
    NSMutableParagraphStyle, NSShadow, NSStringDrawing, NSStringDrawingOptions,
    NSStringNSExtendedStringDrawing,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    NSAttributedStringKey, NSCalendar, NSDate, NSDateFormatter, NSDictionary, NSString,
};

use crate::app::localization::l;
use crate::app::theme;
use crate::clipboard::item::{ClipItem, ClipKind};

/// `ClipCardView.width`.
pub const CARD_W: f64 = 288.0;
/// `stubHeight` — caption row + action row below the perforation.
pub const STUB_H: f64 = 96.0;
const CAPTION_H: f64 = 44.0;
const HEADER_RULE_Y: f64 = 60.0;
/// Title block total height (titleRowHeight 26 + top 4 + bottom 2).
const TITLE_ROW_H: f64 = 32.0;
const TICKET_RADIUS: f64 = 5.0;
const NOTCH: f64 = 11.0;
/// H pad on both card texts and rules; the photo print uses 16/14 instead.
pub(crate) const PAD_X: f64 = 18.0;

/// One card's drawing state, pulled from the model before a redraw.
pub struct CardData {
    pub item: ClipItem,
    pub index: usize,
    pub selected: bool,
    pub on_clipboard: bool,
    pub renaming: bool,
    /// M6 desktop notes: the "已贴在桌面" glyph never shows before then
    /// (the branch and geometry stay, mirroring the Swift view).
    pub on_desktop: bool,
    /// Decoded thumbnail for image / video cards, warmed during reload.
    pub thumb: Option<CFRetained<CGImage>>,
}

impl CardData {
    /// The title row shows when renaming, selected, or a title exists.
    pub fn has_title_row(&self) -> bool {
        self.renaming
            || self.selected
            || self.item.title.as_deref().map(|t| !t.is_empty()) == Some(true)
    }
}

/// Action-stub buttons left to right (`actions`), with the button kind the
/// Swift view picks per clip kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionKind {
    Edit,
    Preview,
    Pin,
    Save,
    Trash,
    Divider,
}

/// `actions`: edit (text/url/image) or preview, pin, save (image/video),
/// trash — with the thin uprights between them.
pub fn action_strip(kind: ClipKind) -> Vec<ActionKind> {
    let mut out = Vec::new();
    out.push(match kind {
        ClipKind::Text | ClipKind::Url | ClipKind::Image => ActionKind::Edit,
        _ => ActionKind::Preview,
    });
    out.push(ActionKind::Divider);
    out.push(ActionKind::Pin);
    if kind == ClipKind::Image || kind == ClipKind::Video {
        out.push(ActionKind::Divider);
        out.push(ActionKind::Save);
    }
    out.push(ActionKind::Divider);
    out.push(ActionKind::Trash);
    out
}

/// Rects of the action strip (assists both drawing and hit-testing):
/// returns (kind, rect) with rects in the same coordinate space as `ticket`.
pub fn action_rects(ticket: CGRect, kind: ClipKind) -> Vec<(ActionKind, CGRect)> {
    let strip = action_strip(kind);
    let row_y = ticket.max().y - (STUB_H - CAPTION_H); // [bottom-52, bottom]
    // HStack .padding(.horizontal, 10): buttons share the width, dividers 1pt.
    let inner_x = ticket.min().x + 10.0;
    let inner_w = CARD_W - 20.0;
    let n_div = strip.iter().filter(|a| **a == ActionKind::Divider).count() as f64;
    let n_btn = (strip.len() as f64 - n_div).max(1.0);
    let btn_w = (inner_w - n_div) / n_btn;
    let mut x = inner_x;
    let mut out = Vec::new();
    for a in strip {
        let w = if a == ActionKind::Divider { 1.0 } else { btn_w };
        out.push((a, CGRect::new(CGPoint::new(x, row_y), CGSize::new(w, STUB_H - CAPTION_H))));
        x += w;
    }
    out
}

/// The ticket outline (`TicketShape`, notch on even-indexed cards).
pub fn ticket_path(ticket: CGRect, index: usize) -> Retained<NSBezierPath> {
    let notch = if index % 2 == 0 { Some(STUB_H) } else { None };
    theme::ticket_path(ticket, notch, TICKET_RADIUS, NOTCH)
}

/// Title-row rect (still used for the inline editor placement).
pub fn title_row_rect(ticket: CGRect) -> CGRect {
    CGRect::new(
        CGPoint::new(ticket.min().x, ticket.min().y + HEADER_RULE_Y + 1.0),
        CGSize::new(CARD_W, TITLE_ROW_H),
    )
}

/// Top of the flexible content area.
pub fn content_top(ticket: CGRect, has_title: bool) -> f64 {
    ticket.min().y + HEADER_RULE_Y + 1.0 + if has_title { TITLE_ROW_H } else { 0.0 }
}

// MARK: Small draw helpers

/// NSFont line height (SwiftUI's one-line Text frame height).
pub(crate) fn line_height(f: &NSFont) -> f64 {
    f.ascender() - f.descender() + f.leading()
}

/// Attribute dictionary for NSString drawing. `line_spacing` of None draws
/// the default paragraph style.
pub(crate) fn attrs(
    font: &NSFont,
    color: &NSColor,
    line_spacing: Option<f64>,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    // SAFETY: the attribute-name statics are immutable extern constants;
    // the upcasts are same-class conversions to the root object type.
    unsafe {
        let font_obj = &*(font as *const NSFont as *const AnyObject);
        let color_obj = &*(color as *const NSColor as *const AnyObject);
        type Dict = NSDictionary<NSAttributedStringKey, AnyObject>;
        match line_spacing {
            Some(ls) => {
                let para = NSMutableParagraphStyle::new();
                para.setLineSpacing(ls);
                let para_obj = &*(objc2::rc::Retained::as_ptr(&para) as *const AnyObject);
                Dict::from_slices(
                    &[
                        objc2_app_kit::NSFontAttributeName,
                        objc2_app_kit::NSForegroundColorAttributeName,
                        objc2_app_kit::NSParagraphStyleAttributeName,
                    ],
                    &[font_obj, color_obj, para_obj],
                )
            }
            None => Dict::from_slices(
                &[
                    objc2_app_kit::NSFontAttributeName,
                    objc2_app_kit::NSForegroundColorAttributeName,
                ],
                &[font_obj, color_obj],
            ),
        }
    }
}

/// Draw one line, vertically centered in a band (`center_y`). SwiftUI
/// HStacks center single-line Text against the icon box this way.
pub(crate) fn draw_line_centered(
    s: &str,
    font: &NSFont,
    color: &NSColor,
    x: f64,
    center_y: f64,
    clip_width: f64,
) {
    let top = center_y - line_height(font) / 2.0;
    let s = NSString::from_str(s);
    let a = attrs(font, color, None);
    let rect = CGRect::new(CGPoint::new(x, top), CGSize::new(clip_width, line_height(font)));
    NSGraphicsContext::saveGraphicsState_class();
    NSBezierPath::bezierPathWithRect(rect).addClip();
    unsafe {
        s.drawAtPoint_withAttributes(CGPoint::new(x, top), Some(&a));
    }
    NSGraphicsContext::restoreGraphicsState_class();
}

/// Multi-line / wrapped text starting at the top of `rect` (line boxes flow
/// down from `rect.min_y`, as in a SwiftUI topLeading Text).
pub(crate) fn draw_text(rect: CGRect, s: &str, font: &NSFont, color: &NSColor, line_spacing: Option<f64>) {
    let s = NSString::from_str(s);
    let a = attrs(font, color, line_spacing);
    unsafe {
        s.drawInRect_withAttributes(rect, Some(&a));
    }
}

/// Measure wrapped text (line-fragment mode).
pub(crate) fn measure(s: &str, font: &NSFont, width: f64, line_spacing: Option<f64>) -> CGSize {
    let s = NSString::from_str(s);
    let a = attrs(font, color_placeholder(), line_spacing);
    unsafe {
        s.boundingRectWithSize_options_attributes_context(
            CGSize::new(width, 10_000.0),
            NSStringDrawingOptions::UsesLineFragmentOrigin | NSStringDrawingOptions::UsesFontLeading,
            Some(&a),
            None,
        )
        .size
    }
}

/// A color for measurement-only dictionaries (text extents do not depend on
/// the paint).
fn color_placeholder() -> &'static NSColor {
    static C: std::sync::OnceLock<Retained<NSColor>> = std::sync::OnceLock::new();
    C.get_or_init(NSColor::blackColor)
}

/// `Image(systemName:)`: point size + weight, tinted. Returns a ready image.
pub(crate) fn symbol(name: &str, size: f64, weight: SymbolWeight, color: &NSColor) -> Option<Retained<NSImage>> {
    let base = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    )?;
    let w = unsafe {
        match weight {
            SymbolWeight::Regular => NSFontWeightRegular,
            SymbolWeight::Medium => NSFontWeightMedium,
            SymbolWeight::Semibold => NSFontWeightSemibold,
            SymbolWeight::Light => NSFontWeightLight,
        }
    };
    let conf = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, w);
    let img = base.imageWithSymbolConfiguration(&conf)?;
    tint(&img, color)
}

#[derive(Clone, Copy)]
pub(crate) enum SymbolWeight {
    Regular,
    Medium,
    Semibold,
    Light,
}

/// Recolor a (mono) symbol image: draw, then SourceIn the tint.
#[allow(deprecated)]
pub(crate) fn tint(img: &NSImage, color: &NSColor) -> Option<Retained<NSImage>> {
    let size = img.size();
    if size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }
    let out = NSImage::initWithSize(NSImage::alloc(), size);
    out.lockFocus();
    img.drawInRect_fromRect_operation_fraction(
        CGRect::new(CGPoint::ZERO, size),
        CGRect::ZERO,
        objc2_app_kit::NSCompositingOperation::SourceOver,
        1.0,
    );
    color.set();
    objc2_app_kit::NSRectFillUsingOperation(
        CGRect::new(CGPoint::ZERO, size),
        objc2_app_kit::NSCompositingOperation::SourceIn,
    );
    out.unlockFocus();
    Some(out)
}

/// Draw `img` upright in `rect`. Every view here is flipped; an NSImage
/// keeps its pixels y-up, so an upside-down CTM pass puts the content back
/// on its feet (the offscreen bakes in theme.rs are unflipped and skip it).
pub(crate) fn draw_image(
    img: &NSImage,
    rect: CGRect,
    op: objc2_app_kit::NSCompositingOperation,
    alpha: f64,
) {
    if let Some(gc) = NSGraphicsContext::currentContext() {
        // SAFETY: property getter on the live context.
        let flipped: bool = unsafe { objc2::msg_send![&*gc, isFlipped] };
        if flipped {
            NSGraphicsContext::saveGraphicsState_class();
            let cg = gc.CGContext();
            objc2_core_graphics::CGContext::translate_ctm(Some(&cg), rect.min().x, rect.max().y);
            objc2_core_graphics::CGContext::scale_ctm(Some(&cg), 1.0, -1.0);
            img.drawInRect_fromRect_operation_fraction(
                CGRect::new(CGPoint::ZERO, rect.size),
                CGRect::ZERO,
                op,
                alpha,
            );
            NSGraphicsContext::restoreGraphicsState_class();
            return;
        }
    }
    img.drawInRect_fromRect_operation_fraction(rect, CGRect::ZERO, op, alpha);
}

/// Draw `img` centered in `rect` (symbol glyphs sit centered in their boxes).
pub(crate) fn draw_centered(img: &NSImage, rect: CGRect, alpha: f64) {
    let size = img.size();
    let at = CGPoint::new(
        rect.min().x + (rect.size.width - size.width) / 2.0,
        rect.min().y + (rect.size.height - size.height) / 2.0,
    );
    draw_image(
        img,
        CGRect::new(at, size),
        objc2_app_kit::NSCompositingOperation::SourceOver,
        alpha,
    );
}

pub(crate) fn shadow(color: &NSColor, blur: f64, dx: f64, dy: f64) -> Retained<NSShadow> {
    let s = NSShadow::new();
    s.setShadowColor(Some(color));
    s.setShadowBlurRadius(blur);
    s.setShadowOffset(CGSize::new(dx, dy));
    s
}

// MARK: Text bits (`sourceTitle`, `when`, `kindLabel`, `note`)

/// Today: 18:44 · earlier: 9/14 18:44.
pub(crate) fn when(ts: f64) -> String {
    use std::cell::RefCell;
    thread_local! {
        static TIME_ONLY: RefCell<Option<Retained<NSDateFormatter>>> = RefCell::new(None);
        static DAY_TIME: RefCell<Option<Retained<NSDateFormatter>>> = RefCell::new(None);
    }
    let date = NSDate::dateWithTimeIntervalSince1970(ts);
    let today = NSCalendar::currentCalendar().isDateInToday(&date);
    let fmt_key = if today { "HH:mm" } else { "M/d HH:mm" };
    let cell = if today { &TIME_ONLY } else { &DAY_TIME };
    cell.with(|c| {
        let mut c = c.borrow_mut();
        let f = c.get_or_insert_with(|| {
            let f = NSDateFormatter::new();
            f.setDateFormat(Some(&NSString::from_str(fmt_key)));
            f
        });
        f.stringFromDate(&date).to_string()
    })
}

/// `Text(note.map { "\(kindLabel) · \($0)" } ?? kindLabel)`.
fn caption(item: &ClipItem) -> String {
    let kind_label = match item.kind {
        ClipKind::Image => l("图片"),
        ClipKind::Text => l("文本"),
        ClipKind::Url => l("链接"),
        ClipKind::Files => l("文件"),
        ClipKind::Video => item.ext.to_uppercase(),
    };
    let note: Option<String> = match item.kind {
        ClipKind::Image => Some(item.snippet.replace('×', " × ")),
        ClipKind::Text => {
            let count = if item.snippet.chars().count() >= 400 {
                (item.byte_count / 3) as usize
            } else {
                item.snippet.chars().count()
            };
            Some(l("%d 字").replacen("%d", &count.to_string(), 1))
        }
        ClipKind::Url => None,
        ClipKind::Files => {
            let n = item.snippet.lines().count();
            Some(l("%d 项").replacen("%d", &n.to_string(), 1))
        }
        ClipKind::Video => {
            let s = item.duration.unwrap_or(0.0).round() as i64;
            Some(format!("{:02}:{:02}", s / 60, s % 60))
        }
    };
    match note {
        Some(n) => format!("{} · {}", kind_label, n),
        None => kind_label,
    }
}

/// `sourceTitle`: the app the clip came from (「导入」 → 「已导入」).
pub fn source_title(item: &ClipItem) -> String {
    if item.source_app_name.as_deref() == Some(crate::clipboard::store::IMPORT_SOURCE_NAME) {
        return l("已导入");
    }
    item.source_app_name
        .clone()
        .unwrap_or_else(|| item.kind.label())
}

/// URL host (the bold line of a link card), mirroring `URL(string:)?.host`.
fn url_host(s: &str) -> Option<String> {
    let after = s.split_once("://").map(|(_, r)| r)?;
    let host = after.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        return None;
    }
    Some(host.to_string())
}

// MARK: The card itself

/// Draw the whole ticket with its top at `ticket.min_y` — the caller adds
/// the selected -6pt shift by passing the shifted rect (mirrors `.offset`).
pub fn draw(ticket: CGRect, d: &CardData) {
    let path = ticket_path(ticket, d.index);
    // The sheet + its shadow, in one fill (SwiftUI `.background(ticket.fill…).
    NSGraphicsContext::saveGraphicsState_class();
    let sh = if d.selected {
        shadow(&NSColor::blackColor().colorWithAlphaComponent(0.55), 14.0, 2.0, 9.0)
    } else {
        shadow(&NSColor::blackColor().colorWithAlphaComponent(0.4), 9.0, 2.0, 6.0)
    };
    sh.set();
    let fill = if d.on_clipboard {
        theme::paper_blue_tile()
    } else {
        theme::paper_tile()
    };
    NSColor::colorWithPatternImage(&fill).setFill();
    path.fill();
    NSGraphicsContext::restoreGraphicsState_class();

    NSGraphicsContext::saveGraphicsState_class();
    path.addClip();
    header(ticket, d);
    let mut y = content_top(ticket, d.has_title_row());
    if d.has_title_row() {
        title(ticket, d);
    } else {
        y -= 0.0; // content starts directly under the header rule
    }
    content(ticket, d, y);
    perforation(ticket);
    caption_row(ticket, d);
    actions(ticket, d);
    NSGraphicsContext::restoreGraphicsState_class();

    pushpin(ticket, d);
}

/// The header: app icon, source, time, then the 1pt rule.
fn header(ticket: CGRect, d: &CardData) {
    let x0 = ticket.min().x;
    let y0 = ticket.min().y;
    // Icon, 22×22 at (18, 30).
    let icon_rect = CGRect::new(CGPoint::new(x0 + PAD_X, y0 + 30.0), CGSize::new(22.0, 22.0));
    match theme::card_icon(d.item.source_bundle_id.as_deref()) {
        Some(img) => draw_image(
            &img,
            icon_rect,
            objc2_app_kit::NSCompositingOperation::SourceOver,
            1.0,
        ),
        None => {
            if let Some(img) = symbol("doc.on.clipboard", 13.0, SymbolWeight::Regular, &theme::ink()) {
                draw_centered(&img, icon_rect, 1.0);
            }
        }
    }
    let text_x = x0 + PAD_X + 22.0 + 8.0;
    let center_y = y0 + 30.0 + 11.0;
    // Time on the right edge (drawn first so the source title stays clear).
    let time_font = theme::serif(14.0, false);
    let time_color = theme::ink().colorWithAlphaComponent(0.75);
    let time = when(d.item.created_at);
    let tw = measure(&time, &time_font, 200.0, None).width;
    draw_line_centered(&time, &time_font, &time_color, x0 + CARD_W - PAD_X - tw, center_y, tw + 1.0);
    // M6 desktop notes: the note.text glyph sits between title and time.
    let mut right = x0 + CARD_W - PAD_X - tw - 8.0;
    if d.on_desktop {
        if let Some(img) = symbol("note.text", 12.0, SymbolWeight::Medium, &theme::ink_muted()) {
            let w = img.size().width;
            draw_centered(
                &img,
                CGRect::new(CGPoint::new(right - w, center_y - 11.0), CGSize::new(w, 22.0)),
                1.0,
            );
            right -= w + 8.0;
        }
    }
    draw_line_centered(
        &source_title(&d.item),
        &theme::serif(16.0, false),
        &theme::ink(),
        text_x,
        center_y,
        (right - text_x).max(0.0),
    );
    // 1pt rule.
    theme::ink().colorWithAlphaComponent(0.7).setFill();
    NSBezierPath::bezierPathWithRect(CGRect::new(
        CGPoint::new(x0 + PAD_X, y0 + HEADER_RULE_Y),
        CGSize::new(CARD_W - 2.0 * PAD_X, 1.0),
    ))
    .fill();
}

/// The handwritten title row (26pt band + pads, rule at block bottom).
fn title(ticket: CGRect, d: &CardData) {
    let x0 = ticket.min().x;
    let row_y = ticket.min().y + HEADER_RULE_Y + 1.0;
    if d.renaming {
        // The inline editor itself is an NSTextField laid over this row by
        // the cards-row view; the rule below still draws here.
    } else if let Some(t) = d.item.title.as_deref().filter(|t| !t.is_empty()) {
        draw_line_centered(
            t,
            &theme::script(20.0),
            &theme::ink(),
            x0 + PAD_X,
            row_y + 4.0 + 13.0,
            CARD_W - 2.0 * PAD_X,
        );
    } else if d.selected {
        draw_line_centered(
            &l("+ 加个标题"),
            &theme::script(17.0),
            &theme::ink_muted().colorWithAlphaComponent(0.8),
            x0 + PAD_X,
            row_y + 4.0 + 13.0,
            CARD_W - 2.0 * PAD_X,
        );
    }
    theme::ink().colorWithAlphaComponent(0.6).setFill();
    NSBezierPath::bezierPathWithRect(CGRect::new(
        CGPoint::new(x0 + PAD_X, row_y + TITLE_ROW_H - 1.0),
        CGSize::new(CARD_W - 2.0 * PAD_X, 1.0),
    ))
    .fill();
}

/// The flexible middle: thumbnail photo print / file list / link / text.
fn content(ticket: CGRect, d: &CardData, top: f64) {
    let x0 = ticket.min().x;
    let bottom = ticket.max().y - STUB_H - 1.0; // perforation sits at bottom-97
    match d.item.kind {
        ClipKind::Image | ClipKind::Video => {
            if let Some(img) = &d.thumb {
                photo(ticket, d, top, bottom, img);
            } else if crate::shelf::panel::thumbnail_missing(&d.item.id) {
                let name = if d.item.kind == ClipKind::Video { "film" } else { "photo" };
                if let Some(img) = symbol(
                    name,
                    34.0,
                    SymbolWeight::Regular,
                    &theme::ink_muted().colorWithAlphaComponent(0.5),
                ) {
                    draw_centered(
                        &img,
                        CGRect::new(
                            CGPoint::new(x0, top),
                            CGSize::new(CARD_W, (bottom - top).max(0.0)),
                        ),
                        1.0,
                    );
                }
            }
            // Otherwise decoding: a clear placeholder for a frame or two.
        }
        ClipKind::Files => {
            let mut y = top + 14.0;
            let icon_font_size = 12.0;
            let text_font = theme::serif(15.0, false);
            for line in d.item.snippet.lines().take(8) {
                if let Some(img) = symbol("doc", icon_font_size, SymbolWeight::Regular, &theme::ink_muted()) {
                    let th = line_height(&text_font);
                    draw_centered(
                        &img,
                        CGRect::new(
                            CGPoint::new(x0 + PAD_X, y + (th - img.size().height) / 2.0),
                            CGSize::new(img.size().width, img.size().height),
                        ),
                        1.0,
                    );
                }
                draw_text(
                    CGRect::new(
                        CGPoint::new(x0 + PAD_X + 12.0 + 8.0, y),
                        CGSize::new(CARD_W - 2.0 * PAD_X - 20.0, line_height(&text_font)),
                    ),
                    line,
                    &text_font,
                    &theme::ink(),
                    None,
                );
                y += line_height(&text_font) + 8.0;
            }
        }
        ClipKind::Url => {
            let mut y = top + 14.0;
            if let Some(host) = url_host(&d.item.snippet) {
                let f = theme::serif(16.0, true);
                draw_text(
                    CGRect::new(
                        CGPoint::new(x0 + PAD_X, y),
                        CGSize::new(CARD_W - 2.0 * PAD_X, line_height(&f)),
                    ),
                    &host,
                    &f,
                    &theme::ink(),
                    None,
                );
                y += line_height(&f) + 6.0;
            }
            let f = theme::serif(14.0, false);
            draw_text(
                CGRect::new(
                    CGPoint::new(x0 + PAD_X, y),
                    CGSize::new(CARD_W - 2.0 * PAD_X, (bottom - y).max(0.0)),
                ),
                &d.item.snippet,
                &f,
                &theme::paper_blue_deep(),
                None,
            );
        }
        ClipKind::Text => {
            let f = theme::serif(15.0, false);
            draw_text(
                CGRect::new(
                    CGPoint::new(x0 + PAD_X, top + 14.0),
                    CGSize::new(CARD_W - 2.0 * PAD_X, (bottom - top - 14.0).max(0.0)),
                ),
                &d.item.snippet,
                &f,
                &theme::ink(),
                Some(6.0),
            );
        }
    }
}

/// Photo print: white border, soft shadow, a slight tilt; video adds the
/// play badge.
fn photo(ticket: CGRect, d: &CardData, top: f64, bottom: f64, img: &CGImage) {
    let x0 = ticket.min().x;
    let px_w = CGImage::width(Some(img)) as f64;
    let px_h = CGImage::height(Some(img)) as f64;
    if px_w <= 0.0 || px_h <= 0.0 {
        return;
    }
    // Image fit: (288 - 2*16 - 2*7) wide, (content - 2*14 - 2*7) tall at most.
    let avail_inner_w = CARD_W - 2.0 * 16.0 - 14.0;
    let avail_inner_h = (bottom - top - 2.0 * 14.0 - 14.0).max(0.0);
    let scale = (avail_inner_w / px_w).min(avail_inner_h / px_h);
    let iw = px_w * scale;
    let ih = px_h * scale;
    let photo_w = iw + 14.0;
    let photo_h = ih + 14.0;
    // Title-less image/video cards center vertically; with a title (or text
    // kinds) the print starts at the 16/14 padding.
    let has_title = d
        .item
        .title
        .as_deref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    let (px, py) = if !has_title {
        (
            x0 + (CARD_W - photo_w) / 2.0,
            top + 14.0 + ((bottom - top - 28.0 - photo_h) / 2.0).max(0.0),
        )
    } else {
        (x0 + 16.0, top + 14.0)
    };
    let photo = CGRect::new(CGPoint::new(px, py), CGSize::new(photo_w, photo_h));
    let center = CGPoint::new(photo.mid().x, photo.mid().y);
    NSGraphicsContext::saveGraphicsState_class();
    // Rotate the canvas −1.6° around the photo's center.
    if let Some(gc) = NSGraphicsContext::currentContext() {
        let cg = gc.CGContext();
        objc2_core_graphics::CGContext::translate_ctm(Some(&cg), center.x, center.y);
        objc2_core_graphics::CGContext::rotate_ctm(Some(&cg), -1.6_f64.to_radians());
        objc2_core_graphics::CGContext::translate_ctm(Some(&cg), -center.x, -center.y);
    }
    // White border with the photo shadow.
    NSGraphicsContext::saveGraphicsState_class();
    shadow(&NSColor::blackColor().colorWithAlphaComponent(0.3), 6.0, 1.0, 4.0).set();
    NSColor::whiteColor().setFill();
    NSBezierPath::bezierPathWithRect(photo).fill();
    NSGraphicsContext::restoreGraphicsState_class();
    let nsimg = NSImage::initWithCGImage_size(NSImage::alloc(), img, CGSize::new(px_w, px_h));
    draw_image(
        &nsimg,
        CGRect::new(
            CGPoint::new(px + 7.0, py + 7.0),
            CGSize::new(iw, ih),
        ),
        objc2_app_kit::NSCompositingOperation::SourceOver,
        1.0,
    );
    if d.item.kind == ClipKind::Video {
        // Play badge: 48pt half-black disc, white play glyph nudged 2pt right.
        let badge = CGRect::new(
            CGPoint::new(center.x - 24.0, center.y - 24.0),
            CGSize::new(48.0, 48.0),
        );
        NSColor::blackColor().colorWithAlphaComponent(0.5).setFill();
        NSBezierPath::bezierPathWithOvalInRect(badge).fill();
        if let Some(img) = symbol("play.fill", 18.0, SymbolWeight::Regular, &NSColor::whiteColor()) {
            draw_centered(
                &img,
                CGRect::new(
                    CGPoint::new(center.x - 24.0 + 2.0, center.y - 24.0),
                    CGSize::new(48.0, 48.0),
                ),
                1.0,
            );
        }
    }
    NSGraphicsContext::restoreGraphicsState_class();
}

/// The dashed mid-line.
fn perforation(ticket: CGRect) {
    let y = ticket.max().y - STUB_H - 0.5;
    let line = theme::line_path(CGRect::new(
        CGPoint::new(ticket.min().x + PAD_X, y - 0.5),
        CGSize::new(CARD_W - 2.0 * PAD_X, 1.0),
    ));
    theme::ink().colorWithAlphaComponent(0.7).setStroke();
    line.setLineWidth(1.0);
    unsafe {
        let dash: [f64; 2] = [3.0, 4.0];
        line.setLineDash_count_phase(dash.as_ptr(), 2, 0.0);
    }
    line.stroke();
}

/// `captionRow`: kind · note on the left, the 「已复制」 capsule on the blue
/// card; a 1pt rule at its bottom.
fn caption_row(ticket: CGRect, d: &CardData) {
    let x0 = ticket.min().x;
    let row_y = ticket.max().y - STUB_H; // [bottom-96, bottom-52]
    let text = caption(&d.item);
    let font = theme::serif(14.0, false);
    let mut right = x0 + CARD_W - PAD_X;
    if d.on_clipboard {
        // Chip: ✓ 已复制, padding 10/4, hairline capsule.
        let chip_font = theme::serif(13.0, false);
        let label = l("已复制");
        let mut content_w = measure(&label, &chip_font, 200.0, None).width;
        let check = symbol("checkmark", 10.0, SymbolWeight::Semibold, &theme::ink());
        let check_w = check.as_ref().map(|i| i.size().width).unwrap_or(0.0);
        content_w += 5.0 + check_w;
        let chip_w = content_w + 20.0;
        let chip_h = line_height(&chip_font) + 8.0;
        let chip = CGRect::new(
            CGPoint::new(right - chip_w, row_y + (CAPTION_H - chip_h) / 2.0),
            CGSize::new(chip_w, chip_h),
        );
        let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            chip,
            chip_h / 2.0,
            chip_h / 2.0,
        );
        theme::ink().colorWithAlphaComponent(0.8).setStroke();
        capsule.setLineWidth(1.0);
        capsule.stroke();
        if let Some(c) = &check {
            draw_centered(
                c,
                CGRect::new(
                    CGPoint::new(chip.min().x + 10.0, chip.min().y),
                    CGSize::new(check_w, chip_h),
                ),
                1.0,
            );
        }
        draw_line_centered(
            &label,
            &chip_font,
            &theme::ink(),
            chip.min().x + 10.0 + check_w + 5.0,
            row_y + CAPTION_H / 2.0,
            content_w,
        );
        right -= chip_w + 8.0;
    }
    draw_line_centered(
        &text,
        &font,
        &theme::ink(),
        x0 + PAD_X,
        row_y + CAPTION_H / 2.0,
        (right - x0 - PAD_X).max(0.0),
    );
    theme::ink().colorWithAlphaComponent(0.7).setFill();
    NSBezierPath::bezierPathWithRect(CGRect::new(
        CGPoint::new(x0 + PAD_X, row_y + CAPTION_H - 1.0),
        CGSize::new(CARD_W - 2.0 * PAD_X, 1.0),
    ))
    .fill();
}

/// The stub: action buttons with thin uprights.
fn actions(ticket: CGRect, d: &CardData) {
    let ink = theme::ink();
    let row_top = ticket.max().y - (STUB_H - CAPTION_H);
    let center_y = row_top + (STUB_H - CAPTION_H) / 2.0;
    for (kind, rect) in action_rects(ticket, d.item.kind) {
        match kind {
            ActionKind::Divider => {
                theme::ink().colorWithAlphaComponent(0.35).setFill();
                NSBezierPath::bezierPathWithRect(CGRect::new(
                    CGPoint::new(rect.min().x, center_y - 11.0),
                    CGSize::new(1.0, 22.0),
                ))
                .fill();
            }
            ActionKind::Pin => pin_button(rect, center_y, d),
            _ => {
                let name = match kind {
                    ActionKind::Edit => "pencil",
                    ActionKind::Preview => "eye",
                    ActionKind::Save => "arrow.down.to.line",
                    ActionKind::Trash => "trash",
                    _ => unreachable!(),
                };
                if let Some(img) = symbol(name, 17.0, SymbolWeight::Regular, &ink) {
                    // Buttons are 36pt tall, centered in the 52pt row.
                    draw_centered(
                        &img,
                        CGRect::new(
                            CGPoint::new(rect.min().x, center_y - 18.0),
                            CGSize::new(rect.size.width, 36.0),
                        ),
                        1.0,
                    );
                }
            }
        }
    }
}

/// `pinAction`: pinned = a small torn patch behind a contrasted pin.
fn pin_button(rect: CGRect, center_y: f64, d: &CardData) {
    let pinned = d.item.pinned;
    let (glyph_color, patch_fill) = if pinned {
        if d.on_clipboard {
            (theme::paper_blue_deep(), theme::paper_tile())
        } else {
            (theme::paper(), theme::paper_blue_tile())
        }
    } else {
        (theme::ink(), theme::paper_tile())
    };
    if pinned {
        // Torn patch 34×30 behind the glyph, contrasting paper + tight shadow.
        let patch_rect = CGRect::new(
            CGPoint::new(rect.min().x + (rect.size.width - 34.0) / 2.0, center_y - 15.0),
            CGSize::new(34.0, 30.0),
        );
        NSGraphicsContext::saveGraphicsState_class();
        shadow(&NSColor::blackColor().colorWithAlphaComponent(0.25), 2.0, 0.0, 1.0).set();
        NSColor::colorWithPatternImage(&patch_fill).setFill();
        theme::torn_paper_path(patch_rect, true, true, true, true, 77, 1.5, 5.0).fill();
        NSGraphicsContext::restoreGraphicsState_class();
    }
    let name = if pinned { "pin.fill" } else { "pin" };
    if let Some(img) = symbol(name, 17.0, SymbolWeight::Regular, &glyph_color) {
        draw_centered(
            &img,
            CGRect::new(
                CGPoint::new(rect.min().x + (rect.size.width - 34.0) / 2.0, center_y - 15.0),
                CGSize::new(34.0, 30.0),
            ),
            1.0,
        );
    }
}

/// The pushpin through the top of the highlighted card.
fn pushpin(ticket: CGRect, d: &CardData) {
    if !d.selected {
        return;
    }
    let Some(img) = theme::pushpin() else {
        return;
    };
    let size = img.size();
    if size.height <= 0.0 {
        return;
    }
    let scale = 44.0 / size.height;
    let w = size.width * scale;
    let rect = CGRect::new(
        CGPoint::new(ticket.min().x + (CARD_W - w) / 2.0, ticket.min().y - 6.0),
        CGSize::new(w, 44.0),
    );
    NSGraphicsContext::saveGraphicsState_class();
    shadow(&NSColor::blackColor().colorWithAlphaComponent(0.28), 2.0, 1.0, 2.0).set();
    draw_image(
        &img,
        rect,
        objc2_app_kit::NSCompositingOperation::SourceOver,
        1.0,
    );
    NSGraphicsContext::restoreGraphicsState_class();
}


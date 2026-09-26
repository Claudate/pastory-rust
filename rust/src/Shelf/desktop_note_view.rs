//! Port of `Shelf/DesktopNoteView.swift` — the card as a sticky note:
//! same paper, full content, three actions (edit / layer / close). Single
//! click copies, double-click pastes into the app in front. Width matches
//! the shelf card; height follows the content (text capped at 60% of the
//! screen, scrolling inside the card only).
//!
//! One `NoteView` draws the whole note (paper + pushpin + header + title +
//! kind content + perforation + caption + the 编辑/图层/关闭 slots) and
//! routes clicks through hit rects; `desktop_notes.rs` owns the windows
//! (move/resize happen at the window level, like the Swift NoteWindow).

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBezierPath, NSEvent, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::app::localization::l;
use crate::app::{permissions, preferences::Preferences, theme};
use crate::clipboard::item::{ClipItem, ClipKind};

pub const NOTE_W: f64 = 288.0;
pub const NOTE_MARGIN: f64 = 14.0;
const SIDE: f64 = 18.0;
/// Text content cap (60% of screen height, same fraction as Swift).
pub const TEXT_CAP_FRACTION: f64 = 0.6;
pub const MIN_W: f64 = 220.0;
pub const MIN_H: f64 = 150.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoteAction {
    Edit,
    Layer,
    Close,
    Copy,
}

pub struct NoteViewIvars {
    item_id: RefCell<String>,
    show_copied: RefCell<bool>,
    hits: RefCell<Vec<(NoteAction, CGRect)>>,
}

define_class!(
    // SAFETY: plain NSView with note-state ivars; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = NoteViewIvars]
    pub struct NoteView;

    unsafe impl NSObjectProtocol for NoteView {}

    impl NoteView {
        #[unsafe(method(isFlipped))]
        fn nv_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn nv_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn nv_draw_rect(&self, _dirty: CGRect) {
            draw_note(self);
        }

        #[unsafe(method(mouseUp:))]
        fn nv_mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            for (action, rect) in self.ivars().hits.borrow().iter() {
                if contains(rect, p) {
                    run_action(*action, self, event);
                    return;
                }
            }
            run_action(NoteAction::Copy, self, event);
        }
    }
);

impl NoteView {
    pub fn make(frame: CGRect, item_id: &str) -> Retained<NoteView> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<NoteView>().set_ivars(NoteViewIvars {
            item_id: RefCell::new(item_id.to_string()),
            show_copied: RefCell::new(false),
            hits: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub fn item_id(&self) -> String {
        self.ivars().item_id.borrow().clone()
    }

    pub fn item(&self) -> Option<ClipItem> {
        let id = self.item_id();
        crate::clipboard::store::read(|s| s.items.iter().find(|it| it.id == id).cloned())
    }

    pub fn set_show_copied(&self, v: bool) {
        *self.ivars().show_copied.borrow_mut() = v;
        self.setNeedsDisplay(true);
    }

    /// `fitToContent`: the note's natural height at NOTE_W (= the card's own
    /// height at 288pt, the body band shrinking to a scroll over tall text).
    pub fn natural_height(item: &ClipItem) -> f64 {
        let mtm = MainThreadMarker::new().expect("main thread");
        let cap = screen_height(mtm)
            * TEXT_CAP_FRACTION;
        let mut h = 26.0 + 8.0 + 1.0 + 9.0; // header rule
        if item.title.as_deref().map(|t| !t.is_empty()).unwrap_or(false) {
            h += 6.0 + 26.0 + 4.0 + 1.0;
        }
        h += 6.0;
        let content = match item.kind {
            ClipKind::Text => {
                let text = crate::clipboard::store::read(|s| s.text(item).unwrap_or_default());
                let m = crate::shelf::card::measure(&text, &theme::serif(14.0, false), NOTE_W - SIDE * 2.0, Some(5.0));
                (m.height.ceil() + 2.0 + 12.0).min(cap) + 12.0
            }
            ClipKind::Url => 12.0 + 44.0 + 12.0,
            ClipKind::Files => 12.0 + (item.snippet.lines().take(12).count() as f64) * 20.0 + 12.0,
            ClipKind::Image | ClipKind::Video => {
                let (pw, ph) = (item.pixel_width.unwrap_or(1).max(1) as f64, item.pixel_height.unwrap_or(1).max(1) as f64);
                6.0 + (NOTE_W - SIDE * 2.0 - 12.0) * ph / pw + 6.0 + 28.0
            }
        };
        h += content;
        h += 5.0 + 40.0; // perforation gap + caption row
        h.max(MIN_H - NOTE_MARGIN * 2.0)
    }
}

fn screen_height(mtm: MainThreadMarker) -> f64 {
    objc2_app_kit::NSScreen::mainScreen(mtm)
        .map(|s| s.visibleFrame().size.height)
        .unwrap_or(900.0)
}

fn contains(r: &CGRect, p: CGPoint) -> bool {
    p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y
}

fn hit(view: &NoteView, action: NoteAction, rect: CGRect) {
    view.ivars().hits.borrow_mut().push((action, rect));
}

fn draw_note(view: &NoteView) {
    view.ivars().hits.borrow_mut().clear();
    let b = view.bounds();
    let margin = NOTE_MARGIN;
    let paper = CGRect::new(
        CGPoint::new(margin, margin),
        CGSize::new((b.size.width - margin * 2.0).max(1.0), (b.size.height - margin * 2.0).max(1.0)),
    );
    {
        objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
        crate::shelf::card::shadow(&objc2_app_kit::NSColor::blackColor().colorWithAlphaComponent(0.22), 6.0, 1.0, 3.0).set();
        theme::paper().setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(paper, 6.0, 6.0).fill();
        objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
    }
    let Some(item) = view.item() else { return };
    if let Some(pin) = theme::pushpin() {
        let h = 40.0f64.min(pin.size().height.max(1.0));
        let w = h * pin.size().width / pin.size().height.max(1.0);
        let rect = CGRect::new(CGPoint::new(paper.mid().x - w / 2.0, paper.min().y - 6.0), CGSize::new(w, h));
        objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
        crate::shelf::card::shadow(&objc2_app_kit::NSColor::blackColor().colorWithAlphaComponent(0.28), 2.0, 1.0, 2.0).set();
        crate::shelf::card::draw_centered(&pin, rect, 1.0);
        objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
    }
    // Resize grip: three short diagonals at the bottom-right band.
    theme::ink().colorWithAlphaComponent(0.35).setStroke();
    for k in [4.0f64, 8.0, 12.0] {
        let path = NSBezierPath::new();
        path.moveToPoint(CGPoint::new(b.max().x - margin - k, b.max().y - margin));
        path.lineToPoint(CGPoint::new(b.max().x - margin, b.max().y - margin - k));
        path.setLineWidth(1.0);
        path.stroke();
    }
    draw_content(view, &item, paper);
}

fn draw_content(view: &NoteView, item: &ClipItem, paper: CGRect) {
    let mut y = paper.min().y + 26.0;
    if let Some(icon) = theme::card_icon(item.source_bundle_id.as_deref()) {
        let image = CGRect::new(CGPoint::new(paper.min().x + SIDE, y - 10.0), CGSize::new(20.0, 20.0));
        crate::shelf::card::draw_centered(&icon, image, 1.0);
    } else if let Some(img) = crate::shelf::card::symbol("doc.on.clipboard", 13.0, crate::shelf::card::SymbolWeight::Regular, &theme::ink()) {
        crate::shelf::card::draw_centered(&img, CGRect::new(CGPoint::new(paper.min().x + SIDE, y - 10.0), CGSize::new(20.0, 20.0)), 1.0);
    }
    let name = crate::shelf::card::source_title(item);
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(paper.min().x + SIDE + 28.0, y - 8.0), CGSize::new(180.0, 22.0)),
        &name,
        &theme::serif(15.0, false),
        &theme::ink(),
        None,
    );
    let when = crate::shelf::card::when(item.created_at);
    let time_font = theme::serif(13.0, false);
    let tw = crate::shelf::card::measure(&when, &time_font, 200.0, None).width;
    crate::shelf::card::draw_line_centered(&when, &time_font, &theme::ink().colorWithAlphaComponent(0.7), paper.max().x - SIDE - tw, y + 2.0, tw + 1.0);
    y += 8.0;
    theme::ink().colorWithAlphaComponent(0.7).setFill();
    NSBezierPath::bezierPathWithRect(CGRect::new(
        CGPoint::new(paper.min().x + SIDE, y),
        CGSize::new(paper.size.width - SIDE * 2.0, 1.0),
    )).fill();
    y += 9.0;
    if let Some(title) = &item.title {
        if !title.is_empty() {
            crate::shelf::card::draw_text(
                CGRect::new(CGPoint::new(paper.min().x + SIDE, y + 6.0), CGSize::new(paper.size.width - SIDE * 2.0, 26.0)),
                title,
                &theme::script(20.0),
                &theme::ink(),
                None,
            );
            theme::ink().colorWithAlphaComponent(0.6).setFill();
            NSBezierPath::bezierPathWithRect(CGRect::new(
                CGPoint::new(paper.min().x + SIDE, y + 6.0 + 26.0 + 3.0),
                CGSize::new(paper.size.width - SIDE * 2.0, 1.0),
            )).fill();
            y += 6.0 + 26.0 + 4.0 + 1.0;
        }
    }
    y += 6.0;
    let content_top = y;
    let caption_h = 40.0;
    let content_bottom = paper.max().y - caption_h - 5.0;
    let inner = CGRect::new(
        CGPoint::new(paper.min().x + SIDE, content_top),
        CGSize::new(paper.size.width - SIDE * 2.0, (content_bottom - content_top).max(0.0)),
    );
    draw_kind(inner, item);
    theme::ink().colorWithAlphaComponent(0.7).setStroke();
    let dash: [f64; 2] = [3.0, 4.0];
    let perf = NSBezierPath::new();
    let y_perf = paper.max().y - caption_h + 0.5;
    perf.moveToPoint(CGPoint::new(paper.min().x + SIDE, y_perf));
    perf.lineToPoint(CGPoint::new(paper.max().x - SIDE, y_perf));
    unsafe {
        perf.setLineDash_count_phase(dash.as_ptr(), 2, 0.0);
    }
    perf.stroke();
    let cap_y = paper.max().y - caption_h;
    let label = kind_label(item);
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(paper.min().x + SIDE, cap_y + 10.0), CGSize::new(150.0, 22.0)),
        &label,
        &theme::serif(13.0, false),
        &theme::ink_muted(),
        None,
    );
    if *view.ivars().show_copied.borrow() {
        let chip_font = theme::serif(13.0, false);
        let chip_text = l("已复制");
        let tw = crate::shelf::card::measure(&chip_text, &chip_font, 200.0, None).width;
        let cell_w = 10.0 + 12.0 + 5.0 + tw + 10.0;
        let cell_h = 22.0;
        let three_slots = 30.0 * 3.0 + 12.0;
        let chip = CGRect::new(
            CGPoint::new(paper.max().x - 8.0 - three_slots - cell_w, cap_y + (40.0 - cell_h) / 2.0),
            CGSize::new(cell_w, cell_h),
        );
        theme::ink().colorWithAlphaComponent(0.8).setStroke();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(chip, cell_h / 2.0, cell_h / 2.0).stroke();
        if let Some(img) = crate::shelf::card::symbol("checkmark", 10.0, crate::shelf::card::SymbolWeight::Semibold, &theme::ink()) {
            crate::shelf::card::draw_centered(&img, CGRect::new(CGPoint::new(chip.min().x + 10.0, chip.min().y + (cell_h - 12.0) / 2.0), CGSize::new(12.0, 12.0)), 1.0);
        }
        crate::shelf::card::draw_line_centered(&chip_text, &chip_font, &theme::ink(), chip.min().x + 10.0 + 12.0 + 5.0, chip.mid().y, tw + 1.0);
    }
    let on_top = crate::shelf::desktop_notes::is_on_top(&item.id);
    let mut ax = paper.max().x - 8.0;
    ax -= 30.0;
    let close_rect = CGRect::new(CGPoint::new(ax, cap_y + 5.0), CGSize::new(30.0, 30.0));
    if let Some(img) = crate::shelf::card::symbol("xmark", 14.0, crate::shelf::card::SymbolWeight::Regular, &theme::ink()) {
        crate::shelf::card::draw_centered(&img, close_rect, 1.0);
    }
    hit(view, NoteAction::Close, close_rect);
    ax -= 30.0;
    let layer_rect = CGRect::new(CGPoint::new(ax, cap_y + 5.0), CGSize::new(30.0, 30.0));
    let layer_icon = if on_top { "square.3.layers.3d.top.filled" } else { "square.3.layers.3d.bottom.filled" };
    if let Some(img) = crate::shelf::card::symbol(layer_icon, 14.0, crate::shelf::card::SymbolWeight::Regular, &theme::ink()) {
        crate::shelf::card::draw_centered(&img, layer_rect, 1.0);
    }
    hit(view, NoteAction::Layer, layer_rect);
    if matches!(item.kind, ClipKind::Text | ClipKind::Url | ClipKind::Image) {
        ax -= 30.0;
        let edit_rect = CGRect::new(CGPoint::new(ax, cap_y + 5.0), CGSize::new(30.0, 30.0));
        if let Some(img) = crate::shelf::card::symbol("pencil", 14.0, crate::shelf::card::SymbolWeight::Regular, &theme::ink()) {
            crate::shelf::card::draw_centered(&img, edit_rect, 1.0);
        }
        hit(view, NoteAction::Edit, edit_rect);
    }
}

/// Truncate middle for captions (file lists use the same treatment).
fn truncate_middle(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars / 2).collect();
    let tail: String = s.chars().rev().take(max_chars / 2).collect::<String>().chars().rev().collect();
    format!("{head}…{tail}")
}

fn draw_kind(inner: CGRect, item: &ClipItem) {
    match item.kind {
        ClipKind::Text => {
            let text = crate::clipboard::store::read(|s| s.text(item).unwrap_or_default());
            crate::shelf::card::draw_text(inner, &text, &theme::serif(14.0, false), &theme::ink(), Some(5.0));
        }
        ClipKind::Url => {
            let host = item
                .snippet
                .split_once("://")
                .map(|(_, r)| r.split(['/', '?', '#']).next().unwrap_or(""))
                .unwrap_or("");
            crate::shelf::card::draw_text(
                CGRect::new(CGPoint::new(inner.min().x, inner.min().y + 4.0), CGSize::new(inner.size.width, 24.0)),
                host,
                &theme::serif(15.0, true),
                &theme::ink(),
                None,
            );
            crate::shelf::card::draw_text(
                CGRect::new(CGPoint::new(inner.min().x, inner.min().y + 30.0), CGSize::new(inner.size.width, inner.size.height - 30.0)),
                &item.snippet,
                &theme::serif(13.0, false),
                &theme::paper_blue_deep(),
                Some(1.0),
            );
        }
        ClipKind::Files => {
            let font = theme::serif(14.0, false);
            let mut y = inner.min().y + 4.0;
            for line in item.snippet.lines().take(12) {
                crate::shelf::card::draw_text(
                    CGRect::new(CGPoint::new(inner.min().x, y), CGSize::new(inner.size.width, 20.0)),
                    &truncate_middle(line, 30),
                    &font,
                    &theme::ink(),
                    None,
                );
                y += 20.0;
                if y > inner.max().y - 16.0 {
                    break;
                }
            }
        }
        ClipKind::Image | ClipKind::Video => {
            if let Some(img) = crate::clipboard::store::with(|s| s.thumbnail(item)) {
                let inner_w = inner.size.width - 12.0;
                let (pw, ph) = (item.pixel_width.unwrap_or(1).max(1) as f64, item.pixel_height.unwrap_or(1).max(1) as f64);
                let scaled = (inner_w * ph / pw).min(inner.size.height - 12.0).max(1.0);
                let w = scaled * pw / ph.max(1.0);
                let rect = CGRect::new(
                    CGPoint::new(inner.min().x + (inner.size.width - w) / 2.0, inner.min().y + 6.0),
                    CGSize::new(w, scaled),
                );
                let white = CGRect::new(
                    CGPoint::new(rect.min().x - 6.0, rect.min().y - 6.0),
                    CGSize::new(rect.size.width + 12.0, rect.size.height + 12.0),
                );
                objc2_app_kit::NSColor::whiteColor().setFill();
                NSBezierPath::bezierPathWithRect(white).fill();
                let mtm = MainThreadMarker::new().expect("main thread");
                let ns_img = objc2_app_kit::NSImage::initWithCGImage_size(mtm.alloc(), &img, rect.size);
                crate::shelf::card::draw_centered(&ns_img, rect, 1.0);
                if item.kind == ClipKind::Video {
                    let circle = CGRect::new(
                        CGPoint::new(rect.mid().x - 22.0, rect.mid().y - 22.0),
                        CGSize::new(44.0, 44.0),
                    );
                    objc2_app_kit::NSColor::blackColor().colorWithAlphaComponent(0.5).setFill();
                    NSBezierPath::bezierPathWithOvalInRect(circle).fill();
                    if let Some(play) = crate::shelf::card::symbol("play.fill", 16.0, crate::shelf::card::SymbolWeight::Regular, &objc2_app_kit::NSColor::whiteColor()) {
                        crate::shelf::card::draw_centered(&play, CGRect::new(
                            CGPoint::new(circle.mid().x - 8.0 + 2.0, circle.mid().y - 8.0),
                            CGSize::new(18.0, 16.0),
                        ), 1.0);
                    }
                }
            } else {
                let icon = if item.kind == ClipKind::Video { "film" } else { "photo" };
                if let Some(img) = crate::shelf::card::symbol(icon, 34.0, crate::shelf::card::SymbolWeight::Light, &theme::ink_muted().colorWithAlphaComponent(0.5)) {
                    crate::shelf::card::draw_centered(&img, CGRect::new(
                        CGPoint::new(inner.mid().x - 20.0, inner.min().y + (inner.size.height - 40.0).max(40.0) / 2.0),
                        CGSize::new(40.0, 40.0),
                    ), 1.0);
                }
            }
        }
    }
}

fn kind_label(item: &ClipItem) -> String {
    match item.kind {
        ClipKind::Image => format!("{} · {}", l("图片"), item.snippet.replace('×', " × ")),
        ClipKind::Text => l("文本"),
        ClipKind::Url => l("链接"),
        ClipKind::Files => l("文件"),
        ClipKind::Video => item.ext.to_uppercase(),
    }
}

// MARK: Card → note actions

fn run_action(action: NoteAction, view: &NoteView, event: &NSEvent) {
    let Some(item) = view.item() else { return };
    match action {
        NoteAction::Copy => {
            crate::clipboard::store::with(|s| s.copy_to_pasteboard(&item));
            if event.clickCount() >= 2 {
                if Preferences::shared().paste_on_double_click() && permissions::has_accessibility() {
                    crate::app::delegate::dispatch_main_after(0.08, Box::new(|| {
                        permissions::send_paste();
                    }));
                }
            } else {
                view.set_show_copied(true);
                let raw = view as *const NoteView as usize;
                crate::app::delegate::dispatch_main_after(1.4, Box::new(move || {
                    if crate::shelf::desktop_notes::view_alive(raw) {
                        let v = unsafe { &*(raw as *const NoteView) };
                        v.set_show_copied(false);
                    }
                }));
            }
        }
        NoteAction::Edit => crate::clipboard::store::read(|s| {
            if let Some(it) = s.items.iter().find(|it| it.id == item.id).cloned() {
                match it.kind {
                    ClipKind::Text | ClipKind::Url => crate::shelf::text_editor::TextEditorWindow::open(&it),
                    ClipKind::Image => crate::shelf::image_editor::ImageEditorWindow::open(&it),
                    _ => {}
                }
            }
        }),
        NoteAction::Layer => crate::shelf::desktop_notes::toggle_layer(&item.id),
        NoteAction::Close => crate::shelf::desktop_notes::close(&item.id),
    }
}

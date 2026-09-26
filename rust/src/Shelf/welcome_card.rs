//! Port of `Shelf/WelcomeCard.swift` — first launch: a card built like
//! every other card on the shelf — pushpin, source line, handwritten
//! title — whose body sets the two shortcuts and then asks you to try
//! them. 「开始使用」 retires it for good (`finishWelcome`).
//!
//! One `WelcomeCardView` subclass draws the whole card and owns its two
//! shortcut-recorder rows (same live capture as the settings pane's).

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBezierPath, NSEvent, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::app::localization::l;
use crate::app::theme;

pub struct WelcomeCardViewIvars {
    state: RefCell<State>,
}

#[derive(Clone)]
struct State {
    capture: crate::app::preferences::Shortcut,
    shelf_sh: crate::app::preferences::Shortcut,
    tried: std::collections::HashSet<String>,
    notice_capture: Option<String>,
    notice_shelf: Option<String>,
    capturing: Option<&'static str>,
    taking_capture: bool,
    taking_shelf: bool,
}

impl Default for State {
    fn default() -> State {
        State {
            capture: crate::app::preferences::Shortcut::NONE,
            shelf_sh: crate::app::preferences::Shortcut::NONE,
            tried: std::collections::HashSet::new(),
            notice_capture: None,
            notice_shelf: None,
            capturing: None,
            taking_capture: false,
            taking_shelf: false,
        }
    }
}

impl State {
    fn tried_tag(&self, tag: &str) -> bool {
        self.tried.contains(tag)
    }
    fn capture_taken(&self) -> bool {
        crate::app::hotkey::HotKeyCenter::shared().failed().iter().any(|n| n == "capture")
    }
    fn shelf_taken(&self) -> bool {
        crate::app::hotkey::HotKeyCenter::shared().failed().iter().any(|n| n == "shelf")
    }
}

define_class!(
    // SAFETY: plain NSView; main-thread only. Shortcut capture reuses the
    // settings recorder's model contract (⌘/⌥/⌃/⇧ + a key; bare ⌘/⇧ warns).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = WelcomeCardViewIvars]
    pub struct WelcomeCardView;

    unsafe impl NSObjectProtocol for WelcomeCardView {}

    impl WelcomeCardView {
        #[unsafe(method(isFlipped))]
        fn wc_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn wc_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn wc_draw_rect(&self, _dirty: CGRect) {
            draw(self);
        }

        #[unsafe(method(mouseUp:))]
        fn wc_mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let b = self.bounds();
            for (which, col_x, col_w) in [("shelf", left_column_x(b), column_width(b)), ("capture", left_column_x(b), column_width(b))] {
                let rect = shortcut_rect(b, which, col_x, col_w);
                if contains(&rect, p) {
                    start_capture(self, which);
                    return;
                }
            }
            let done = done_rect(b);
            if contains(&done, p) {
                crate::shelf::panel::with_model_mut(|m| m.finish_welcome());
                crate::shelf::view::refresh();
            }
        }
    }
);

impl WelcomeCardView {
    pub fn make(frame: CGRect) -> Retained<WelcomeCardView> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<WelcomeCardView>().set_ivars(WelcomeCardViewIvars {
            state: RefCell::new(State::default()),
        });
        let v: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        v.refresh_state();
        v
    }

    pub fn refresh_state(&self) {
        let prefs = crate::app::preferences::Preferences::shared();
        let mut st = self.ivars().state.borrow_mut();
        st.capture = prefs.shortcut(crate::app::preferences::key::HOTKEY_CAPTURE);
        st.shelf_sh = prefs.shortcut(crate::app::preferences::key::HOTKEY_SHELF);
        st.tried = prefs.welcome_tried();
        drop(st);
        self.setNeedsDisplay(true);
    }

    pub fn set_notice(&self, which: &'static str, note: Option<String>) {
        let mut st = self.ivars().state.borrow_mut();
        match which {
            "capture" => st.notice_capture = note,
            _ => st.notice_shelf = note,
        }
        drop(st);
        self.setNeedsDisplay(true);
    }

    pub fn set_capturing(&self, which: Option<&'static str>) {
        self.ivars().state.borrow_mut().capturing = which;
        self.setNeedsDisplay(true);
    }

    pub fn capturing(&self) -> Option<&'static str> {
        self.ivars().state.borrow().capturing
    }

    pub fn on_shortcuts_changed(&self) {
        self.refresh_state();
    }
}

fn contains(r: &CGRect, p: CGPoint) -> bool {
    p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y
}

fn left_column_x(b: CGRect) -> f64 {
    b.min().x + 18.0
}
fn column_width(b: CGRect) -> f64 {
    (b.size.width - 18.0 * 2.0 - 40.0 - 18.0 * 2.0) / 2.0
}
fn right_column_x(b: CGRect) -> f64 {
    left_column_x(b) + column_width(b) + 40.0
}

/// The keycap row area for each step (recorder hit).
fn shortcut_rect(b: CGRect, which: &'static str, col_x: f64, col_w: f64) -> CGRect {
    let _ = col_w;
    let top = rule_y(b) + 14.0 + 26.0 + 8.0;
    let step_h = 22.0 + 8.0 + 28.0;
    let index = if which == "shelf" { 0.0 } else { 1.0 };
    CGRect::new(
        CGPoint::new(col_x, top + index * (step_h + 8.0) + 22.0),
        CGSize::new(200.0, 28.0),
    )
}

fn rule_y(b: CGRect) -> f64 {
    let _ = b;
    26.0 + 30.0 + 2.0
}

fn done_rect(b: CGRect) -> CGRect {
    let bw = 128.0;
    CGRect::new(
        CGPoint::new(b.max().x - 18.0 - bw, b.max().y - 12.0 - 34.0),
        CGSize::new(bw, 34.0),
    )
}

// MARK: Draw

pub fn draw(view: &WelcomeCardView) {
    let b = view.bounds();
    let w = if crate::app::localization::is_english() { 560.0 } else { 480.0 };
    let h = b.size.height;
    let card = CGRect::new(CGPoint::ZERO, CGSize::new(w, h));
    {
        objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
        crate::shelf::card::shadow(&objc2_app_kit::NSColor::blackColor().colorWithAlphaComponent(0.4), 9.0, 2.0, 6.0).set();
        theme::paper().setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(card, 6.0, 6.0).fill();
        objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
    }
    if let Some(pin) = theme::pushpin() {
        let ph = 44.0f64.min(pin.size().height.max(1.0));
        let pw = ph * pin.size().width / pin.size().height.max(1.0);
        let rect = CGRect::new(CGPoint::new(card.mid().x - pw / 2.0, card.min().y - 6.0), CGSize::new(pw, ph));
        objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
        crate::shelf::card::shadow(&objc2_app_kit::NSColor::blackColor().colorWithAlphaComponent(0.28), 2.0, 1.0, 2.0).set();
        crate::shelf::card::draw_centered(&pin, rect, 1.0);
        objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
    }
    // Header: logo · 「Pastory」 serif 16, rule under.
    if let Some(logo) = theme::logo() {
        let r = CGRect::new(CGPoint::new(18.0, 26.0), CGSize::new(22.0, 22.0));
        crate::shelf::card::draw_centered(&logo, r, 1.0);
    }
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(48.0, 26.0), CGSize::new(200.0, 22.0)),
        "Pastory",
        &theme::serif(16.0, false),
        &theme::ink(),
        None,
    );
    rule(18.0, 48.0, w - 36.0);
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(18.0, 56.0), CGSize::new(w - 36.0, 30.0)),
        &l("欢迎使用 Pastory"),
        &theme::script(24.0),
        &theme::ink(),
        None,
    );
    theme::ink().colorWithAlphaComponent(0.6).setFill();
    NSBezierPath::bezierPathWithRect(CGRect::new(CGPoint::new(18.0, rule_y(b)), CGSize::new(w - 36.0, 1.0))).fill();

    let st = view.ivars().state.borrow();
    let ry = rule_y(b);
    let left = left_column_x(b);
    let right = right_column_x(b);
    heading(left, ry + 14.0, &l("设置常用快捷键"));
    heading(right, ry + 14.0, &l("试一试"));
    theme::ink().colorWithAlphaComponent(0.15).setFill();
    NSBezierPath::bezierPathWithRect(CGRect::new(CGPoint::new(right - 20.0, ry + 18.0), CGSize::new(1.0, h - ry - 100.0))).fill();

    let mut y_step = ry + 14.0 + 26.0 + 8.0;
    step_row(view, left, y_step, &l("显示 / 隐藏剪贴板"), "shelf");
    y_step += 22.0 + 8.0 + 28.0 + 8.0;
    step_row(view, left, y_step, &l("截图"), "capture");

    let mut y_todo = ry + 14.0 + 26.0 + 8.0;
    let todos: [(bool, String); 6] = [
        (st.tried_tag("shelf"), if st.shelf_sh.is_set() { l("按 %@ 打开或收起剪贴板").replacen("%@", &st.shelf_sh.display(), 1) } else { l("先给剪贴板设一个快捷键") }),
        (st.tried_tag("capture"), if st.capture.is_set() { l("按 %@ 截一张图").replacen("%@", &st.capture.display(), 1) } else { l("先给截图设一个快捷键") }),
        (st.tried_tag("copy"), l("复制一段文本")),
        (st.tried_tag("pin"), l("将一个卡片 Pin 起来")),
        (st.tried_tag("desktop"), l("把一张卡片向上拖出面板，贴到桌面上")),
        (false, l("点击「开始使用」，卡片消失")),
    ];
    for (done_flag, text) in &todos {
        todo_row(right, y_todo, *done_flag, text);
        y_todo += 26.0;
    }
    drop(st);

    ticket_rule(b.max().y - h + h - 54.0, w);
    if let Some(menu) = theme::menu_icon() {
        crate::shelf::card::draw_centered(&menu, CGRect::new(CGPoint::new(18.0, h - 30.0), CGSize::new(20.0, 20.0)), 1.0);
    }
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(48.0, h - 34.0), CGSize::new(300.0, 20.0)),
        &l("稍后也能在设置中调整哦"),
        &theme::serif(12.0, false),
        &theme::ink_muted(),
        None,
    );
    let done = done_rect(b);
    {
        let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(done, 8.0, 8.0);
        theme::brown().setFill();
        capsule.fill();
    }
    let label = l("开始使用");
    let font = theme::serif(15.0, true);
    let tw = crate::shelf::card::measure(&label, &font, 200.0, None).width;
    crate::shelf::card::draw_line_centered(&label, &font, &theme::on_brown(), done.min().x + (done.size.width - tw) / 2.0, done.mid().y, tw + 1.0);
}

fn rule(x: f64, y: f64, w: f64) {
    theme::ink().colorWithAlphaComponent(0.7).setFill();
    NSBezierPath::bezierPathWithRect(CGRect::new(CGPoint::new(x, y), CGSize::new(w, 1.0))).fill();
}

/// Section label on a torn brown scrap (the welcome's own label shape).
fn heading(x: f64, y: f64, s: &str) {
    let font = theme::serif(14.0, true);
    let tw = crate::shelf::card::measure(s, &font, 400.0, None).width;
    let rect = CGRect::new(CGPoint::new(x, y), CGSize::new(tw + 24.0, crate::shelf::card::line_height(&font) + 8.0));
    theme::brown().setFill();
    theme::torn_paper_path(rect, true, true, true, true, 21, 1.6, 6.0).fill();
    crate::shelf::card::draw_line_centered(s, &font, &theme::on_brown(), rect.min().x + 12.0, rect.mid().y, tw + 1.0);
}

/// Icon · title · keycaps.
fn step_row(view: &WelcomeCardView, x: f64, y: f64, title: &str, which: &'static str) {
    let st = view.ivars().state.borrow();
    let (shortcut, taken, notice) = match which {
        "capture" => (st.capture, st.capture_taken(), st.notice_capture.clone()),
        _ => (st.shelf_sh, st.shelf_taken(), st.notice_shelf.clone()),
    };
    let capturing = st.capturing == Some(which);
    drop(st);
    let title_font = theme::serif(14.0, false);
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(x, y), CGSize::new(200.0, 22.0)),
        title,
        &title_font,
        &theme::ink(),
        None,
    );
    if let Some(n) = &notice {
        crate::shelf::card::draw_text(
            CGRect::new(CGPoint::new(x, y + 20.0), CGSize::new(220.0, 16.0)),
            n,
            &theme::serif(11.0, false),
            &theme::ink().colorWithAlphaComponent(0.45),
            Some(1.0),
        );
    } else if taken {
        crate::shelf::card::draw_text(
            CGRect::new(CGPoint::new(x, y + 20.0), CGSize::new(220.0, 16.0)),
            &l("被其他应用占用，点击换一个"),
            &theme::serif(11.0, false),
            &theme::ink().colorWithAlphaComponent(0.45),
            Some(1.0),
        );
    }
    let caps = if capturing {
        vec![l("按下组合键")]
    } else if shortcut.is_set() {
        keycaps(shortcut)
    } else {
        vec![l("立即设置")]
    };
    let mut cx = x;
    for cap in caps {
        draw_keycap(&cap, &mut cx, y + 40.0);
    }
}

fn keycaps(s: crate::app::preferences::Shortcut) -> Vec<String> {
    if !s.is_set() {
        return Vec::new();
    }
    let mut caps: Vec<String> = Vec::new();
    if s.carbon_modifiers & crate::app::preferences::CONTROL_KEY != 0 { caps.push("⌃".into()); }
    if s.carbon_modifiers & crate::app::preferences::OPTION_KEY != 0 { caps.push("⌥".into()); }
    if s.carbon_modifiers & crate::app::preferences::SHIFT_KEY != 0 { caps.push("⇧".into()); }
    if s.carbon_modifiers & crate::app::preferences::CMD_KEY != 0 { caps.push("⌘".into()); }
    caps.push(crate::app::preferences::key_code_name(s.key_code).to_string());
    caps
}

fn draw_keycap(s: &str, x: &mut f64, y: f64) {
    let font = if s.chars().count() > 1 {
        objc2_app_kit::NSFont::systemFontOfSize(11.0)
    } else {
        objc2_app_kit::NSFont::systemFontOfSize(13.0)
    };
    let tw = crate::shelf::card::measure(s, &font, 200.0, None).width;
    let w = (tw + if s.chars().count() > 1 { 14.0 } else { 0.0 }).max(28.0);
    let rect = CGRect::new(CGPoint::new(*x, y), CGSize::new(w, 28.0));
    {
        objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
        crate::shelf::card::shadow(&objc2_app_kit::NSColor::blackColor().colorWithAlphaComponent(0.18), 1.5, 0.0, 2.0).set();
        theme::paper().setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 8.0, 8.0).fill();
        objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
    }
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 8.0, 8.0);
    theme::ink().colorWithAlphaComponent(0.18).setStroke();
    path.setLineWidth(1.0);
    path.stroke();
    crate::shelf::card::draw_line_centered(s, &font, &theme::ink(), rect.min().x + (rect.size.width - tw) / 2.0, rect.mid().y, tw + 1.0);
    *x += w + 10.0;
}

/// Checklist line: empty box / filled with a strike when done.
fn todo_row(x: f64, y: f64, done: bool, text_s: &str) {
    let icon = if done { "checkmark.square.fill" } else { "square" };
    let color = if done { theme::paper_blue_deep().colorWithAlphaComponent(0.6) } else { theme::ink().colorWithAlphaComponent(0.5) };
    if let Some(img) = crate::shelf::card::symbol(icon, 14.0, crate::shelf::card::SymbolWeight::Light, &color) {
        crate::shelf::card::draw_centered(&img, CGRect::new(CGPoint::new(x, y + 2.0), CGSize::new(20.0, 20.0)), 1.0);
    }
    let font = theme::serif(14.0, false);
    let color_t = if done { theme::ink().colorWithAlphaComponent(0.35) } else { theme::ink() };
    let tw = crate::shelf::card::measure(text_s, &font, 400.0, None).width;
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(x + 28.0, y + 2.0), CGSize::new(tw + 1.0, 22.0)),
        text_s,
        &font,
        &color_t,
        Some(1.0),
    );
    if done {
        theme::ink().colorWithAlphaComponent(0.35).setFill();
        NSBezierPath::bezierPathWithRect(CGRect::new(
            CGPoint::new(x + 28.0, y + 2.0 + crate::shelf::card::line_height(&font) / 2.0 - 1.0),
            CGSize::new(tw, 1.0),
        )).fill();
    }
}

/// Dashed tear-off line with the two edge notches (welcome's ticketRule).
fn ticket_rule(y: f64, w: f64) {
    theme::ink().colorWithAlphaComponent(0.35).setStroke();
    let dash: [f64; 2] = [4.0, 4.0];
    let line = NSBezierPath::new();
    line.moveToPoint(CGPoint::new(18.0, y));
    line.lineToPoint(CGPoint::new(w - 18.0, y));
    unsafe {
        line.setLineDash_count_phase(dash.as_ptr(), 2, 0.0);
    }
    line.stroke();
    theme::brown().setFill();
    NSBezierPath::bezierPathWithOvalInRect(CGRect::new(CGPoint::new(18.0 - 8.0, y - 8.0), CGSize::new(16.0, 16.0))).fill();
    NSBezierPath::bezierPathWithOvalInRect(CGRect::new(
        CGPoint::new(w - 18.0 - 8.0, y - 8.0),
        CGSize::new(16.0, 16.0),
    )).fill();
}

// MARK: Recorder

fn start_capture(view: &WelcomeCardView, which: &'static str) {
    if view.capturing().is_some() {
        return;
    }
    view.set_capturing(Some(which));
    crate::app::hotkey::HotKeyCenter::shared().suspend();
}

/// Called with a captured key event through the panel's handle() (same
/// settings recorder contract).
pub fn recorder_key(view: &WelcomeCardView, event: &NSEvent) -> bool {
    let Some(which) = view.capturing() else { return false };
    match event.keyCode() {
        53 => stop_recorder(view, which, None),
        51 => stop_recorder(view, which, Some(crate::app::preferences::Shortcut::NONE)),
        _ => {
            if let Some(s) = crate::shelf::settings_pane::shortcut_from_event_pub(event) {
                stop_recorder(view, which, Some(s));
            } else {
                stop_recorder(view, which, None);
                view.set_notice(which, Some(l("至少两个键：⌘ ⌥ ⌃ ⇧ 中的一个加一个键")));
            }
        }
    }
    true
}

fn stop_recorder(view: &WelcomeCardView, which: &'static str, new_value: Option<crate::app::preferences::Shortcut>) {
    view.set_capturing(None);
    crate::app::hotkey::HotKeyCenter::shared().resume();
    let Some(new_value) = new_value else { return };
    if new_value.is_set() {
        let p = crate::app::preferences::Preferences::shared();
        let mine: [(&str, &str, &str); 3] = [
            ("capture", crate::app::preferences::key::HOTKEY_CAPTURE, "截图"),
            ("shelf", crate::app::preferences::key::HOTKEY_SHELF, "剪贴板"),
            ("search", crate::app::preferences::key::HOTKEY_SEARCH, "搜索剪贴板"),
        ];
        for (owner, key, label) in mine {
            if owner != which && p.shortcut(key) == new_value {
                view.set_notice(which, Some(l("已被 Pastory 的「%@」占用，换一个").replacen("%@", &l(label), 1)));
                return;
            }
        }
        if !crate::app::hotkey::HotKeyCenter::shared().is_available(new_value) {
            view.set_notice(which, Some(l("已被其他应用占用，换一个")));
            return;
        }
    }
    let key = match which {
        "capture" => crate::app::preferences::key::HOTKEY_CAPTURE,
        _ => crate::app::preferences::key::HOTKEY_SHELF,
    };
    crate::app::preferences::Preferences::shared().set_shortcut(key, new_value);
    crate::shelf::settings_pane::post_shortcuts_changed();
    let m = new_value.carbon_modifiers;
    if new_value.is_set() && (m == crate::app::preferences::CMD_KEY || m == crate::app::preferences::SHIFT_KEY || m == (crate::app::preferences::CMD_KEY | crate::app::preferences::SHIFT_KEY)) {
        view.set_notice(which, Some(l("已设置。注意：所有应用里的 %@ 都会变成这个功能").replacen("%@", &new_value.display(), 1)));
    } else {
        view.set_notice(which, None);
    }
    view.refresh_state();
}

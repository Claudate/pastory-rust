//! Port of `Shelf/SettingsPane.swift` — the shelf's right half swapped for
//! the paper settings pages (two columns of sheets over the desk), plus
//! `SettingsWindow.swift`'s shortcut recorder (the three rows reuse the
//! same hit target / capture logic).
//!
//! One `SettingsView` subclass draws the whole pane in `drawRect` and keeps
//! click targets in ivars; the two interactive kinds (shortcut recorder,
//! pill buttons) drive real model actions through `run_choice`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBezierPath, NSColor, NSEvent, NSView};
use objc2_app_kit::NSGraphicsContext;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

use crate::app::hotkey::HotKeyCenter;
use crate::app::localization::l;
use crate::app::{permissions, preferences, theme};
use crate::app::preferences::Shortcut;
use crate::app::preferences_m6;
use crate::clipboard::item;
use crate::clipboard::retention;
use crate::clipboard::store;

// MARK: Geometry

const SHEET_PAD: f64 = 16.0;
/// Fixed header band (title + back/close); the two sheet columns scroll under it.
const HEADER_H: f64 = 62.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    Back,
    Close,
    CheckUpdates,
    CheckForUpdates,
    Recorder(&'static str),
    Retention(i64),
    CleanupHour(i64),
    ClearNow,
    LaunchAtLogin,
    Language(&'static str),
    ScreenRec,
    Pause,
    Password,
    Paste(&'static str),
    GrantAX,
    ImageStorage(&'static str),
    Codec(bool),
    ChooseExport,
    OpenStore,
    ImportDb,
    RemoveImported,
}

#[derive(Clone, Debug)]
pub enum Hot {
    Row(CGRect, Choice),
    Clear(CGRect, &'static str),
}

/// Live UI state (read from Preferences each refresh).
impl Default for State {
    fn default() -> State {
        State {
            capture: Shortcut::NONE,
            shelf: Shortcut::NONE,
            search: Shortcut::NONE,
            capture_taken: false,
            shelf_taken: false,
            search_taken: false,
            capture_notice: None,
            shelf_notice: None,
            search_notice: None,
            capturing: None,
            update_note: None,
            checking: false,
            import_note: None,
            cleared: false,
            has_sr: false,
            has_ax: false,
            login_on: false,
        }
    }
}

#[derive(Clone)]
pub struct State {
    capture: preferences::Shortcut,
    shelf: preferences::Shortcut,
    search: preferences::Shortcut,
    capture_taken: bool,
    shelf_taken: bool,
    search_taken: bool,
    capture_notice: Option<String>,
    shelf_notice: Option<String>,
    search_notice: Option<String>,
    capturing: Option<&'static str>,
    update_note: Option<String>,
    checking: bool,
    import_note: Option<String>,
    cleared: bool,
    has_sr: bool,
    has_ax: bool,
    login_on: bool,
}

pub struct SettingsViewIvars {
    state: RefCell<State>,
    hotspots: RefCell<Vec<Hot>>,
    scan_open: std::cell::Cell<bool>,
    /// Content scroll offset under the fixed header (Swift's ScrollView).
    scroll: std::cell::Cell<f64>,
    /// Total content height below the header, measured at the end of draw.
    content_h: std::cell::Cell<f64>,
}

define_class!(
    // SAFETY: plain NSView drawing the pane; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SettingsViewIvars]
    pub struct SettingsView;

    unsafe impl NSObjectProtocol for SettingsView {}

    impl SettingsView {
        #[unsafe(method(isFlipped))]
        fn sv_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn sv_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn sv_draw_rect(&self, _dirty: CGRect) {
            draw(self);
        }

        #[unsafe(method(mouseUp:))]
        fn sv_mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut pending: Option<Choice> = None;
            for hot in self.ivars().hotspots.borrow().iter() {
                match hot {
                    Hot::Row(rect, choice) if contains(rect, p) => {
                        pending = Some(*choice);
                        break;
                    }
                    Hot::Clear(rect, which) if contains(rect, p) => {
                        stop_recorder(self, which, Some(Shortcut::NONE));
                        return;
                    }
                    _ => {}
                }
            }
            if let Some(choice) = pending {
                run_choice(choice, self);
                return;
            }
            // Click anywhere else: cancel a live recorder.
            if let Some(which) = self.ivars().state.borrow().capturing {
                stop_recorder(self, which, None);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn sv_scroll_wheel(&self, event: &NSEvent) {
            // Hour wheel: scroll over the hour chip steps the hour
            // (Swift's ScrollSteps; the hot rect for the chip is in hotspots).
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            for hot in self.ivars().hotspots.borrow().iter() {
                if let Hot::Row(rect, Choice::CleanupHour(99)) = hot {
                    if contains(rect, p) && event.momentumPhase().is_empty() {
                        let steps = if event.hasPreciseScrollingDeltas() {
                            (event.scrollingDeltaY() / 18.0) as i64
                        } else {
                            event.scrollingDeltaY() as i64
                        };
                        if steps != 0 {
                            let p_pref = preferences::Preferences::shared();
                            let next = (p_pref.cleanup_hour() + steps).rem_euclid(24);
                            p_pref.set_cleanup_hour(next);
                            retention::schedule();
                            self.refresh_state();
                            return;
                        }
                    }
                }
            }
            // Page scroll (Swift's ScrollView). Flipped coords: a positive
            // deltaY moves content down, i.e. towards the top.
            let dy = event.scrollingDeltaY();
            if dy != 0.0 {
                let visible = (self.bounds().size.height - HEADER_H).max(1.0);
                let max_scroll = (self.ivars().content_h.get() - visible).max(0.0);
                let cur = self.ivars().scroll.get();
                let next = (cur - dy).clamp(0.0, max_scroll);
                if next != cur {
                    self.ivars().scroll.set(next);
                    self.setNeedsDisplay(true);
                }
            }
        }
    }
);

impl SettingsView {
    pub fn make(frame: CGRect) -> Retained<SettingsView> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let this = mtm.alloc::<SettingsView>().set_ivars(SettingsViewIvars {
            state: RefCell::new(State::default()),
            hotspots: RefCell::new(Vec::new()),
            scan_open: std::cell::Cell::new(false),
            scroll: std::cell::Cell::new(0.0),
            content_h: std::cell::Cell::new(0.0),
        });
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.refresh_state();
        if !crate::app::sandbox::launch().is_self_test() {
            start_badge_poll();
        }
        post_shortcut_binding_changed();
        view
    }

    pub fn refresh_state(&self) {
        let p = preferences::Preferences::shared();
        let failed = HotKeyCenter::shared().failed();
        let mut st = self.ivars().state.borrow_mut();
        st.capture = p.shortcut(preferences::key::HOTKEY_CAPTURE);
        st.shelf = p.shortcut(preferences::key::HOTKEY_SHELF);
        st.search = p.shortcut(preferences::key::HOTKEY_SEARCH);
        st.capture_taken = failed.iter().any(|n| n == "capture");
        st.shelf_taken = failed.iter().any(|n| n == "shelf");
        st.search_taken = failed.iter().any(|n| n == "search");
        st.has_sr = permissions::has_screen_recording();
        st.has_ax = permissions::has_accessibility()
            && !crate::app::sandbox::launch().env.iter().any(|(k, _)| k == "PASTORY_NOAX");
        st.login_on = preferences_m6::launch_at_login();
        drop(st);
        self.setNeedsDisplay(true);
    }

    pub fn set_update_note(&self, note: Option<String>) {
        self.ivars().state.borrow_mut().update_note = note;
        self.setNeedsDisplay(true);
    }

    pub fn set_import_note(&self, note: Option<String>) {
        self.ivars().state.borrow_mut().import_note = note;
        self.setNeedsDisplay(true);
    }

    pub fn set_cleared(&self) {
        self.ivars().state.borrow_mut().cleared = true;
        self.setNeedsDisplay(true);
    }

        /// Raw pointer for identity checks across dispatch hops (same
    /// character as update_progress::Window::raw_pub — main thread only).
    pub fn raw_pub(&self) -> usize {
        self as *const SettingsView as usize
    }

    pub fn set_capturing(&self, which: Option<&'static str>) {
        self.ivars().state.borrow_mut().capturing = which;
        self.setNeedsDisplay(true);
    }

    pub fn set_notice(&self, which: &'static str, note: Option<String>) {
        let mut st = self.ivars().state.borrow_mut();
        match which {
            "capture" => st.capture_notice = note,
            "search" => st.search_notice = note,
            _ => st.shelf_notice = note,
        }
        drop(st);
        self.setNeedsDisplay(true);
    }
}

/// Type of the live settings view (the shelf keeps one while the pane is up).
fn live_settings_view() -> Option<Retained<SettingsView>> {
    crate::shelf::view::settings_view()
}

fn contains(r: &CGRect, p: CGPoint) -> bool {
    p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y
}

// MARK: 1 s badge poll

fn start_badge_poll() {
    fn tick() {
        crate::app::delegate::dispatch_main_after(1.0, Box::new(|| {
            if let Some(v) = live_settings_view() {
                if crate::shelf::panel::with_model(|m| m.show_settings) {
                    v.refresh_state();
                    tick();
                }
            }
        }));
    }
    tick();
}

// MARK: Drawing

fn draw(view: &SettingsView) {
    let b = view.bounds();
    // The pane's frame already is the content column (view::content_rect), so
    // Swift's `.padding(.horizontal, 24)` lands at x=4 within the view
    // (view x = 162+20; Swift content starts at 162+24). Measured against the
    // Swift benchmark rendered at the real panel size (1920×482).
    let cx = 4.0;
    let cw = (b.size.width - 8.0).max(0.0);
    let state = view.ivars().state.borrow().clone();
    let mut hotspots: Vec<Hot> = Vec::new();
    // Header: title + 返回剪贴板 + 关闭.
    let title_font = theme::serif(22.0, true);
    text(view, CGPoint::new(cx, 18.0), &l("设置"), &title_font, &theme::on_brown(), None);
    let back_label = format!("‹ {}", l("返回剪贴板"));
    hit_text(view, &mut hotspots, &back_label,
        CGRect::new(CGPoint::new(cx + cw - 200.0, 18.0), CGSize::new(160.0, 36.0)),
        &theme::serif(13.0, true), &theme::on_brown(), Choice::Back, true);
    hit_text(view, &mut hotspots, "✕",
        CGRect::new(CGPoint::new(cx + cw - 40.0, 18.0), CGSize::new(40.0, 36.0)),
        &theme::serif(16.0, false), &theme::on_brown(), Choice::Close, true);
    // Sheet columns scroll under the fixed header (Swift's ScrollView). The
    // scroll offset just shifts the y accumulators; every derived rect
    // (rows, buttons, hotspots) follows, and the band clips to the header line.
    let scroll = view.ivars().scroll.get().clamp(
        0.0,
        (view.ivars().content_h.get() - (b.size.height - HEADER_H)).max(0.0),
    );
    NSGraphicsContext::saveGraphicsState_class();
    NSBezierPath::bezierPathWithRect(CGRect::new(
        CGPoint::new(0.0, HEADER_H),
        CGSize::new(b.size.width, (b.size.height - HEADER_H).max(0.0)),
    ))
    .addClip();
    let mut y = HEADER_H - scroll;

    let col_w = (cw - 16.0) / 2.0;
    let col1 = cx;
    let col2 = cx + col_w + 16.0;

    // Column 1: 版本更新 / 快捷键 / 清理 / 系统.
    sheet_header(view, &mut y, col1, col_w, &l("版本更新"));
    sheet(view, &mut y, col1, col_w, 2, |v, y_pos| {
        let row = sheet_row(y_pos, col1, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("当前版本 %@").replacen("%@", &crate::app::updater::current_version(), 1), &theme::serif(15.0, false), &theme::ink(), None);
        if let Some(n) = &state.update_note {
            let font = theme::serif(12.0, false);
            let w = crate::shelf::card::measure(n, &font, 220.0, None).width.min(200.0);
            text(v, CGPoint::new(row.0.x + col_w - 32.0 - 160.0 - w, row.0.y + 12.0), n, &font, &theme::ink_muted(), Some(1.0));
        }
        let check_label = if state.checking { l("检查中…") } else { l("手动检查更新") };
        hit_button(view, &mut hotspots, &check_label,
            CGPoint::new(row.0.x + col_w - 32.0 - 160.0, row.0.y + 6.0), Choice::CheckUpdates, 160.0, false);
        let row2 = sheet_row(y_pos, col1, col_w, 1);
        text(v, CGPoint::new(row2.0.x + SHEET_PAD, row2.0.y + (row2.1 - line_height_f()) / 2.0), &l("每天自动检查一次（app 唯一的联网请求，不带任何标识）"), &theme::serif(13.0, false), &theme::ink_muted(), Some(1.0));
        toggle(view, &mut hotspots, CGPoint::new(row2.0.x + col_w - 32.0 - 44.0, row2.0.y + 6.0), Choice::CheckForUpdates);
    });
    sheet_header(view, &mut y, col1, col_w, &l("快捷键"));
    sheet(view, &mut y, col1, col_w, 3, |v, y_pos| {
        for (i, (label, which, taken)) in [
            (&l("截图"), "capture", state.capture_taken),
            (&l("显示 / 隐藏剪贴板"), "shelf", state.shelf_taken),
            (&l("搜索剪贴板"), "search", state.search_taken),
        ].iter().enumerate() {
            let row = sheet_row(y_pos, col1, col_w, i);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), label, &theme::serif(15.0, false), &theme::ink(), None);
            recorder_row(v, &mut hotspots, which, &state, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 6.0));
            let _ = taken;
        }
    });
    sheet_header(view, &mut y, col1, col_w, &l("清理"));
    {
        let prefs = preferences::Preferences::shared();
        sheet(view, &mut y, col1, col_w, 3, |v, y_pos| {
            let row = sheet_row(y_pos, col1, col_w, 0);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("未 Pin 内容保留时间"), &theme::serif(15.0, false), &theme::ink(), None);
            retention_segments(v, &mut hotspots, &prefs, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 5.0));
            let row = sheet_row(y_pos, col1, col_w, 1);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("当日清理时间"), &theme::serif(15.0, false), &theme::ink(), None);
            hour_wheel(v, &mut hotspots, &prefs, CGPoint::new(row.0.x + col_w - 32.0 - 96.0, row.0.y + 5.0));
            let row = sheet_row(y_pos, col1, col_w, 2);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("手动清空一次（不含已 Pin 内容）"), &theme::serif(15.0, false), &theme::ink(), None);
            let clear_label = if state.cleared { l("已清空") } else { l("现在清空") };
            hit_button(view, &mut hotspots, &clear_label,
                CGPoint::new(row.0.x + col_w - 32.0 - 120.0, row.0.y + 6.0), Choice::ClearNow, 120.0, state.cleared);
        });
    }
    sheet_header(view, &mut y, col1, col_w, &l("系统"));
    sheet(view, &mut y, col1, col_w, 3, |v, y_pos| {
        let row = sheet_row(y_pos, col1, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("登录时启动"), &theme::serif(15.0, false), &theme::ink(), None);
        switch_control(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0 - 44.0, row.0.y + 6.0), Choice::LaunchAtLogin, state.login_on, view);
        let row = sheet_row(y_pos, col1, col_w, 1);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("语言"), &theme::serif(15.0, false), &theme::ink(), None);
        let prefs = preferences::Preferences::shared();
        let lang = prefs.language().to_string();
        let opts: [(&str, &str, Choice); 3] = [
            ("system", &l("跟随系统"), Choice::Language("system")),
            ("zh", &l("中文"), Choice::Language("zh")),
            ("en", "English", Choice::Language("en")),
        ];
        segment_choice(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 5.0), &opts, &lang);
        let row = sheet_row(y_pos, col1, col_w, 2);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("屏幕录制权限（截图、录屏需要）"), &theme::serif(15.0, false), &theme::ink(), None);
        let sr_label = if state.has_sr { l("已授权") } else { l("未授权") };
        tag(v, CGPoint::new(row.0.x + col_w - 32.0 - (if state.has_sr { 74.0 } else { 154.0 }), row.0.y + 9.0), &sr_label, state.has_sr);
        if !state.has_sr {
            hit_button(view, &mut hotspots, &l("去授权"), CGPoint::new(row.0.x + col_w - 32.0 - 74.0, row.0.y + 6.0), Choice::ScreenRec, 74.0, false);
        }
    });

    // Column 2: 剪贴板 / 截图与录屏 / 位置 / 导入.
    let prefs = preferences::Preferences::shared();
    let mut y2 = HEADER_H - scroll;
    sheet_header(view, &mut y2, col2, col_w, &l("剪贴板"));
    sheet(view, &mut y2, col2, col_w, 3, |v, y_pos| {
        let row = sheet_row(y_pos, col2, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("暂停记录剪贴板"), &theme::serif(15.0, false), &theme::ink(), None);
        switch_control(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0 - 44.0, row.0.y + 6.0), Choice::Pause, prefs.monitoring_paused(), view);
        let row = sheet_row(y_pos, col2, col_w, 1);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("记录密码管理器复制的内容"), &theme::serif(15.0, false), &theme::ink(), None);
        let hint = if prefs.record_password_managers() { &l("从 1Password 等「密码管理软件」复制的内容也会进剪贴板。") } else { &l("从 1Password 等「密码管理软件」复制的内容均不进剪贴板。") };
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), hint, &theme::serif(12.0, false), &theme::ink_muted(), Some(1.0));
        switch_control(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0 - 44.0, row.0.y + 12.0), Choice::Password, prefs.record_password_managers(), view);
        let row = sheet_row(y_pos, col2, col_w, 2);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("直接粘贴到刚才的应用"), &theme::serif(15.0, false), &theme::ink(), None);
        let hint = match prefs.paste_mode().as_str() {
            "off" => l("双击和回车都只复制并收起"),
            "double" => l("双击卡片，内容直接贴进刚才的应用"),
            _ => l("用方向键选中卡片后，回车也会直接粘贴"),
        };
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), &hint, &theme::serif(12.0, false), &theme::ink_muted(), Some(1.0));
        if prefs.paste_mode() != "off" && !state.has_ax {
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 44.0), &l("需要辅助功能权限"), &theme::serif(12.0, false), &theme::ink(), Some(1.0));
            hit_button(view, &mut hotspots, &l("去授权"), CGPoint::new(row.0.x + SHEET_PAD + 160.0, row.0.y + 40.0), Choice::GrantAX, 80.0, false);
        }
        let opts: [(&str, &str, Choice); 3] = [
            ("off", &crate::app::localization::l_ctx("关闭", "paste"), Choice::Paste("off")),
            ("double", &l("双击"), Choice::Paste("double")),
            ("return", &l("双击 + 回车"), Choice::Paste("return")),
        ];
        segment_choice(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 10.0), &opts, &prefs.paste_mode());
    });
    sheet_header(view, &mut y2, col2, col_w, &l("截图与录屏"));
    sheet(view, &mut y2, col2, col_w, 2, |v, y_pos| {
        let row = sheet_row(y_pos, col2, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("本地数据库截图存储方式"), &theme::serif(15.0, false), &theme::ink(), None);
        let hint = if prefs.stores_heic() { &l("高质量有损压缩，体积约为 PNG 的三分之一") } else { &l("无损，体积最大") };
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), hint, &theme::serif(12.0, false), &theme::ink_muted(), Some(1.0));
        let opts: [(&str, &str, Choice); 2] = [("heic", "HEIC", Choice::ImageStorage("heic")), ("png", "PNG", Choice::ImageStorage("png"))];
        segment_choice(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 10.0), &opts, &prefs.image_storage());
        let row = sheet_row(y_pos, col2, col_w, 1);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("录屏编码"), &theme::serif(15.0, false), &theme::ink(), None);
        let hint = if prefs.record_hevc() { &l("体积小一半；老设备和部分 Windows 打不开") } else { &l("所有设备都能播放") };
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), hint, &theme::serif(12.0, false), &theme::ink_muted(), Some(1.0));
        let opts: [(&str, &str, Choice); 2] = [("h264", "H.264", Choice::Codec(false)), ("hevc", "HEVC", Choice::Codec(true))];
        segment_choice(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 10.0), &opts, if prefs.record_hevc() { "hevc" } else { "h264" });
    });
    sheet_header(view, &mut y2, col2, col_w, &l("位置"));
    sheet(view, &mut y2, col2, col_w, 2, |v, y_pos| {
        let row = sheet_row(y_pos, col2, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("「保存到本地」默认打开的文件夹"), &theme::serif(15.0, false), &theme::ink(), None);
        let dir = prefs.custom_export_dir().unwrap_or_else(|| prefs.export_directory().to_string_lossy().into_owned());
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), &abbreviate_home(&dir), &theme::serif(12.0, false), &theme::ink_muted(), Some(1.0));
        hit_button(view, &mut hotspots, &l("选择…"), CGPoint::new(row.0.x + col_w - 32.0 - 90.0, row.0.y + 8.0), Choice::ChooseExport, 90.0, false);
        let row = sheet_row(y_pos, col2, col_w, 1);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("剪贴板内容临时存放位置"), &theme::serif(15.0, false), &theme::ink(), None);
        let (path, bytes, count) = store::read(|s| {
            (s.root.to_string_lossy().into_owned(), dir_size(&s.root), s.items.len())
        });
        let meta = format!("{} · {} · · {}", l("SQLite 数据库"), abbreviate_home(&path), desc_size(bytes, count));
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), &meta, &theme::serif(12.0, false), &theme::ink_muted(), Some(1.0));
        hit_button(view, &mut hotspots, &l("查看"), CGPoint::new(row.0.x + col_w - 32.0 - 70.0, row.0.y + 8.0), Choice::OpenStore, 70.0, false);
    });
    let has_imported = store::read(|s| s.items.iter().any(|it| it.source_app_name.as_deref() == Some(store::IMPORT_SOURCE_NAME)));
    sheet_header(view, &mut y2, col2, col_w, &l("导入"));
    sheet(view, &mut y2, col2, col_w, if has_imported || state.import_note.is_some() { 2 } else { 1 }, |v, y_pos| {
        let row = sheet_row(y_pos, col2, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("从其他剪贴板工具导入（SQLite）"), &theme::serif(15.0, false), &theme::ink(), None);
        if let Some(n) = &state.import_note {
            let font = theme::serif(12.0, false);
            let w = crate::shelf::card::measure(n, &font, 220.0, None).width.min(200.0);
            text(v, CGPoint::new(row.0.x + col_w - 32.0 - 120.0 - w - 8.0, row.0.y + 12.0), n, &font, &theme::ink_muted(), Some(1.0));
        }
        hit_button(view, &mut hotspots, &l("选择数据库…"), CGPoint::new(row.0.x + col_w - 32.0 - 120.0, row.0.y + 6.0), Choice::ImportDb, 120.0, false);
        if has_imported {
            let row = sheet_row(y_pos, col2, col_w, 1);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("移除所有导入进来的条目（来源为「导入」）"), &theme::serif(15.0, false), &theme::ink(), None);
            hit_button(view, &mut hotspots, &l("移除"), CGPoint::new(row.0.x + col_w - 32.0 - 80.0, row.0.y + 6.0), Choice::RemoveImported, 80.0, false);
        }
    });
    // Content height = the lower column's bottom, back in content coords.
    view.ivars().content_h.set(y.max(y2) + scroll - HEADER_H);
    NSGraphicsContext::restoreGraphicsState_class();
    // Hotspots fully scrolled above the clip band must not answer clicks;
    // the header's Back/Close live above the band by design.
    hotspots.retain(|h| match h {
        Hot::Row(r, c) => matches!(c, Choice::Back | Choice::Close) || r.max().y > HEADER_H,
        Hot::Clear(r, _) => r.max().y > HEADER_H,
    });
    *view.ivars().hotspots.borrow_mut() = hotspots;
}

// MARK: Row scaffolding

fn sheet_row(sheet_y: f64, col_x: f64, _col_w: f64, index: usize) -> (CGPoint, f64) {
    let height = 44.0;
    (CGPoint::new(col_x + 16.0, sheet_y + 28.0 + index as f64 * height), height)
}

fn line_height_f() -> f64 {
    22.0
}

/// Section header (`section` in Swift): serif 13 bold muted, padded 16.
fn sheet_header(view: &SettingsView, y: &mut f64, col_x: f64, _col_w: f64, title: &str) {
    *y += 12.0;
    let font = theme::serif(13.0, true);
    text(view, CGPoint::new(col_x + SHEET_PAD, *y), title, &font, &theme::ink_muted(), None);
    *y += 22.0;
}

/// Paper sheet with rows painted as label/control pairs.
fn sheet(
    view: &SettingsView,
    y: &mut f64,
    col_x: f64,
    col_w: f64,
    rows: usize,
    draw_rows: impl FnOnce(&SettingsView, f64),
) {
    let sheet_y = *y + 8.0;
    let h = 28.0 + rows as f64 * 44.0;
    let rect = CGRect::new(CGPoint::new(col_x, sheet_y), CGSize::new(col_w, h));
    {
        NSGraphicsContext::saveGraphicsState_class();
        let s = crate::shelf::card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.35), 8.0, 1.0, 4.0);
        s.set();
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 6.0, 6.0);
        theme::draw_paper(&path, &theme::paper());
        NSGraphicsContext::restoreGraphicsState_class();
    }
    *y = sheet_y;
    draw_rows(view, sheet_y);
    *y += h + 16.0;
}

fn text(
    _view: &SettingsView,
    origin: CGPoint,
    s: &str,
    font: &objc2_app_kit::NSFont,
    color: &objc2_app_kit::NSColor,
    spacing: Option<f64>,
) {
    let _ = _view;
    let h = crate::shelf::card::line_height(font);
    crate::shelf::card::draw_text(
        CGRect::new(origin, CGSize::new(500.0, h)),
        s,
        font,
        color,
        spacing.map(|_| h - 6.0),
    );
}

/// Drawn label with a click hot rect (header buttons).
fn hit_text(
    _view: &SettingsView,
    hotspots: &mut Vec<Hot>,
    s: &str,
    rect: CGRect,
    font: &objc2_app_kit::NSFont,
    color: &objc2_app_kit::NSColor,
    choice: Choice,
    opaque: bool,
) {
    if opaque {
        let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, rect.size.height / 2.0, rect.size.height / 2.0);
        theme::on_brown().colorWithAlphaComponent(0.4).setStroke();
        capsule.setLineWidth(1.0);
        capsule.stroke();
    }
    let h = crate::shelf::card::line_height(font);
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(rect.min().x + 12.0, rect.min().y + (rect.size.height - h) / 2.0), CGSize::new(rect.size.width - 12.0, h)),
        s,
        font,
        color,
        None,
    );
    hotspots.push(Hot::Row(rect, choice));
}

/// Paper pill button (paperButtonExport shapes; a drawn capsule answers clicks).
fn hit_button(
    _view: &SettingsView,
    hotspots: &mut Vec<Hot>,
    title: &str,
    origin: CGPoint,
    choice: Choice,
    width: f64,
    disabled: bool,
) {
    let font = objc2_app_kit::NSFont::systemFontOfSize(13.0);
    let tw = crate::shelf::card::measure(title, &font, 400.0, None).width;
    let w = width.max(tw + 24.0);
    let rect = CGRect::new(origin, CGSize::new(w, 24.0));
    let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 12.0, 12.0);
    theme::ink().colorWithAlphaComponent(if disabled { 0.25 } else { 0.6 }).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    let color = theme::ink().colorWithAlphaComponent(if disabled { 0.4 } else { 1.0 });
    crate::shelf::card::draw_line_centered(title, &font, &color, rect.min().x + 12.0, rect.mid().y, tw + 1.0);
    if !disabled {
        hotspots.push(Hot::Row(rect, choice));
    }
}

/// Perf badge: 已授权/未授权 and never a warning colour (Swift's `tag`).
fn tag(_view: &SettingsView, origin: CGPoint, text_s: &str, on: bool) {
    let font = theme::serif(12.0, false);
    let tw = crate::shelf::card::measure(text_s, &font, 200.0, None).width;
    let h = crate::shelf::card::line_height(&font) + 6.0;
    let rect = CGRect::new(origin, CGSize::new(tw + 16.0, h));
    let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, h / 2.0, h / 2.0);
    if on {
        theme::paper_blue().setFill();
        capsule.fill();
    }
    theme::ink().colorWithAlphaComponent(if on { 0.25 } else { 0.5 }).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    crate::shelf::card::draw_line_centered(text_s, &font, &theme::ink(), rect.min().x + 8.0, rect.mid().y, tw + 1.0);
}

/// PaperToggle look (44×24) anchored to the right edge.
fn toggle(_view: &SettingsView, hotspots: &mut Vec<Hot>, origin: CGPoint, choice: Choice) {
    hotspots.push(Hot::Row(CGRect::new(origin, CGSize::new(44.0, 24.0)), choice));
}

/// The actual switch rendering (independent so state never lags).
fn switch_control(view: &SettingsView, hotspots: &mut Vec<Hot>, origin: CGPoint, choice: Choice, on: bool, _state_view: &SettingsView) {
    let rect = CGRect::new(origin, CGSize::new(44.0, 24.0));
    let r = 12.0;
    let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, r, r);
    (if on { theme::paper_blue() } else { theme::paper_dim() }).setFill();
    capsule.fill();
    theme::ink().colorWithAlphaComponent(if on { 0.35 } else { 0.5 }).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    let knob_d = 20.0;
    let knob_x = if on { rect.max().x - 2.0 - knob_d } else { rect.min().x + 2.0 };
    let knob = NSBezierPath::bezierPathWithOvalInRect(CGRect::new(CGPoint::new(knob_x, rect.min().y + 2.0), CGSize::new(knob_d, knob_d)));
    {
        NSGraphicsContext::saveGraphicsState_class();
        let shadow = crate::shelf::card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.25), 1.5, 0.0, 1.0);
        shadow.set();
        theme::paper().setFill();
        knob.fill();
        NSGraphicsContext::restoreGraphicsState_class();
    }
    theme::ink().colorWithAlphaComponent(0.45).setStroke();
    knob.setLineWidth(1.0);
    knob.stroke();
    let _ = view;
    hotspots.push(Hot::Row(rect, choice));
}

/// Short segmented group with the whole-group outline (choices; the
/// selected item is paperBlue with semibold type).
fn segment_choice(
    _view: &SettingsView,
    hotspots: &mut Vec<Hot>,
    origin_right: CGPoint,
    opts: &[(&str, &str, Choice)],
    selected: &str,
) {
    let font13 = theme::serif(12.5, false);
    let font13b = theme::serif(12.5, true);
    let mut widths: Vec<f64> = Vec::new();
    let mut total = 0.0;
    for (_, title, _) in opts {
        let tw = crate::shelf::card::measure(title, &font13, 400.0, None).width;
        let w = tw + 22.0;
        widths.push(w);
        total += w;
    }
    total += (opts.len() as f64 - 1.0) * 4.0 + 12.0;
    let mut x = origin_right.x - total;
    for ((_, title, choice), w) in opts.iter().zip(widths.iter()) {
        let rect = CGRect::new(CGPoint::new(x, origin_right.y), CGSize::new(*w, 26.0));
        let is_sel = matches!(choice,
            Choice::Language(code) if *code == selected)
            || matches!(choice, Choice::Paste(code) if *code == selected)
            || matches!(choice, Choice::ImageStorage(code) if *code == selected)
            || matches!(choice, Choice::Codec(hevc) if (*hevc) == (selected == "hevc"));
        if is_sel {
            let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                CGRect::new(CGPoint::new(rect.min().x + 1.0, rect.min().y + 3.0), CGSize::new(rect.size.width - 2.0, 20.0)),
                10.0,
                10.0,
            );
            theme::paper_blue().setFill();
            capsule.fill();
        }
        let font = if is_sel { &font13b } else { &font13 };
        let tw = crate::shelf::card::measure(title, font, 400.0, None).width;
        crate::shelf::card::draw_line_centered(title, font, &theme::ink(), rect.min().x + (rect.size.width - tw) / 2.0, rect.mid().y, tw + 1.0);
        hotspots.push(Hot::Row(rect, *choice));
        x += w + 4.0;
    }
    let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        CGRect::new(CGPoint::new(origin_right.x - total, origin_right.y - 2.0), CGSize::new(total, 30.0)),
        15.0,
        15.0,
    );
    theme::ink().colorWithAlphaComponent(0.5).setStroke();
    outline.setLineWidth(1.0);
    outline.stroke();
}

/// The retention-len six-button strip; the current setting is the blue one.
fn retention_segments(_view: &SettingsView, hotspots: &mut Vec<Hot>, prefs: &preferences::Preferences, origin_right: CGPoint) {
    let opts: [(&str, i64); 6] = [
        (&l("1 天"), 1), (&l("3 天"), 3), (&l("7 天"), 7), (&l("30 天"), 30), (&l("一年"), 365), (&l("永不删除"), 0),
    ];
    let sel = prefs.retention_days();
    let fonts = (theme::serif(12.5, false), theme::serif(12.5, true));
    let mut widths: Vec<f64> = Vec::new();
    let mut total = 0.0;
    for (title, _) in &opts {
        let tw = crate::shelf::card::measure(title, &fonts.0, 200.0, None).width;
        let w = tw + 20.0;
        widths.push(w);
        total += w;
    }
    total += (opts.len() as f64 - 1.0) * 4.0 + 12.0;
    let mut x = origin_right.x - total;
    for ((title, days), w) in opts.iter().zip(widths.iter()) {
        let rect = CGRect::new(CGPoint::new(x, origin_right.y), CGSize::new(*w, 26.0));
        if sel == *days {
            let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                CGRect::new(CGPoint::new(rect.min().x + 1.0, rect.min().y + 3.0), CGSize::new(rect.size.width - 2.0, 20.0)),
                10.0,
                10.0,
            );
            theme::paper_blue().setFill();
            capsule.fill();
        }
        let font = if sel == *days { &fonts.1 } else { &fonts.0 };
        let tw = crate::shelf::card::measure(title, font, 200.0, None).width;
        crate::shelf::card::draw_line_centered(title, font, &theme::ink(), rect.min().x + (rect.size.width - tw) / 2.0, rect.mid().y, tw + 1.0);
        hotspots.push(Hot::Row(rect, Choice::Retention(*days)));
        x += w + 4.0;
    }
    let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        CGRect::new(CGPoint::new(origin_right.x - total, origin_right.y - 2.0), CGSize::new(total, 30.0)),
        15.0,
        15.0,
    );
    theme::ink().colorWithAlphaComponent(0.5).setStroke();
    outline.setLineWidth(1.0);
    outline.stroke();
}

/// HourWheel: 04:00 + ▲▼ (the map's CleanupHour(99) rect carries scroll).
fn hour_wheel(_view: &SettingsView, hotspots: &mut Vec<Hot>, prefs: &preferences::Preferences, origin_right: CGPoint) {
    let hour = prefs.cleanup_hour();
    let active = prefs.retention_days() != 0;
    let rect = CGRect::new(origin_right, CGSize::new(96.0, 26.0));
    let alpha = if active { 1.0 } else { 0.35 };
    let font = objc2_app_kit::NSFont::monospacedDigitSystemFontOfSize_weight(13.5, 0.5);
    theme::ink().colorWithAlphaComponent(0.5).setStroke();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 13.0, 13.0).stroke();
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(rect.min().x + 12.0, rect.min().y + (26.0 - crate::shelf::card::line_height(&font)) / 2.0), CGSize::new(58.0, crate::shelf::card::line_height(&font))),
        &format!("{:02}:00", hour),
        &font,
        &theme::ink().colorWithAlphaComponent(alpha),
        None,
    );
    for (dy, choice, name) in [
        (2.0, 99i64, "chevron.up"),
        (13.0, 98i64, "chevron.down"),
    ] {
        let r = CGRect::new(CGPoint::new(rect.max().x - 24.0, rect.min().y + dy), CGSize::new(18.0, 11.0));
        if let Some(img) = crate::shelf::card::symbol(name, 8.0, crate::shelf::card::SymbolWeight::Semibold, &theme::ink_muted()) {
            crate::shelf::card::draw_centered(&img, r, alpha);
        }
        if active {
            hotspots.push(Hot::Row(r, Choice::CleanupHour(choice)));
        }
    }
    // Scroll-hit: an invisible rect so scrollWheel knows where the wheel is.
    if active {
        hotspots.push(Hot::Row(rect, Choice::CleanupHour(99)));
    }
}

/// The shortcut-recorder rows (SettingsWindow.swift's ShortcutRecorder).
fn recorder_row(_view: &SettingsView, hotspots: &mut Vec<Hot>, which: &'static str, state: &State, origin_right: CGPoint) {
    let (shortcut, taken, notice) = match which {
        "capture" => (state.capture, state.capture_taken, &state.capture_notice),
        "search" => (state.search, state.search_taken, &state.search_notice),
        _ => (state.shelf, state.shelf_taken, &state.shelf_notice),
    };
    let capturing = state.capturing == Some(which);
    let font = objc2_app_kit::NSFont::monospacedSystemFontOfSize_weight(13.0, 0.5);
    let text = if capturing {
        l("按下组合键")
    } else if shortcut.is_set() {
        shortcut.display()
    } else {
        l("立即设置")
    };
    let tw = crate::shelf::card::measure(&text, &font, 300.0, None).width.max(96.0);
    let show_clear = shortcut.is_set() && !capturing;
    let w = tw + 24.0 + if show_clear { 16.0 } else { 0.0 };
    let origin_x = origin_right.x - w;
    let rect = CGRect::new(CGPoint::new(origin_x, origin_right.y), CGSize::new(w, 26.0));
    let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 13.0, 13.0);
    if capturing {
        theme::paper_blue().setFill();
        capsule.fill();
    }
    theme::ink().colorWithAlphaComponent(0.6).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    let color = if shortcut.is_set() || capturing { theme::ink() } else { theme::ink_muted() };
    crate::shelf::card::draw_line_centered(&text, &font, &color, rect.min().x + 12.0, rect.mid().y, tw + 1.0);
    if show_clear {
        let clear_rect = CGRect::new(CGPoint::new(rect.max().x - 16.0, rect.min().y + 5.0), CGSize::new(16.0, 16.0));
        if let Some(img) = crate::shelf::card::symbol("xmark.circle.fill", 13.0, crate::shelf::card::SymbolWeight::Regular, &theme::ink_muted()) {
            crate::shelf::card::draw_centered(&img, clear_rect, 1.0);
        }
        hotspots.push(Hot::Clear(clear_rect, which));
    }
    hotspots.push(Hot::Row(rect, Choice::Recorder(which)));
    // Inline notices to the left of the capsule.
    let notice_text: Option<String> = if capturing {
        None
    } else if let Some(n) = notice {
        Some(n.clone())
    } else if taken {
        Some(if false { l("被其他应用占用，点击换一个") } else { l("被其他应用占用") })
    } else {
        None
    };
    if let Some(n) = notice_text {
        let font_n = theme::serif(12.0, false);
        let w_n = crate::shelf::card::measure(&n, &font_n, 300.0, None).width;
        crate::shelf::card::draw_text(
            CGRect::new(CGPoint::new(origin_x - 8.0 - w_n, origin_right.y + 4.0), CGSize::new(w_n + 1.0, crate::shelf::card::line_height(&font_n))),
            &n,
            &font_n,
            &theme::ink_muted(),
            Some(1.0),
        );
    }
}

// MARK: State helpers

fn abbreviate_home(path: &str) -> String {
    let home = std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
    if path.starts_with(&format!("{home}/")) {
        return format!("~/{}", &path[home.len() + 1..]);
    }
    path.to_string()
}

fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if let Ok(m) = entry.metadata() {
                total += m.len();
            }
        }
    }
    total
}

use std::path::Path;

fn desc_size(bytes: u64, items: usize) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    let size = if mb >= 100.0 {
        format!("{mb:.0} MB")
    } else if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{} KB", bytes / 1024)
    };
    format!("· {} · {}", size, l("%d 项").replacen("%d", &items.to_string(), 1))
}


// MARK: Choice actions

fn run_choice(choice: Choice, view: &SettingsView) {
    match choice {
        Choice::Back => {
            crate::shelf::panel::with_model_mut(|m| m.show_settings = false);
            crate::shelf::view::refresh();
        }
        Choice::Close => crate::shelf::panel::hide(),
        Choice::CheckUpdates => check_updates(view),
        Choice::CheckForUpdates => {
            let p = preferences::Preferences::shared();
            p.set_check_for_updates(!p.check_for_updates());
        }
        Choice::Recorder(which) => recorder_start(view, which),
        Choice::Retention(days) => change_retention(view, days),
        Choice::CleanupHour(delta) => {
            let p = preferences::Preferences::shared();
            p.set_cleanup_hour((p.cleanup_hour() + delta).rem_euclid(24));
            retention::schedule();
        }
        Choice::ClearNow => {
            let ids = crate::clipboard::store::read(|s| s.items.iter().filter(|it| !it.pinned).map(|it| it.id.clone()).collect::<Vec<_>>());
            crate::clipboard::store::with(|s| s.remove_ids(&ids));
            crate::shelf::panel::with_model_mut(|m| m.refresh_order());
            view.set_cleared();
            crate::shelf::view::refresh();
        }
        Choice::LaunchAtLogin => {
            preferences_m6::toggle_launch_at_login();
        }
        Choice::Language(code) => {
            preferences::Preferences::shared().set_language_pref(code);
            crate::shelf::panel::with_model_mut(|m| m.lang_tick = m.lang_tick.wrapping_add(1));
            let was_visible = crate::shelf::panel::is_visible();
            if was_visible { crate::shelf::panel::hide(); }
            crate::shelf::panel::show();
        }
        Choice::ScreenRec => permissions::open_settings("Privacy_ScreenCapture"),
        Choice::Pause => {
            let p = preferences::Preferences::shared();
            p.set_monitoring_paused(!p.monitoring_paused());
        }
        Choice::Password => {
            let p = preferences::Preferences::shared();
            p.set_record_password_managers(!p.record_password_managers());
        }
        Choice::Paste(mode) => preferences::Preferences::shared().set_paste_mode(mode),
        Choice::GrantAX => {
            permissions::request_accessibility();
            permissions::open_settings("Privacy_Accessibility");
        }
        Choice::ImageStorage(mode) => preferences::Preferences::shared().set_image_storage(mode),
        Choice::Codec(hevc) => preferences::Preferences::shared().set_record_hevc(hevc),
        Choice::ChooseExport => {
            crate::shelf::panel::with_dialog(|| {
                let mtm = MainThreadMarker::new().expect("main thread");
                let panel = objc2_app_kit::NSOpenPanel::openPanel(mtm);
                panel.setCanChooseDirectories(true);
                panel.setCanChooseFiles(false);
                panel.setPrompt(Some(&NSString::from_str(&l("选择"))));
                if panel.runModal() == objc2_app_kit::NSModalResponseOK {
                    if let Some(url) = panel.URL() {
                        if let Some(path) = url.path() {
                            preferences::Preferences::shared().set_custom_export_dir(Some(&path.to_string()));
                        }
                    }
                }
            });
        }
        Choice::OpenStore => {
            let root = crate::clipboard::store::read(|s| s.root.clone());
            let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&root.to_string_lossy()));
            objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
        }
        Choice::ImportDb => import_database(view),
        Choice::RemoveImported => remove_imported(view),
    }
    if !matches!(choice, Choice::Back | Choice::Close | Choice::ImportDb | Choice::CheckUpdates | Choice::RemoveImported | Choice::Language(_)) {
        view.refresh_state();
        crate::shelf::view::refresh();
    }
}

/// Shortening the retention can wipe a lot at once; say how much and ask.
fn change_retention(_view: &SettingsView, days: i64) {
    let p = preferences::Preferences::shared();
    let effective = |d: i64| if d == 0 { i64::MAX } else { d };
    if effective(days) < effective(p.retention_days()) {
        let doomed = crate::clipboard::store::read(|s| {
            s.items
                .iter()
                .filter(|it| retention::is_expired(it, item::now(), p.cleanup_hour(), days))
                .count()
        });
        if doomed > 0 {
            let go = crate::shelf::panel::with_dialog(|| {
                let mtm = MainThreadMarker::new().expect("main thread");
                let alert = objc2_app_kit::NSAlert::new(mtm);
                alert.setMessageText(&NSString::from_str(
                    &l("把保留期改成 %d 天？").replacen("%d", &days.to_string(), 1),
                ));
                alert.setInformativeText(&NSString::from_str(
                    &l("会立刻清掉 %d 条未 Pin 的记录。Pin 住的不受影响。")
                        .replacen("%d", &doomed.to_string(), 1),
                ));
                alert.addButtonWithTitle(&NSString::from_str(&l("改并清理")));
                alert.addButtonWithTitle(&NSString::from_str(&l("取消")));
                alert.runModal() == objc2_app_kit::NSAlertFirstButtonReturn
            });
            if !go {
                return;
            }
        }
    }
    p.set_retention_days(days);
    retention::schedule();
}

/// The settings-pane quiet check: outcome lands in the update note
/// (Swift's `check(interactive: true, quiet: true)` + updateNote).
fn check_updates(view: &SettingsView) {
    let view_raw = view.raw_pub();
    crate::app::updater::check_with(true, true, move |outcome| {
        let note = match outcome {
            crate::app::updater::Outcome::UpToDate => Some(l("已是最新版本")),
            crate::app::updater::Outcome::Available(v) => Some(l("有新版本 %@").replacen("%@", &v, 1)),
            crate::app::updater::Outcome::Failed(msg) => Some(l("检查失败：%@").replacen("%@", &msg, 1)),
            crate::app::updater::Outcome::Skipped => None,
        };
        let Some(note) = note else { return };
        crate::app::delegate::dispatch_main_async(Box::new(move || {
            let Some(v) = live_settings_view() else { return };
            if v.raw_pub() == view_raw {
                v.set_update_note(Some(note));
            }
        }));
    });
}

/// Pick a .sqlite (or a Pastory folder), count what is inside, ask, import.
fn import_database(view: &SettingsView) {
    let picked = crate::shelf::panel::with_dialog(|| {
        let mtm = MainThreadMarker::new().expect("main thread");
        let panel = objc2_app_kit::NSOpenPanel::openPanel(mtm);
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(true);
        panel.setPrompt(Some(&NSString::from_str(&l("扫描"))));
        panel.setMessage(Some(&NSString::from_str(&l(
            "选另一个剪贴板工具的数据库文件或它的数据文件夹，或另一台机器的 Pastory 文件夹",
        ))));
        if panel.runModal() == objc2_app_kit::NSModalResponseOK {
            panel.URL().and_then(|u| u.path().map(|p| std::path::PathBuf::from(p.to_string())))
        } else {
            None
        }
    });
    let Some(picked) = picked else { return };
    view.set_import_note(Some(l("扫描中…")));
    let view_raw = view.raw_pub();
    std::thread::spawn(move || {
        let outcome = crate::clipboard::importer::scan(&picked);
        crate::app::delegate::dispatch_main_async(Box::new(move || {
            let scan = match outcome {
                Ok(s) => s,
                Err(e) => {
                    if let Some(v) = live_settings_view() {
                        if v.raw_pub() == view_raw {
                            v.set_import_note(Some(e.message()));
                        }
                    }
                    return;
                }
            };
            let p = preferences::Preferences::shared();
            let cleans = !p.never_cleans();
            let days = p.retention_days();
            let texts = scan.texts();
            let images = scan.images();
            let from = picked.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let go = crate::shelf::panel::with_dialog(|| {
                let mtm = MainThreadMarker::new().expect("main thread");
                let alert = objc2_app_kit::NSAlert::new(mtm);
                alert.setMessageText(&NSString::from_str(
                    &l("找到 %d 条文本、%d 张图片")
                        .replacen("%d", &texts.to_string(), 1)
                        .replacen("%d", &images.to_string(), 1),
                ));
                let mut info = l("来自 %@。已经在 Pastory 里的内容会自动跳过，Pin 会保留；导入的内容排在 Pastory 自己记录的后面。")
                    .replacen("%@", &from, 1);
                if cleans {
                    info.push_str(&l("\n\n当前保留期是 %d 天，而这些内容都比保留期老：导入会同时把保留期改为「永不删除」，否则它们马上就会被清掉。").replacen("%d", &days.to_string(), 1));
                }
                alert.setInformativeText(&NSString::from_str(&info));
                let import_title = if cleans { l("导入并改为永不删除") } else { l("导入") };
                alert.addButtonWithTitle(&NSString::from_str(&import_title));
                alert.addButtonWithTitle(&NSString::from_str(&l("取消")));
                alert.runModal() == objc2_app_kit::NSAlertFirstButtonReturn
            });
            if !go {
                if let Some(v) = live_settings_view() {
                    v.set_import_note(None);
                }
                return;
            }
            if cleans {
                p.set_retention_days(0);
                retention::schedule();
            }
            if let Some(v) = live_settings_view() {
                v.set_import_note(Some(l("导入中…")));
            }
            let heic = p.stores_heic();
            let entries = scan.entries;
            std::thread::spawn(move || {
                let prepared = crate::clipboard::store::ClipStore::prepare_import(entries, heic);
                crate::app::delegate::dispatch_main_async(Box::new(move || {
                    let n = crate::clipboard::store::with(|s| s.commit_import(prepared));
                    crate::shelf::panel::with_model_mut(|m| m.refresh_order());
                    if let Some(v) = live_settings_view() {
                        v.set_import_note(Some(
                            if n == 0 {
                                l("没有新内容（都已存在）")
                            } else {
                                l("已导入 %d 条").replacen("%d", &n.to_string(), 1)
                            },
                        ));
                        v.refresh_state();
                    }
                    crate::shelf::view::refresh();
                }));
            });
        }));
    });
}

fn remove_imported(view: &SettingsView) {
    let n = crate::clipboard::store::read(|s| {
        s.items
            .iter()
            .filter(|it| it.source_app_name.as_deref() == Some(store::IMPORT_SOURCE_NAME))
            .count()
    });
    if n == 0 {
        return;
    }
    let go = crate::shelf::panel::with_dialog(|| {
        let mtm = MainThreadMarker::new().expect("main thread");
        let alert = objc2_app_kit::NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(
            &l("移除 %d 条导入的内容？").replacen("%d", &n.to_string(), 1),
        ));
        alert.setInformativeText(&NSString::from_str(&l(
            "只删来源标为「导入」的条目，包括其中已 Pin 的；其他内容不动。",
        )));
        alert.addButtonWithTitle(&NSString::from_str(&l("移除")));
        alert.addButtonWithTitle(&NSString::from_str(&l("取消")));
        alert.runModal() == objc2_app_kit::NSAlertFirstButtonReturn
    });
    if !go {
        return;
    }
    let ids: Vec<String> = crate::clipboard::store::read(|s| {
        s.items
            .iter()
            .filter(|it| it.source_app_name.as_deref() == Some(store::IMPORT_SOURCE_NAME))
            .map(|it| it.id.clone())
            .collect()
    });
    crate::clipboard::store::with(|s| s.remove_ids(&ids));
    crate::shelf::panel::with_model_mut(|m| m.refresh_order());
    view.set_import_note(Some(l("已移除 %d 条").replacen("%d", &n.to_string(), 1)));
    view.refresh_state();
    crate::shelf::view::refresh();
}

// MARK: Shortcut recorder (SettingsWindow.swift's ShortcutRecorder)

/// One recorder at a time; the hotkeys suspend so our own combos reach the
/// box instead of firing.
pub fn recorder_start(view: &SettingsView, which: &'static str) {
    let capturing = view.ivars().state.borrow().capturing;
    if capturing.is_some() {
        return;
    }
    view.set_capturing(Some(which));
    HotKeyCenter::shared().suspend();
}

/// A captured key: ⎋ cancels, ⌫ clears, anything else registers.
pub fn recorder_key_down(view: &SettingsView, event: &NSEvent) -> bool {
    let Some(which) = view.ivars().state.borrow().capturing else { return false };
    match event.keyCode() {
        53 => stop_recorder(view, which, None),
        51 => stop_recorder(view, which, Some(Shortcut::NONE)),
        _ => {
            if let Some(s) = shortcut_from_event(event) {
                stop_recorder(view, which, Some(s));
            } else {
                stop_recorder(view, which, None);
                view.set_notice(which, Some(l("至少两个键：⌘ ⌥ ⌃ ⇧ 中的一个加一个键")));
            }
        }
    }
    true
}

pub fn shortcut_from_event_pub(e: &NSEvent) -> Option<Shortcut> {
    shortcut_from_event(e)
}

fn shortcut_from_event(e: &NSEvent) -> Option<Shortcut> {
    let flags = e
        .modifierFlags()
        .intersection(objc2_app_kit::NSEventModifierFlags::DeviceIndependentFlagsMask);
    let mut carbon = 0u32;
    if flags.contains(objc2_app_kit::NSEventModifierFlags::Command) { carbon |= preferences::CMD_KEY; }
    if flags.contains(objc2_app_kit::NSEventModifierFlags::Shift) { carbon |= preferences::SHIFT_KEY; }
    if flags.contains(objc2_app_kit::NSEventModifierFlags::Option) { carbon |= preferences::OPTION_KEY; }
    if flags.contains(objc2_app_kit::NSEventModifierFlags::Control) { carbon |= preferences::CONTROL_KEY; }
    if carbon == 0 {
        return None;
    }
    Some(Shortcut { key_code: e.keyCode() as u32, carbon_modifiers: carbon })
}

/// Commit or cancel. Suspended hotkeys come back either way.
fn stop_recorder(view: &SettingsView, which: &'static str, new_value: Option<Shortcut>) {
    view.set_capturing(None);
    HotKeyCenter::shared().resume();
    let Some(new_value) = new_value else { return };
    // Self-conflict: another one of ours already holds the combo.
    if new_value.is_set() {
        let p = preferences::Preferences::shared();
        let mine: [(&str, &str, &str); 3] = [
            ("capture", preferences::key::HOTKEY_CAPTURE, "截图"),
            ("shelf", preferences::key::HOTKEY_SHELF, "剪贴板"),
            ("search", preferences::key::HOTKEY_SEARCH, "搜索剪贴板"),
        ];
        for (owner, key, label) in mine {
            if owner != which && p.shortcut(key) == new_value {
                view.set_notice(which, Some(l("已被 Pastory 的「%@」占用，换一个").replacen("%@", &l(label), 1)));
                return;
            }
        }
        if !HotKeyCenter::shared().is_available(new_value) {
            view.set_notice(which, Some(l("已被其他应用占用，换一个")));
            return;
        }
    }
    let key = match which {
        "capture" => preferences::key::HOTKEY_CAPTURE,
        "search" => preferences::key::HOTKEY_SEARCH,
        _ => preferences::key::HOTKEY_SHELF,
    };
    preferences::Preferences::shared().set_shortcut(key, new_value);
    post_shortcuts_changed();
    // Bare ⌘/⇧ combo is legal but global: say so once instead of refusing.
    let m = new_value.carbon_modifiers;
    if new_value.is_set() && (m == preferences::CMD_KEY || m == preferences::SHIFT_KEY || m == (preferences::CMD_KEY | preferences::SHIFT_KEY)) {
        view.set_notice(which, Some(l("已设置。注意：所有应用里的 %@ 都会变成这个功能").replacen("%@", &new_value.display(), 1)));
    } else {
        view.set_notice(which, None);
    }
    view.refresh_state();
}

pub fn post_shortcuts_changed() {
    unsafe {
        objc2_foundation::NSNotificationCenter::defaultCenter().postNotificationName_object(
            objc2_foundation::ns_string!("pastory.shortcutsChanged"),
            None,
        );
    }
}

pub fn post_shortcut_binding_changed() {
    unsafe {
        objc2_foundation::NSNotificationCenter::defaultCenter().postNotificationName_object(
            objc2_foundation::ns_string!("pastory.shortcutBindingChanged"),
            None,
        );
    }
}

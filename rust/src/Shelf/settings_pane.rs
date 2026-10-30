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
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBezierPath, NSColor, NSEvent, NSEventModifierFlags, NSEventType, NSView};
use objc2_app_kit::NSGraphicsContext;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

use crate::app::hotkey::HotKeyCenter;
use crate::app::localization::l;
use crate::app::{layout_skin, permissions, preferences, theme, theme_skin};
use crate::app::preferences::Shortcut;
use crate::app::preferences_m6;
use crate::clipboard::item;
use crate::clipboard::retention;
use crate::clipboard::store;

// MARK: Geometry

const SHEET_PAD: f64 = 16.0;
/// Fixed header band (title + back/close); the two sheet columns scroll under it.
const HEADER_H: f64 = 62.0;
/// 左栏标签导航的宽度。
const NAV_W: f64 = 148.0;
/// 导航列与内容列之间的间距。
const NAV_GAP: f64 = 18.0;
/// 内容列的最大宽度。一行里的标签和它的控件必须能一眼连起来：面板取整屏宽，
/// 34" 超宽屏上是 3440pt，不设上限的话标签贴着左缘、控件贴着右缘，中间隔开
/// 一千多 pt，读起来是两件无关的东西。720 与 macOS 自带设置页一个量级。
const CONTENT_MAX_W: f64 = 720.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
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
    /// Relaunch so a just-granted Screen Recording / Accessibility permission
    /// reaches this process.
    Restart,
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
    /// Switch the pane to another settings tab (`tab_items` ids).
    Tab(&'static str),
    /// Pick a skin by id (`theme_skin::Skin::id`).
    Skin(&'static str),
    /// Pick a shelf form by id (`layout_skin::Layout::id`).
    Layout(&'static str),
    /// Pick the panel's own background image (设置 › 外观).
    ChooseBackground,
    /// Drop the background image; the skin's own ground returns.
    ClearBackground,
    /// One of `background::DIM_STEPS`, in tenths (photo opacity).
    BackgroundOpacity(i64),
    /// Turn the remembered photo back on (the skin owns the panel until
    /// then — the two are mutually exclusive).
    EnableBackground,
    /// 晾衣绳 card size, as a percentage of the base (100 / 120 / 150).
    LineSize(isize),
    /// 晾衣绳 gap between cards, in points (24 / 48 / 80).
    LineGap(isize),
    /// 晾衣绳 reveals itself when the pointer reaches the menu bar.
    LineAutoReveal(bool),
    /// 晾衣绳 photos sway on the rope.
    LineSwing(bool),
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
    /// 当前设置页（`tab_items` 的 id）。面板可视高只有 ~420pt，
    /// 分页比两栏长滚动可靠，也让背景图这类功能一眼看得见。
    tab: std::cell::Cell<&'static str>,
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
            tab: std::cell::Cell::new("appearance"),
            scroll: std::cell::Cell::new(0.0),
            content_h: std::cell::Cell::new(0.0),
        });
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.refresh_state();
        // `--selftest settings` otherwise always lands on 外观, and the other
        // pages are only reachable by clicking — which a render cannot do.
        // `PASTORY_SETTINGS_TAB=<id>` opens that page instead.
        let launch = crate::app::sandbox::launch();
        if let Some((_, want)) = launch.env.iter().find(|(k, _)| k == "PASTORY_SETTINGS_TAB") {
            if let Some((id, _)) = tab_items().iter().find(|(id, _)| *id == want.as_str()) {
                view.ivars().tab.set(*id);
            }
        }
        if !launch.is_self_test() {
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
        st.has_sr = permissions::has_screen_recording()
            && !crate::app::sandbox::launch().env.iter().any(|(k, _)| k == "PASTORY_NOSR");
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

/// Work a settings click has to defer to after its own event.
///
/// Both variants rebuild the whole view tree, and a rebuild destroys the very
/// `SettingsView` that handled the click: `attach` swaps the panel's content
/// view, the old root is released, and its subtree — this view, its ivars and
/// the `Choice` mid-dispatch — goes with it. The click therefore only stores
/// the request here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelCommand {
    /// Rebuild the tree in the newly selected skin (`panel::retheme`).
    Reskin,
    /// Swap the shelf's whole form (`panel::relayout`). Separate from
    /// `Reskin` because a form swap also moves and resizes the window — the
    /// panel form is a bottom drawer, the line form a top strip — whereas a
    /// skin swap must keep the frame so the shelf does not jump.
    Relayout,
    /// Rebuild the panel so every string re-resolves in the new language.
    Relanguage,
}

thread_local! {
    static PANEL_COMMANDS: RefCell<Vec<PanelCommand>> = const { RefCell::new(Vec::new()) };
}

/// Record `c` and make sure something will act on it once the current event
/// dispatch — the click that asked for it — has returned.
fn queue_panel_command(c: PanelCommand) {
    PANEL_COMMANDS.with(|q| q.borrow_mut().push(c));
    crate::app::delegate::dispatch_main_after(
        0.0,
        Box::new(crate::shelf::view::apply_panel_commands),
    );
}

/// Take the pending commands; the caller runs them.
pub fn take_panel_commands() -> Vec<PanelCommand> {
    PANEL_COMMANDS.with(|q| std::mem::take(&mut *q.borrow_mut()))
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
    let tab = view.ivars().tab.get();
    let mut hotspots: Vec<Hot> = Vec::new();
    // 导航列和内容列合成一个模块，在可用宽度里居中。
    //
    // 面板宽度取的是整个屏幕（panel.rs:327），34" 超宽屏上是 3440pt，而设置页
    // 真正需要的宽度只有 NAV_W + NAV_GAP + CONTENT_MAX_W = 886pt。贴左排会在右侧
    // 留下两千多 pt 死白，看着像画歪了；居中后两侧留白对称，是一条有意排出来的
    // 居中栏，而不是贴着侧栏的一段。窄面板下 module_w 被 .min(cw) 收住，
    // (cw - module_w) 为 0 或负，取 max(0.0) 后退化为占满可用宽度。
    let module_w = (NAV_W + NAV_GAP + CONTENT_MAX_W).min(cw);
    let module_x = cx + ((cw - module_w) / 2.0).max(0.0);
    let module_right = module_x + module_w;
    // Header: title + 关闭（⎋ 即返回；「‹ 返回剪贴板」按钮已按用户要求移除）.
    let title_font = theme::serif(22.0, true);
    text(view, CGPoint::new(module_x, 18.0), &l("设置"), &title_font, &theme::label(), None);
    hit_text(view, &mut hotspots, "✕",
        CGRect::new(CGPoint::new(module_right - 40.0, 18.0), CGSize::new(40.0, 36.0)),
        &theme::serif(16.0, false), &theme::label(), Choice::Close, true);
    // Sheet columns scroll under the fixed header (Swift's ScrollView). The
    // scroll offset just shifts the y accumulators; every derived rect
    // (rows, buttons, hotspots) follows, and the band clips to the header line.
    let scroll = view.ivars().scroll.get().clamp(
        0.0,
        (view.ivars().content_h.get() - (b.size.height - HEADER_H)).max(0.0),
    );
    // 左栏标签导航（固定在 header 之下，不随页面滚动）。
    let mut nav_y = HEADER_H + 4.0;
    for (id, label) in tab_items() {
        let rect = CGRect::new(CGPoint::new(module_x, nav_y), CGSize::new(NAV_W, 30.0));
        let is_sel = id == tab;
        if is_sel {
            let pill = theme::rounded_rect(rect, theme::radius_control());
            theme::accent_tint().setFill();
            pill.fill();
        }
        let font = theme::serif(13.0, is_sel);
        let color = if is_sel { theme::accent() } else { theme::label() };
        let lh = crate::shelf::card::line_height(&font);
        text(view, CGPoint::new(module_x + 14.0, nav_y + (30.0 - lh) / 2.0), &label, &font, &color, None);
        hotspots.push(Hot::Row(rect, Choice::Tab(id)));
        nav_y += 32.0;
    }

    // 内容区在导航右侧；滚动只作用于此。宽度上限取自 CONTENT_MAX_W，
    // 位置由上面的模块左缘决定（不在剩余宽度里二次居中）。
    let content_x = module_x + NAV_W + NAV_GAP;
    let content_w = (module_right - content_x).max(0.0);
    NSGraphicsContext::saveGraphicsState_class();
    NSBezierPath::bezierPathWithRect(CGRect::new(
        CGPoint::new(content_x, HEADER_H),
        CGSize::new(content_w, (b.size.height - HEADER_H).max(0.0)),
    ))
    .addClip();
    let mut y = HEADER_H - scroll;

    let col_w = content_w;
    let col1 = content_x;
    let col2 = content_x;

    if tab == "appearance" {
        sheet_header(view, &mut y, col1, col_w, &l("形态"));
        // `layout_picker` 的卡片是 56pt 高（不是 44pt 行），所以高度直接给：
        // 28 顶部留白 + 56 卡片 + 12 底部留白，与皮肤网格的下留白一致。
        sheet_h(view, &mut y, col1, col_w, 28.0 + 56.0 + 12.0, |v, y_pos| {
            layout_picker(v, &mut hotspots, col1, col_w, y_pos);
        });
        // 晾衣绳自己的显示设置：卡片大小 / 间距 / 自动呼出 / 摆动。面板下改这几
        // 项不会立刻看见，所以分组名里点出「晾衣绳」；一经改动就落盘，下次切到
        // 晾衣绳（或当前正在用时立即）生效。
        let line_on = layout_skin::is_line();
        sheet_header(view, &mut y, col1, col_w, &line_group_title());
        sheet(view, &mut y, col1, col_w, 4, |v, y_pos| {
            line_settings(v, &mut hotspots, col1, col_w, y_pos, line_on);
        });
        sheet_header(view, &mut y, col1, col_w, &l("皮肤"));
        sheet(view, &mut y, col1, col_w, 4, |v, y_pos| {
            skin_grid(v, &mut hotspots, col1, col_w, y_pos);
        });
        // 背景图：与皮肤的配色互斥 —— 选了皮肤照片就停用，选了照片皮肤色
        // 就让位；停用不忘记文件，再点启用就回来。透明度即照片浓度。
        sheet_header(view, &mut y, col1, col_w, &l("背景图"));
        let has_image = crate::app::background::has_image();
        let on = crate::app::background::active();
        let rows = if has_image { 3 } else { 2 };
        sheet(view, &mut y, col1, col_w, rows, |v, y_pos| {
            let row = sheet_row(y_pos, col1, col_w, 0);
            let name = crate::app::background::display_name();
            let (label, sub) = if name.is_empty() {
                (crate::app::localization::l_ctx("未设置", "background"), l("选一张照片铺在内容区，皮肤配色会让位"))
            } else if on {
                (name, abbreviate_home(&crate::app::background::display_path()))
            } else {
                (name, l("已停用 —— 皮肤配色在使用中，点「启用」换回照片"))
            };
            let ink = if has_image && !on { theme::label_secondary() } else { theme::label() };
            text_color(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 6.0), &label, &theme::serif(15.0, false), &ink, None);
            text_color(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 24.0), &sub, &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
            hit_button(view, &mut hotspots, &l("选择图片…"),
                CGPoint::new(row.0.x + col_w - 32.0 - 110.0, row.0.y + 10.0), Choice::ChooseBackground, 110.0, false);
            if has_image {
                let row2 = sheet_row(y_pos, col1, col_w, 1);
                text(v, CGPoint::new(row2.0.x + SHEET_PAD, row2.0.y + (row2.1 - line_height_f()) / 2.0), &l("启用"), &theme::serif(15.0, false), &theme::label(), None);
                switch_control(v, &mut hotspots, CGPoint::new(row2.0.x + col_w - 32.0 - 44.0, row2.0.y + 10.0), Choice::EnableBackground, on, v);
                let row3 = sheet_row(y_pos, col1, col_w, 2);
                text(v, CGPoint::new(row3.0.x + SHEET_PAD, row3.0.y + (row3.1 - line_height_f()) / 2.0), &l("透明度"), &theme::serif(15.0, false), &theme::label(), None);
                dim_segments(v, &mut hotspots, CGPoint::new(row3.0.x + col_w - 32.0, row3.0.y + 9.0));
            }
        });
    }
    if tab == "updates" {
    sheet_header(view, &mut y, col1, col_w, &l("版本更新"));
    sheet(view, &mut y, col1, col_w, 3, |v, y_pos| {
        let row = sheet_row(y_pos, col1, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("当前版本 %@").replacen("%@", &crate::app::updater::current_version(), 1), &theme::serif(15.0, false), &theme::label(), None);
        if let Some(n) = &state.update_note {
            let font = theme::serif(12.0, false);
            let w = crate::shelf::card::measure(n, &font, 220.0, None).width.min(200.0);
            text(v, CGPoint::new(row.0.x + col_w - 32.0 - 160.0 - w, row.0.y + 12.0), n, &font, &theme::label_secondary(), Some(1.0));
        }
        let check_label = if state.checking { l("检查中…") } else { l("手动检查更新") };
        hit_button(view, &mut hotspots, &check_label,
            CGPoint::new(row.0.x + col_w - 32.0 - 160.0, row.0.y + 10.0), Choice::CheckUpdates, 160.0, false);
        // 构建指纹：版本号 1.0.7 在每次重建里都一样，只有这一行能说明
        // 眼前跑的是哪个二进制（git 短哈希 + 是否带未提交改动 + 构建时间）。
        let build_row = sheet_row(y_pos, col1, col_w, 1);
        text(v, CGPoint::new(build_row.0.x + SHEET_PAD, build_row.0.y + (build_row.1 - line_height_f()) / 2.0), &l("构建 %@").replacen("%@", crate::app::updater::build_stamp(), 1), &theme::serif(13.0, false), &theme::label_secondary(), Some(1.0));
        let row2 = sheet_row(y_pos, col1, col_w, 2);
        text(v, CGPoint::new(row2.0.x + SHEET_PAD, row2.0.y + (row2.1 - line_height_f()) / 2.0), &l("每天自动检查一次（app 唯一的联网请求，不带任何标识）"), &theme::serif(13.0, false), &theme::label_secondary(), Some(1.0));
        // toggle() 只登记热区、不画任何像素，这一行的开关在界面上完全看不见。
        // 换成真正会绘制的那种，并把开关落在与单行标签同一个视觉中线上。
        switch_control(v, &mut hotspots, CGPoint::new(row2.0.x + col_w - 32.0 - 44.0, row2.0.y + 10.0), Choice::CheckForUpdates, preferences::Preferences::shared().check_for_updates(), v);
    });
    }
    if tab == "shortcuts" {
    sheet_header(view, &mut y, col1, col_w, &l("快捷键"));
    sheet(view, &mut y, col1, col_w, 3, |v, y_pos| {
        for (i, (label, which, taken)) in [
            (&l("截图"), "capture", state.capture_taken),
            (&l("显示 / 隐藏剪贴板"), "shelf", state.shelf_taken),
            (&l("搜索剪贴板"), "search", state.search_taken),
        ].iter().enumerate() {
            let row = sheet_row(y_pos, col1, col_w, i);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), label, &theme::serif(15.0, false), &theme::label(), None);
            recorder_row(v, &mut hotspots, which, &state, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 9.0));
            let _ = taken;
        }
    });
    }
    if tab == "cleanup" {
    sheet_header(view, &mut y, col1, col_w, &l("清理"));
    {
        let prefs = preferences::Preferences::shared();
        sheet(view, &mut y, col1, col_w, 3, |v, y_pos| {
            let row = sheet_row(y_pos, col1, col_w, 0);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("未 Pin 内容保留时间"), &theme::serif(15.0, false), &theme::label(), None);
            retention_segments(v, &mut hotspots, &prefs, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 9.0));
            let row = sheet_row(y_pos, col1, col_w, 1);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("当日清理时间"), &theme::serif(15.0, false), &theme::label(), None);
            hour_wheel(v, &mut hotspots, &prefs, CGPoint::new(row.0.x + col_w - 32.0 - 96.0, row.0.y + 9.0));
            let row = sheet_row(y_pos, col1, col_w, 2);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("手动清空一次（不含已 Pin 内容）"), &theme::serif(15.0, false), &theme::label(), None);
            let clear_label = if state.cleared { l("已清空") } else { l("现在清空") };
            hit_button(view, &mut hotspots, &clear_label,
                CGPoint::new(row.0.x + col_w - 32.0 - 120.0, row.0.y + 10.0), Choice::ClearNow, 120.0, state.cleared);
        });
    }
    }
    if tab == "system" {
    sheet_header(view, &mut y, col1, col_w, &l("系统"));
    sheet(view, &mut y, col1, col_w, if state.has_sr { 3 } else { 4 }, |v, y_pos| {
        let row = sheet_row(y_pos, col1, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("登录时启动"), &theme::serif(15.0, false), &theme::label(), None);
        switch_control(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0 - 44.0, row.0.y + 10.0), Choice::LaunchAtLogin, state.login_on, view);
        let row = sheet_row(y_pos, col1, col_w, 1);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("语言"), &theme::serif(15.0, false), &theme::label(), None);
        let prefs = preferences::Preferences::shared();
        let lang = prefs.language().to_string();
        let opts: [(&str, &str, Choice); 3] = [
            ("system", &l("跟随系统"), Choice::Language("system")),
            ("zh", &l("中文"), Choice::Language("zh")),
            ("en", "English", Choice::Language("en")),
        ];
        segment_choice(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 9.0), &opts, &lang);
        let row = sheet_row(y_pos, col1, col_w, 2);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("屏幕录制权限（截图、录屏需要）"), &theme::serif(15.0, false), &theme::label(), None);
        let sr_label = if state.has_sr { l("已授权") } else { l("未授权") };
        let right = row.0.x + col_w - 32.0;
        // 右侧按实际量出来的宽度从右往左排：英文的「Not granted / Grant… /
        // Restart PasteJar」比中文宽，写死宽度会互相压住甚至溢出纸卡。
        let grant_label = l("去授权");
        let grant_w = button_w(&grant_label);
        let tag_x = if state.has_sr { right - tag_w(&sr_label) } else { right - grant_w - 8.0 - tag_w(&sr_label) };
        tag(v, CGPoint::new(tag_x, row.0.y + 11.0), &sr_label, state.has_sr);
        if !state.has_sr {
            hit_button(view, &mut hotspots, &grant_label, CGPoint::new(right - grant_w, row.0.y + 10.0), Choice::ScreenRec, grant_w, false);
            // 授权是在系统设置里点的开关，但屏幕录制只对「之后启动」的进程生效。
            // 用户在系统设置里看到开关已经打开、软件里却还是未授权，这一行是
            // 那个时候唯一的出口（旧界面只把人送回那个已经是打开的页面）。
            let row = sheet_row(y_pos, col1, col_w, 3);
            text_color(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("在系统设置里已打开的话，重启后才会生效"), &theme::serif(12.0, false), &theme::label_secondary(), None);
            let restart_label = l("重启 PasteJar");
            let restart_w = button_w(&restart_label);
            hit_button(view, &mut hotspots, &restart_label, CGPoint::new(row.0.x + col_w - 32.0 - restart_w, row.0.y + 10.0), Choice::Restart, restart_w, false);
        }
    });
    }

    // 剪贴板 / 截图与录屏 / 位置 / 导入.
    let prefs = preferences::Preferences::shared();
    let mut y2 = HEADER_H - scroll;
    if tab == "clipboard" {
    sheet_header(view, &mut y2, col2, col_w, &l("剪贴板"));
    // 「双击/回车粘贴」那一行在没有辅助功能权限时还要多出一条提示，44pt 行高
    // 塞不下三段文字（原先把提示画在 sheet 底边上），所以那时给它单独一行。
    let paste_needs_ax = prefs.paste_mode() != "off" && !state.has_ax;
    sheet(view, &mut y2, col2, col_w, if paste_needs_ax { 4 } else { 3 }, |v, y_pos| {
        let row = sheet_row(y_pos, col2, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("暂停记录剪贴板"), &theme::serif(15.0, false), &theme::label(), None);
        switch_control(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0 - 44.0, row.0.y + 10.0), Choice::Pause, prefs.monitoring_paused(), view);
        let row = sheet_row(y_pos, col2, col_w, 1);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("记录密码管理器复制的内容"), &theme::serif(15.0, false), &theme::label(), None);
        let hint = if prefs.record_password_managers() { &l("从 1Password 等「密码管理软件」复制的内容也会进剪贴板。") } else { &l("从 1Password 等「密码管理软件」复制的内容均不进剪贴板。") };
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), hint, &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
        switch_control(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0 - 44.0, row.0.y + 10.0), Choice::Password, prefs.record_password_managers(), view);
        let row = sheet_row(y_pos, col2, col_w, 2);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("直接粘贴到刚才的应用"), &theme::serif(15.0, false), &theme::label(), None);
        let hint = match prefs.paste_mode().as_str() {
            "off" => l("双击和回车都只复制并收起"),
            "double" => l("双击卡片，内容直接贴进刚才的应用"),
            _ => l("用方向键选中卡片后，回车也会直接粘贴"),
        };
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), &hint, &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
        let opts: [(&str, &str, Choice); 3] = [
            ("off", &crate::app::localization::l_ctx("关闭", "paste"), Choice::Paste("off")),
            ("double", &l("双击"), Choice::Paste("double")),
            ("return", &l("双击 + 回车"), Choice::Paste("return")),
        ];
        segment_choice(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 10.0), &opts, &prefs.paste_mode());
        if paste_needs_ax {
            let row = sheet_row(y_pos, col2, col_w, 3);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("需要辅助功能权限"), &theme::serif(12.0, false), &theme::label(), None);
            hit_button(view, &mut hotspots, &l("去授权"), CGPoint::new(row.0.x + col_w - 32.0 - 80.0, row.0.y + 10.0), Choice::GrantAX, 80.0, false);
        }
    });
    }
    if tab == "capture" {
    sheet_header(view, &mut y2, col2, col_w, &l("截图与录屏"));
    sheet(view, &mut y2, col2, col_w, 2, |v, y_pos| {
        let row = sheet_row(y_pos, col2, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("本地数据库截图存储方式"), &theme::serif(15.0, false), &theme::label(), None);
        let hint = if prefs.stores_heic() { &l("高质量有损压缩，体积约为 PNG 的三分之一") } else { &l("无损，体积最大") };
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), hint, &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
        let opts: [(&str, &str, Choice); 2] = [("heic", "HEIC", Choice::ImageStorage("heic")), ("png", "PNG", Choice::ImageStorage("png"))];
        segment_choice(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 10.0), &opts, &prefs.image_storage());
        let row = sheet_row(y_pos, col2, col_w, 1);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("录屏编码"), &theme::serif(15.0, false), &theme::label(), None);
        let hint = if prefs.record_hevc() { &l("体积小一半；老设备和部分 Windows 打不开") } else { &l("所有设备都能播放") };
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), hint, &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
        let opts: [(&str, &str, Choice); 2] = [("h264", "H.264", Choice::Codec(false)), ("hevc", "HEVC", Choice::Codec(true))];
        segment_choice(v, &mut hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 10.0), &opts, if prefs.record_hevc() { "hevc" } else { "h264" });
    });
    }
    if tab == "location" {
    sheet_header(view, &mut y2, col2, col_w, &l("位置"));
    sheet(view, &mut y2, col2, col_w, 2, |v, y_pos| {
        let row = sheet_row(y_pos, col2, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("「保存到本地」默认打开的文件夹"), &theme::serif(15.0, false), &theme::label(), None);
        let dir = prefs.custom_export_dir().unwrap_or_else(|| prefs.export_directory().to_string_lossy().into_owned());
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), &abbreviate_home(&dir), &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
        hit_button(view, &mut hotspots, &l("选择…"), CGPoint::new(row.0.x + col_w - 32.0 - 90.0, row.0.y + 10.0), Choice::ChooseExport, 90.0, false);
        let row = sheet_row(y_pos, col2, col_w, 1);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 8.0), &l("剪贴板内容临时存放位置"), &theme::serif(15.0, false), &theme::label(), None);
        let (path, bytes, count) = store::read(|s| {
            (s.root.to_string_lossy().into_owned(), dir_size(&s.root), s.items.len())
        });
        let meta = format!("{} · {} · · {}", l("SQLite 数据库"), abbreviate_home(&path), desc_size(bytes, count));
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 26.0), &meta, &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
        hit_button(view, &mut hotspots, &l("查看"), CGPoint::new(row.0.x + col_w - 32.0 - 70.0, row.0.y + 10.0), Choice::OpenStore, 70.0, false);
    });
    }
    let has_imported = store::read(|s| s.items.iter().any(|it| it.source_app_name.as_deref() == Some(store::IMPORT_SOURCE_NAME)));
    if tab == "import" {
    sheet_header(view, &mut y2, col2, col_w, &l("导入"));
    sheet(view, &mut y2, col2, col_w, if has_imported || state.import_note.is_some() { 2 } else { 1 }, |v, y_pos| {
        let row = sheet_row(y_pos, col2, col_w, 0);
        text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("从其他剪贴板工具导入（SQLite）"), &theme::serif(15.0, false), &theme::label(), None);
        if let Some(n) = &state.import_note {
            let font = theme::serif(12.0, false);
            let w = crate::shelf::card::measure(n, &font, 220.0, None).width.min(200.0);
            text(v, CGPoint::new(row.0.x + col_w - 32.0 - 120.0 - w - 8.0, row.0.y + 12.0), n, &font, &theme::label_secondary(), Some(1.0));
        }
        hit_button(view, &mut hotspots, &l("选择数据库…"), CGPoint::new(row.0.x + col_w - 32.0 - 120.0, row.0.y + 10.0), Choice::ImportDb, 120.0, false);
        if has_imported {
            let row = sheet_row(y_pos, col2, col_w, 1);
            text(v, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + (row.1 - line_height_f()) / 2.0), &l("移除所有导入进来的条目（来源为「导入」）"), &theme::serif(15.0, false), &theme::label(), None);
            hit_button(view, &mut hotspots, &l("移除"), CGPoint::new(row.0.x + col_w - 32.0 - 80.0, row.0.y + 10.0), Choice::RemoveImported, 80.0, false);
        }
    });
    }
    // Content height = 当前页的底部，换回内容坐标。
    view.ivars().content_h.set(y.max(y2) + scroll - HEADER_H);
    NSGraphicsContext::restoreGraphicsState_class();
    // Hotspots fully scrolled above the clip band must not answer clicks;
    // the header's Close lives above the band by design.
    hotspots.retain(|h| match h {
        Hot::Row(r, c) => matches!(c, Choice::Close) || r.max().y > HEADER_H,
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
    text(view, CGPoint::new(col_x + SHEET_PAD, *y), title, &font, &theme::label_secondary(), None);
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
    sheet_h(view, y, col_x, col_w, 28.0 + rows as f64 * 44.0, draw_rows);
}

/// Paper sheet whose height is stated outright — for content that does not sit
/// on the 44 pt row grid (`layout_picker` draws 56 pt cards). With the row
/// count alone the sheet came out 12 pt short and the cards hung out of it.
fn sheet_h(
    view: &SettingsView,
    y: &mut f64,
    col_x: f64,
    col_w: f64,
    h: f64,
    draw_rows: impl FnOnce(&SettingsView, f64),
) {
    let sheet_y = *y + 8.0;
    let rect = CGRect::new(CGPoint::new(col_x, sheet_y), CGSize::new(col_w, h));
    {
        NSGraphicsContext::saveGraphicsState_class();
        let s = crate::shelf::card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.35), 8.0, 1.0, 4.0);
        s.set();
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, theme::radius_control(), theme::radius_control());
        theme::draw_surface(&path, &theme::surface());
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
    text_color(_view, origin, s, font, color, spacing)
}

/// `text` with an explicit ink — the paused-photo row uses the muted tier.
fn text_color(
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
        theme::label().colorWithAlphaComponent(0.4).setStroke();
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
    theme::label().colorWithAlphaComponent(if disabled { 0.25 } else { 0.6 }).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    let color = theme::label().colorWithAlphaComponent(if disabled { 0.4 } else { 1.0 });
    crate::shelf::card::draw_line_centered(title, &font, &color, rect.min().x + 12.0, rect.mid().y, tw + 1.0);
    if !disabled {
        hotspots.push(Hot::Row(rect, choice));
    }
}

/// `hit_button` 最终画出来的宽度（`max(width, text + 24)`）——先量好再定
/// origin，按钮才不会往右长出纸卡。
fn button_w(s: &str) -> f64 {
    crate::shelf::card::measure(s, &objc2_app_kit::NSFont::systemFontOfSize(13.0), 400.0, None).width + 24.0
}

/// `tag` 的盒子宽度（text + 16），供右对齐排布使用。
fn tag_w(s: &str) -> f64 {
    crate::shelf::card::measure(s, &theme::serif(12.0, false), 200.0, None).width + 16.0
}

/// Perf badge: 已授权/未授权 and never a warning colour (Swift's `tag`).
fn tag(_view: &SettingsView, origin: CGPoint, text_s: &str, on: bool) {
    let font = theme::serif(12.0, false);
    let tw = crate::shelf::card::measure(text_s, &font, 200.0, None).width;
    let h = crate::shelf::card::line_height(&font) + 6.0;
    let rect = CGRect::new(origin, CGSize::new(tw + 16.0, h));
    let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, h / 2.0, h / 2.0);
    if on {
        theme::accent().setFill();
        capsule.fill();
    }
    theme::label().colorWithAlphaComponent(if on { 0.25 } else { 0.5 }).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    crate::shelf::card::draw_line_centered(text_s, &font, &theme::label(), rect.min().x + 8.0, rect.mid().y, tw + 1.0);
}

/// The actual switch rendering (independent so state never lags).
fn switch_control(view: &SettingsView, hotspots: &mut Vec<Hot>, origin: CGPoint, choice: Choice, on: bool, _state_view: &SettingsView) {
    let rect = CGRect::new(origin, CGSize::new(44.0, 24.0));
    let r = 12.0;
    let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, r, r);
    (if on { theme::accent() } else { theme::surface_alt() }).setFill();
    capsule.fill();
    theme::label().colorWithAlphaComponent(if on { 0.35 } else { 0.5 }).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    let knob_d = 20.0;
    let knob_x = if on { rect.max().x - 2.0 - knob_d } else { rect.min().x + 2.0 };
    let knob = NSBezierPath::bezierPathWithOvalInRect(CGRect::new(CGPoint::new(knob_x, rect.min().y + 2.0), CGSize::new(knob_d, knob_d)));
    {
        NSGraphicsContext::saveGraphicsState_class();
        let shadow = crate::shelf::card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.25), 1.5, 0.0, 1.0);
        shadow.set();
        theme::surface().setFill();
        knob.fill();
        NSGraphicsContext::restoreGraphicsState_class();
    }
    theme::label().colorWithAlphaComponent(0.45).setStroke();
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
            || matches!(choice, Choice::LineSize(step) if format!("{}%", step) == selected)
            || matches!(choice, Choice::LineGap(step) if gap_name(*step) == selected)
            || matches!(choice, Choice::Codec(hevc) if (*hevc) == (selected == "hevc"));
        if is_sel {
            let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                CGRect::new(CGPoint::new(rect.min().x + 1.0, rect.min().y + 3.0), CGSize::new(rect.size.width - 2.0, 20.0)),
                10.0,
                10.0,
            );
            theme::accent().setFill();
            capsule.fill();
        }
        let font = if is_sel { &font13b } else { &font13 };
        let tw = crate::shelf::card::measure(title, font, 400.0, None).width;
        crate::shelf::card::draw_line_centered(title, font, &theme::label(), rect.min().x + (rect.size.width - tw) / 2.0, rect.mid().y, tw + 1.0);
        hotspots.push(Hot::Row(rect, *choice));
        x += w + 4.0;
    }
    let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        CGRect::new(CGPoint::new(origin_right.x - total, origin_right.y - 2.0), CGSize::new(total, 30.0)),
        15.0,
        15.0,
    );
    theme::label().colorWithAlphaComponent(0.5).setStroke();
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
            theme::accent().setFill();
            capsule.fill();
        }
        let font = if sel == *days { &fonts.1 } else { &fonts.0 };
        let tw = crate::shelf::card::measure(title, font, 200.0, None).width;
        crate::shelf::card::draw_line_centered(title, font, &theme::label(), rect.min().x + (rect.size.width - tw) / 2.0, rect.mid().y, tw + 1.0);
        hotspots.push(Hot::Row(rect, Choice::Retention(*days)));
        x += w + 4.0;
    }
    let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        CGRect::new(CGPoint::new(origin_right.x - total, origin_right.y - 2.0), CGSize::new(total, 30.0)),
        15.0,
        15.0,
    );
    theme::label().colorWithAlphaComponent(0.5).setStroke();
    outline.setLineWidth(1.0);
    outline.stroke();
}

/// The 透明度 strip: 淡 / 中 / 浓 / 全亮, the current one in the accent.
fn dim_segments(_view: &SettingsView, hotspots: &mut Vec<Hot>, origin_right: CGPoint) {
    let cur = crate::app::background::dim_step();
    let fonts = (theme::serif(12.5, false), theme::serif(12.5, true));
    let opts: Vec<(String, i64)> = crate::app::background::DIM_STEPS
        .iter()
        .map(|(v, key)| (crate::app::localization::l_ctx(key, "veil"), *v))
        .collect();
    // 与 segment_choice / retention_segments 同一套基准：total 本身就包含两端
    // 12pt 内衬，所以起点和 outline 宽度都用 total。先前把 12 预放进 total、
    // 又在 outline 宽度里减掉一次，整个分段条整体左偏 12pt，描边还比内容短一截。
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
    for ((title, step), w) in opts.iter().zip(widths.iter()) {
        let rect = CGRect::new(CGPoint::new(x, origin_right.y), CGSize::new(*w, 26.0));
        if cur == *step {
            let capsule = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                CGRect::new(CGPoint::new(rect.min().x + 1.0, rect.min().y + 3.0), CGSize::new(rect.size.width - 2.0, 20.0)),
                10.0,
                10.0,
            );
            theme::accent().setFill();
            capsule.fill();
        }
        let font = if cur == *step { &fonts.1 } else { &fonts.0 };
        let tw = crate::shelf::card::measure(title, font, 200.0, None).width;
        crate::shelf::card::draw_line_centered(title, font, &theme::label(), rect.min().x + (rect.size.width - tw) / 2.0, rect.mid().y, tw + 1.0);
        hotspots.push(Hot::Row(rect, Choice::BackgroundOpacity(*step)));
        x += w + 4.0;
    }
    let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        CGRect::new(CGPoint::new(origin_right.x - total, origin_right.y - 2.0), CGSize::new(total, 30.0)),
        15.0,
        15.0,
    );
    theme::label().colorWithAlphaComponent(0.5).setStroke();
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
    theme::label().colorWithAlphaComponent(0.5).setStroke();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 13.0, 13.0).stroke();
    crate::shelf::card::draw_text(
        CGRect::new(CGPoint::new(rect.min().x + 12.0, rect.min().y + (26.0 - crate::shelf::card::line_height(&font)) / 2.0), CGSize::new(58.0, crate::shelf::card::line_height(&font))),
        &format!("{:02}:00", hour),
        &font,
        &theme::label().colorWithAlphaComponent(alpha),
        None,
    );
    for (dy, choice, name) in [
        (2.0, 99i64, "chevron.up"),
        (13.0, 98i64, "chevron.down"),
    ] {
        let r = CGRect::new(CGPoint::new(rect.max().x - 24.0, rect.min().y + dy), CGSize::new(18.0, 11.0));
        if let Some(img) = crate::shelf::card::symbol(name, 8.0, crate::shelf::card::SymbolWeight::Semibold, &theme::label_secondary()) {
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

/// The left-hand tab rail. The panel is wide but only ~420pt tall, so the
/// two long sheet columns became one page per tab; 外观 stays first because
/// that is where skin and background live.
fn tab_items() -> [(&'static str, String); 9] {
    [
        ("appearance", l("外观")),
        ("clipboard", l("剪贴板")),
        ("capture", l("截图与录屏")),
        ("location", l("位置")),
        ("import", l("导入")),
        ("updates", l("版本更新")),
        ("shortcuts", l("快捷键")),
        ("cleanup", l("清理")),
        ("system", l("系统")),
    ]
}

/// The skin picker as a 3-wide grid: nine skins with swatch, name and blurb
/// in three cards per row, so 外观 fits one screen in a ~420pt band.
fn skin_grid(view: &SettingsView, hotspots: &mut Vec<Hot>, col_x: f64, col_w: f64, sheet_y: f64) {
    let current = theme_skin::current_id();
    let cols = 3usize;
    let gap = 10.0;
    let row_h = 56.0;
    let cell_h = 52.0;
    let cell_w = (col_w - 32.0 - gap * (cols as f64 - 1.0)) / cols as f64;
    for (i, skin) in theme_skin::all().iter().enumerate() {
        let r = i / cols;
        let c = i % cols;
        let top = sheet_y + 28.0 + r as f64 * row_h;
        let x = col_x + SHEET_PAD + c as f64 * (cell_w + gap);
        let rect = CGRect::new(CGPoint::new(x, top), CGSize::new(cell_w, cell_h));
        let is_sel = skin.id == current;
        if is_sel {
            theme::accent_tint().setFill();
            theme::rounded_rect(rect, theme::radius_control()).fill();
        }
        let frame = theme::rounded_rect(rect, theme::radius_control());
        theme::separator().setStroke();
        frame.setLineWidth(1.0);
        frame.stroke();
        // Swatch: the skin's ground, the accent as a right-hand band, and a
        // mark in the ink that card would put on its accent.
        let (bg, accent_c, _ink, acc_ink) = theme_skin::swatch(skin);
        let sw = CGRect::new(
            CGPoint::new(rect.min().x + 8.0, rect.min().y + 8.0),
            CGSize::new(36.0, cell_h - 16.0),
        );
        let chip = theme::rounded_rect(sw, 4.0);
        bg.setFill();
        chip.fill();
        {
            NSGraphicsContext::saveGraphicsState_class();
            chip.addClip();
            accent_c.setFill();
            objc2_app_kit::NSRectFill(CGRect::new(
                CGPoint::new(sw.min().x + sw.size.width / 2.0, sw.min().y),
                CGSize::new(sw.size.width / 2.0, sw.size.height),
            ));
            acc_ink.setFill();
            objc2_app_kit::NSRectFill(CGRect::new(
                CGPoint::new(sw.min().x + 7.0, sw.mid().y - 2.0),
                CGSize::new(4.0, 4.0),
            ));
            NSGraphicsContext::restoreGraphicsState_class();
        }
        theme::separator().setStroke();
        chip.setLineWidth(1.0);
        chip.stroke();
        // Name and blurb, clipped to the card so a long English blurb cannot
        // spill into the neighbouring one.
        let text_x = sw.max().x + 8.0;
        let text_w = (rect.max().x - text_x - 8.0).max(0.0);
        let name_font = theme::serif(13.0, is_sel);
        let name_color = if is_sel { theme::accent() } else { theme::label() };
        NSGraphicsContext::saveGraphicsState_class();
        let body = CGRect::new(
            CGPoint::new(rect.min().x + 4.0, rect.min().y),
            CGSize::new(cell_w - 8.0, cell_h),
        );
        NSBezierPath::bezierPathWithRect(body).addClip();
        crate::shelf::card::draw_text(
            CGRect::new(CGPoint::new(text_x, rect.min().y + 7.0), CGSize::new(text_w, 17.0)),
            &l(skin.name),
            &name_font,
            &name_color,
            None,
        );
        crate::shelf::card::draw_text(
            CGRect::new(CGPoint::new(text_x, rect.min().y + 26.0), CGSize::new(text_w, 15.0)),
            &l(skin.blurb),
            &theme::serif(11.0, false),
            &theme::label_secondary(),
            None,
        );
        NSGraphicsContext::restoreGraphicsState_class();
        hotspots.push(Hot::Row(rect, Choice::Skin(skin.id)));
    }
    let _ = view;
}

/// The form picker: one card per layout (`layout_skin::all`), each with a
/// small schematic of the shelf's shape — a band at the bottom of the screen
/// for 面板, a strip along the top for 晾衣绳 — so the two forms read apart
/// without a screenshot.
fn layout_picker(view: &SettingsView, hotspots: &mut Vec<Hot>, col_x: f64, col_w: f64, sheet_y: f64) {
    let current = layout_skin::current_id();
    let cols = 2usize;
    let gap = 10.0;
    let cell_w = (col_w - 32.0 - gap * (cols as f64 - 1.0)) / cols as f64;
    let cell_h = 56.0;
    for (i, layout) in layout_skin::all().iter().enumerate() {
        let c = i % cols;
        let x = col_x + SHEET_PAD + c as f64 * (cell_w + gap);
        let rect = CGRect::new(CGPoint::new(x, sheet_y + 28.0), CGSize::new(cell_w, cell_h));
        let is_sel = layout.id == current;
        if is_sel {
            theme::accent_tint().setFill();
            theme::rounded_rect(rect, theme::radius_control()).fill();
        }
        let frame = theme::rounded_rect(rect, theme::radius_control());
        theme::separator().setStroke();
        frame.setLineWidth(1.0);
        frame.stroke();
        // Schematic: a mini screen with the shelf's band where it sits.
        let sw = CGRect::new(
            CGPoint::new(rect.min().x + 8.0, rect.min().y + 8.0),
            CGSize::new(56.0, cell_h - 16.0),
        );
        let screen = theme::rounded_rect(sw, 3.0);
        theme::surface_alt().setFill();
        screen.fill();
        theme::separator().setStroke();
        screen.setLineWidth(1.0);
        screen.stroke();
        {
            NSGraphicsContext::saveGraphicsState_class();
            screen.addClip();
            let band_h = sw.size.height * 0.34;
            // 晾衣绳 hangs from the top edge; 面板 rises from the bottom.
            let band_y = if layout.id == "line" { sw.max().y - band_h } else { sw.min().y };
            theme::accent().setFill();
            objc2_app_kit::NSRectFill(CGRect::new(
                CGPoint::new(sw.min().x, band_y),
                CGSize::new(sw.size.width, band_h),
            ));
            NSGraphicsContext::restoreGraphicsState_class();
        }
        // Name and blurb, clipped to the card.
        let text_x = sw.max().x + 8.0;
        let text_w = (rect.max().x - text_x - 8.0).max(0.0);
        let name_font = theme::serif(13.0, is_sel);
        let name_color = if is_sel { theme::accent() } else { theme::label() };
        NSGraphicsContext::saveGraphicsState_class();
        let body = CGRect::new(CGPoint::new(rect.min().x + 4.0, rect.min().y), CGSize::new(cell_w - 8.0, cell_h));
        NSBezierPath::bezierPathWithRect(body).addClip();
        crate::shelf::card::draw_text(
            CGRect::new(CGPoint::new(text_x, rect.min().y + 7.0), CGSize::new(text_w, 17.0)),
            &l(layout.name),
            &name_font,
            &name_color,
            None,
        );
        crate::shelf::card::draw_text(
            CGRect::new(CGPoint::new(text_x, rect.min().y + 26.0), CGSize::new(text_w, 15.0)),
            &l(layout.blurb),
            &theme::serif(11.0, false),
            &theme::label_secondary(),
            None,
        );
        NSGraphicsContext::restoreGraphicsState_class();
        hotspots.push(Hot::Row(rect, Choice::Layout(layout.id)));
    }
    let _ = view;
}

/// The 晾衣绳 group's header — names the form so it is obvious these four rows
/// only change the clothesline, not the panel drawer.
fn line_group_title() -> String {
    l("晾衣绳")
}

/// Store the new size / gap and make a live strip adopt it. A strip that is on
/// screen is rebuilt in place at the new metrics; a parked one only needs its
/// cached metrics dropped, which the next `relayout` does. The whole tree is
/// rebuilt because the strip's *height* and the document width both depend on
/// the card size, so a repaint alone would draw the wrong geometry.
fn line_settings_changed() {
    // The cached metrics are process-wide, so they must go whether or not a
    // strip happens to be on screen right now: the page is hosted by the panel
    // form (see `line_panel::switch_to_settings`), so `is_line()` is false the
    // whole time the user is looking at these rows. Leaving the reset to
    // `relayout` would make "the strip comes back at the new size" depend on
    // that call happening to drop the cache — and the whole point of the rows
    // is that the next strip adopts them.
    crate::shelf::line_card::metrics::reset();
    if !layout_skin::is_line() || !crate::shelf::line_panel::is_visible() {
        // Nothing on screen to rebuild: the strip picks the values up from
        // Preferences on its next `relayout`.
        return;
    }
    crate::shelf::line_panel::refresh_metrics();
}

/// Drive the 晾衣绳 metrics handler from a self-test. The pane's real rows
/// store through `metrics::store_*`, which drops the cache by itself — driving
/// those would hide whether the handler does. See `metrics::debug_write_size`.
pub fn debug_line_settings_changed() {
    line_settings_changed();
}

/// The 晾衣绳 display rows: 卡片大小 / 卡片间距 / 自动呼出 / 摆动.
///
/// All four are the strip's own — they do nothing to the panel because the two
/// forms are different products (see `line_card`'s metrics note). They are
/// always drawn (so the user can set them up before switching), and their
/// subtitles say plainly which form they affect.
fn line_settings(
    view: &SettingsView,
    hotspots: &mut Vec<Hot>,
    col_x: f64,
    col_w: f64,
    sheet_y: f64,
    line_on: bool,
) {
    // 卡片大小：100% / 120% / 150%（对应 line_card::metrics::SIZE_STEPS）。
    let row = sheet_row(sheet_y, col_x, col_w, 0);
    let sub = if line_on {
        l("照片挂在绳子上时的大小")
    } else {
        l("切到晾衣绳后生效")
    };
    text(view, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 6.0), &l("卡片大小"), &theme::serif(15.0, false), &theme::label(), None);
    text_color(view, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 24.0), &sub, &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
    let cur = crate::shelf::line_card::metrics::stored_size();
    // 三档尺寸标签是固定字面量，直接列出来，避免为分段控件动态造 &str。
    let size_opts: [(&str, &str, Choice); 3] = [
        ("100%", "100%", Choice::LineSize(100)),
        ("120%", "120%", Choice::LineSize(120)),
        ("150%", "150%", Choice::LineSize(150)),
    ];
    let cur_size = match cur {
        120 => "120%",
        150 => "150%",
        _ => "100%",
    };
    segment_choice(view, hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 9.0), &size_opts, cur_size);

    // 卡片间距：窄 / 标准 / 宽（24 / 48 / 80）。
    let row = sheet_row(sheet_y, col_x, col_w, 1);
    let sub = l("卡片之间的缝隙");
    text(view, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 6.0), &l("卡片间距"), &theme::serif(15.0, false), &theme::label(), None);
    text_color(view, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 24.0), &sub, &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
    let cur_gap = crate::shelf::line_card::metrics::stored_gap();
    let gap_labels = [l("窄"), l("标准"), l("宽")];
    let mut gap_opts: Vec<(&str, &str, Choice)> = Vec::new();
    for (i, step) in crate::shelf::line_card::metrics::GAP_STEPS.iter().enumerate() {
        gap_opts.push((gap_labels[i].as_str(), gap_labels[i].as_str(), Choice::LineGap(*step)));
    }
    let cur_gap_name = gap_name(cur_gap);
    segment_choice(view, hotspots, CGPoint::new(row.0.x + col_w - 32.0, row.0.y + 9.0), &gap_opts, &cur_gap_name);

    // 自动呼出：指针碰到菜单栏时自己滑出来。
    let row = sheet_row(sheet_y, col_x, col_w, 2);
    text(view, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 6.0), &l("自动呼出"), &theme::serif(15.0, false), &theme::label(), None);
    text_color(view, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 24.0), &l("指针碰到菜单栏时自己滑出来；关掉后只按快捷键或右键菜单呼出"), &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
    let auto_on = crate::shelf::line_panel::auto_reveal_enabled();
    switch_control(view, hotspots, CGPoint::new(row.0.x + col_w - 32.0 - 44.0, row.0.y + 10.0), Choice::LineAutoReveal(!auto_on), auto_on, view);

    // 摆动：照片在绳子上轻轻晃动。
    let row = sheet_row(sheet_y, col_x, col_w, 3);
    text(view, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 6.0), &l("摆动"), &theme::serif(15.0, false), &theme::label(), None);
    text_color(view, CGPoint::new(row.0.x + SHEET_PAD, row.0.y + 24.0), &l("照片挂在绳子上轻轻晃动；关掉就是静止的一排"), &theme::serif(12.0, false), &theme::label_secondary(), Some(1.0));
    let swing_on = crate::shelf::line_panel::swing_enabled();
    switch_control(view, hotspots, CGPoint::new(row.0.x + col_w - 32.0 - 44.0, row.0.y + 10.0), Choice::LineSwing(!swing_on), swing_on, view);
}

/// The 间距 labels in step order (24 / 48 / 80 → 窄 / 标准 / 宽).
fn gap_name(step: isize) -> String {
    match step {
        24 => l("窄"),
        80 => l("宽"),
        _ => l("标准"),
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
        theme::accent().setFill();
        capsule.fill();
    }
    theme::label().colorWithAlphaComponent(0.6).setStroke();
    capsule.setLineWidth(1.0);
    capsule.stroke();
    let color = if shortcut.is_set() || capturing { theme::label() } else { theme::label_secondary() };
    crate::shelf::card::draw_line_centered(&text, &font, &color, rect.min().x + 12.0, rect.mid().y, tw + 1.0);
    if show_clear {
        let clear_rect = CGRect::new(CGPoint::new(rect.max().x - 16.0, rect.min().y + 5.0), CGSize::new(16.0, 16.0));
        if let Some(img) = crate::shelf::card::symbol("xmark.circle.fill", 13.0, crate::shelf::card::SymbolWeight::Regular, &theme::label_secondary()) {
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
            &theme::label_secondary(),
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
        Choice::Close => crate::shelf::line_panel::close_settings(),
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
            // Every string in the tree was resolved when it was built, so the
            // tree is rebuilt rather than repainted. Queued for the same reason
            // as Skin above: the rebuild releases this very view.
            queue_panel_command(PanelCommand::Relanguage);
        }
        Choice::ScreenRec => permissions::open_settings("Privacy_ScreenCapture"),
        Choice::Restart => permissions::relaunch(),
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
                        if url.path().is_some() {
                            preferences::Preferences::shared().set_custom_export_dir(&url);
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
        Choice::Tab(id) => {
            // 切页时回到顶部：上一页滚到的位置在新页里没有意义。
            view.ivars().tab.set(id);
            view.ivars().scroll.set(0.0);
            view.setNeedsDisplay(true);
        }
        Choice::Skin(id) => {
            if theme_skin::select(id) {
                theme_skin::store(id);
                // Mutual exclusion: the skin's colour scheme owns the panel;
                // the remembered photo pauses until 启用 or the next pick.
                preferences::Preferences::shared().set_panel_background_enabled(false);
                // Every view baked in the old skin's faces and radii when it
                // was built, so the tree is rebuilt rather than repainted —
                // but not from inside this method. The rebuild replaces the
                // panel's content view, which deallocates the whole old tree
                // including the `SettingsView` whose `mouseUp:` is still on
                // the stack (and the `choice` being dispatched with it).
                // Queue it; `view::apply_panel_commands` runs it on the next
                // main-queue turn, after this event has returned.
                queue_panel_command(PanelCommand::Reskin);
            }
        }
        Choice::Layout(id) => {
            if layout_skin::select(id) {
                layout_skin::store(id);
                // The user has just made an explicit choice about their shelf;
                // a page that was opened from the strip must not also switch on
                // the way out and override it (`line_panel::close_settings`).
                crate::shelf::line_panel::forget_return_to_line();
                // Same deferral as a skin switch: the rebuild replaces the very
                // view dispatching this click, so queue it (see PanelCommand).
                queue_panel_command(PanelCommand::Relayout);
            }
        }
        Choice::ChooseBackground => {
            let picked = crate::shelf::panel::with_dialog(|| {
                let mtm = MainThreadMarker::new().expect("main thread");
                let panel = objc2_app_kit::NSOpenPanel::openPanel(mtm);
                panel.setCanChooseDirectories(false);
                panel.setCanChooseFiles(true);
                panel.setAllowsMultipleSelection(false);
                panel.setPrompt(Some(&NSString::from_str(&l("选择"))));
                panel.setMessage(Some(&NSString::from_str(&l("选一张照片铺在内容区，皮肤配色会让位"))));
                if panel.runModal() == objc2_app_kit::NSModalResponseOK {
                    panel.URL().map(|u| u.clone())
                } else {
                    None
                }
            });
            if let Some(url) = picked {
                preferences::Preferences::shared().set_panel_background(&url);
                crate::app::background::invalidate();
                // The pick resumes the photo over the skin (mutual exclusion).
                crate::shelf::view::refresh();
            }
        }
        Choice::ClearBackground => {
            preferences::Preferences::shared().clear_panel_background();
            crate::app::background::invalidate();
        }
        Choice::EnableBackground => {
            // Resumes the remembered photo; the skin's colours yield.
            let p = preferences::Preferences::shared();
            let next = !p.panel_background_enabled();
            p.set_panel_background_enabled(next);
        }
        Choice::BackgroundOpacity(step) => {
            preferences::Preferences::shared().set_panel_background_dim(step as f64 / 10.0);
        }
        Choice::LineSize(step) => {
            crate::shelf::line_card::metrics::store_size(step);
            line_settings_changed();
        }
        Choice::LineGap(step) => {
            crate::shelf::line_card::metrics::store_gap(step);
            line_settings_changed();
        }
        Choice::LineAutoReveal(on) => {
            crate::shelf::line_panel::store_auto_reveal(on);
        }
        Choice::LineSwing(on) => {
            crate::shelf::line_panel::store_swing(on);
        }
    }
    if !matches!(choice, Choice::Close | Choice::ImportDb | Choice::CheckUpdates | Choice::RemoveImported | Choice::Language(_) | Choice::Skin(_) | Choice::Layout(_) | Choice::Tab(_) | Choice::BackgroundOpacity(_) | Choice::EnableBackground | Choice::LineAutoReveal(_) | Choice::LineSwing(_)) {
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
                let mut info = l("来自 %@。已经在 PasteJar 里的内容会自动跳过，Pin 会保留；导入的内容排在 PasteJar 自己记录的后面。")
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

/// Self-test hook: every click target the pane currently holds, so a test can
/// press each one through the real `mouseUp:` route.
pub fn selftest_hotspots(view: &SettingsView) -> Vec<(Choice, CGPoint)> {
    view.ivars()
        .hotspots
        .borrow()
        .iter()
        .filter_map(|h| match h {
            Hot::Row(rect, c) => Some((*c, rect.mid())),
            _ => None,
        })
        .collect()
}

/// Whether clicking `c` would block the process in a modal system dialog or
/// touch state a self-test has no business touching (login items, the user's
/// real permissions panes). The exhaustive click test skips these; every other
/// control is fair game.
pub fn selftest_blocks(c: Choice) -> bool {
    matches!(
        c,
        Choice::ChooseBackground
            | Choice::EnableBackground
            | Choice::ChooseExport
            | Choice::Retention(_)
            | Choice::ImportDb
            | Choice::RemoveImported
            | Choice::CheckUpdates
            | Choice::GrantAX
            | Choice::ScreenRec
            | Choice::LaunchAtLogin
            | Choice::OpenStore
            | Choice::Close
    )
}

/// Which tab the pane is showing (`tab_items` id).
pub fn current_tab(view: &SettingsView) -> &'static str {
    view.ivars().tab.get()
}

/// Move the pane to `id`. A rebuilt tree starts on 外观 again, so whoever
/// rebuilds it carries the tab the user was on across (`view::apply_panel_commands`).
pub fn set_tab(view: &SettingsView, id: &'static str) {
    view.ivars().tab.set(id);
    view.setNeedsDisplay(true);
}

/// Self-test hook: which tab the pane is showing (`tab_items` id).
pub fn selftest_tab(view: &SettingsView) -> &'static str {
    current_tab(view)
}

/// Self-test hook: the centre of the hotspot that would run `choice`, in this
/// view's own (flipped) coordinates, so a test can aim a real click at it.
/// `None` until the pane has drawn once — the hotspots live in drawRect.
pub fn selftest_hotspot_center(view: &SettingsView, choice: Choice) -> Option<CGPoint> {
    view.ivars().hotspots.borrow().iter().find_map(|h| match h {
        Hot::Row(rect, c) if *c == choice => Some(rect.mid()),
        _ => None,
    })
}

/// Self-test hook: deliver a click at `p` (this view's coordinates) through
/// the real `mouseUp:` route, so hit-testing, `run_choice` and whatever that
/// schedules all run exactly as they do for a user's click — the route the
/// offscreen renders and `skinlive` cannot reach.
pub fn selftest_click_at(view: &SettingsView, p: CGPoint) -> bool {
    let Some(window) = view.window() else {
        return false;
    };
    let loc = view.convertPoint_toView(p, None);
    let number = window.windowNumber();
    let event = |kind| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            loc,
            NSEventModifierFlags::empty(),
            0.0,
            number,
            None,
            0,
            1,
            0.0,
        )
        .expect("synthetic mouse event")
    };
    // SAFETY: `SettingsView` is an NSView subclass; this is the same dispatch
    // AppKit performs for a real click in the pane.
    let obj: &AnyObject = unsafe { &*(view as *const SettingsView as *const AnyObject) };
    unsafe {
        let up = event(NSEventType::LeftMouseUp);
        let _: () = msg_send![obj, mouseUp: &*up];
    }
    true
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
                view.set_notice(which, Some(l("已被 PasteJar 的「%@」占用，换一个").replacen("%@", &l(label), 1)));
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

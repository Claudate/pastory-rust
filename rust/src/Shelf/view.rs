//! Port of `Shelf/ShelfView.swift` — the shelf's whole view tree, hand-laid
//! out with AppKit views (fixed geometry; no Auto Layout), so the render
//! lands on the SwiftUI values pixel for pixel.
//!
//! Tree: `ShelfRootView` (desk + rounded top corners, sidebar ground) with
//! `SidebarView`, `HeaderView` (tabs / search / close), an `NSScrollView`
//! holding `CardsRowView` (one draw pass for every card), and `FooterView`
//! (the paper scrollbar). Interactions dispatch through a main-thread UI
//! state block; model access goes through the panel controller.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSEvent, NSGraphicsContext, NSMenu, NSMenuItem, NSProgressIndicator,
    NSScrollView, NSTextField, NSTextView, NSView, NSControlSize, NSProgressIndicatorStyle,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSMutableAttributedString, NSNotification, NSString};

use crate::app::localization::l;
use crate::app::theme;
use crate::clipboard::item::{ClipItem, ClipKind};
use crate::shelf::card::{self, CardData, SymbolWeight};
use crate::shelf::model::{ShelfFilter, ShelfModel};

/// Height of the cards band inside the root (welcome card fills it).
fn scroll_h(size: CGSize) -> f64 {
    (size.height - HEADER_H - FOOTER_H).max(1.0)
}

// MARK: Layout constants (ShelfView.swift verbatim values)

const SIDEBAR_W: f64 = 162.0;
const PAD_H: f64 = 20.0;
const HEADER_H: f64 = 60.0;
const FOOTER_H: f64 = 34.0;
const BRAND_SIZE: f64 = 42.0;
/// Cards inside the scroll view: spacing 16, top 16, bottom 6, sides 4.
const CARD_SPACING: f64 = 16.0;
const CARD_TOP: f64 = 16.0;
const CARD_BOTTOM: f64 = 6.0;
const CARD_SIDE: f64 = 4.0;

fn nav_rows() -> [(&'static str, &'static str); 3] {
    [
        ("clipboard", "剪贴板"),
        ("gearshape", "设置"),
        ("hand.wave", "联系我"),
    ]
}

/// Shared UI state (main thread; the shelf exists at most once).
pub(crate) struct Ui {
    built: bool,
    root: Option<Retained<ShelfRootView>>,
    sidebar: Option<Retained<SidebarView>>,
    header: Option<Retained<HeaderView>>,
    scroll: Option<Retained<NSScrollView>>,
    row: Option<Retained<CardsRowView>>,
    footer: Option<Retained<FooterView>>,
    search: Option<Retained<NSTextField>>,
    search_delegate: Option<Retained<SearchDelegate>>,
    rename: Option<Retained<NSTextField>>,
    rename_delegate: Option<Retained<RenameDelegate>>,
    cards: Vec<CardData>,
    /// The pane view while 设置 is up (M6): drawn over the content column,
    /// cards/header/footer hidden behind it (the pane reads keyboard ⎋/⌘F
    /// through the panel's `handle`).
    settings_view: Option<Retained<crate::shelf::settings_pane::SettingsView>>,
    contact_view: Option<Retained<crate::shelf::contact_pane::ContactView>>,
    welcome_view: Option<Retained<crate::shelf::welcome_card::WelcomeCardView>>,
    /// Footer scrollbar state, refreshed from the clip view each reload.
    scroll_fraction: f64,
    scroll_visible: f64,
    scroll_range: f64,
    last_selected: Option<String>,
    last_open_tick: u64,
    last_focus_search: u64,
    last_renaming: Option<String>,
    rename_for: Option<String>,
    spinner: Option<Retained<NSProgressIndicator>>,
}

impl Ui {
    fn new() -> Ui {
        Ui {
            built: false,
            root: None,
            sidebar: None,
            header: None,
            scroll: None,
            row: None,
            footer: None,
            search: None,
            search_delegate: None,
            rename: None,
            rename_delegate: None,
            cards: Vec::new(),
            settings_view: None,
            contact_view: None,
            welcome_view: None,
            scroll_fraction: 0.0,
            scroll_visible: 1.0,
            scroll_range: 0.0,
            last_selected: None,
            last_open_tick: u64::MAX,
            last_focus_search: u64::MAX,
            last_renaming: None,
            rename_for: None,
            spinner: None,
        }
    }
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::new());
}

pub(crate) fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(&mut u.borrow_mut()))
}

/// Drop every child reference (fresh tree for the next host).
pub(crate) fn reset_ui() {
    with_ui(|ui| *ui = Ui::new());
}

/// The pane view while 设置 is up (M6; the badge poll + updater note hook it).
pub(crate) fn settings_view() -> Option<Retained<crate::shelf::settings_pane::SettingsView>> {
    with_ui(|ui| ui.settings_view.clone())
}

/// The welcome card while the first launch shows it (its shortcut recorders).
pub(crate) fn welcome_view() -> Option<Retained<crate::shelf::welcome_card::WelcomeCardView>> {
    with_ui(|ui| ui.welcome_view.clone())
}

/// Show/hide the settings pane over the content column. An open welcome card
/// or pane exchange takes precedence; cards/header/footer hide behind the pane.
fn sync_settings_view(ui: &mut Ui, s: &Snapshot) {
    let Some(root) = ui.root.clone() else { return };
    let show = s.show_settings;
    if show {
        if ui.settings_view.is_none() {
            let rect = content_rect(root.bounds().size);
            let pane = crate::shelf::settings_pane::SettingsView::make(CGRect::new(
                rect.min(),
                CGSize::new(rect.size.width, rect.size.height),
            ));
            pane.setFrame(CGRect::new(
                rect.min(),
                CGSize::new(rect.size.width, rect.size.height),
            ));
            root.addSubview(&pane);
            ui.settings_view = Some(pane);
        }
    } else if let Some(pane) = ui.settings_view.take() {
        pane.removeFromSuperview();
    }
    // 联系我 pane (same over-the-content-column shape).
    if s.show_contact {
        if ui.contact_view.is_none() {
            let rect = content_rect(root.bounds().size);
            let pane = crate::shelf::contact_pane::ContactView::make(CGRect::new(
                rect.min(),
                CGSize::new(rect.size.width, rect.size.height),
            ));
            root.addSubview(&pane);
            ui.contact_view = Some(pane);
        }
    } else if let Some(pane) = ui.contact_view.take() {
        pane.removeFromSuperview();
    }
    // 歓迎カード: shows as the first card (tallest fitting card slot the
    // shelf's row shapes like any card; the welcome's own height = card row).
    if s.show_welcome && !show && !s.show_contact {
        if ui.welcome_view.is_none() {
            let welcome_h = scroll_h(root.bounds().size);
            let w = if crate::app::localization::is_english() { 560.0 } else { 480.0 };
            let rect = CGRect::new(
                CGPoint::new(content_rect(root.bounds().size).min().x + 8.0, 0.0),
                CGSize::new(w, welcome_h),
            );
            let v = crate::shelf::welcome_card::WelcomeCardView::make(rect);
            root.addSubview(&v);
            ui.welcome_view = Some(v);
        }
    } else if let Some(v) = ui.welcome_view.take() {
        v.removeFromSuperview();
    }
    let pane_open = show || s.show_contact;
    if let Some(scroll) = &ui.scroll { scroll.setHidden(pane_open); }
    if let Some(footer) = &ui.footer { footer.setHidden(pane_open); }
    if let Some(search) = &ui.search { search.setHidden(pane_open); }
}

/// The card snapshot for hit-testing outside draws.
pub(crate) fn cards() -> Vec<String> {
    with_ui(|ui| ui.cards.iter().map(|c| c.item.id.clone()).collect())
}

// MARK: Model access (the shelf UI always talks to the shared controller)

fn with_model<R>(f: impl FnOnce(&ShelfModel) -> R) -> R {
    crate::shelf::panel::with_model(f)
}

fn with_model_mut<R>(f: impl FnOnce(&mut ShelfModel) -> R) -> R {
    crate::shelf::panel::with_model_mut(f)
}

/// Pull the whole drawable state out of the model. Warms thumbnails as a
/// side effect (they land in later reloads, like SwiftUI's `thumbTick`).
fn reload() {
    let snapshot = with_model_mut(|m| {
        let items = m.items();
        let on_clipboard = m.with_store(|s| s.items.first().map(|it| it.id.clone()));
        let mut cards: Vec<CardData> = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let thumb = if item.kind == ClipKind::Image || item.kind == ClipKind::Video {
                m.thumbnail(item)
            } else {
                None
            };
            cards.push(CardData {
                item: item.clone(),
                index,
                selected: m.selected_id() == Some(item.id.as_str()),
                on_clipboard: on_clipboard.as_deref() == Some(item.id.as_str()),
                renaming: m.renaming_id.as_deref() == Some(item.id.as_str()),
                on_desktop: crate::shelf::desktop_notes::is_on_desktop(&item.id),
                thumb,
            });
        }
        Snapshot {
            cards,
            counts: m.counts(),
            filter: m.filter(),
            query: m.query().to_string(),
            searching: m.is_searching(),
            selected: m.selected_id().map(|s| s.to_string()),
            renaming: m.renaming_id.clone(),
            open_tick: m.open_tick,
            focus_search: m.focus_search,
            show_settings: m.show_settings,
            show_contact: m.show_contact,
            show_welcome: m.show_welcome,
        }
    });
    apply_snapshot(snapshot);
}

struct Snapshot {
    cards: Vec<CardData>,
    counts: crate::shelf::model::FilterCounts,
    filter: ShelfFilter,
    query: String,
    searching: bool,
    selected: Option<String>,
    renaming: Option<String>,
    open_tick: u64,
    focus_search: u64,
    show_settings: bool,
    show_contact: bool,
    show_welcome: bool,
}

/// The geometry of the content column (everything right of the sidebar).
fn content_rect(size: CGSize) -> CGRect {
    CGRect::new(
        CGPoint::new(SIDEBAR_W + PAD_H, 0.0),
        CGSize::new((size.width - SIDEBAR_W - 2.0 * PAD_H).max(0.0), size.height),
    )
}

/// Reposition the doc view after a reload: width = card row, height = clip.
fn layout_row(ui: &Ui) {
    let (Some(scroll), Some(row)) = (&ui.scroll, &ui.row) else { return };
    let n = ui.cards.len();
    let clip = scroll.contentView();
    let clip_size = clip.bounds().size;
    let w = (CARD_SIDE * 2.0 + n as f64 * card::CARD_W + n.saturating_sub(1) as f64 * CARD_SPACING)
        .max(clip_size.width)
        .max(1.0);
    row.setFrame(CGRect::new(
        CGPoint::ZERO,
        CGSize::new(w, clip_size.height.max(1.0)),
    ));
}

/// Apply a model snapshot to the views (the SwiftUI body re-evaluation).
fn apply_snapshot(mut s: Snapshot) {
    with_ui(|ui| {
        // Rename bookkeeping used to commit before switching away.
        let switched_card = match (&ui.last_renaming, &s.renaming) {
            (Some(a), b) if Some(a) != b.as_ref() && b.is_some() => Some(a.clone()),
            _ => None,
        };
        if switched_card.is_some() {
            commit_rename(ui);
        }
        ui.last_renaming = s.renaming.clone();
        ui.cards = std::mem::take(&mut s.cards);
        layout_row(ui);
        // Sync the search text + clear button (unless it already matches).
        if let Some(field) = &ui.search {
            if field.stringValue().to_string() != s.query {
                field.setStringValue(&NSString::from_str(&s.query));
            }
        }
        if let Some(header) = &ui.header {
            header.set_tab_state(s.filter, s.counts, s.query.clone(), s.show_settings, s.show_contact);
        }
        if let Some(sidebar) = &ui.sidebar {
            sidebar.set_active(nav_index(&s));
        }
        // Scroll state for the footer.
        if let Some(scroll) = &ui.scroll {
            let clip = scroll.contentView();
            let bounds = clip.bounds();
            let content_w = scroll.documentView().map(|d| d.frame().size.width).unwrap_or(0.0);
            let range = (content_w - bounds.size.width).max(0.0);
            ui.scroll_range = range;
            ui.scroll_visible = (bounds.size.width / content_w.max(1.0)).min(1.0);
            ui.scroll_fraction = if range > 0.0 {
                (bounds.origin.x / range).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
        // Selection moved: center it (0.1s easeOut, like the SwiftUI proxy).
        if s.selected != ui.last_selected {
            ui.last_selected = s.selected.clone();
            if let Some(sel) = &s.selected {
                center_card(ui, sel, true);
            }
        }
        // Rename box placement.
        sync_rename_field(ui, &s);
        // Focus events: open tick drops focus; focus_search grabs it.
        if s.open_tick != ui.last_open_tick {
            ui.last_open_tick = s.open_tick;
            if let Some(root) = &ui.root {
                if let Some(w) = root.window() {
                    w.makeFirstResponder(None);
                }
            }
        }
        if s.focus_search != ui.last_focus_search {
            ui.last_focus_search = s.focus_search;
            focus_search_field(ui);
        }
        // Empty state.
        sync_empty(ui, &s);
        sync_settings_view(ui, &s);
        mark_dirty(ui);
    });
}

fn mark_dirty(ui: &Ui) {
    if let Some(v) = &ui.root {
        v.setNeedsDisplay(true);
    }
    if let Some(v) = &ui.sidebar {
        v.setNeedsDisplay(true);
    }
    if let Some(v) = &ui.header {
        v.setNeedsDisplay(true);
    }
    if let Some(r) = &ui.row {
        r.setNeedsDisplay(true);
    }
    if let Some(f) = &ui.footer {
        f.setNeedsDisplay(true);
    }
}

/// Which nav row reads active (settings / contact are M6 panes; the rows
/// themselves are live).
fn nav_index(s: &Snapshot) -> usize {
    if s.show_settings {
        1
    } else if s.show_contact {
        2
    } else {
        0
    }
}

/// Scroll the clip so the card centers (animated when the shelf is live).
fn center_card(ui: &Ui, id: &str, animated: bool) {
    let (Some(scroll), Some(row)) = (&ui.scroll, &ui.row) else { return };
    let Some(i) = ui.cards.iter().position(|c| c.item.id == id) else { return };
    let ticket = ticket_rect(row.bounds().size.height, i, ui.cards[i].selected);
    let clip = scroll.contentView();
    let mut x = ticket.mid().x - clip.bounds().size.width / 2.0;
    let range = (row.bounds().size.width - clip.bounds().size.width).max(0.0);
    x = x.clamp(0.0, range);
    let origin = CGPoint::new(x, 0.0);
    // Swift runs a 0.1s easeOut here (`withAnimation` + `proxy.scrollTo`);
    // the terminal state is what every snapshot and the next frame need, so
    // the scroll lands directly (the blend is a nicety, not layout). The
    // `animated` flag stays for the interactive pass to reintroduce if the
    // illusion is ever missed.
    let _ = animated;
    clip.setBoundsOrigin(origin);
    if let Some(f) = &ui.footer {
        f.setNeedsDisplay(true);
    }
}

/// The search field gets the keyboard; pending keystrokes land right after
/// (Swift appends `pendingQuery` post-focus so select-all cannot eat it).
fn focus_search_field(ui: &mut Ui) {
    let (Some(root), Some(field)) = (&ui.root, &ui.search) else { return };
    let Some(window) = root.window() else { return };
    window.makeFirstResponder(Some(field));
    crate::app::delegate::dispatch_main_after(0.0, Box::new(|| {
        let typed = crate::shelf::panel::with_model_mut(|m| {
            let t = m.pending_query.clone();
            m.pending_query.clear();
            if !t.is_empty() {
                let mut q = m.query().to_string();
                q.push_str(&t);
                m.set_query(q);
            }
            t
        });
        if !typed.is_empty() {
            reload();
        }
    }));
}

/// Show / move / commit the inline title editor.
fn sync_rename_field(ui: &mut Ui, s: &Snapshot) {
    let Some(row) = ui.row.clone() else { return };
    match &s.renaming {
        Some(id) if ui.cards.iter().any(|c| c.item.id == *id) => {
            if ui.rename.is_none() {
                let mtm = MainThreadMarker::new().expect("main thread");
                let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), CGRect::ZERO);
                field.setBezeled(false);
                field.setDrawsBackground(false);
                field.setFocusRingType(objc2_app_kit::NSFocusRingType(0)); // none
                field.setFont(Some(&theme::script(20.0)));
                field.setTextColor(Some(&theme::ink()));
                field.setEditable(true);
                field.setSelectable(true);
                let delegate = ui
                    .rename_delegate
                    .get_or_insert_with(|| RenameDelegate::new(mtm));
                unsafe {
                    // SAFETY: RenameDelegate implements the informal
                    // NSTextField delegate selectors.
                    let d: Option<&AnyObject> = Some(&*(&**delegate as *const RenameDelegate as *const AnyObject));
                    let _: () = msg_send![&field, setDelegate: d];
                    field.setTarget(d);
                    let _: () = msg_send![&field, setAction: sel!(renameDone:)];
                }
                ui.rename = Some(field);
            }
            let field = ui.rename.clone().unwrap();
            let i = ui.cards.iter().position(|c| c.item.id == *id).unwrap();
            let ticket = ticket_rect(row.bounds().size.height, i, ui.cards[i].selected);
            let title_row = card::title_row_rect(ticket);
            // Leave room for the checkmark button on the right.
            let frame = CGRect::new(
                CGPoint::new(title_row.min().x + card::PAD_X - 2.0, title_row.min().y + 4.0 - 2.0),
                CGSize::new(card::CARD_W - 2.0 * card::PAD_X - 8.0 - 20.0, 30.0),
            );
            field.setFrame(frame);
            if unsafe { field.superview() }.is_none() {
                row.addSubview(&field);
            }
            // Fresh target card: seed the draft.
            if ui.rename_for.as_deref() != Some(id.as_str()) {
                ui.rename_for = Some(id.clone());
                let title = ui.cards[i].item.title.clone().unwrap_or_default();
                field.setStringValue(&NSString::from_str(&title));
                if let Some(w) = row.window() {
                    w.makeFirstResponder(Some(&field));
                }
            }
        }
        _ => {
            if let Some(field) = ui.rename.take() {
                field.removeFromSuperview();
            }
            ui.rename_for = None;
        }
    }
}

/// Leaving the box, by any route, is the save. An unchanged draft writes
/// nothing (Swift `onDisappear`).
fn commit_rename(ui: &mut Ui) {
    let Some(id) = ui.rename_for.clone() else { return };
    let Some(field) = ui.rename.clone() else { return };
    let draft = field.stringValue().to_string();
    let trimmed = draft.trim().to_string();
    crate::shelf::panel::with_model_mut(|m| {
        let unchanged = m
            .items()
            .iter()
            .find(|it| it.id == id)
            .map(|it| it.title.clone().unwrap_or_default() == trimmed)
            .unwrap_or(true);
        if unchanged {
            return;
        }
        m.with_store_mut(|s| s.set_title((!trimmed.is_empty()).then_some(trimmed.as_str()), &id));
    });
}

/// End the inline rename from any route (commit first), mirroring the Swift
/// disappear-save.
pub(crate) fn end_rename() {
    with_ui(|ui| {
        if ui.last_renaming.is_none() {
            return;
        }
        commit_rename(ui);
        with_model_mut(|m| m.renaming_id = None);
    });
    reload();
}

/// The empty-state overlay (search spinner / glyphs + hint text).
fn sync_empty(ui: &mut Ui, s: &Snapshot) {
    let Some(root) = ui.root.clone() else { return };
    let show = ui.cards.is_empty()
        && !s.show_welcome
        && !s.show_settings
        && !s.show_contact;
    root.set_empty(match (show, s.searching, s.query.is_empty()) {
        (true, true, _) => Some(EmptyState::Searching),
        (true, _, true) => Some(EmptyState::NoClips),
        (true, _, false) => Some(EmptyState::NoMatches),
        _ => None,
    });
    if show && s.searching && ui.spinner.is_none() {
        let mtm = MainThreadMarker::new().expect("main thread");
        let sp = NSProgressIndicator::initWithFrame(
            NSProgressIndicator::alloc(mtm),
            CGRect::new(CGPoint::ZERO, CGSize::new(16.0, 16.0)),
        );
        sp.setControlSize(NSControlSize::Small);
        sp.setStyle(NSProgressIndicatorStyle::Spinning);
        sp.setDisplayedWhenStopped(false);
        root.addSubview(&sp);
        ui.spinner = Some(sp);
    }
    if let Some(sp) = &ui.spinner {
        if show && s.searching {
            let r = root.bounds();
            let area = CGRect::new(
                CGPoint::new(SIDEBAR_W + PAD_H, HEADER_H),
                CGSize::new(r.size.width - SIDEBAR_W - 2.0 * PAD_H, r.size.height - HEADER_H - FOOTER_H),
            );
            sp.setFrame(CGRect::new(
                CGPoint::new(area.mid().x - 8.0, area.mid().y - 18.0),
                CGSize::new(16.0, 16.0),
            ));
            unsafe { sp.startAnimation(None) };
        } else {
            sp.removeFromSuperview();
            ui.spinner = None;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EmptyState {
    Searching,
    NoClips,
    NoMatches,
}

/// Card geometry inside the doc view (the Swift LazyHStack row).
fn ticket_rect(row_h: f64, index: usize, selected: bool) -> CGRect {
    let h = (row_h - CARD_TOP - CARD_BOTTOM).max(1.0);
    let x = CARD_SIDE + index as f64 * (card::CARD_W + CARD_SPACING);
    let y = CARD_TOP + if selected { -6.0 } else { 0.0 };
    CGRect::new(CGPoint::new(x, y), CGSize::new(card::CARD_W, h))
}

// MARK: Build

/// Build the whole tree for a host of `size`; returns the root view. The
/// same object drives the real panel (host = content view) and the render
/// self-tests.
pub fn build(mtm: MainThreadMarker, size: CGSize) -> Retained<ShelfRootView> {
    reset_ui();
    let root = ShelfRootView::new(mtm, CGRect::new(CGPoint::ZERO, size));
    let content = content_rect(size);
    let sidebar = SidebarView::new(mtm, CGRect::new(CGPoint::ZERO, CGSize::new(SIDEBAR_W, size.height)));
    root.addSubview(&sidebar);
    let header = HeaderView::new(mtm, CGRect::new(content.min(), CGSize::new(content.size.width, HEADER_H)));
    root.addSubview(&header);
    // Search field inside the header.
    let search = make_search_field(mtm);
    header.addSubview(&search);
    let scroll_rect = CGRect::new(
        CGPoint::new(content.min().x, HEADER_H),
        CGSize::new(content.size.width, (size.height - HEADER_H - FOOTER_H).max(1.0)),
    );
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), scroll_rect);
    scroll.setHasHorizontalScroller(false);
    scroll.setHasVerticalScroller(false);
    scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
    scroll.setDrawsBackground(false);
    scroll.setAutohidesScrollers(true);
    scroll.contentView().setDrawsBackground(false);
    let row = CardsRowView::new(mtm, CGRect::new(CGPoint::ZERO, scroll_rect.size));
    scroll.setDocumentView(Some(&row));
    root.addSubview(&scroll);
    let footer = FooterView::new(
        mtm,
        CGRect::new(
            CGPoint::new(content.min().x, size.height - FOOTER_H),
            CGSize::new(content.size.width, FOOTER_H),
        ),
    );
    root.addSubview(&footer);
    with_ui(|ui| {
        ui.built = true;
        ui.root = Some(root.clone());
        ui.sidebar = Some(sidebar);
        ui.header = Some(header);
        ui.scroll = Some(scroll);
        ui.row = Some(row);
        ui.footer = Some(footer);
        ui.search = Some(search);
    });
    reload();
    root
}

fn make_search_field(mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), CGRect::ZERO);
    field.setBezeled(false);
    field.setDrawsBackground(false);
    field.setFocusRingType(objc2_app_kit::NSFocusRingType(0)); // none
    field.setFont(Some(&theme::serif(15.0, false)));
    field.setTextColor(Some(&theme::on_brown()));
    field.setEditable(true);
    field.setSelectable(true);
    update_placeholder(&field, true);
    let delegate = SearchDelegate::new(mtm);
    unsafe {
        // SAFETY: SearchDelegate implements the informal NSTextField
        // delegate selectors.
        let d: Option<&AnyObject> = Some(&*(&*delegate as *const SearchDelegate as *const AnyObject));
        let _: () = msg_send![&field, setDelegate: d];
    }
    with_ui(|ui| ui.search_delegate = Some(delegate));
    field
}

/// SwiftUI draws the placeholder only when the field is empty AND unfocused.
fn update_placeholder(field: &NSTextField, visible: bool) {
    if !visible {
        field.setPlaceholderAttributedString(None);
        return;
    }
    let s = NSMutableAttributedString::initWithString(
        NSMutableAttributedString::alloc(),
        &NSString::from_str(&l("搜索剪贴板")),
    );
    let range = objc2_foundation::NSRange { location: 0, length: s.length() };
    let font = theme::serif(15.0, false);
    // SAFETY: same-class upcasts for the erased attribute value slots.
    let font_obj = unsafe { &*(objc2::rc::Retained::as_ptr(&font) as *const AnyObject) };
    let color = theme::on_brown_muted();
    let color_obj = unsafe { &*(objc2::rc::Retained::as_ptr(&color) as *const AnyObject) };
    unsafe {
        s.addAttribute_value_range(objc2_app_kit::NSFontAttributeName, font_obj, range);
        s.addAttribute_value_range(objc2_app_kit::NSForegroundColorAttributeName, color_obj, range);
    }
    field.setPlaceholderAttributedString(Some(&s));
}

// MARK: Root view

pub struct ShelfRootIvars {
    empty: RefCell<Option<EmptyState>>,
}

define_class!(
    // SAFETY:
    // - NSView subclass; main-thread UI like every AppKit view.
    // - No torn references: ivars are plain data behind RefCell.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ShelfRootIvars]
    pub struct ShelfRootView;

    unsafe impl NSObjectProtocol for ShelfRootView {}

    impl ShelfRootView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            let bounds = self.bounds();
            NSGraphicsContext::saveGraphicsState_class();
            rounded_top(bounds, 14.0).addClip();
            // Paint.desk tiled over the window; top corners clipped round.
            NSColor::colorWithPatternImage(&theme::desk_tile()).setFill();
            NSBezierPath::bezierPathWithRect(bounds).fill();
            // Sidebar ground (brownDeep 0.6) + the hand-ruled seam.
            theme::brown_deep()
                .colorWithAlphaComponent(0.6)
                .setFill();
            NSBezierPath::bezierPathWithRect(CGRect::new(
                CGPoint::ZERO,
                CGSize::new(SIDEBAR_W, bounds.size.height),
            ))
            .fill();
            theme::on_brown()
                .colorWithAlphaComponent(0.28)
                .setFill();
            theme::torn_paper_path(
                CGRect::new(
                    CGPoint::new(SIDEBAR_W - 1.5, 0.0),
                    CGSize::new(1.5, bounds.size.height),
                ),
                false, true, false, false, 5, 0.8, 5.0,
            )
            .fill();
            NSGraphicsContext::restoreGraphicsState_class();
            // Empty hints (spinner is a real NSProgressIndicator subview).
            match *self.ivars().empty.borrow() {
                Some(EmptyState::NoClips) => empty_hint(bounds, "clipboard", &no_clips_text()),
                Some(EmptyState::NoMatches) => empty_hint(bounds, "magnifyingglass", &l("没有匹配的内容")),
                Some(EmptyState::Searching) => {
                    let r = bounds;
                    let area = CGRect::new(
                        CGPoint::new(SIDEBAR_W + PAD_H, HEADER_H),
                        CGSize::new(r.size.width - SIDEBAR_W - 2.0 * PAD_H, r.size.height - HEADER_H - FOOTER_H),
                    );
                    card::draw_text(
                        CGRect::new(
                            CGPoint::new(area.min().x, area.mid().y + 4.0),
                            CGSize::new(area.size.width, 22.0),
                        ),
                        &l("正在搜索…"),
                        &theme::serif(15.0, false),
                        &theme::on_brown_muted(),
                        None,
                    );
                }
                None => {}
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            // Desk tap: drop the keyboard out of the search field.
            end_rename();
            if let Some(w) = self.window() {
                w.makeFirstResponder(None);
            }
        }
    }
);

/// The top-left / top-right rounded clip (UnevenRoundedRectangle 14).
/// Both arcs reuse the ticket's proven corner calls (theme.rs ticket
/// corners 1 and 4, byte-verified against the Swift runtime).
fn rounded_top(r: CGRect, radius: f64) -> Retained<NSBezierPath> {
    let (x0, y0) = (r.min().x, r.min().y);
    let (x1, y1) = (r.max().x, r.max().y);
    let p = NSBezierPath::new();
    p.moveToPoint(CGPoint::new(x0, y1));
    p.lineToPoint(CGPoint::new(x0, y0 + radius));
    // Top-left: left edge → top edge (ticket corner 4).
    p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
        CGPoint::new(x0 + radius, y0 + radius),
        radius,
        180.0,
        270.0,
        false,
    );
    p.lineToPoint(CGPoint::new(x1 - radius, y0));
    // Top-right: top edge → right edge (ticket corner 1).
    p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
        CGPoint::new(x1 - radius, y0 + radius),
        radius,
        -90.0,
        0.0,
        false,
    );
    p.lineToPoint(CGPoint::new(x1, y1));
    p.closePath();
    p
}

/// 「还没有内容…」 with today's capture shortcut filled in.
fn no_clips_text() -> String {
    let display = crate::app::preferences::Preferences::shared()
        .shortcut(crate::app::preferences::key::HOTKEY_CAPTURE)
        .display();
    l("还没有内容。复制点什么，或者按 %@ 截个图。").replacen("%@", &display, 1)
}

/// The empty-state icon + line, centered in the cards band.
fn empty_hint(bounds: CGRect, icon: &str, text: &str) {
    let area = CGRect::new(
        CGPoint::new(SIDEBAR_W + PAD_H, HEADER_H),
        CGSize::new(bounds.size.width - SIDEBAR_W - 2.0 * PAD_H, bounds.size.height - HEADER_H - FOOTER_H),
    );
    if let Some(img) = card::symbol(
        icon,
        34.0,
        SymbolWeight::Light,
        &theme::on_brown_muted().colorWithAlphaComponent(0.7),
    ) {
        let sz = img.size();
        card::draw_centered(
            &img,
            CGRect::new(
                CGPoint::new(area.mid().x - sz.width / 2.0, area.mid().y - 10.0 - sz.height / 2.0),
                sz,
            ),
            1.0,
        );
    }
    let font = theme::serif(15.0, false);
    let w = card::measure(text, &font, area.size.width, None).width;
    card::draw_text(
        CGRect::new(
            CGPoint::new(area.mid().x - w / 2.0, area.mid().y + 4.0),
            CGSize::new(w + 1.0, 22.0),
        ),
        text,
        &font,
        &theme::on_brown_muted(),
        None,
    );
}

impl ShelfRootView {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = mtm.alloc::<ShelfRootView>().set_ivars(ShelfRootIvars {
            empty: RefCell::new(None),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn set_empty(&self, state: Option<EmptyState>) {
        *self.ivars().empty.borrow_mut() = state;
    }
}

// MARK: Sidebar

struct SidebarIvars {
    active: RefCell<usize>,
}

define_class!(
    // SAFETY: plain NSView; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SidebarIvars]
    struct SidebarView;

    unsafe impl NSObjectProtocol for SidebarView {}

    impl SidebarView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            let brand_font = theme::brand_font(BRAND_SIZE);
            // 「Pastory」 script, padded 22 / 10 / 22.
            card::draw_text(
                CGRect::new(CGPoint::new(22.0, 10.0), CGSize::new(SIDEBAR_W - 22.0, card::line_height(&brand_font))),
                "Pastory",
                &brand_font,
                &theme::on_brown(),
                None,
            );
            let rows_top = 10.0 + card::line_height(&brand_font) + 22.0;
            let active = *self.ivars().active.borrow();
            for (i, (icon, title)) in nav_rows().into_iter().enumerate() {
                let row_y = rows_top + i as f64 * 60.0;
                let center_y = row_y + 30.0;
                if i == active {
                    // PaperPatch: torn blue sheet off the left edge, soft shadow.
                    let patch = CGRect::new(CGPoint::new(6.0, row_y), CGSize::new(SIDEBAR_W - 16.0, 60.0));
                    NSGraphicsContext::saveGraphicsState_class();
                    card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.4), 5.0, 1.0, 3.0).set();
                    NSColor::colorWithPatternImage(&theme::paper_blue_tile()).setFill();
                    theme::torn_paper_path(patch, true, true, true, true, 11, 2.2, 7.0).fill();
                    NSGraphicsContext::restoreGraphicsState_class();
                }
                let tint = if i == active { theme::ink() } else { theme::on_brown() };
                let mut x = 22.0;
                if let Some(img) = card::symbol(icon, 17.0, SymbolWeight::Regular, &tint) {
                    let isz = img.size();
                    card::draw_image(
                        &img,
                        CGRect::new(CGPoint::new(x, center_y - isz.height / 2.0), isz),
                        objc2_app_kit::NSCompositingOperation::SourceOver,
                        1.0,
                    );
                    x += isz.width + 12.0;
                }
                card::draw_line_centered(
                    &l(title),
                    &theme::serif(17.0, false),
                    &tint,
                    x,
                    center_y,
                    SIDEBAR_W - x - 6.0,
                );
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let brand_font = theme::brand_font(BRAND_SIZE);
            let rows_top = 10.0 + card::line_height(&brand_font) + 22.0;
            if p.x < 0.0 || p.x > SIDEBAR_W {
                return;
            }
            let i = ((p.y - rows_top) / 60.0).floor() as i64;
            if !(0..3).contains(&i) {
                return;
            }
            with_model_mut(|m| {
                m.show_settings = i == 1;
                m.show_contact = i == 2;
            });
            reload();
        }
    }
);

impl SidebarView {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = mtm.alloc::<SidebarView>().set_ivars(SidebarIvars {
            active: RefCell::new(0),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn set_active(&self, i: usize) {
        *self.ivars().active.borrow_mut() = i;
    }
}

// MARK: Header

struct HeaderIvars {
    filter: RefCell<ShelfFilter>,
    counts: RefCell<crate::shelf::model::FilterCounts>,
    query: RefCell<String>,
    tabs: RefCell<Vec<(ShelfFilter, CGRect)>>,
    clear_hit: RefCell<CGRect>,
    close_hit: RefCell<CGRect>,
    settings: RefCell<bool>,
    contact: RefCell<bool>,
}

define_class!(
    // SAFETY: plain NSView; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = HeaderIvars]
    struct HeaderView;

    unsafe impl NSObjectProtocol for HeaderView {}

    impl HeaderView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            let pane_open = *self.ivars().settings.borrow() || *self.ivars().contact.borrow();
            if let Some(f) = with_ui(|ui| ui.search.clone()) {
                f.setHidden(pane_open);
            }
            if pane_open {
                *self.ivars().tabs.borrow_mut() = Vec::new();
                *self.ivars().clear_hit.borrow_mut() = CGRect::ZERO;
                *self.ivars().close_hit.borrow_mut() = CGRect::ZERO;
                return;
            }
            let w = self.bounds().size.width;
            let mut tabs: Vec<(ShelfFilter, CGRect)> = Vec::new();
            let mut x = 0.0;
            let name_font = theme::serif(17.0, false);
            let count_font = theme::serif(16.0, false);
            let active = *self.ivars().filter.borrow();
            let counts = *self.ivars().counts.borrow();
            for (i, f) in ShelfFilter::ALL.into_iter().enumerate() {
                let name = tab_name(f);
                let nw = card::measure(&name, &name_font, 400.0, None).width;
                let cw = card::measure(&counts.get(f).to_string(), &count_font, 100.0, None).width;
                let tab_w = 40.0 + nw + 18.0 + cw;
                let rect = CGRect::new(CGPoint::new(x, 12.0), CGSize::new(tab_w, 40.0));
                if f == active {
                    // Active tab: torn blue scrap (vertical inset 1).
                    let patch = CGRect::new(CGPoint::new(x, 13.0), CGSize::new(tab_w, 38.0));
                    NSGraphicsContext::saveGraphicsState_class();
                    card::shadow(&NSColor::blackColor().colorWithAlphaComponent(0.4), 5.0, 1.0, 3.0).set();
                    NSColor::colorWithPatternImage(&theme::paper_blue_tile()).setFill();
                    theme::torn_paper_path(patch, true, true, true, true, 23 + i as u64, 2.0, 7.0).fill();
                    NSGraphicsContext::restoreGraphicsState_class();
                } else {
                    // Hand-ruled box: left, bottom, right — no top edge.
                    theme::on_brown().colorWithAlphaComponent(0.5).setStroke();
                    let p = theme::ruled_box_path(rect, 40 + i as u64, 0.9);
                    p.setLineWidth(1.0);
                    p.stroke();
                }
                let tint = if f == active { theme::ink() } else { theme::on_brown() };
                card::draw_line_centered(&name, &name_font, &tint, x + 20.0, 32.0, nw + 1.0);
                card::draw_line_centered(
                    &counts.get(f).to_string(),
                    &count_font,
                    &tint,
                    x + 20.0 + nw + 18.0,
                    32.0,
                    cw + 1.0,
                );
                tabs.push((f, rect));
                x += tab_w + 12.0;
            }
            *self.ivars().tabs.borrow_mut() = tabs;
            // Search group: magnifier, field, clear; torn stroke around.
            let gx = w - 440.0;
            if let Some(img) = card::symbol("magnifyingglass", 14.0, SymbolWeight::Regular, &theme::on_brown_muted()) {
                let sz = img.size();
                card::draw_image(
                    &img,
                    CGRect::new(CGPoint::new(gx + 14.0, 32.0 - sz.height / 2.0), sz),
                    objc2_app_kit::NSCompositingOperation::SourceOver,
                    1.0,
                );
            }
            if let Some(field) = with_ui(|ui| ui.search.clone()) {
                field.setFrame(CGRect::new(
                    CGPoint::new(gx + 38.0, 12.0 + (40.0 - 22.0) / 2.0),
                    CGSize::new(321.0, 22.0),
                ));
            }
            let has_query = !self.ivars().query.borrow().is_empty();
            if has_query {
                if let Some(img) = card::symbol("xmark.circle.fill", 17.0, SymbolWeight::Regular, &theme::on_brown_muted()) {
                    let sz = img.size();
                    card::draw_image(
                        &img,
                        CGRect::new(CGPoint::new(w - 54.0 - sz.width, 32.0 - sz.height / 2.0), sz),
                        objc2_app_kit::NSCompositingOperation::SourceOver,
                        1.0,
                    );
                    *self.ivars().clear_hit.borrow_mut() = CGRect::new(
                        CGPoint::new(w - 54.0 - sz.width + (sz.width - 40.0) / 2.0, 12.0),
                        CGSize::new(40.0, 40.0),
                    );
                }
            } else {
                *self.ivars().clear_hit.borrow_mut() = CGRect::ZERO;
            }
            theme::on_brown().colorWithAlphaComponent(0.45).setStroke();
            let stroke = theme::torn_paper_path(
                CGRect::new(CGPoint::new(gx, 12.0), CGSize::new(400.0, 40.0)),
                true, true, true, true, 31, 0.7, 6.0,
            );
            stroke.setLineWidth(1.0);
            stroke.stroke();
            // Close button.
            if let Some(img) = card::symbol("xmark", 15.0, SymbolWeight::Regular, &theme::on_brown()) {
                let sz = img.size();
                card::draw_image(
                    &img,
                    CGRect::new(CGPoint::new(w - 20.0 - sz.width / 2.0, 32.0 - sz.height / 2.0), sz),
                    objc2_app_kit::NSCompositingOperation::SourceOver,
                    1.0,
                );
            }
            *self.ivars().close_hit.borrow_mut() =
                CGRect::new(CGPoint::new(w - 40.0, 12.0), CGSize::new(40.0, 40.0));
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            if contains(&self.ivars().close_hit.borrow(), p) {
                crate::shelf::panel::hide();
                return;
            }
            if contains(&self.ivars().clear_hit.borrow(), p) {
                with_model_mut(|m| m.set_query(String::new()));
                reload();
                return;
            }
            for (f, r) in self.ivars().tabs.borrow().iter() {
                if contains(r, p) {
                    with_model_mut(|m| m.set_filter(*f));
                    reload();
                    return;
                }
            }
        }
    }
);

fn contains(r: &CGRect, p: CGPoint) -> bool {
    p.x >= r.min().x && p.x <= r.max().x && p.y >= r.min().y && p.y <= r.max().y
}

/// The PICS tab names: «tab» context, Pin has no translation row (Swift too).
fn tab_name(f: ShelfFilter) -> String {
    match f {
        ShelfFilter::All => crate::app::localization::l_ctx("全部", "tab"),
        ShelfFilter::Pinned => "Pin".to_string(),
        ShelfFilter::Images => crate::app::localization::l_ctx("图片", "tab"),
        ShelfFilter::Videos => crate::app::localization::l_ctx("录屏", "tab"),
        ShelfFilter::Text => crate::app::localization::l_ctx("文本", "tab"),
    }
}

impl HeaderView {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = mtm.alloc::<HeaderView>().set_ivars(HeaderIvars {
            filter: RefCell::new(ShelfFilter::All),
            counts: RefCell::new(Default::default()),
            query: RefCell::new(String::new()),
            tabs: RefCell::new(Vec::new()),
            clear_hit: RefCell::new(CGRect::ZERO),
            close_hit: RefCell::new(CGRect::ZERO),
            settings: RefCell::new(false),
            contact: RefCell::new(false),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn set_tab_state(
        &self,
        filter: ShelfFilter,
        counts: crate::shelf::model::FilterCounts,
        query: String,
        settings: bool,
        contact: bool,
    ) {
        *self.ivars().filter.borrow_mut() = filter;
        *self.ivars().counts.borrow_mut() = counts;
        *self.ivars().query.borrow_mut() = query;
        *self.ivars().settings.borrow_mut() = settings;
        *self.ivars().contact.borrow_mut() = contact;
    }
}

// MARK: Cards row (document view of the scroll view)

struct CardsRowIvars {
    /// (`beginTear`) candidate: card + down-point; torn when the drag goes
    /// clearly upward out of the row (Swift's DragGesture gate).
    tear_candidate: RefCell<Option<(String, CGPoint)>>,
}

define_class!(
    // SAFETY: plain NSView; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = CardsRowIvars]
    struct CardsRowView;

    unsafe impl NSObjectProtocol for CardsRowView {}

    impl CardsRowView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: CGRect) {
            // One draw pass for the whole row, cards culled to the dirty band
            // (+1 card of shadow bleed on each side).
            with_ui(|ui| {
                let row_h = self.bounds().size.height;
                for c in &ui.cards {
                    let ticket = ticket_rect(row_h, c.index, c.selected);
                    let bleed = CGRect::new(
                        CGPoint::new(ticket.min().x - 30.0, ticket.min().y - 30.0),
                        CGSize::new(ticket.size.width + 60.0, ticket.size.height + 60.0),
                    );
                    if !intersects(&bleed, &dirty) {
                        continue;
                    }
                    card::draw(ticket, c);
                }
            });
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            if let Some((i, _)) = hit_card(self, p) {
                let Some(item) = card_item(i) else { return };
                *self.ivars().tear_candidate.borrow_mut() = Some((item.id, NSEvent::mouseLocation()));
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            let Some((id, down)) = self.ivars().tear_candidate.borrow().clone() else { return };
            let now = NSEvent::mouseLocation();
            // Only a clearly upward pull starts the tear (the Swift gate).
            let dy = now.y - down.y; // flipped: up = Cocoa +y
            let dx = (now.x - down.x).abs();
            if dy > 18.0 && dy > dx * 0.8 {
                if crate::shelf::desktop_notes::is_on_desktop(&id) {
                    crate::shelf::desktop_notes::bring_to_front(&id);
                } else {
                    crate::shelf::desktop_notes::begin_tear(&id, now);
                }
            }
            if crate::shelf::desktop_notes::is_tearing(&id) {
                crate::shelf::desktop_notes::move_tear(now);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            // Tear-out end decides before the body-tap copy runs.
            let candidate = self.ivars().tear_candidate.borrow_mut().take();
            if let Some((id, _)) = candidate {
                if crate::shelf::desktop_notes::is_tearing(&id) {
                    crate::shelf::desktop_notes::end_tear(crate::shelf::panel::frame_on_screen());
                    return;
                }
            }
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let Some((i, zone)) = hit_card(self, p) else {
                return;
            };
            let Some(item) = card_item(i) else { return };
            match zone {
                HitZone::Action(action) => run_action(action, &item, event),
                HitZone::TitleCheck => finalize_rename_click(&item),
                HitZone::Title => title_click(&item, event),
                HitZone::Body => {
                    if event.clickCount() >= 2 {
                        with_model_mut(|m| m.copy_and_close(&item, true));
                    } else {
                        with_model_mut(|m| m.copy(&item));
                    }
                    reload();
                }
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let Some((i, _)) = hit_card(self, p) else { return };
            let Some(item) = card_item(i) else { return };
            show_context_menu(&item, event, self);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            // SwiftUI's horizontal ScrollView maps a vertical mouse wheel onto
            // the horizontal axis; a bare NSScrollView does not. Shift the
            // clip origin by hand so the wheel pages through the card row.
            let dx = event.scrollingDeltaX();
            let dy = event.scrollingDeltaY();
            let mut delta = if dx.abs() >= dy.abs() { dx } else { dy };
            if !event.hasPreciseScrollingDeltas() {
                // Wheel notches arrive ~0.1/notch ("lines"); scroll 10pt a line.
                delta *= 10.0;
            }
            if delta == 0.0 {
                return;
            }
            with_ui(|ui| {
                let Some(scroll) = &ui.scroll else { return };
                let clip = scroll.contentView();
                let doc_w = scroll
                    .documentView()
                    .map(|d| d.frame().size.width)
                    .unwrap_or(0.0);
                let max_x = (doc_w - clip.bounds().size.width).max(0.0);
                // Flipped or not, horizontal follows the vertical convention:
                // content towards the user (delta > 0) walks back to the start.
                let x = (clip.bounds().origin.x - delta).clamp(0.0, max_x);
                if x == clip.bounds().origin.x {
                    return;
                }
                clip.scrollToPoint(CGPoint::new(x, clip.bounds().origin.y));
                scroll.reflectScrolledClipView(&clip);
            });
        }
    }
);

fn intersects(a: &CGRect, b: &CGRect) -> bool {
    a.min().x < b.max().x && b.min().x < a.max().x && a.min().y < b.max().y && b.min().y < a.max().y
}

impl CardsRowView {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = mtm.alloc::<CardsRowView>().set_ivars(CardsRowIvars {
            tear_candidate: RefCell::new(None),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// Where a click landed inside a card.
enum HitZone {
    Body,
    Title,
    TitleCheck,
    Action(card::ActionKind),
}

/// Card index + zone under `p` (doc-view coordinates).
fn hit_card(row: &CardsRowView, p: CGPoint) -> Option<(usize, HitZone)> {
    with_ui(|ui| {
        let row_h = row.bounds().size.height;
        for c in &ui.cards {
            let ticket = ticket_rect(row_h, c.index, c.selected);
            if !contains(&ticket, p) {
                continue;
            }
            // Action strip hit first (bottom 52pt).
            for (kind, rect) in card::action_rects(ticket, c.item.kind) {
                if kind != card::ActionKind::Divider && contains(&rect, p) {
                    return Some((c.index, HitZone::Action(kind)));
                }
            }
            if c.has_title_row() && contains(&card::title_row_rect(ticket), p) {
                // Checkmark button next to the inline editor.
                if c.renaming {
                    let check = CGRect::new(
                        CGPoint::new(ticket.max().x - card::PAD_X - 20.0, card::title_row_rect(ticket).min().y + 4.0),
                        CGSize::new(20.0, 26.0),
                    );
                    if contains(&check, p) {
                        return Some((c.index, HitZone::TitleCheck));
                    }
                }
                return Some((c.index, HitZone::Title));
            }
            return Some((c.index, HitZone::Body));
        }
        None
    })
}

fn card_item(index: usize) -> Option<ClipItem> {
    with_ui(|ui| ui.cards.get(index).map(|c| c.item.clone()))
}

/// The checkmark in a rename row: commit and close (Swift's toggle).
fn finalize_rename_click(_item: &ClipItem) {
    end_rename();
}

/// The title row hands clicks to rename (Swift `beginRenameFromClick`): the
/// second half of a double-click still pastes.
fn title_click(item: &ClipItem, event: &NSEvent) {
    if event.clickCount() >= 2 {
        with_model_mut(|m| m.copy_and_close(item, true));
        reload();
        return;
    }
    with_model_mut(|m| {
        m.cancel_pending_hand_back();
        m.set_selected_id(Some(item.id.clone()));
        m.renaming_id = Some(item.id.clone());
    });
    reload();
}

/// Action-strip dispatch (the Swift card's stub buttons).
fn run_action(action: card::ActionKind, item: &ClipItem, _event: &NSEvent) {
    match action {
        card::ActionKind::Edit => {
            with_model_mut(|m| {
                m.set_selected_id(Some(item.id.clone()));
                m.edit(item);
            });
        }
        card::ActionKind::Preview => {
            with_model_mut(|m| {
                m.set_selected_id(Some(item.id.clone()));
                m.edit(item);
            });
        }
        card::ActionKind::Pin => {
            with_model_mut(|m| m.with_store_mut(|s| s.toggle_pin(&item.id, true)));
        }
        card::ActionKind::Save => {
            crate::shelf::exporter::export(item);
        }
        card::ActionKind::Trash => {
            crate::shelf::panel::delete_item(item);
        }
        card::ActionKind::Divider => {}
    }
    reload();
}

// MARK: Search / rename field delegates

struct SearchDelegateIvars {}

define_class!(
    // SAFETY: NSObject delegate; main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SearchDelegateIvars]
    struct SearchDelegate;

    unsafe impl NSObjectProtocol for SearchDelegate {}

    impl SearchDelegate {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _n: &NSNotification) {
            with_ui(|ui| {
                let Some(field) = ui.search.clone() else { return };
                let q = field.stringValue().to_string();
                with_model_mut(|m| {
                    m.set_query(q);
                    m.begin_search();
                });
            });
            reload();
        }

        #[unsafe(method(controlTextDidBeginEditing:))]
        fn began(&self, _n: &NSNotification) {
            with_ui(|ui| {
                if let Some(f) = &ui.search {
                    update_placeholder(f, false);
                    tint_caret(f);
                }
            });
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn ended(&self, _n: &NSNotification) {
            with_ui(|ui| {
                if let Some(f) = &ui.search {
                    update_placeholder(f, true);
                }
            });
        }
    }
);

impl SearchDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<SearchDelegate>().set_ivars(SearchDelegateIvars {});
        unsafe { msg_send![super(this), init] }
    }
}

/// SwiftUI `.tint(paperBlue)`: paper-blue caret (and selection outline).
fn tint_caret(field: &NSTextField) {
    if let Some(editor) = field.currentEditor() {
        unsafe {
            let text_view: &NSTextView = &*(Retained::as_ptr(&editor) as *const NSTextView);
            text_view.setInsertionPointColor(Some(&theme::paper_blue()));
        }
    }
}

struct RenameDelegateIvars {}

define_class!(
    // SAFETY: NSObject delegate; main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RenameDelegateIvars]
    struct RenameDelegate;

    unsafe impl NSObjectProtocol for RenameDelegate {}

    impl RenameDelegate {
        #[unsafe(method(controlTextDidEndEditing:))]
        fn ended(&self, _n: &NSNotification) {
            // Focus left the box: save.
            end_rename();
        }

        #[unsafe(method(renameDone:))]
        fn rename_done(&self, _sender: &AnyObject) {
            end_rename();
        }
    }
);

impl RenameDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<RenameDelegate>().set_ivars(RenameDelegateIvars {});
        unsafe { msg_send![super(this), init] }
    }
}

// MARK: Context menu (ShelfView.menu(for:))

/// One handler object per shown menu; selectors carry the item id.
struct MenuHandlerIvars {
    item: RefCell<ClipItem>,
}

define_class!(
    // SAFETY: NSObject action target; main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuHandlerIvars]
    struct MenuHandler;

    unsafe impl NSObjectProtocol for MenuHandler {}

    impl MenuHandler {
        #[unsafe(method(copyItem:))]
        fn copy_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| m.copy(&item));
            reload();
        }

        #[unsafe(method(copyCloseItem:))]
        fn copy_close(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| m.copy_and_close(&item, false));
        }

        #[unsafe(method(renameItem:))]
        fn rename_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| {
                m.set_selected_id(Some(item.id.clone()));
                m.renaming_id = Some(item.id.clone());
            });
            reload();
        }

        #[unsafe(method(removeTitleItem:))]
        fn remove_title(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| m.with_store_mut(|s| s.set_title(None, &item.id)));
            reload();
        }

        #[unsafe(method(editItem:))]
        fn edit_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| {
                m.set_selected_id(Some(item.id.clone()));
                m.edit(&item);
            });
        }

        #[unsafe(method(previewItem:))]
        fn preview_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| {
                m.set_selected_id(Some(item.id.clone()));
                m.preview_selected();
            });
        }

        #[unsafe(method(pinItem:))]
        fn pin_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| m.with_store_mut(|s| s.toggle_pin(&item.id, true)));
            reload();
        }

        #[unsafe(method(stickItem:))]
        fn stick_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            if crate::shelf::desktop_notes::is_on_desktop(&item.id) {
                crate::shelf::desktop_notes::bring_to_front(&item.id);
            } else {
                crate::shelf::desktop_notes::place(&item.id, None);
            }
            reload();
        }

        #[unsafe(method(unstickItem:))]
        fn unstick_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            crate::shelf::desktop_notes::close(&item.id);
            reload();
        }

        #[unsafe(method(saveItem:))]
        fn save_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            crate::shelf::exporter::export(&item);
        }

        #[unsafe(method(showInFinderItem:))]
        fn show_in_finder(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            let urls = with_model(|m| m.with_store(|s| s.file_urls(&item)));
            let ns: Vec<Retained<objc2_foundation::NSURL>> = urls
                .iter()
                .map(|p| objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())))
                .collect();
            let refs: Vec<&objc2_foundation::NSURL> = ns.iter().map(|u| &**u).collect();
            let arr = NSArray::from_slice(&refs);
            objc2_app_kit::NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&arr);
        }

        #[unsafe(method(openItem:))]
        fn open_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            let path = with_model(|m| m.with_store(|s| s.payload_url(&item)));
            let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
        }

        #[unsafe(method(copyOcrItem:))]
        fn copy_ocr(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            let Some(text) = item.ocr_text.clone().filter(|t| !t.is_empty()) else { return };
            let source = crate::clipboard::store::Source::pastory();
            let inserted = crate::clipboard::store::with(|s| s.insert_text(&text, None, &source));
            crate::capture::pasteboard_writer::write_text(
                &text,
                None,
                inserted.as_ref().map(|i| i.id.as_str()).unwrap_or(""),
            );
            crate::shelf::panel::hide();
        }

        #[unsafe(method(deleteItem:))]
        fn delete_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            crate::shelf::panel::delete_item(&item);
            reload();
        }
    }
);

impl MenuHandler {
    fn new(mtm: MainThreadMarker, item: ClipItem) -> Retained<Self> {
        let this = mtm.alloc::<MenuHandler>().set_ivars(MenuHandlerIvars {
            item: RefCell::new(item),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// The card's right-click menu (ShelfView `menu(for:)`, in Swift's order;
/// desktop-note / export items stay as M6 no-ops with anchors).
fn build_menu(mtm: MainThreadMarker, handler: &MenuHandler, item: &ClipItem) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    let add = |title: &str, action: objc2::runtime::Sel| {
        let mi = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(""),
            )
        };
        unsafe {
            let target: Option<&AnyObject> = Some(&*(handler as *const MenuHandler as *const AnyObject));
            let _: () = msg_send![&mi, setTarget: target];
        }
        menu.addItem(&mi);
    };
    add(&l("复制"), sel!(copyItem:));
    add(&l("复制并关闭"), sel!(copyCloseItem:));
    if item.title.is_some() {
        add(&l("重命名…"), sel!(renameItem:));
        add(&l("去掉标题"), sel!(removeTitleItem:));
    } else {
        add(&l("命名…"), sel!(renameItem:));
    }
    if matches!(item.kind, ClipKind::Text | ClipKind::Url | ClipKind::Image) {
        add(&l("编辑"), sel!(editItem:));
    }
    add(&l("预览"), sel!(previewItem:));
    let pin_title = if item.pinned { l("取消 Pin") } else { "Pin".to_string() };
    add(&pin_title, sel!(pinItem:));
    if crate::shelf::desktop_notes::is_on_desktop(&item.id) {
        add(&l("在桌面上显示"), sel!(stickItem:));
        add(&l("从桌面收起"), sel!(unstickItem:));
    } else {
        add(&l("贴到桌面"), sel!(stickItem:));
    }
    add(&l("保存到本地…"), sel!(saveItem:));
    if item.kind == ClipKind::Files {
        add(&l("在 Finder 中显示"), sel!(showInFinderItem:));
    }
    if item.kind == ClipKind::Video {
        add(&l("打开"), sel!(openItem:));
    }
    if item.kind == ClipKind::Image
        && item.ocr_text.as_deref().map(|t| !t.is_empty()).unwrap_or(false)
    {
        add(&l("复制识别出的文字"), sel!(copyOcrItem:));
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    add(&l("删除"), sel!(deleteItem:));
    menu
}

fn show_context_menu(item: &ClipItem, event: &NSEvent, view: &CardsRowView) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let handler = MenuHandler::new(mtm, item.clone());
    let menu = build_menu(mtm, &handler, item);
    NSMenu::popUpContextMenu_withEvent_forView(&menu, event, view);
}

// MARK: Footer

struct FooterIvars {}

define_class!(
    // SAFETY: plain NSView; main-thread only.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = FooterIvars]
    struct FooterView;

    unsafe impl NSObjectProtocol for FooterView {}

    impl FooterView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            let w = self.bounds().size.width;
            let center_y = 8.0 + 7.0; // HStack vertically centered in the 34pt frame
            // Chevrons.
            let (lc, rc) = (
                CGRect::new(CGPoint::new(0.0, 8.0), CGSize::new(14.0, 14.0)),
                CGRect::new(CGPoint::new(w - 14.0, 8.0), CGSize::new(14.0, 14.0)),
            );
            for (r, name) in [(&lc, "chevron.left"), (&rc, "chevron.right")] {
                if let Some(img) = card::symbol(name, 13.0, SymbolWeight::Regular, &theme::on_brown_muted()) {
                    card::draw_centered(&img, *r, 1.0);
                }
            }
            let geo = CGRect::new(
                CGPoint::new(14.0 + 14.0, center_y - 7.0),
                CGSize::new(w - 2.0 * 28.0, 14.0),
            );
            let (fraction, visible) = with_ui(|ui| (ui.scroll_fraction, ui.scroll_visible));
            let thumb = (40.0_f64).max(geo.size.width * visible);
            let travel = (geo.size.width - thumb).max(0.0);
            // Track + thumb capsules (3pt).
            for (rect, alpha) in [
                (CGRect::new(CGPoint::new(geo.min().x, center_y - 1.5), CGSize::new(geo.size.width, 3.0)), 0.10),
                (CGRect::new(CGPoint::new(geo.min().x + travel * fraction, center_y - 1.5), CGSize::new(thumb, 3.0)), 0.5),
            ] {
                theme::on_brown().colorWithAlphaComponent(alpha).setFill();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 1.5, 1.5).fill();
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let w = self.bounds().size.width;
            if p.x < 14.0 + 10.0 {
                with_model_mut(|m| m.move_selection(-3));
                reload();
                return;
            }
            if p.x > w - 14.0 - 10.0 {
                with_model_mut(|m| m.move_selection(3));
                reload();
                return;
            }
            // Drag the paper thumb (DragGesture minimumDistance: 0): the thumb
            // centers on the down point and tracks until mouse up.
            drag_scroll(self, event);
        }
    }
);

impl FooterView {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = mtm.alloc::<FooterView>().set_ivars(FooterIvars {});
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// The thumb-drag event loop (nextEventMatchingMask until mouse-up).
fn drag_scroll(view: &FooterView, first: &NSEvent) {
    let w = view.bounds().size.width;
    let geo_w = w - 2.0 * 28.0;
    let handle = |ev: &NSEvent| {
        let p = view.convertPoint_fromView(ev.locationInWindow(), None);
        with_ui(|ui| {
            let Some(scroll) = ui.scroll.clone() else { return };
            let thumb = (40.0_f64).max(geo_w * ui.scroll_visible);
            let travel = (geo_w - thumb).max(0.0);
            let f = if travel > 0.0 {
                ((p.x - 28.0 - thumb / 2.0) / travel).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let clip = scroll.contentView();
            clip.setBoundsOrigin(CGPoint::new(f * ui.scroll_range, 0.0));
        });
        reload();
    };
    handle(first);
    loop {
        let Some(window) = view.window() else { break };
        let next = unsafe {
            window.nextEventMatchingMask_untilDate_inMode_dequeue(
                objc2_app_kit::NSEventMask::LeftMouseDragged | objc2_app_kit::NSEventMask::LeftMouseUp,
                None,
                objc2_foundation::NSDefaultRunLoopMode,
                true,
            )
        };
        let Some(ev) = next else { break };
        match ev.r#type() {
            objc2_app_kit::NSEventType::LeftMouseUp => break,
            objc2_app_kit::NSEventType::LeftMouseDragged | objc2_app_kit::NSEventType::LeftMouseDown => {
                handle(&ev);
            }
            _ => {}
        }
    }
}

// MARK: Public entry points used by panel.rs / the render self-test

/// Re-pull the model and repaint (SwiftUI's body re-evaluation).
pub(crate) fn refresh() {
    if with_ui(|ui| ui.built) {
        reload();
    }
}

/// The live root view, when the tree is built.
pub(crate) fn root_view() -> Option<Retained<ShelfRootView>> {
    with_ui(|ui| ui.root.clone())
}

/// Bump the search field into first responder and flush pending keystrokes
/// (⌘F / showSearch / direct typing).
pub(crate) fn focus_search_now() {
    with_ui(focus_search_field);
}

/// Drop keyboard focus after show() (the caret must not sit in the box).
pub(crate) fn drop_focus_now() {
    with_ui(|ui| {
        if let Some(root) = &ui.root {
            if let Some(w) = root.window() {
                w.makeFirstResponder(None);
            }
        }
    });
}

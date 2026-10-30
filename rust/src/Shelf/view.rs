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
    NSBezierPath, NSEvent, NSGraphicsContext, NSMenu, NSMenuItem, NSProgressIndicator,
    NSScrollView, NSTextField, NSTextView, NSView, NSControlSize, NSProgressIndicatorStyle,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSMutableAttributedString, NSNotification, NSRange, NSString};

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
    /// The ground layer (panel background image / gradient / sidebar band).
    /// Its own view, directly above the material: drawn in the root's
    /// `drawRect` it sat UNDER the glass and the live panel showed neither the
    /// user's picture nor the skin's gradient.
    ground: Option<Retained<GroundView>>,
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
            ground: None,
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

/// The contact pane while 联系我 is up (the self-tests assert on it).
pub(crate) fn contact_view() -> Option<Retained<crate::shelf::contact_pane::ContactView>> {
    with_ui(|ui| ui.contact_view.clone())
}

/// The welcome card while the first launch shows it (its shortcut recorders).
pub(crate) fn welcome_view() -> Option<Retained<crate::shelf::welcome_card::WelcomeCardView>> {
    with_ui(|ui| ui.welcome_view.clone())
}

/// Show/hide the settings pane over the content column. An open welcome card
/// or pane exchange takes precedence; cards/header/footer hide behind the pane.
///
/// Returns the search field's pending visibility instead of applying it: the
/// pane swap runs inside the `with_ui` borrow, and hiding a field that holds
/// first responder ends its editing synchronously — AppKit answers with
/// `controlTextDidEndEditing:`, whose delegate reaches back into `with_ui`
/// and would borrow it twice. apply_snapshot lands it after the borrow.
fn sync_settings_view(ui: &mut Ui, s: &Snapshot) -> Option<(Retained<NSTextField>, bool)> {
    let Some(root) = ui.root.clone() else { return None };
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
            // A view added to a visible window draws on the next display pass;
            // the flag only makes that explicit. An occluded window defers it
            // (the self-tests force a draw through the bitmap path instead) —
            // in front of the user the pane paints like the settings pane.
            pane.setNeedsDisplay(true);
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
    let mut hide_search = None;
    if let Some(search) = &ui.search {
        if search.isHidden() != pane_open {
            hide_search = Some((search.clone(), pane_open));
        }
    }
    hide_search
}

/// The live search field. `--selftest searchtype` drives this real control,
/// which `searchlive` cannot: it records the query itself. `None` before the
/// tree is built.
pub(crate) fn search_field() -> Option<Retained<NSTextField>> {
    with_ui(|ui| ui.search.clone())
}

/// The live inline title editor, while a rename is open. The box only exists
/// during a rename, so `None` is how a self-test tells "the click opened the
/// editor" from "the click did nothing".
pub(crate) fn rename_field() -> Option<Retained<NSTextField>> {
    with_ui(|ui| ui.rename.clone())
}

/// Post the notification AppKit posts on every text edit, so a self-test can
/// reach `SearchDelegate::controlTextDidChange:` without synthesizing keys.
pub(crate) fn post_search_did_change() {
    let Some(field) = search_field() else { return };
    unsafe {
        // SAFETY: the field is live, `NSControlTextDidChangeNotification` is
        // the name AppKit posts for a text edit, and `object` is the control
        // that changed. Only the search field observes that name, through the
        // delegate it already has.
        let name = objc2_app_kit::NSControlTextDidChangeNotification;
        let center = objc2_foundation::NSNotificationCenter::defaultCenter();
        let obj: Option<&AnyObject> = Some(&*(&*field as *const NSTextField as *const AnyObject));
        let _: () = msg_send![&*center, postNotificationName: name, object: obj];
    }
}

/// The card snapshot for hit-testing outside draws.
pub(crate) fn cards() -> Vec<String> {
    with_ui(|ui| ui.cards.iter().map(|c| c.item.id.clone()).collect())
}

/// How many cards the visible band currently covers (the warm band's size).
/// `--selftest thumbcull` reads it to tell "the on-screen cards have pictures"
/// from "nothing was warmed at all".
pub(crate) fn visible_card_count() -> usize {
    let n = with_ui(|ui| ui.cards.len());
    match visible_warm_range(n) {
        Some((lo, hi)) => hi.saturating_sub(lo) + 1,
        None => 0,
    }
}

// MARK: Model access (the shelf UI always talks to the shared controller)

fn with_model<R>(f: impl FnOnce(&ShelfModel) -> R) -> R {
    crate::shelf::panel::with_model(f)
}

fn with_model_mut<R>(f: impl FnOnce(&mut ShelfModel) -> R) -> R {
    crate::shelf::panel::with_model_mut(f)
}

/// The index band of cards whose thumbnails are worth warming: what the clip
/// view shows, plus one card of bleed on each side (a scroll can bring the
/// neighbour in before the next reload). `n` is the number of cards about to be
/// shown (`ui.cards` is still the previous reload's list here). `None` when the
/// tree is not up yet — the caller then warms nothing, and the first `reload`
/// after `build` (which runs with the scroll view in place) fills the band.
fn visible_warm_range(n: usize) -> Option<(usize, usize)> {
    if n == 0 {
        return Some((0, 0));
    }
    with_ui(|ui| {
        let (Some(scroll), Some(_row)) = (&ui.scroll, &ui.row) else {
            return None;
        };
        let clip = scroll.contentView().bounds();
        let stride = card::CARD_W + CARD_SPACING;
        // Card x ranges: [CARD_SIDE + i*stride, +CARD_W]. The visible span
        // reaches up to CARD_W of bleed past the clip on each side.
        let lo_x = clip.min().x - card::CARD_W - CARD_SIDE;
        let hi_x = clip.max().x + card::CARD_W;
        let lo = (((lo_x - CARD_SIDE) / stride).floor().max(0.0)) as usize;
        let hi = (((hi_x - CARD_SIDE) / stride).ceil().max(0.0)) as usize;
        Some((lo.min(n - 1), hi.min(n - 1)))
    })
}

/// Pull the whole drawable state out of the model. Warms thumbnails as a
/// side effect (they land in later reloads, like SwiftUI's `thumbTick`).
fn reload() {
    // Which cards ask for a thumbnail: only the ones on screen (plus a card of
    // margin), like SwiftUI's LazyHStack, which built just the cards it showed.
    //
    // Warming the whole list made the long 全部 tab decode every picture in the
    // history on each 0.25 s poll tick. Past the store's 100-thumbnail cache
    // cap that evicted the oldest third — the leftmost, on-screen cards — which
    // then decoded again next tick and were evicted again: the pictures blinked
    // between placeholder and image. Off-screen cards are not drawn, so they
    // need no thumbnail.
    //
    // The band is computed from the item count before the model borrow: the
    // card list in `ui` is still the previous reload's.
    let item_count = with_model_mut(|m| m.items().len());
    let warm = visible_warm_range(item_count);
    let snapshot = with_model_mut(|m| {
        // Publish a background search that has landed, then let the poll tick
        // start whatever the current query still needs (`serviceSearch`).
        m.drain_completions();
        m.service_search();
        let items = m.items();
        debug_assert_eq!(items.len(), item_count);
        let on_clipboard = m.with_store(|s| s.items.first().map(|it| it.id.clone()));
        let mut cards: Vec<CardData> = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let on_screen = warm.map_or(false, |(lo, hi)| index >= lo && index <= hi);
            let thumb = if (item.kind == ClipKind::Image || item.kind == ClipKind::Video) && on_screen
            {
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
///
/// The calls AppKit can answer with a delegate notification — changing the
/// first responder, writing a live field's value, pulling the rename box out
/// of the tree — are collected here and applied after the `with_ui` borrow is
/// released. Each of them can end editing synchronously, and the NSTextField
/// delegates under it call back into `with_ui`, whose `borrow_mut` would
/// abort the process.
fn apply_snapshot(mut s: Snapshot) {
    let mut root: Option<Retained<ShelfRootView>> = None;
    // `Some(None)` clears the responder; `None` leaves it alone.
    let mut responder: Option<Option<Retained<NSTextField>>> = None;
    let mut drop_rename: Option<Retained<NSTextField>> = None;
    let mut search_text: Option<(Retained<NSTextField>, String)> = None;
    // Where the clip must sit to center a freshly selected card. Collected
    // like the responder changes: writing a clip's bounds runs AppKit layout,
    // and `HeaderView::layout` reaches back into `with_ui`.
    let mut center: Option<(Retained<NSScrollView>, CGPoint)> = None;
    // The search field's pending visibility, applied out of the borrow: hiding
    // a first-responder field ends its editing synchronously and the search
    // delegate borrows `with_ui` again (the contact-pane crash).
    let mut hide_search: Option<(Retained<NSTextField>, bool)> = None;
    with_ui(|ui| {
        root = ui.root.clone();
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
        // Written after the borrow: setting the value of a field whose editor
        // is live can fire `controlTextDidChange:` on the spot.
        if let Some(field) = &ui.search {
            if field.stringValue().to_string() != s.query {
                search_text = Some((field.clone(), s.query.clone()));
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
        // Selection moved: center it.
        if s.selected != ui.last_selected {
            ui.last_selected = s.selected.clone();
            if let Some(sel) = &s.selected {
                center = center_card_origin(ui, sel);
                if let Some(f) = &ui.footer {
                    f.setNeedsDisplay(true);
                }
            }
        }
        // Rename box placement. A fresh target card asks for the caret; an
        // ended rename hands the box back to be removed.
        let handoff = sync_rename_field(ui, &s);
        responder = handoff.focus.map(Some);
        drop_rename = handoff.drop;
        // Focus events: open tick drops focus; focus_search grabs it.
        if s.open_tick != ui.last_open_tick {
            ui.last_open_tick = s.open_tick;
            responder = Some(None);
        }
        if s.focus_search != ui.last_focus_search {
            ui.last_focus_search = s.focus_search;
            if let Some(field) = focus_search_plan(ui) {
                responder = Some(Some(field));
            }
        }
        // Empty state.
        sync_empty(ui, &s);
        hide_search = sync_settings_view(ui, &s);
        mark_dirty(ui);
    });

    // Out of the borrow: only now may AppKit run a delegate callback.
    // Hiding the search field last: with the caret in it, setHidden ends
    // editing synchronously and the search delegate fires — it needs both
    // the with_ui and the model borrows gone.
    if let Some((field, hidden)) = hide_search {
        field.setHidden(hidden);
    }
    if let Some((scroll, origin)) = center {
        scroll.contentView().setBoundsOrigin(origin);
    }
    if let Some((field, text)) = search_text {
        set_search_text(&field, &text);
    }
    if let Some(field) = drop_rename {
        field.removeFromSuperview();
    }
    if let Some(choice) = responder {
        if let Some(w) = root.as_ref().and_then(|r| r.window()) {
            match &choice {
                Some(field) => {
                    w.makeFirstResponder(Some(field));
                }
                None => {
                    w.makeFirstResponder(None);
                }
            }
        }
    }
}

fn mark_dirty(ui: &Ui) {
    if let Some(v) = &ui.root {
        v.setNeedsDisplay(true);
    }
    // The ground is its own view now; without this a background image picked
    // from the settings pane would not repaint until something else did.
    if let Some(v) = &ui.ground {
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

/// Where the clip has to sit so the card centers, or `None` when the tree is
/// not up or the card is gone.
///
/// The caller writes the origin after the `with_ui` borrow is released:
/// setting a clip view's bounds runs AppKit layout synchronously, and the
/// header's layout pass calls `with_ui` for the search field — the same
/// RefCell, borrowed twice, aborting the process. Swift runs a 0.1s easeOut
/// here (`withAnimation` + `proxy.scrollTo`); the terminal state is what every
/// snapshot and the next frame need, so the scroll lands directly.
fn center_card_origin(ui: &Ui, id: &str) -> Option<(Retained<NSScrollView>, CGPoint)> {
    let (scroll, row) = (ui.scroll.clone()?, ui.row.as_ref()?);
    let i = ui.cards.iter().position(|c| c.item.id == id)?;
    let ticket = ticket_rect(row.bounds().size.height, i, ui.cards[i].selected);
    let clip = scroll.contentView();
    let mut x = ticket.mid().x - clip.bounds().size.width / 2.0;
    let range = (row.bounds().size.width - clip.bounds().size.width).max(0.0);
    x = x.clamp(0.0, range);
    Some((scroll, CGPoint::new(x, 0.0)))
}

/// Write the query into the search box.
///
/// While the box is being edited, what it shows and where the caret sits live
/// in the field editor (a shared `NSTextView`), not in the cell —
/// `setStringValue:` only updates the cell behind it. That matters on the
/// hand-off path: with nothing focused, the first character goes to
/// `pending_query`, the caret lands, and the flush raises the query to "n".
/// Writing only the cell left the editor empty, so the box looked like it had
/// swallowed the keystroke and the character typed next replaced it (the
/// editor's single letter became the whole query). Write the editor too, and
/// leave the caret after the text so the next keystroke appends.
fn set_search_text(field: &NSTextField, text: &str) {
    field.setStringValue(&NSString::from_str(text));
    let Some(editor) = field.currentEditor() else { return };
    // SAFETY: a field editor is an `NSTextView` for a text control; the
    // pointer upcast is the same one `tint_caret` uses.
    let tv: &NSTextView = unsafe { &*(Retained::as_ptr(&editor) as *const NSTextView) };
    let s = NSString::from_str(text);
    if tv.string().to_string() != text {
        tv.setString(&s);
    }
    let end = s.length();
    tv.setSelectedRange(NSRange::new(end, 0));
}

/// Arm the search-field focus grab and return the field for the caller to
/// make first responder once the UI borrow is gone.
///
/// Pending keystrokes land right after (Swift appends `pendingQuery`
/// post-focus so select-all cannot eat it).
fn focus_search_plan(ui: &mut Ui) -> Option<Retained<NSTextField>> {
    let field = ui.search.clone()?;
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
    Some(field)
}

/// The two responder changes `sync_rename_field` cannot make itself, because
/// it runs inside the UI borrow and each one reaches a delegate that calls
/// back into `with_ui`.
struct RenameHandoff {
    /// A fresh target card was seeded: put the caret in the box.
    focus: Option<Retained<NSTextField>>,
    /// The rename ended: pull the box out of the tree.
    drop: Option<Retained<NSTextField>>,
}

impl RenameHandoff {
    fn none() -> RenameHandoff {
        RenameHandoff { focus: None, drop: None }
    }
}

/// Show / move / commit the inline title editor.
fn sync_rename_field(ui: &mut Ui, s: &Snapshot) -> RenameHandoff {
    let Some(row) = ui.row.clone() else { return RenameHandoff::none() };
    match &s.renaming {
        Some(id) if ui.cards.iter().any(|c| c.item.id == *id) => {
            if ui.rename.is_none() {
                let mtm = MainThreadMarker::new().expect("main thread");
                let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), CGRect::ZERO);
                field.setBezeled(false);
                field.setDrawsBackground(false);
                field.setFocusRingType(objc2_app_kit::NSFocusRingType(0)); // none
                field.setFont(Some(&theme::script(20.0)));
                field.setTextColor(Some(&theme::label()));
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
                // Focusing this box ends the search field's editing.
                return RenameHandoff { focus: Some(field), drop: None };
            }
            RenameHandoff::none()
        }
        _ => {
            ui.rename_for = None;
            RenameHandoff { focus: None, drop: ui.rename.take() }
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
    // The material goes in first so it backs every subview below; a snapshot
    // cannot composite one, so the shelf's render self-tests skip it (the
    // opaque ground then draws the same frame).
    if crate::shelf::panel::is_material_enabled() && crate::app::theme_skin::material_code() != 0 {
        let glass = make_material_view(mtm, CGRect::new(CGPoint::ZERO, size));
        root.addSubview(&glass);
    }
    // Round the host layer as well as the drawn path: the material is a
    // full-bleed subview, so without this it would square off the silhouette
    // the path rounds.
    theme::clip_to_radius(&root, theme::radius_panel());
    // The ground goes in right after the material: above the glass, below
    // every card. Painted in `ShelfRootView::drawRect` it landed UNDER the
    // material — which is a full-bleed subview — so on the live panel the
    // user's background image and the skin's gradient were both invisible,
    // while the offscreen render (no material) showed them.
    let ground = GroundView::new(mtm, CGRect::new(CGPoint::ZERO, size));
    root.addSubview(&ground);
    with_ui(|ui| ui.ground = Some(ground.clone()));
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
    // Place the search box now that it is reachable from the UI state. Its
    // position lives in `HeaderView::layout`, which AppKit only runs when the
    // view is flagged; without this first pass the box stays at the 0×0 frame
    // it was created with, and a 0×0 field is invisible to `hitTest:` — the
    // mouse never reached it.
    if let Some(header) = with_ui(|ui| ui.header.clone()) {
        header.setNeedsLayout(true);
        header.layoutSubtreeIfNeeded();
    }
    reload();
    root
}

/// The panel's backing material. `BehindWindow` is what makes the desktop
/// tint the panel; the appearance resolves light or dark on its own.
///
/// The state must be `.active`, and that is not cosmetic. The shelf is an
/// accessory window that never takes activation, so
/// `.followsWindowActiveState` resolves to its INACTIVE shade — which on
/// macOS 15 is opaque, not translucent. Measured with a solid backdrop
/// behind the panel: 0.03% of the panel's pixels changed between a red and
/// a blue desktop (i.e. it saw nothing), against 37.2% with `.active`. The
/// inactive shade would make the material a no-op.
fn make_material_view(mtm: MainThreadMarker, frame: CGRect) -> Retained<objc2_app_kit::NSVisualEffectView> {
    use objc2_app_kit::{
        NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    };
    let v = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    // The skin picks the material; 0 means it paints its own solid ground
    // (the callers skip this view entirely in that case).
    let code = crate::app::theme_skin::material_code();
    v.setMaterial(if code == 0 {
        NSVisualEffectMaterial::UnderWindowBackground
    } else {
        NSVisualEffectMaterial(code as isize)
    });
    v.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    v.setState(NSVisualEffectState::Active);
    v.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    v
}

struct SearchFieldIvars {}

define_class!(
    // SAFETY: NSTextField subclass; main-thread only, no ivars.
    #[unsafe(super(NSTextField))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SearchFieldIvars]
    struct SearchField;

    unsafe impl NSObjectProtocol for SearchField {}

    impl SearchField {
        /// Take the click even while the panel is not the key window.
        ///
        /// The shelf is a borderless `NonactivatingPanel`. When a click
        /// arrives before the panel holds the keyboard, AppKit consults
        /// `acceptsFirstMouse:` on the view `hitTest:` picked — this field —
        /// and NOT on its superviews, so the `true` that `ShelfRootView`,
        /// `HeaderView` and the rest already return never applied here. The
        /// click was swallowed: the box read as a dead grey rectangle and
        /// only a second click got the caret in.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> objc2::runtime::Bool {
            true.into()
        }
    }
);

impl SearchField {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(SearchFieldIvars {});
        unsafe { msg_send![super(this), initWithFrame: CGRect::ZERO] }
    }
}

fn make_search_field(mtm: MainThreadMarker) -> Retained<NSTextField> {
    // `SearchField` rather than a stock `NSTextField`: the field is what a
    // first click in a non-key panel lands on, and a stock one refuses it.
    let field: Retained<NSTextField> = Retained::into_super(SearchField::new(mtm));
    field.setBezeled(false);
    field.setDrawsBackground(false);
    field.setFocusRingType(objc2_app_kit::NSFocusRingType(0)); // none
    field.setFont(Some(&theme::serif(15.0, false)));
    field.setTextColor(Some(&theme::label()));
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
    let color = theme::label_tertiary();
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
            // The ground (the user's background image / the skin's gradient /
            // the flat colour) and the sidebar band live in `GroundView` one
            // layer down: a `drawRect` here paints UNDER every subview, and the
            // material is one of them.
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
                        &theme::label_tertiary(),
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

/// The shelf's top-corner clip: the panel rounds its top edge only, the
/// bottom staying square where it meets the screen edge.
fn rounded_top(r: CGRect, radius: f64) -> Retained<NSBezierPath> {
    theme::rounded_rect_corners(r, radius, theme::Corners::TOP)
}

pub struct GroundIvars {}

define_class!(
    // SAFETY: plain NSView; main-thread UI like every AppKit view.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = GroundIvars]
    struct GroundView;

    unsafe impl NSObjectProtocol for GroundView {}

    impl GroundView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> objc2::runtime::Bool {
            true.into()
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            let bounds = self.bounds();
            NSGraphicsContext::saveGraphicsState_class();
            rounded_top(bounds, theme::radius_panel()).addClip();
            // The panel ground: the skin's own colour scheme across the whole
            // surface, with the user's background photo blended over the
            // content column only (right of the sidebar — the sidebar keeps
            // the skin's recessed band, never the photo). While the shelf's
            // material is on (the live window) the flat case is mostly
            // transparent and the desktop shows through; a snapshot paints
            // it opaque.
            theme::draw_panel_ground_in(
                bounds,
                CGRect::new(
                    CGPoint::new(SIDEBAR_W, 0.0),
                    CGSize::new((bounds.size.width - SIDEBAR_W).max(0.0), bounds.size.height),
                ),
            );
            // Sidebar: one step recessed, closed off by a hairline rather
            // than by a painted band with a hand-torn edge.
            theme::ground_deep().setFill();
            NSBezierPath::bezierPathWithRect(CGRect::new(
                CGPoint::ZERO,
                CGSize::new(SIDEBAR_W, bounds.size.height),
            ))
            .fill();
            theme::separator().setFill();
            NSBezierPath::bezierPathWithRect(CGRect::new(
                CGPoint::new(SIDEBAR_W - 1.0, 0.0),
                CGSize::new(1.0, bounds.size.height),
            ))
            .fill();
            NSGraphicsContext::restoreGraphicsState_class();
        }
    }
);

impl GroundView {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(GroundIvars {});
        let v: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        v.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        v
    }
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
        &theme::label_secondary().colorWithAlphaComponent(0.8),
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
        &theme::label_secondary(),
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
            // 左上角是 app 自己的图标（旧的是「PasteJar」品牌字）。这一带的
            // 高度仍按品牌字号的行高算：导航行的命中区在 mouseUp 与
            // selftest_sidebar_row_center 里各算了一遍同一个公式，
            // 改高度就得三处一起改，所以这里只换画什么、不动行高。
            let brand_font = theme::brand_font(BRAND_SIZE);
            let brand_band = card::line_height(&brand_font);
            let brand_avail = SIDEBAR_W - 44.0;
            if let Some(logo) = theme::logo() {
                let s = logo.size();
                let h = brand_band.min(brand_avail).min(52.0);
                let w = if s.height > 0.0 { (s.width * h / s.height).min(brand_avail) } else { h };
                card::draw_image(
                    &logo,
                    CGRect::new(CGPoint::new(22.0, 10.0), CGSize::new(w, h)),
                    objc2_app_kit::NSCompositingOperation::SourceOver,
                    1.0,
                );
            }
            let rows_top = 10.0 + card::line_height(&brand_font) + 22.0;
            let active = *self.ivars().active.borrow();
            for (i, (icon, title)) in nav_rows().into_iter().enumerate() {
                let row_y = rows_top + i as f64 * 60.0;
                let center_y = row_y + 30.0;
                if i == active {
                    // Selection: the accent chip, inset inside the sidebar.
                    let patch = CGRect::new(CGPoint::new(6.0, row_y), CGSize::new(SIDEBAR_W - 12.0, 60.0));
                    theme::accent().setFill();
                    theme::rounded_rect(patch, theme::radius_control()).fill();
                }
                // Ink on the accent chip: the skin decides, so a pale accent
                // gets dark ink instead of vanishing white text.
                let tint = if i == active {
                    theme::text_on_accent()
                } else {
                    theme::label()
                };
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

        #[unsafe(method(layout))]
        fn layout_search(&self) {
            // Position the search box off the header's own geometry. This has
            // to happen in the layout pass, not in `drawRect:`: the draw pass
            // is skipped whole whenever a pane is open (the early return
            // below) or the dirty rect is empty, and the field then keeps the
            // 0×0 frame it was created with. A 0×0 field is invisible to
            // `hitTest:`, so a real click at the box the panel draws never
            // reached the control — the box looked dead while a programmatic
            // `makeFirstResponder` still worked.
            let gx = self.bounds().size.width - 440.0;
            if let Some(field) = with_ui(|ui| ui.search.clone()) {
                // The field covers the capsule the header draws, not a 22pt
                // strip inside it. At 22pt tall the top and bottom thirds of
                // the visible 40pt box were `HeaderView` — a click there did
                // nothing, so the box read as dead. It stops short of the
                // right edge so the clear button still takes its own click.
                field.setFrame(CGRect::new(
                    CGPoint::new(gx + 34.0, 12.0),
                    CGSize::new(400.0 - 34.0 - 34.0, 40.0),
                ));
            }
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
            for (_i, f) in ShelfFilter::ALL.into_iter().enumerate() {
                let name = tab_name(f);
                let nw = card::measure(&name, &name_font, 400.0, None).width;
                let cw = card::measure(&counts.get(f).to_string(), &count_font, 100.0, None).width;
                let tab_w = 40.0 + nw + 18.0 + cw;
                let rect = CGRect::new(CGPoint::new(x, 12.0), CGSize::new(tab_w, 40.0));
                if f == active {
                    // Selected segment: an accent-tinted pill. Unselected
                    // segments carry no chrome at all.
                    let patch = CGRect::new(CGPoint::new(x, 13.0), CGSize::new(tab_w, 38.0));
                    theme::accent_tint().setFill();
                    theme::rounded_rect(patch, 19.0).fill();
                }
                let tint = if f == active { theme::accent() } else { theme::label_secondary() };
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
            if let Some(img) = card::symbol("magnifyingglass", 14.0, SymbolWeight::Regular, &theme::label_tertiary()) {
                let sz = img.size();
                card::draw_image(
                    &img,
                    CGRect::new(CGPoint::new(gx + 14.0, 32.0 - sz.height / 2.0), sz),
                    objc2_app_kit::NSCompositingOperation::SourceOver,
                    1.0,
                );
            }
            // The box's position is set in `layout` above. A frame written
            // here would only apply on the passes that actually reach this
            // point, and the pane branch above returns before it.
            let has_query = !self.ivars().query.borrow().is_empty();
            if has_query {
                if let Some(img) = card::symbol("xmark.circle.fill", 17.0, SymbolWeight::Regular, &theme::label_tertiary()) {
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
            theme::separator().setStroke();
            let stroke = theme::rounded_rect(
                CGRect::new(CGPoint::new(gx, 12.0), CGSize::new(400.0, 40.0)),
                theme::radius_control(),
            );
            stroke.setLineWidth(1.0);
            stroke.stroke();
            // Close button.
            if let Some(img) = card::symbol("xmark", 15.0, SymbolWeight::Regular, &theme::label()) {
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
                HitZone::Action(action) => run_action(action, &item),
                HitZone::TitleCheck => finalize_rename_click(&item),
                HitZone::Title => title_click(&item, event),
                HitZone::Thumb => crate::shelf::image_viewer::open(&item.id),
                HitZone::Body => {
                    if event.clickCount() >= 2 {
                        with_model_mut(|m| m.copy_and_close(&item, true));
                        // The model only queued the close: draw the copy first,
                        // then run it once the borrow is gone.
                        reload();
                        crate::shelf::panel::finish_pending_close();
                        return;
                    }
                    with_model_mut(|m| m.copy(&item));
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
            // Work out the target inside the borrow, scroll outside it: a
            // scroll re-lays the clip synchronously and the header's layout
            // pass re-enters `with_ui`.
            let plan = with_ui(|ui| {
                let scroll = ui.scroll.clone()?;
                let clip = scroll.contentView();
                let doc_w = scroll
                    .documentView()
                    .map(|d| d.frame().size.width)
                    .unwrap_or(0.0);
                let max_x = (doc_w - clip.bounds().size.width).max(0.0);
                // Flipped or not, horizontal follows the vertical convention:
                // content towards the user (delta > 0) walks back to the start.
                let x = (clip.bounds().origin.x - delta).clamp(0.0, max_x);
                (x != clip.bounds().origin.x).then_some((scroll, x))
            });
            if let Some((scroll, x)) = plan {
                let clip = scroll.contentView();
                clip.scrollToPoint(CGPoint::new(x, clip.bounds().origin.y));
                scroll.reflectScrolledClipView(&clip);
            }
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
    /// The picture itself: a click opens it full size.
    Thumb,
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
            // The picture answers before the card body: clicking it opens the
            // full-size look, clicking anywhere else on the card still copies.
            // Image cards only — a video's payload is not something `NSImage`
            // can show, and a card with no thumbnail yet (text / link / file,
            // or one still decoding) keeps its whole body as a copy target.
            if c.item.kind == ClipKind::Image {
                if let Some(img) = &c.thumb {
                    let top = if c.has_title_row() {
                        card::title_row_rect(ticket).max().y
                    } else {
                        ticket.min().y + card::BODY_TOP
                    };
                    let title = c
                        .item
                        .title
                        .as_deref()
                        .map(|t| !t.is_empty())
                        .unwrap_or(false);
                    if contains(&card::photo_rect(ticket, top, title, img), p) {
                        return Some((c.index, HitZone::Thumb));
                    }
                }
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
        crate::shelf::panel::finish_pending_close();
        return;
    }
    with_model_mut(|m| {
        m.cancel_pending_hand_back();
        m.set_selected_id(Some(item.id.clone()));
        m.renaming_id = Some(item.id.clone());
    });
    reload();
}

/// Open whatever this card's edit/preview action leads to.
///
/// Text/URL and image open editor windows that hide the shelf; a video has no
/// editor and goes to Quick Look. Never call this with a model borrow held: the
/// editors and `toggle_quick_look` all read the model themselves, and a second
/// `borrow_mut` on the same `RefCell` aborts the process (the same rule the
/// pin-delete confirm follows).
fn open_editor_or_preview(item: &ClipItem) {
    match item.kind {
        ClipKind::Text | ClipKind::Url => crate::shelf::text_editor::TextEditorWindow::open(item),
        ClipKind::Image => crate::shelf::image_editor::ImageEditorWindow::open(item),
        _ => crate::shelf::panel::toggle_quick_look(),
    }
}

/// Action-strip dispatch (the Swift card's stub buttons).
fn run_action(action: card::ActionKind, item: &ClipItem) {
    match action {
        card::ActionKind::Edit | card::ActionKind::Preview => {
            // Select inside the borrow, then let it go: the preview resolves
            // its file from the model, so running it in here re-enters the
            // RefCell and aborts the process.
            with_model_mut(|m| m.select_for_action(item));
            open_editor_or_preview(item);
        }
        card::ActionKind::Pin => {
            with_model_mut(|m| m.with_store_mut(|s| s.toggle_pin(&item.id, true)));
        }
        card::ActionKind::Save => {
            // The save panel is modal: run it with no model borrow held, or the
            // poll timer's reload inside it aborts the process.
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
                // Only record it: the poll tick starts the search
                // (`serviceSearch`). Spawning here would hand the main thread
                // the join of the job the next keystroke cancels.
                with_model_mut(|m| m.set_query(q));
            });
            reload();
        }

        #[unsafe(method(controlTextDidBeginEditing:))]
        fn began(&self, _n: &NSNotification) {
            // Clone the field out of the cell first. `update_placeholder` and
            // `tint_caret` talk to AppKit, and AppKit answers a call made from
            // inside a delegate with further delegate callbacks
            // (`controlTextDidEndEditing:` among them, as the click path shows).
            // Running those while the cell is borrowed makes the re-entrant
            // `with_ui` panic; only the clone may happen under the borrow.
            let Some(field) = with_ui(|ui| ui.search.clone()) else { return };
            update_placeholder(&field, false);
            tint_caret(&field);
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn ended(&self, _n: &NSNotification) {
            let Some(field) = with_ui(|ui| ui.search.clone()) else { return };
            update_placeholder(&field, true);
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
            text_view.setInsertionPointColor(Some(&theme::accent()));
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
            reload();
            crate::shelf::panel::finish_pending_close();
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

        // Both of these act after the model borrow is released: the editors and
        // the preview panel read the model themselves, and doing that inside
        // this borrow re-enters the RefCell and aborts the process.
        #[unsafe(method(editItem:))]
        fn edit_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| m.select_for_action(&item));
            open_editor_or_preview(&item);
        }

        #[unsafe(method(previewItem:))]
        fn preview_item(&self, _s: &AnyObject) {
            let item = self.ivars().item.borrow().clone();
            with_model_mut(|m| m.select_for_action(&item));
            open_editor_or_preview(&item);
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
                if let Some(img) = card::symbol(name, 13.0, SymbolWeight::Regular, &theme::label_tertiary()) {
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
                theme::label().colorWithAlphaComponent(alpha).setFill();
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
        // Same split as the wheel: the arithmetic happens under the borrow,
        // the clip write (which re-enters `with_ui` through layout) does not.
        let plan = with_ui(|ui| {
            let scroll = ui.scroll.clone()?;
            let thumb = (40.0_f64).max(geo_w * ui.scroll_visible);
            let travel = (geo_w - thumb).max(0.0);
            let f = if travel > 0.0 {
                ((p.x - 28.0 - thumb / 2.0) / travel).clamp(0.0, 1.0)
            } else {
                0.0
            };
            Some((scroll, f * ui.scroll_range))
        });
        if let Some((scroll, x)) = plan {
            scroll.contentView().setBoundsOrigin(CGPoint::new(x, 0.0));
        }
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
///
/// Stands down while a system dialog is up. The dialog runs its own nested
/// event loop, so this — driven by the 0.25 s poll timer, which common modes
/// keep firing — lands in the middle of it: repainting under a modal sheet
/// would steal the keyboard back, and any caller still holding the model
/// borrow would be borrowed twice and abort the process.
pub(crate) fn refresh() {
    // Deferred panel rebuilds first: a settings click asks for one rather than
    // doing it inline, because it would otherwise destroy the view running the
    // click (see `settings_pane::PanelCommand`). This is the earliest point
    // that is always outside that dispatch.
    apply_panel_commands();
    if crate::shelf::panel::dialog_open() {
        return;
    }
    if with_ui(|ui| ui.built) {
        reload();
    }
}

/// Run whatever a settings click deferred, in order.
///
/// Nothing here may run inside the click's own `mouseUp:`: both commands swap
/// the panel's content view, which deallocates the old tree — the view whose
/// method would still be on the stack. Safe from `refresh` (the poll tick and
/// the notify hook) and from the main queue right after the event returns.
///
/// The rebuild is done in place (`panel::retheme`) rather than through
/// `hide()`/`show()`: `show()` resets the model, which would close the very
/// settings pane the click came from — and switching the language or the skin
/// must not throw the user out of the page they were on.
pub fn apply_panel_commands() {
    let cmds = crate::shelf::settings_pane::take_panel_commands();
    if cmds.is_empty() {
        return;
    }
    // A fresh tree opens on the first settings page; carry the user's page over.
    let tab = settings_view().map(|v| crate::shelf::settings_pane::current_tab(&v));
    // A form swap (`Relayout`) rebuilds the window *and* its frame; a skin or
    // language swap (`Reskin`/`Relanguage`) only the tree, keeping the frame so
    // the shelf does not jump. `panel::relayout` sorts the two forms out.
    if cmds
        .iter()
        .any(|c| matches!(c, crate::shelf::settings_pane::PanelCommand::Relayout))
    {
        crate::shelf::panel::relayout();
    } else {
        crate::shelf::panel::retheme();
    }
    if let (Some(tab), Some(view)) = (tab, settings_view()) {
        crate::shelf::settings_pane::set_tab(&view, tab);
    }
}

/// The live root view, when the tree is built.
pub(crate) fn root_view() -> Option<Retained<ShelfRootView>> {
    with_ui(|ui| ui.root.clone())
}

/// Drive the first video card through the same route its "查看" button takes
/// (`hit_card` → `HitZone::Action` → `run_action`), for `--selftest
/// previewvideo`. False when the shelf holds no video card.
///
/// This is the exact call that used to abort: the preview resolves the
/// selected file from the model, so running it inside a model borrow re-entered
/// the RefCell. A test that only opened the data source a different way would
/// not have caught that.
pub(crate) fn debug_preview_first_video() -> bool {
    let index = with_ui(|ui| {
        ui.cards
            .iter()
            .find(|c| c.item.kind == ClipKind::Video)
            .map(|c| c.index)
    });
    let Some(index) = index else { return false };
    let Some(item) = card_item(index) else { return false };
    run_action(card::ActionKind::Preview, &item);
    true
}

/// Drive the first image card's picture through the same route a real click
/// takes (`hit_card` → `HitZone::Thumb`), for `--selftest viewer`. False when
/// no image card has a decoded thumbnail, which is what the caller waits on.
///
/// The seed only runs on an empty store (`seed_store_path`), and once the
/// shelf's default retention has trimmed the older cards away there may be no
/// image card left — so a viewer render needs a fresh `PASTORY_STORE`.
pub(crate) fn debug_click_first_thumbnail() -> bool {
    let row = match with_ui(|ui| (ui.row.clone(), ui.cards.iter().find(|c| c.item.kind == ClipKind::Image && c.thumb.is_some()).map(|c| c.index))) {
        (Some(row), Some(index)) => (row, index),
        _ => return false,
    };
    let (row, index) = row;
    let p = match with_ui(|ui| {
        let c = ui.cards.get(index)?;
        let ticket = ticket_rect(row.bounds().size.height, c.index, c.selected);
        let img = c.thumb.as_ref()?;
        let top = if c.has_title_row() {
            card::title_row_rect(ticket).max().y
        } else {
            ticket.min().y + card::BODY_TOP
        };
        let title = c.item.title.as_deref().map(|t| !t.is_empty()).unwrap_or(false);
        let r = card::photo_rect(ticket, top, title, img);
        Some(CGPoint::new(r.mid().x, r.mid().y))
    }) {
        Some(p) => p,
        None => return false,
    };
    match hit_card(&row, p) {
        Some((i, HitZone::Thumb)) => {
            if let Some(item) = card_item(i) {
                crate::shelf::image_viewer::open(&item.id);
                return true;
            }
            false
        }
        _ => false,
    }
}

/// Self-test hook: the centre of `action`'s button on card `index`, in the
/// cards row's own coordinates — the same space `hit_card` tests against, so
/// a click there must land on that button.
pub(crate) fn selftest_action_center(index: usize, action: card::ActionKind) -> Option<CGPoint> {
    let row = with_ui(|ui| ui.row.clone())?;
    let (kind, selected) =
        with_ui(|ui| ui.cards.get(index).map(|c| (c.item.kind, c.selected)))?;
    let ticket = ticket_rect(row.bounds().size.height, index, selected);
    card::action_rects(ticket, kind)
        .into_iter()
        .find(|(k, _)| *k == action)
        .map(|(_, r)| r.mid())
}

/// Self-test hook: the centre of card `index`'s title row, in the cards row's
/// own coordinates — the same space `hit_card` and the inline editor's frame
/// are computed in, so a click there must land on the drawn title. `None` when
/// the card has no title row.
pub(crate) fn selftest_title_row_center(index: usize) -> Option<CGPoint> {
    let row = with_ui(|ui| ui.row.clone())?;
    let (selected, titled) = with_ui(|ui| {
        ui.cards.get(index).map(|c| (c.selected, c.has_title_row()))
    })?;
    if !titled {
        return None;
    }
    let r = card::title_row_rect(ticket_rect(row.bounds().size.height, index, selected));
    Some(CGPoint::new(r.mid().x.round(), r.mid().y.round()))
}

/// Self-test hook: a point in the cards row's own coordinates, expressed in the
/// row's window space — what a real click has to carry. The conversion walks the
/// whole superview chain, so the scroll view's bounds origin is included.
pub(crate) fn selftest_row_point_in_window(p: CGPoint) -> Option<CGPoint> {
    let row = with_ui(|ui| ui.row.clone())?;
    Some(row.convertPoint_toView(p, None))
}

/// Self-test hook: deliver a click at `p` (cards-row coordinates) through the
/// real `mouseDown:` / `mouseUp:` route, so hit-testing, action dispatch and
/// the reload that follows all run exactly as they do for a user's click.
/// Returns false when the tree or its window is missing.
pub(crate) fn selftest_click_at(p: CGPoint) -> bool {
    let Some(row) = with_ui(|ui| ui.row.clone()) else {
        return false;
    };
    let Some(window) = row.window() else {
        return false;
    };
    let loc = row.convertPoint_toView(p, None);
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
    // SAFETY: `CardsRowView` is an NSView subclass; this is the same dispatch
    // AppKit performs for a real click in the row.
    let row_obj: &AnyObject = unsafe { &*(&*row as *const CardsRowView as *const AnyObject) };
    unsafe {
        let down = event(objc2_app_kit::NSEventType::LeftMouseDown);
        let up = event(objc2_app_kit::NSEventType::LeftMouseUp);
        let _: () = msg_send![row_obj, mouseDown: &*down];
        let _: () = msg_send![row_obj, mouseUp: &*up];
    }
    true
}

/// Self-test hook: the centre of sidebar nav row `i` (0 clipboard, 1 settings,
/// 2 contact) in window coordinates, so a self-test can aim a real click at
/// it. `None` before the tree is built. The row geometry mirrors the draw and
/// mouseUp code above on purpose: one formula, three readers.
pub(crate) fn selftest_sidebar_row_center(i: usize) -> Option<CGPoint> {
    let sidebar = with_ui(|ui| ui.sidebar.clone())?;
    let brand_font = theme::brand_font(BRAND_SIZE);
    let rows_top = 10.0 + card::line_height(&brand_font) + 22.0;
    let center = CGPoint::new(SIDEBAR_W / 2.0, rows_top + i as f64 * 60.0 + 30.0);
    Some(sidebar.convertPoint_toView(center, None))
}

/// Self-test hook: deliver a click at `p` (window coordinates) straight to
/// the sidebar's `mouseDown:`/`mouseUp:` — the same dispatch a real click in
/// the sidebar takes, without going through the window server.
pub(crate) fn selftest_sidebar_click_at(p: CGPoint) -> bool {
    let Some(sidebar) = with_ui(|ui| ui.sidebar.clone()) else {
        return false;
    };
    let Some(window) = sidebar.window() else {
        return false;
    };
    let number = window.windowNumber();
    let event = |kind| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            p,
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
    // SAFETY: `SidebarView` is an NSView subclass; this is the same dispatch
    // AppKit performs for a real click in the sidebar.
    let obj: &AnyObject = unsafe { &*(&*sidebar as *const SidebarView as *const AnyObject) };
    unsafe {
        let down = event(objc2_app_kit::NSEventType::LeftMouseDown);
        let up = event(objc2_app_kit::NSEventType::LeftMouseUp);
        let _: () = msg_send![obj, mouseDown: &*down];
        let _: () = msg_send![obj, mouseUp: &*up];
    }
    true
}

/// Bump the search field into first responder and flush pending keystrokes
/// (⌘F / showSearch / direct typing).
pub(crate) fn focus_search_now() {
    let field = with_ui(focus_search_plan);
    if let Some(field) = field {
        let root = with_ui(|ui| ui.root.clone());
        if let Some(w) = root.as_ref().and_then(|r| r.window()) {
            w.makeFirstResponder(Some(&field));
        }
    }
}

/// Drop keyboard focus after show() (the caret must not sit in the box).
pub(crate) fn drop_focus_now() {
    let root = with_ui(|ui| ui.root.clone());
    if let Some(w) = root.as_ref().and_then(|r| r.window()) {
        w.makeFirstResponder(None);
    }
}

//! Port of the `ShelfModel` half of `Shelf/ShelfPanel.swift:287-546` — the
//! shelf's data, search and selection logic, without the AppKit half of that
//! file (panel, controller, Quick Look, paste-into-previous-app), which is
//! the UI slice of M2. Effects the Swift model performs through the
//! controller are marked at their call sites with a slice-C anchor.
//!
//! The Swift model is `@MainActor` with SwiftUI-driven revalidation. The
//! port drives the same pipeline through two explicit drivers:
//! `update_search_blocking` (synchronous, used by self-tests) and
//! `begin_search` (background thread; the UI drains completions when the
//! notification callback fires).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::preferences::Preferences;
use crate::clipboard::item::{now, ClipItem, ClipKind};
use crate::clipboard::search_index::{Cancelled, ClipSearchIndex};
use crate::clipboard::store::ClipStore;

/// `ShelfFilter` — the tab categories. Raw values are the Swift storage
/// strings; the Pin row has no translation entry there either.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ShelfFilter {
    All,
    Pinned,
    Images,
    Videos,
    Text,
}

impl ShelfFilter {
    /// Swift `CaseIterable` order.
    pub const ALL: [ShelfFilter; 5] = [
        ShelfFilter::All,
        ShelfFilter::Pinned,
        ShelfFilter::Images,
        ShelfFilter::Videos,
        ShelfFilter::Text,
    ];

    pub fn raw(&self) -> &'static str {
        match self {
            ShelfFilter::All => "全部",
            ShelfFilter::Pinned => "Pin",
            ShelfFilter::Images => "图片",
            ShelfFilter::Videos => "录屏",
            ShelfFilter::Text => "文本",
        }
    }
}

/// `ShelfModel.SearchRequest` — the normalized query pinned to a store
/// version; a store write or a keystroke makes the old request obsolete.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SearchRequest {
    pub normalized_query: String,
    pub store_version: u64,
}

struct SearchResult {
    request: SearchRequest,
    ids: HashSet<String>,
}

/// Totals per filter (ignoring the search box), for the pills — one pass
/// over the store, not five; cached against the store version.
#[derive(Clone, Copy, Default, Debug)]
pub struct FilterCounts {
    pub all: usize,
    pub pinned: usize,
    pub images: usize,
    pub videos: usize,
    pub text: usize,
}

impl FilterCounts {
    pub fn get(&self, filter: ShelfFilter) -> usize {
        match filter {
            ShelfFilter::All => self.all,
            ShelfFilter::Pinned => self.pinned,
            ShelfFilter::Images => self.images,
            ShelfFilter::Videos => self.videos,
            ShelfFilter::Text => self.text,
        }
    }
}

#[derive(Clone, PartialEq)]
struct ListKey {
    request: SearchRequest,
    snapshot: u64,
    filter: ShelfFilter,
}

/// Where the model reads its store from: the process-wide singleton
/// (`ClipStore.shared`) or one owned instance (Swift `ShelfModel(store:)`;
/// the search self-test drives a scratch store).
pub enum StoreHandle {
    Shared,
    Owned(ClipStore),
}

/// A finished background search waiting for the main side to publish it.
type Completion = (SearchRequest, Result<HashSet<String>, Cancelled>);

/// `updateSearch`'s running task. Dropping a job cancels and joins it, so no
/// search outlives its owner; `cancel` arms the flag the index checks.
pub struct SearchJob {
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SearchJob {
    fn done() -> SearchJob {
        SearchJob {
            cancel: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }

    /// `Task.cancel`: checked before the scan, per item, around the payload
    /// reads, and before publish.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// `await task.value` — wait for the thread to end (its completion has
    /// already been queued; cancelling does not push results).
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for SearchJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub struct ShelfModel {
    handle: StoreHandle,
    index: Arc<ClipSearchIndex>,
    query: String,
    /// True once the highlight moved by arrow keys or a click. The first
    /// search hit is highlighted automatically; hitting Return on that only
    /// copies, it never types into another app.
    picked_by_hand: bool,
    filter: ShelfFilter,
    /// Settings pane up instead of the shelf (M6 builds the pane itself; the
    /// flag, nav row and ⎋/⌘F handling are slice C).
    pub show_settings: bool,
    /// Contact pane up (M6 pane; flag slice C).
    pub show_contact: bool,
    /// First launch until 「开始使用」 is pressed; the welcome card leads the
    /// row (M6 card; the flag exists for the render commands).
    pub show_welcome: bool,
    /// The highlight moves UI-side; the setter hooks what Swift's didSet
    /// does (Quick Look reload + closing an open rename box).
    selected_id: Option<String>,
    /// Bumped to drop keyboard focus into the search field (the view's job).
    pub focus_search: u64,
    /// Characters typed while nothing was focused; the search field takes
    /// them once it has focus.
    pub pending_query: String,
    /// Card whose title is being edited inline.
    pub renaming_id: Option<String>,
    /// Bumped on every show(); the view uses it to drop keyboard focus so
    /// the caret does not sit in the search box.
    pub open_tick: u64,
    /// Card order is frozen while the shelf is open, so copying (which bumps
    /// the item in the store) does not make cards jump around. Rebuilt on
    /// every show.
    order_snapshot: HashMap<String, usize>,
    snapshot_id: u64,
    count_cache: Option<(u64, FilterCounts)>,
    ordered_cache: (u64, u64, Vec<ClipItem>),
    search_result: Option<SearchResult>,
    list_cache: Option<(ListKey, Vec<ClipItem>)>,
    last_shown: Vec<ClipItem>,
    completions: Arc<Mutex<VecDeque<Completion>>>,
    /// Set by the UI slice: called on the main queue after a background
    /// search lands, to drain completions and redraw.
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
    /// `initialWarmup`: the launch-time cache warmer, cancelled by the first
    /// view-owned search.
    warmup: Option<SearchJob>,
    /// `lastCloseAt`: the copy-and-close 0.6 s re-entry guard.
    last_close_at: f64,
    /// `langTick`: bumped when the language changes; the view rebuilds.
    pub lang_tick: u64,
    /// `pendingHandBack`: generation of the scheduled keyboard hand-back; a
    /// newer copy (or copy-and-close) retires the older one by bumping this.
    pending_hand_back: u64,
}

impl ShelfModel {
    fn with_handle(handle: StoreHandle) -> ShelfModel {
        ShelfModel {
            handle,
            index: Arc::new(ClipSearchIndex::new()),
            query: String::new(),
            picked_by_hand: false,
            filter: ShelfFilter::All,
            show_settings: false,
            show_contact: false,
            // Swift reads `!Preferences.shared.didWelcome` here; the welcome
            // card is M6, and the render commands force the flag off.
            show_welcome: false,
            selected_id: None,
            focus_search: 0,
            pending_query: String::new(),
            renaming_id: None,
            open_tick: 0,
            order_snapshot: HashMap::new(),
            snapshot_id: 0,
            count_cache: None,
            ordered_cache: (0, u64::MAX, Vec::new()),
            search_result: None,
            list_cache: None,
            last_shown: Vec::new(),
            completions: Arc::new(Mutex::new(VecDeque::new())),
            notify: None,
            warmup: None,
            last_close_at: 0.0,
            pending_hand_back: 0,
            lang_tick: 0,
        }
    }

    /// `ShelfModel(store: nil)` — over the process-wide store.
    pub fn shared() -> ShelfModel {
        ShelfModel::with_handle(StoreHandle::Shared)
    }

    /// `ShelfModel(store: store)` — over a store of its own.
    pub fn owned(store: ClipStore) -> ShelfModel {
        ShelfModel::with_handle(StoreHandle::Owned(store))
    }

    pub fn with_store<R>(&self, f: impl FnOnce(&ClipStore) -> R) -> R {
        match &self.handle {
            StoreHandle::Shared => crate::clipboard::store::read(f),
            StoreHandle::Owned(s) => f(s),
        }
    }

    pub fn with_store_mut<R>(&mut self, f: impl FnOnce(&mut ClipStore) -> R) -> R {
        match &mut self.handle {
            StoreHandle::Shared => crate::clipboard::store::with(f),
            StoreHandle::Owned(s) => f(s),
        }
    }

    /// Slice C installs the redraw hook (dispatched on the main queue when a
    /// background search lands).
    pub fn set_notify(&mut self, notify: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.notify = notify;
    }

    /// Slice C: decoded thumbnail through the model's store
    /// (`ClipStore.thumbnail`); warms in the background when absent.
    pub fn thumbnail(
        &mut self,
        item: &ClipItem,
    ) -> Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>> {
        self.with_store_mut(|s| s.thumbnail(item))
    }

    /// `pendingHandBack` generation for the scheduled hand-back check.
    pub fn pending_hand_back_generation(&self) -> u64 {
        self.pending_hand_back
    }

    // MARK: Query / filter / counts

    pub fn query(&self) -> &str {
        &self.query
    }

    /// `query` didSet: a changed query drops the hand-picked flag (a fresh
    /// auto highlight is not a deliberate pick).
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = query.into();
        if query != self.query {
            self.picked_by_hand = false;
        }
        self.query = query;
    }

    pub fn picked_by_hand(&self) -> bool {
        self.picked_by_hand
    }

    pub fn filter(&self) -> ShelfFilter {
        self.filter
    }

    /// `filter` didSet: reselect inside the new list.
    pub fn set_filter(&mut self, filter: ShelfFilter) {
        self.filter = filter;
        self.select_available_item();
    }

    /// `counts` — cached per store version.
    pub fn counts(&mut self) -> FilterCounts {
        let version = self.with_store(|s| s.version);
        if let Some((v, c)) = self.count_cache {
            if v == version {
                return c;
            }
        }
        let mut c = FilterCounts::default();
        self.with_store(|s| {
            for it in &s.items {
                c.all += 1;
                if it.pinned {
                    c.pinned += 1;
                }
                match it.kind {
                    ClipKind::Image => c.images += 1,
                    ClipKind::Video => c.videos += 1,
                    ClipKind::Text | ClipKind::Url => c.text += 1,
                    ClipKind::Files => {}
                }
            }
        });
        self.count_cache = Some((version, c));
        c
    }

    /// Store items in frozen shelf order, re-sorted only when the store or
    /// the snapshot changed (`orderedItems`).
    fn ordered_items(&mut self) -> Vec<ClipItem> {
        let version = self.with_store(|s| s.version);
        let snapshot = self.snapshot_id;
        if self.ordered_cache.0 == version && self.ordered_cache.1 == snapshot {
            return self.ordered_cache.2.clone();
        }
        // The items array is already date-sorted; the stable sort keeps that
        // order among cards the snapshot does not know yet.
        let mut sorted = self.with_store(|s| s.items.clone());
        sorted.sort_by_key(|it| {
            self.order_snapshot.get(&it.id).copied().unwrap_or(usize::MAX)
        });
        self.ordered_cache = (version, snapshot, sorted.clone());
        sorted
    }

    pub fn search_request(&self) -> SearchRequest {
        SearchRequest {
            normalized_query: ClipSearchIndex::normalized_query(&self.query),
            store_version: self.with_store(|s| s.version),
        }
    }

    pub fn is_searching(&self) -> bool {
        let request = self.search_request();
        !request.normalized_query.is_empty()
            && self.search_result.as_ref().map(|r| &r.request) != Some(&request)
    }

    /// The currently displayed list (`items`). While the next result is on
    /// its way the previous list stays on screen, so the shelf does not
    /// blink to empty between keystrokes — but as live store values: a
    /// deleted card leaves at once, even mid-search.
    pub fn items(&mut self) -> Vec<ClipItem> {
        let request = self.search_request();
        if !request.normalized_query.is_empty()
            && self.search_result.as_ref().map(|r| &r.request) != Some(&request)
        {
            let live: HashMap<String, ClipItem> = self.with_store(|s| {
                s.items
                    .iter()
                    .map(|it| (it.id.clone(), it.clone()))
                    .collect()
            });
            return self
                .last_shown
                .iter()
                .filter_map(|it| live.get(&it.id).cloned())
                .collect();
        }
        let key = ListKey {
            request: request.clone(),
            snapshot: self.snapshot_id,
            filter: self.filter,
        };
        if let Some((k, items)) = &self.list_cache {
            if *k == key {
                return items.clone();
            }
        }
        let ordered = self.ordered_items();
        let ids = self.search_result.as_ref().map(|r| &r.ids);
        let filtered: Vec<ClipItem> = ordered
            .into_iter()
            .filter(|item| {
                match self.filter {
                    ShelfFilter::All => {}
                    ShelfFilter::Pinned => {
                        if !item.pinned {
                            return false;
                        }
                    }
                    ShelfFilter::Images => {
                        if item.kind != ClipKind::Image {
                            return false;
                        }
                    }
                    ShelfFilter::Videos => {
                        if item.kind != ClipKind::Video {
                            return false;
                        }
                    }
                    ShelfFilter::Text => {
                        if item.kind != ClipKind::Text && item.kind != ClipKind::Url {
                            return false;
                        }
                    }
                }
                request.normalized_query.is_empty()
                    || ids.map(|s| s.contains(&item.id)).unwrap_or(false)
            })
            .collect();
        self.list_cache = Some((key, filtered.clone()));
        self.last_shown = filtered.clone();
        filtered
    }

    // MARK: Search pipeline

    fn spawn(&self, query: String, publish: bool) -> SearchJob {
        let index = self.index.clone();
        let (items, directory) =
            self.with_store(|s| (s.items.clone(), s.root.join("items")));
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let completions = publish.then(|| self.completions.clone());
        let notify = self.notify.clone();
        let request = self.search_request();
        let handle = std::thread::spawn(move || {
            // Swift reads `store.items` after the 80 ms debounce; here the
            // list is captured up front — a store write mid-flight bumps the
            // version and the publish guard rejects the stale outcome.
            if !query.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            let outcome = index
                .search(&query, &items, &directory, &thread_cancel)
                .and_then(|ids| {
                    // `Task.checkCancellation()` after the scan: a cancelled
                    // search never reaches the publish queue as a result.
                    if thread_cancel.load(Ordering::SeqCst) {
                        Err(Cancelled)
                    } else {
                        Ok(ids)
                    }
                });
            if let Some(queue) = completions {
                queue.lock().expect("completions poisoned").push_back((request, outcome));
                if let Some(n) = notify {
                    crate::app::delegate::dispatch_main_after(0.0, Box::new(move || n()));
                }
            }
        });
        SearchJob {
            cancel,
            handle: Some(handle),
        }
    }

    /// Start at app launch even if the shelf has not appeared yet
    /// (`prewarmSearch`). An empty query warms the full text in the
    /// background; nothing is published to the result slot.
    pub fn prewarm_search(&mut self) {
        self.warmup = None; // replacing a running warmup cancels it
        self.warmup = Some(self.spawn(String::new(), false));
    }

    /// The Swift view cancels the warmup when its first search takes over.
    fn cancel_warmup(&mut self) {
        self.warmup = None;
    }

    /// `updateSearch` without the actor hop and the 80 ms wait — the
    /// self-test drives the whole pipeline synchronously.
    pub fn update_search_blocking(&mut self) {
        self.cancel_warmup();
        let request = self.search_request();
        if self.search_result.as_ref().map(|r| &r.request) == Some(&request) {
            return;
        }
        let (items, directory) =
            self.with_store(|s| (s.items.clone(), s.root.join("items")));
        let never = AtomicBool::new(false);
        if let Ok(ids) = self
            .index
            .search(&request.normalized_query, &items, &directory, &never)
        {
            // Publish only while the request is still current (Swift's guard).
            if self.search_request() == request {
                self.search_result = Some(SearchResult { request, ids });
                self.select_available_item();
            }
        }
    }

    /// The SwiftUI-driven `updateSearch`: settle a burst of typing for
    /// 80 ms, scan off the main thread, queue the outcome for the main side.
    /// Cancelling the returned job is `Task.cancel`.
    pub fn begin_search(&mut self) -> SearchJob {
        self.cancel_warmup();
        let request = self.search_request();
        if self.search_result.as_ref().map(|r| &r.request) == Some(&request) {
            return SearchJob::done();
        }
        self.spawn(request.normalized_query.clone(), true)
    }

    /// Main-queue side of `updateSearch`: publish finished jobs whose
    /// request is still current; a cancelled job's outcome is dropped (a
    /// newer query owns the result — never publish the obsolete one).
    pub fn drain_completions(&mut self) {
        loop {
            let next = self
                .completions
                .lock()
                .expect("completions poisoned")
                .pop_front();
            let Some((request, outcome)) = next else { break };
            let Ok(ids) = outcome else { continue };
            if self.search_request() == request {
                self.search_result = Some(SearchResult { request, ids });
                self.select_available_item();
            }
        }
    }

    // MARK: Selection

    fn select_available_item(&mut self) {
        let list = self.items();
        if !list.iter().any(|it| Some(&it.id) == self.selected_id.as_ref()) {
            self.put_selected(list.first().map(|it| it.id.clone()));
            self.picked_by_hand = false;
        }
    }

    /// The highlighted card id (`selectedID`).
    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    /// `selectedID = …` — the Swift didSet hooks ride along: Quick Look
    /// reloads when its panel is already up, and moving on closes an open
    /// title box.
    pub fn set_selected_id(&mut self, id: Option<String>) {
        self.put_selected(id);
    }

    fn put_selected(&mut self, id: Option<String>) {
        self.selected_id = id.clone();
        // QL asks for the data on its own tick, so a deferred reload matches
        // the Swift call site (and never re-enters the model mid-mutation).
        crate::shelf::panel::quick_look_selection_changed();
        if let Some(r) = &self.renaming_id {
            if Some(r) != id.as_ref() {
                self.renaming_id = None;
            }
        }
    }

    /// After bulk changes (import, remove-imported) the frozen order is
    /// stale: freeze the store's current order again (`refreshOrder`).
    pub fn refresh_order(&mut self) {
        let items = self.with_store(|s| s.items.clone());
        self.order_snapshot = items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.id.clone(), i))
            .collect();
        self.snapshot_id += 1;
        let version = self.with_store(|s| s.version);
        self.ordered_cache = (version, self.snapshot_id, items);
    }

    /// `reset()` — the shelf opening state.
    pub fn reset(&mut self) {
        self.set_query(String::new());
        self.show_settings = false;
        self.show_contact = false;
        self.renaming_id = None;
        self.open_tick += 1;
        self.refresh_order();
        self.set_filter(ShelfFilter::All);
        let first = self.with_store(|s| s.items.first().map(|it| it.id.clone()));
        self.put_selected(first);
        self.picked_by_hand = false;
    }

    /// `move(_:)` — arrow-key navigation.
    pub fn move_selection(&mut self, delta: i64) {
        let list = self.items();
        if list.is_empty() {
            return;
        }
        let i = list
            .iter()
            .position(|it| Some(&it.id) == self.selected_id.as_ref())
            .map(|p| p as i64)
            .unwrap_or(-1);
        let n = (i + delta).clamp(0, list.len() as i64 - 1);
        self.put_selected(Some(list[n as usize].id.clone()));
        self.picked_by_hand = true;
    }

    /// The highlighted card, and only that; keys never fall back to the
    /// first card silently. Nil while searching: never act on a list that is
    /// about to change.
    fn selected(&mut self) -> Option<ClipItem> {
        if self.is_searching() {
            return None;
        }
        let id = self.selected_id.clone()?;
        self.items().into_iter().find(|it| it.id == id)
    }

    pub fn selected_item(&mut self) -> Option<ClipItem> {
        self.selected()
    }

    // MARK: Actions

    /// `cancelPendingHandBack` — a newer copy supersedes the scheduled one.
    pub fn cancel_pending_hand_back(&mut self) {
        self.pending_hand_back = self.pending_hand_back.wrapping_add(1);
    }

    /// Single click: copy and stay (`copy(_:)`); the 已复制 tag moves to the
    /// card because the monitor bumps it.
    pub fn copy(&mut self, item: &ClipItem) {
        self.with_store_mut(|s| s.copy_to_pasteboard(item));
        self.put_selected(Some(item.id.clone()));
        self.picked_by_hand = true;
        // The copy and the highlight are immediate. Only the hand-back of the
        // keyboard waits out a possible second click, so a double-click still
        // finds the shelf exactly as it was.
        self.cancel_pending_hand_back();
        let generation = self.pending_hand_back;
        crate::app::delegate::dispatch_main_after(
            objc2_app_kit::NSEvent::doubleClickInterval(),
            Box::new(move || {
                crate::shelf::panel::hand_back_focus_if_current(generation);
            }),
        );
    }

    /// ⏎ / double-click: copy, close, and (with Accessibility) paste into
    /// the app you came from (`copyAndClose`). The 0.6 s guard stands: the
    /// shelf is already on its way out.
    pub fn copy_and_close(&mut self, item: &ClipItem, paste: bool) {
        let t = now();
        if t - self.last_close_at <= 0.6 {
            return;
        }
        self.last_close_at = t;
        self.copy(item);
        self.cancel_pending_hand_back();
        crate::shelf::panel::hide();
        if paste {
            crate::shelf::panel::paste_into_previous_app();
        }
    }

    /// Return: copy and close; paste too only when the setting allows it and
    /// the card was picked by hand.
    pub fn copy_selected(&mut self) {
        let Some(s) = self.selected() else { return };
        let paste = Preferences::shared().paste_on_return() && self.picked_by_hand;
        self.copy_and_close(&s, paste);
    }

    /// ␣ (`previewSelected`) — the Quick Look panel itself is slice C.
    pub fn preview_selected(&self) {
        crate::shelf::panel::toggle_quick_look();
    }

    /// Text → our editor window; image → the annotation editor
    /// (`ShelfPanelController.showEditor`).
    pub fn edit(&self, item: &ClipItem) {
        match item.kind {
            ClipKind::Text | ClipKind::Url => crate::shelf::text_editor::TextEditorWindow::open(item),
            ClipKind::Image => crate::shelf::image_editor::ImageEditorWindow::open(item),
            _ => self.preview_selected(),
        }
    }

    pub fn pin_selected(&mut self) {
        if let Some(s) = self.selected() {
            let id = s.id.clone();
            self.with_store_mut(|store| store.toggle_pin(&id, true));
        }
    }

    pub fn export_selected(&mut self) {
        let Some(s) = self.selected() else { return };
        crate::shelf::exporter::export(&s);
    }

    /// Only a card that is actually highlighted in the current list; never a
    /// silent fallback (`deleteSelected`).
    pub fn delete_selected(&mut self, confirm: impl FnOnce(&ClipItem) -> bool) {
        let Some(s) = self.selected() else { return };
        let list = self.items();
        let i = list.iter().position(|it| it.id == s.id).unwrap_or(0);
        if !self.delete(&s, confirm) {
            return;
        }
        let rest = self.items();
        let next = if rest.is_empty() {
            None
        } else {
            Some(rest[i.min(rest.len() - 1)].id.clone())
        };
        self.put_selected(next);
        self.picked_by_hand = false; // the neighbour was chosen for you
    }

    /// 「开始使用」 retires the welcome card for good (`finishWelcome`).
    pub fn finish_welcome(&mut self) {
        crate::app::preferences::Preferences::shared().set_did_welcome(true);
        self.show_welcome = false;
    }

    /// `noteWelcomeTried(tag)` — the onboarding checklist (welcome card).
    pub fn note_welcome_tried(&mut self, tag: &'static str) {
        let prefs = crate::app::preferences::Preferences::shared();
        let mut tried = prefs.welcome_tried();
        if tried.insert(tag.to_string()) {
            prefs.set_welcome_tried(&tried);
        }
    }

    /// Whether one checklist row already happened (`welcomeTried.contains`).
    pub fn welcome_tried_has(&self, tag: &str) -> bool {
        crate::app::preferences::Preferences::shared()
            .welcome_tried()
            .contains(tag)
    }

    /// The one way to delete from the shelf: a pinned card asks first
    /// (`confirm` shows the NSAlert in slice C), everything else goes
    /// straight away.
    pub fn delete(&mut self, item: &ClipItem, confirm: impl FnOnce(&ClipItem) -> bool) -> bool {
        if item.pinned && !confirm(item) {
            return false;
        }
        self.with_store_mut(|store| store.remove(&item.id));
        true
    }
}

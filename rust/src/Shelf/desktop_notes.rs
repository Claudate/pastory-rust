//! Port of `Shelf/DesktopNotes.swift` — cards pinned to the desktop as
//! sticky notes. A note is the same paper card in its own borderless
//! window, floating above every window or just above the desktop icons,
//! per the setting. Where you drop it is where it stays: positions are
//! remembered and restored at launch.
//!
//! The window-level drag moves resizes live here so no view gesture
//! swallows them; the card itself draws in `desktop_note_view.rs`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSPanel, NSScreen, NSView, NSWindowCollectionBehavior, NSWindowStyleMask};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::app::preferences::Preferences;
use crate::clipboard::store;
use crate::shelf::desktop_note_view::{NoteView, NOTE_W, NOTE_MARGIN, MIN_H, MIN_W, TEXT_CAP_FRACTION};

/// Floating above windows, or one step above the desktop icons (`level(top:)`).
fn level_for(top: bool) -> isize {
    if top {
        objc2_app_kit::NSFloatingWindowLevel
    } else {
        objc2_core_graphics::CGWindowLevelForKey(objc2_core_graphics::CGWindowLevelKey::DesktopIconWindowLevelKey) as isize + 1
    }
}

// MARK: Note window

pub struct NoteWindowIvars {
    id: RefCell<String>,
    view: RefCell<Option<Retained<NoteView>>>,
    on_top: RefCell<bool>,
    down_at: RefCell<Option<CGPoint>>,
    mode: RefCell<DragMode>,
    /// Window-level move/resize bookkeeping (plain data — the window's own state).
    resize_start: RefCell<(CGPoint, CGSize)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DragMode {
    Idle,
    Deciding,
    Moving,
    Resizing,
}

define_class!(
    // SAFETY: NSPanel + own sendEvent, main-thread only; one per note.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[ivars = NoteWindowIvars]
    pub struct NoteWindow;

    unsafe impl NSObjectProtocol for NoteWindow {}

    impl NoteWindow {
        #[unsafe(method(canBecomeKey))]
        fn ns_can_become_key(&self) -> objc2::runtime::Bool {
            false.into()
        }

        #[unsafe(method(canBecomeMain))]
        fn ns_can_become_main(&self) -> objc2::runtime::Bool {
            false.into()
        }

        #[unsafe(method(sendEvent:))]
        fn ns_send_event(&self, event: &NSEvent) {
            const GRIP: f64 = 22.0;
            match event.r#type() {
                objc2_app_kit::NSEventType::LeftMouseDown => {
                    *self.ivars().down_at.borrow_mut() = Some(NSEvent::mouseLocation());
                    let p = event.locationInWindow();
                    let f = self.frame();
                    let in_grip = p.x >= f.size.width - NOTE_MARGIN - GRIP && p.y <= NOTE_MARGIN + GRIP;
                    *self.ivars().mode.borrow_mut() = if in_grip {
                        *self.ivars().resize_start.borrow_mut() = (f.min(), f.size);
                        DragMode::Resizing
                    } else {
                        DragMode::Deciding
                    };
                    if in_grip {
                        return;
                    }
                }
                objc2_app_kit::NSEventType::LeftMouseDragged => {
                    let Some(down) = *self.ivars().down_at.borrow() else { return };
                    let now = NSEvent::mouseLocation();
                    let mode = *self.ivars().mode.borrow();
                    match mode {
                        DragMode::Deciding => {
                            let dx = now.x - down.x;
                            let dy = now.y - down.y;
                            if dx.hypot(dy) > 4.0 {
                                *self.ivars().mode.borrow_mut() = DragMode::Moving;
                                self.performWindowDragWithEvent(event);
                                return;
                            }
                        }
                        DragMode::Moving => {
                            return;
                        }
                        DragMode::Resizing => {
                            let (origin, size) = *self.ivars().resize_start.borrow();
                            let dx = now.x - down.x;
                            let dy = now.y - down.y;
                            let w = (size.width + dx).max(MIN_W);
                            let h = (size.height - dy).max(MIN_H);
                            self.setFrame_display(CGRect::new(
                                CGPoint::new(origin.x, origin.y + size.height - h),
                                CGSize::new(w, h),
                            ), true);
                            return;
                        }
                        DragMode::Idle => {}
                    }
                }
                objc2_app_kit::NSEventType::LeftMouseUp => {
                    let was = *self.ivars().mode.borrow();
                    *self.ivars().mode.borrow_mut() = DragMode::Idle;
                    *self.ivars().down_at.borrow_mut() = None;
                    match was {
                        DragMode::Resizing => {
                            persist();
                            return;
                        }
                        DragMode::Moving => {
                            return;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            unsafe {
                let _: () = msg_send![super(self), sendEvent: event];
            }
        }
    }
);

impl NoteWindow {
    fn make(id: &str, size: Option<CGSize>) -> Retained<NoteWindow> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let initial = CGRect::new(CGPoint::ZERO, CGSize::new(NOTE_W + NOTE_MARGIN * 2.0, 200.0));
        let this = mtm.alloc::<NoteWindow>().set_ivars(NoteWindowIvars {
            id: RefCell::new(id.to_string()),
            view: RefCell::new(None),
            on_top: RefCell::new(true),
            down_at: RefCell::new(None),
            mode: RefCell::new(DragMode::Idle),
            resize_start: RefCell::new((CGPoint::ZERO, CGSize::ZERO)),
        });
        let this: Retained<NoteWindow> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: initial,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: objc2_app_kit::NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        this.setOpaque(false);
        this.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
        this.setHasShadow(false);
        this.setHidesOnDeactivate(false);
        unsafe {
            this.setReleasedWhenClosed(false);
        }
        this.setLevel(level_for(true));
        this.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        let view = NoteView::make(CGRect::new(CGPoint::ZERO, initial.size), id);
        this.setContentView(Some(&view));
        *this.ivars().view.borrow_mut() = Some(view);
        if let Some(size) = size {
            this.apply_size(size);
        } else {
            this.fit_to_content(id);
        }
        this
    }

    fn apply_size(&self, size: CGSize) {
        let top = self.frame().max().y;
        self.setFrame_display(CGRect::new(
            CGPoint::new(self.frame().min().x, top - size.height),
            size,
        ), true);
    }

    /// First appearance: the natural size of the card for this content.
    fn fit_to_content(&self, id: &str) {
        let Some(item) = store::read(|s| s.items.iter().find(|it| it.id == id).cloned()) else { return };
        let mut h = NoteView::natural_height(&item) + NOTE_MARGIN * 2.0;
        let mtm = MainThreadMarker::new().expect("main thread");
        let cap = NSScreen::mainScreen(mtm)
            .map(|s| s.visibleFrame().size.height)
            .unwrap_or(900.0) * TEXT_CAP_FRACTION;
        h = h.min(cap).max(MIN_H);
        self.apply_size(CGSize::new((NOTE_W + NOTE_MARGIN * 2.0).max(MIN_W), h));
    }

    fn item_id(&self) -> String {
        self.ivars().id.borrow().clone()
    }

    fn on_top_flag(&self) -> bool {
        *self.ivars().on_top.borrow()
    }
}

// MARK: Registry

struct Registry(std::cell::UnsafeCell<Option<std::collections::HashMap<String, Retained<NoteWindow>>>>);
unsafe impl Sync for Registry {}

static REGISTRY: Registry = Registry(std::cell::UnsafeCell::new(None));

fn reg() -> &'static mut std::collections::HashMap<String, Retained<NoteWindow>> {
    unsafe { (&mut *REGISTRY.0.get()).get_or_insert_with(std::collections::HashMap::new) }
}

struct TearingId(std::cell::UnsafeCell<Option<String>>);
unsafe impl Sync for TearingId {}
static TEARING: TearingId = TearingId(std::cell::UnsafeCell::new(None));

fn tearing() -> Option<String> {
    unsafe { (*TEARING.0.get()).clone() }
}

fn set_tearing(id: Option<&str>) {
    unsafe {
        *TEARING.0.get() = id.map(|s| s.to_string());
    }
}

// MARK: Lifecycle

/// Launch: bring back every note whose card still exists, where it was.
/// An unreadable store erases nothing (`loadFailed` guard).
pub fn restore() {
    if store::read(|s| s.load_failed) {
        return;
    }
    let prefs = Preferences::shared();
    let entries = prefs.desktop_notes_raw();
    let alive: Vec<(String, f64, f64, Option<(f64, f64)>, bool)> = entries
        .into_iter()
        .filter(|(id, _, _, _, _)| store::read(|s| s.items.iter().any(|it| &it.id == id)))
        .collect();
    for (id, x, y, size, top) in alive {
        show(&id, CGPoint::new(x, y), size.map(|(w, h)| CGSize::new(w, h)), false);
        if !top {
            set_on_top(&id, false);
        }
    }
    persist();
}

/// Put a card on the desktop (`place(_:at:)`); nil origin = stepped spot
/// near the top-right of the main screen.
pub fn place(id: &str, origin: Option<CGPoint>) {
    let Some(item) = store::read(|s| s.items.iter().find(|it| it.id == id).cloned()) else { return };
    if !item.pinned {
        store::with(|s| s.toggle_pin(id, false)); // a note must not vanish with the nightly cleanup
    }
    if let Some(w) = reg().get(id) {
        w.orderFrontRegardless();
        return;
    }
    show(id, origin.unwrap_or_else(next_free_spot), None, true);
    crate::shelf::panel::with_model_mut(|m| m.note_welcome_tried("desktop"));
}

pub fn close(id: &str) {
    if let Some(w) = reg().remove(id) {
        w.orderOut(None);
    }
    persist();
}

pub fn bring_to_front(id: &str) {
    if let Some(w) = reg().get(id) {
        w.orderFrontRegardless();
    }
}

/// Cards deleted from the shelf take their notes with them (`itemsGone`).
pub fn items_gone(ids: &[String]) {
    for id in ids {
        if reg().contains_key(id) {
            close(id);
        }
    }
}

pub fn is_on_desktop(id: &str) -> bool {
    reg().contains_key(id)
}

pub fn is_on_top(id: &str) -> bool {
    reg().get(id).map(|w| w.on_top_flag()).unwrap_or(true)
}

pub fn set_on_top(id: &str, top: bool) {
    let keys: Vec<String> = reg().keys().cloned().collect();
    if !keys.iter().any(|k| k == id) {
        return;
    }
    if let Some(w) = reg().get_mut(id) {
        *w.ivars().on_top.borrow_mut() = top;
        w.setLevel(level_for(top));
        if top {
            w.orderFrontRegardless();
        }
        refresh_view(w);
    }
    persist();
}

pub fn toggle_layer(id: &str) {
    set_on_top(id, !is_on_top(id));
}

fn refresh_view(w: &NoteWindow) {
    if let Some(v) = w.ivars().view.borrow().as_ref() {
        v.setNeedsDisplay(true);
    }
}

/// Whether the raw pointer still addresses a live note view (the delayed
/// 已复制 reset only lands on the same note).
pub fn view_alive(raw: usize) -> bool {
    // The Registry's map is read through; a stale raw was already dropped.
    for w in reg().values() {
        if let Some(v) = w.ivars().view.borrow().as_ref() {
            if (&**v as *const NoteView as usize) == raw {
                return true;
            }
        }
    }
    false
}

/// Self-test: any note's view, for `--selftest note` renders.
pub fn any_note_view() -> Option<Retained<NSView>> {
    let (_key, w) = reg().iter().next()?;
    let v = w.ivars().view.borrow().clone()?;
    // NoteView ⊂ NSView — same-class cast.
    Some(unsafe { objc2::rc::Retained::cast_unchecked(v) })
}

// MARK: Tear-out from the shelf

/// Whether id is mid tear-out (between beginTear and endTear).
pub fn is_tearing(id: &str) -> bool {
    tearing().as_deref() == Some(id)
}

/// The card is being dragged out of the panel: a note appears under the
/// pointer and follows it (`beginTear`).
pub fn begin_tear(id: &str, at: CGPoint) {
    if reg().contains_key(id) {
        return;
    }
    let note = NoteWindow::make(id, None);
    note.setFrameOrigin(origin_for_mouse(at, note.frame().size));
    note.orderFrontRegardless();
    note.setAlphaValue(0.85);
    reg().insert(id.to_string(), note);
    // Not persisted yet: the drop may still cancel the note.
    unsafe {
        *TEARING.0.get() = Some(id.to_string());
    }
}

pub fn move_tear(to: CGPoint) {
    let Some(id) = tearing() else { return };
    if let Some(w) = reg().get(&id) {
        w.setFrameOrigin(origin_for_mouse(to, w.frame().size));
    }
}

/// Dropped. The pointer decides, not the note's frame: a long note hangs
/// down over the shelf while its header is up on the desktop, and that is a
/// valid drop. Pointer still over the shelf = changed your mind.
pub fn end_tear(over_shelf: Option<CGRect>) {
    let Some(id) = tearing() else { return };
    set_tearing(None);
    let Some(w) = reg().get(&id) else { return };
    let mouse = NSEvent::mouseLocation();
    if let Some(shelf) = over_shelf {
        if mouse.x >= shelf.min().x && mouse.x <= shelf.max().x && mouse.y >= shelf.min().y && mouse.y <= shelf.max().y {
            reg().remove(&id).map(|w| w.orderOut(None));
            return;
        }
    }
    let frame = w.frame();
    w.setFrameOrigin(clamp_origin(frame.min(), frame.size));
    w.setAlphaValue(1.0);
    if let Some(item) = store::read(|s| s.items.iter().find(|it| it.id == id).cloned()) {
        if !item.pinned {
            store::with(|s| s.toggle_pin(&id, false));
        }
    }
    crate::shelf::panel::with_model_mut(|m| m.note_welcome_tried("desktop"));
    persist();
}

// MARK: Placement helpers

fn show(id: &str, origin: CGPoint, size: Option<CGSize>, persist_now: bool) {
    let w = NoteWindow::make(id, size);
    w.setFrameOrigin(clamp_origin(origin, w.frame().size));
    w.orderFrontRegardless();
    reg().insert(id.to_string(), w);
    if persist_now {
        persist();
    }
}

fn origin_for_mouse(mouse: CGPoint, size: CGSize) -> CGPoint {
    CGPoint::new(mouse.x - size.width / 2.0, mouse.y - size.height + 28.0)
}

fn next_free_spot() -> CGPoint {
    let mtm = MainThreadMarker::new().expect("main thread");
    let vf = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame()).unwrap_or(CGRect::new(CGPoint::ZERO, CGSize::new(1440.0, 900.0)));
    let n = reg().len() as f64;
    CGPoint::new(
        vf.max().x - (NOTE_W + NOTE_MARGIN * 2.0) - 40.0 - (n % 6.0) * 24.0,
        vf.max().y - 320.0 - (n % 6.0) * 24.0,
    )
}

/// Clamp a note window to whichever screen its center lands on.
pub fn clamp_origin(origin: CGPoint, size: CGSize) -> CGPoint {
    let mtm = MainThreadMarker::new().expect("main thread");
    let center = CGPoint::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
    let screen = NSScreen::screens(mtm)
        .iter()
        .find(|s| {
            let vf = s.visibleFrame();
            center.x >= vf.min().x && center.x <= vf.max().x && center.y >= vf.min().y && center.y <= vf.max().y
        })
        .or_else(|| NSScreen::screens(mtm).iter().next());
    let Some(screen) = screen else { return origin };
    let vf = screen.visibleFrame();
    CGPoint::new(
        (origin.x).max(vf.min().x).min(vf.max().x - size.width),
        (origin.y).max(vf.min().y).min(vf.max().y - size.height),
    )
}

/// Persist all open note placements (skipped while a tear-out is live).
pub fn persist() {
    if tearing().is_some() {
        return;
    }
    let notes: Vec<(String, f64, f64, Option<(f64, f64)>, bool)> = reg()
        .iter()
        .map(|(id, w)| {
            let f = w.frame();
            (id.clone(), f.min().x, f.min().y, Some((f.size.width, f.size.height)), w.on_top_flag())
        })
        .collect();
    Preferences::shared().set_desktop_notes_raw(&notes);
}

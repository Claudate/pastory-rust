//! Port of `App/AppDelegate.swift` (M0 slice: accessory app + menu-bar shell).
//!
//! The menu-bar shell: status item with the folded-P template icon, a
//! right-click menu, the app menu (Quit) and the Edit menu (without which
//! ⌘V/⌘C are dead in every text field). M2 adds the shelf toggle; M3 the
//! capture item; M6 the rest of the menu rows.

use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, NSString};

use crate::app::{localization as loc, preferences, theme, preferences::Preferences};
use crate::app::hotkey::HotKeyCenter;

/// NSApplication.delegate is weak; keep the delegate alive here (same as the
/// Swift `appDelegate` global). Everything ObjC here is main-thread-confined:
/// the Mutex only guards initialization, reads happen on main.
static APP_DELEGATE: Mutex<Option<usize>> = Mutex::new(None);

fn set_app_delegate(d: Retained<AppDelegate>) {
    *APP_DELEGATE.lock().unwrap() = Some(Retained::into_raw(d) as usize);
}

/// The live delegate; main-thread-only, so the raw pointer is valid.
/// M2+ callers (the shelf toggle) read it.
pub(crate) fn app_delegate() -> Option<&'static AppDelegate> {
    APP_DELEGATE.lock().unwrap().map(|p| {
        // SAFETY: the pointer was stored from a Retained that lives for the
        // process lifetime, and reads happen on the main thread only.
        unsafe { &*(p as *const AppDelegate) }
    })
}

pub struct AppDelegateIvars {
    status_item: std::cell::RefCell<Option<Retained<NSStatusItem>>>,
    menu: Retained<NSMenu>,
}

define_class!(
    // SAFETY:
    // - NSObject + NSMenuDelegate: standard AppKit delegate shapes.
    // - The delegate is only ever used from the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    pub(crate) struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    impl AppDelegate {
        #[unsafe(method(menuQuit:))]
        fn menu_quit(&self, _sender: &AnyObject) {
            let mtm = MainThreadMarker::new().unwrap();
            NSApplication::sharedApplication(mtm).terminate(None);
            dispatch_main_after(1.0, Box::new(|| std::process::exit(0)));
        }

        #[unsafe(method(menuCapture:))]
        fn menu_capture(&self, _sender: &AnyObject) {
            eprintln!("[pastory] menu 截图");
            crate::capture::coordinator::start();
        }

        #[unsafe(method(menuShelf:))]
        fn menu_shelf(&self, _sender: &AnyObject) {
            crate::shelf::panel::toggle();
        }

        #[unsafe(method(menuSearch:))]
        fn menu_search(&self, _sender: &AnyObject) {
            crate::shelf::panel::show_search();
        }

        // The status item's action (was set to a selector that didn't exist
        // on this class — clicks silently went nowhere).
        #[unsafe(method(statusClicked:))]
        fn status_clicked_objc(&self, _sender: &AnyObject) {
            self.status_clicked();
        }

        #[unsafe(method(menuTogglePause:))]
        fn menu_toggle_pause(&self, _sender: &AnyObject) {
            let p = preferences::Preferences::shared();
            p.set_monitoring_paused(!p.monitoring_paused());
        }

        #[unsafe(method(menuOpenStore:))]
        fn menu_open_store(&self, _sender: &AnyObject) {
            let root = crate::clipboard::store::read(|s| s.root.clone());
            let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&root.to_string_lossy()));
            objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
        }

        #[unsafe(method(menuSettings:))]
        fn menu_settings(&self, _sender: &AnyObject) {
            if !crate::shelf::panel::is_visible() {
                crate::shelf::panel::show();
            }
            crate::shelf::panel::with_model_mut(|m| m.show_settings = true);
            crate::shelf::view::refresh();
        }

        #[unsafe(method(menuCheckUpdates:))]
        fn menu_check_updates(&self, _sender: &AnyObject) {
            crate::app::updater::check(true, false);
        }
    }

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &objc2_foundation::NSNotification) {
            self.did_finish_launching();
        }

        // Launchpad / Dock icon clicked while already running: a menu-bar app
        // has no window to bring up, so open the shelf (M2).
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn application_should_handle_reopen_has_visible_windows(
            &self,
            _sender: &NSApplication,
            _flag: bool,
        ) -> bool {
            crate::shelf::panel::show();
            false
        }
    }

    unsafe impl NSMenuDelegate for AppDelegate {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.rebuild_menu(menu);
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<AppDelegate> {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
        let this = mtm.alloc::<AppDelegate>().set_ivars(AppDelegateIvars {
            status_item: std::cell::RefCell::new(None),
            menu,
        });
        unsafe { msg_send![super(this), init] }
    }

    /// `ivars()` hands out a shared borrow, so the status item lives behind a
    /// RefCell (the delegate is main-thread-only; the cell is only touched
    /// from the main thread).
    fn set_status_item(&self, item: Retained<NSStatusItem>) {
        *self.ivars().status_item.borrow_mut() = Some(item);
    }

    fn did_finish_launching(&self) {
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        selftest_dispatch(&app);
        build_main_menu(mtm);
        let status_item =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            match theme::menu_icon() {
                Some(icon) => button.setImage(Some(&icon)),
                None => {
                    // Fallback, as in the Swift version.
                    let symbol = objc2_app_kit::NSImage::imageWithSystemSymbolName_accessibilityDescription(
                        ns_string!("scissors"),
                        Some(ns_string!("Pastory")),
                    );
                    match symbol {
                        Some(img) => button.setImage(Some(&img)),
                        None => {}
                    }
                }
            }
            unsafe {
                // setTarget: takes Option<&AnyObject>; upcast the reference
                // (SAFETY: the target outlives the button — the delegate is
                // kept alive in APP_DELEGATE for the process lifetime).
                let target: Option<&AnyObject> =
                    Some(&*(self as *const Self as *const AnyObject));
                let _: () = msg_send![&button, setTarget: target];
                let _: () = msg_send![&button, setAction: sel!(statusClicked:)];
                // [.leftMouseUp, .rightMouseUp] — the old raw-mask msg_send
                // was both the wrong value and mis-encoded (debug-verify
                // panic: NSControl.sendActionOn: returns the previous mask).
                let _: objc2_foundation::NSInteger = button.sendActionOn(
                    objc2_app_kit::NSEventMask::LeftMouseUp
                        | objc2_app_kit::NSEventMask::RightMouseUp,
                );
            }
        }
        // Store the status item in the ivar; the delegate is main-thread-only
        // so the RefCell is only touched from here.
        self.set_status_item(status_item);

        bind_shortcuts();
        observe_notifications(mtm);
        // §5.15 launch order: retention first, then the monitor, the shelf
        // prewarm, notes, the how-to seed, the updater, the welcome card,
        // and the menu's language observer (installed above).
        crate::clipboard::retention::schedule();
        crate::clipboard::monitor::start();
        crate::shelf::panel::prewarm();
        crate::shelf::desktop_notes::restore();
        crate::shelf::how_to_card::seed_if_needed();
        crate::app::updater::schedule();
        // First launch on this Mac: the shelf opens by itself with the
        // welcome card; the card's own 开始使用 sets the flag when done.
        if !Preferences::shared().did_welcome() {
            crate::shelf::panel::with_model_mut(|m| {
                m.show_welcome = true;
            });
            crate::app::delegate::dispatch_main_after(0.6, Box::new(|| {
                crate::shelf::panel::show();
            }));
        }
    }

    /// Left click toggles the shelf; right click (or control-click, as
    /// everywhere on the Mac) opens the menu (Swift `statusClicked`).
    fn status_clicked(&self) {
        eprintln!("[pastory] status item clicked");
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        let ivars = self.ivars();
        let menu_click = app
            .currentEvent()
            .map(|e| {
                let t = e.r#type();
                t == objc2_app_kit::NSEventType::RightMouseUp
                    || (t == objc2_app_kit::NSEventType::LeftMouseUp
                        && e.modifierFlags()
                            .contains(objc2_app_kit::NSEventModifierFlags::Control))
            })
            .unwrap_or(false);
        if !menu_click {
            crate::shelf::panel::toggle();
            return;
        }
        if let (Some(item), Some(_event)) = (
            ivars.status_item.borrow().as_ref(),
            app.currentEvent(),
        ) {
            item.setMenu(Some(&ivars.menu));
            if let Some(button) = item.button(mtm) {
                // SAFETY: the button's target/action point at this delegate.
                unsafe { button.performClick(None) };
            }
            item.setMenu(None);
        }
    }

    /// Rebuild the pop-up menu on demand (`menuNeedsUpdate`): the live
    /// visibility/pause state reads from Preferences each time, which is the
    /// Swift behavior (menuNeedsUpdate re-runs on every open).
    fn rebuild_menu(&self, menu: &NSMenu) {
        let mtm = MainThreadMarker::new().unwrap();
        menu.removeAllItems();
        let p = preferences::Preferences::shared();
        let capture = p.shortcut(preferences::key::HOTKEY_CAPTURE);
        let shelf = p.shortcut(preferences::key::HOTKEY_SHELF);
        let search = p.shortcut(preferences::key::HOTKEY_SEARCH);
        let visible = crate::shelf::panel::is_visible();
        unsafe {
            let target: Option<&AnyObject> = Some(&*(self as *const Self as *const AnyObject));
            for (title, hint, action) in [
                (loc::l("截图"), Some(&capture), sel!(menuCapture:)),
                (if visible { loc::l("隐藏剪贴板") } else { loc::l("显示剪贴板") }, Some(&shelf), sel!(menuShelf:)),
                (loc::l("搜索剪贴板"), Some(&search), sel!(menuSearch:)),
            ] {
                let item = add_item(mtm, menu, &title, None, hint);
                let _: () = msg_send![&*item, setTarget: target];
                let _: () = msg_send![&*item, setAction: Some(action)];
            }
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            let pause = add_item(mtm, menu, &loc::l("暂停记录剪贴板"), None, None);
            let _: () = msg_send![&*pause, setTarget: target];
            let _: () = msg_send![&*pause, setAction: Some(sel!(menuTogglePause:))];
            if p.monitoring_paused() {
                let _: () = msg_send![&*pause, setState: 1_isize];
            }
            let store = add_item(mtm, menu, &loc::l("打开存储文件夹"), None, None);
            let _: () = msg_send![&*store, setTarget: target];
            let _: () = msg_send![&*store, setAction: Some(sel!(menuOpenStore:))];
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            let updates = add_item(mtm, menu, &loc::l("检查更新…"), None, None);
            let _: () = msg_send![&*updates, setTarget: target];
            let _: () = msg_send![&*updates, setAction: Some(sel!(menuCheckUpdates:))];
            let settings = add_item(mtm, menu, &loc::l("设置…"), None, None);
            let _: () = msg_send![&*settings, setTarget: target];
            let _: () = msg_send![&*settings, setAction: Some(sel!(menuSettings:))];
            settings.setKeyEquivalent(ns_string!(","));
            let quit = add_item(mtm, menu, &loc::l("退出 Pastory"), None, None);
            let _: () = msg_send![&*quit, setTarget: target];
            quit.setKeyEquivalent(ns_string!("q"));
        }
    }
}

/// `add(menu, title, action, hint)` — plain M0 rows carry no action yet.
/// M2+ wires each row to a real selector as its feature lands.
fn add_item(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    title: &str,
    _action: Option<&str>,
    hint: Option<&preferences::Shortcut>,
) -> Retained<NSMenuItem> {
    // SAFETY: initWithTitle: with a None action is a plain construction.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            None,
            ns_string!(""),
        )
    };
    if let Some(h) = hint {
        if h.is_set() {
            // "   ⌥⌘S" hint. M0 keeps plain titles (the attributed variant
            // with tertiaryLabelColor lands with the full theme in M2).
            let _ = &title;
        }
    }
    menu.addItem(&item);
    item
}

/// Bind the three shortcuts (capture / shelf / search). Shelf + search act
/// for real from M2; capture stays a no-op until the M3 slice.
fn bind_shortcuts() {
    let p = preferences::Preferences::shared();
    let capture = p.shortcut(preferences::key::HOTKEY_CAPTURE);
    let shelf = p.shortcut(preferences::key::HOTKEY_SHELF);
    let search = p.shortcut(preferences::key::HOTKEY_SEARCH);
    let ok_capture = HotKeyCenter::shared().bind(capture, "capture", Box::new(|| {
        crate::capture::coordinator::start();
    }));
    let ok_shelf = HotKeyCenter::shared().bind(shelf, "shelf", Box::new(|| {
        // (The M6 welcome checklist notes "shelf" tried here.)
        crate::shelf::panel::toggle();
    }));
    let ok_search = HotKeyCenter::shared().bind(search, "search", Box::new(|| {
        crate::shelf::panel::show_search();
    }));
    eprintln!("[pastory] hotkeys bound: capture={ok_capture} shelf={ok_shelf} search={ok_search}");
}

/// `buildMainMenu` — menu-bar apps get no menu for free; without an Edit
/// menu, ⌘V/⌘C are dead in every text field (search box, rename box).
fn build_main_menu(mtm: MainThreadMarker) {
    let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
    // SAFETY: plain NSMenuItem construction.
    let app_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!(""),
            None,
            ns_string!(""),
        )
    };
    let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
    let quit = add_item_with_action(mtm, &app_menu, &loc::l("退出 Pastory"), sel!(menuQuit:), "q");
    // The delegate answers menuQuit: (target set in did_finish_launching's
    // AppDelegate, which outlives everything).
    if let Some(d) = app_delegate() {
        unsafe {
            let target: Option<&AnyObject> = Some(&*(d as *const AppDelegate as *const AnyObject));
            let _: () = msg_send![&quit, setTarget: target];
        }
    }
    app_item.setSubmenu(Some(&app_menu));
    main.addItem(&app_item);

    // SAFETY: plain NSMenuItem construction.
    let edit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!(""),
            None,
            ns_string!(""),
        )
    };
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(&loc::l("编辑")));
    for (title, action, key) in [
        ("撤销", "undo:", "z"),
        ("重做", "redo:", "Z"),
    ] {
        edit.addItem(&add_item_named(mtm, &loc::l(title), action, key));
    }
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    for (title, action, key) in [
        ("剪切", "cut:", "x"),
        ("拷贝", "copy:", "c"),
        ("粘贴", "paste:", "v"),
        ("全选", "selectAll:", "a"),
    ] {
        edit.addItem(&add_item_named(mtm, &loc::l(title), action, key));
    }
    edit_item.setSubmenu(Some(&edit));
    main.addItem(&edit_item);
    let app = NSApplication::sharedApplication(mtm);
    app.setMainMenu(Some(&main));
}

/// Quit with a trailing exit, for the menu bar app (Swift `menuQuit`).
fn add_item_with_action(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    title: &str,
    action: objc2::runtime::Sel,
    key: &str,
) -> Retained<NSMenuItem> {
    // SAFETY: plain NSMenuItem construction.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    menu.addItem(&item);
    item
}

/// Edit-menu rows send standard responder-chain commands (no target).
fn add_item_named(mtm: MainThreadMarker, title: &str, action: &str, key: &str) -> Retained<NSMenuItem> {
    let sel = objc2::runtime::Sel::register(std::ffi::CString::new(action).unwrap().as_c_str());
    // SAFETY: plain NSMenuItem construction; Command is the edit-menu mask.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(sel),
            &NSString::from_str(key),
        )
    };
    item.setKeyEquivalentModifierMask(objc2_app_kit::NSEventModifierFlags::Command);
    item
}

/// The selftest path already exited before the app starts (main.rs), so this
/// is a no-op in the shell; kept for symmetry with the Swift launch order.
fn selftest_dispatch(_app: &NSApplication) {}

/// `observe(shortcutsChanged / languageChanged)` — a recorder save re-binds
/// the three combos; a language switch rebuilds the menu (menuNeedsUpdate
/// reads the fresh l()).
fn observe_notifications(_mtm: MainThreadMarker) {
    unsafe {
        let center = objc2_foundation::NSNotificationCenter::defaultCenter();
        let block = block2::RcBlock::new(|_n: std::ptr::NonNull<objc2_foundation::NSNotification>| {
            bind_shortcuts();
            crate::shelf::settings_pane::post_shortcut_binding_changed();
            if let Some(v) = crate::shelf::view::settings_view() {
                v.refresh_state();
            }
        });
        let obs = center.addObserverForName_object_queue_usingBlock(
            Some(ns_string!("pastory.shortcutsChanged")),
            None,
            None,
            &block,
        );
        std::mem::forget(obs);
        let block2 = block2::RcBlock::new(|_n: std::ptr::NonNull<objc2_foundation::NSNotification>| {
            let mtm = MainThreadMarker::new().expect("main thread");
            build_main_menu(mtm);
            if let Some(d) = app_delegate() {
                let menu = d.ivars().menu.clone();
                d.rebuild_menu(&menu);
            }
        });
        let obs2 = center.addObserverForName_object_queue_usingBlock(
            Some(ns_string!("pastory.languageChanged")),
            None,
            None,
            &block2,
        );
        std::mem::forget(obs2);
    }
}

/// `app.run()` — build the delegate, go accessory, and never come back.
pub fn run(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::new(mtm);
    set_app_delegate(delegate.clone());
    let proto: &ProtocolObject<dyn NSApplicationDelegate> =
        ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(proto));
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.run();
}

/// `--selftest statusclick`: build the menu-bar shell exactly as at launch,
/// then `performClick` the status button (the same action dispatch a real
/// left click triggers) and assert the shelf panel actually shows. Proves
/// the target/action/selector/toggle chain without needing a mouse.
pub fn statusclick_selftest() -> bool {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = AppDelegate::new(mtm);
    delegate.did_finish_launching();
    let Some(button) = delegate
        .ivars()
        .status_item
        .borrow()
        .as_ref()
        .and_then(|item| item.button(mtm))
    else {
        println!("FAIL status item has no button");
        return false;
    };
    // Sanity surface: the button must carry a target and an action now.
    // (`sendActionOn:` is the void setter, not a getter — nothing to read
    // back. The mask was written at startup with the Swift value 0b10100.)
    let (has_target, has_action) = unsafe {
        let target: Option<Retained<AnyObject>> = msg_send![&button, target];
        let action: Option<objc2::runtime::Sel> = msg_send![&button, action];
        (target.is_some(), action.is_some())
    };
    println!("button: target={} action={:?}", has_target, has_action);
    if !has_target || !has_action {
        println!("FAIL button wiring incomplete");
        return false;
    }
    unsafe {
        // SAFETY: same dispatch as a real left click (fires the action).
        button.performClick(None);
    }
    // Pump the run loop so the slide-in animation can start.
    let run_loop = objc2_foundation::NSRunLoop::mainRunLoop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(800);
    while std::time::Instant::now() < deadline {
        let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
        let mode: &'static objc2_foundation::NSRunLoopMode =
            unsafe { objc2_foundation::NSDefaultRunLoopMode };
        let _ = run_loop.runMode_beforeDate(mode, &limit);
        if crate::shelf::panel::is_visible() {
            break;
        }
    }
    let visible = crate::shelf::panel::is_visible();
    println!("{} shelf visible after programmatic click: {}", if visible { "ok  " } else { "FAIL" }, visible);
    if visible {
        crate::shelf::panel::hide();
    }
    visible
}

/// `DispatchQueue.main.async` — hop a closure to the main queue (used by the
/// SCK completion-handler bounces).
pub fn dispatch_main_async(f: Box<dyn FnOnce() + Send + 'static>) {
    dispatch_main_after(0.0, f);
}

/// `DispatchQueue.main.asyncAfter` — used by the paste retry loop.
pub fn dispatch_main_after(seconds: f64, f: Box<dyn FnOnce() + Send + 'static>) {
    unsafe extern "C-unwind" {
        fn dispatch_after_f(
            when: *const DispatchTimeT,
            queue: *mut core::ffi::c_void,
            context: *mut core::ffi::c_void,
            work: unsafe extern "C-unwind" fn(*mut core::ffi::c_void),
        );
        static _dispatch_main_q: core::ffi::c_void;
    }
    type DispatchTimeT = u64;
    const NSEC_PER_SEC: u64 = 1_000_000_000;
    unsafe extern "C-unwind" fn trampoline(ctx: *mut core::ffi::c_void) {
        let f: Box<Box<dyn FnOnce() + Send>> = Box::from_raw(ctx as *mut _);
        f();
    }
    let when = DispatchTimeT::wrapping_add(
        DispatchTimeT::wrapping_mul(seconds as u64, NSEC_PER_SEC),
        0,
    );
    let raw = Box::into_raw(Box::new(f));
    unsafe {
        dispatch_after_f(
            &when,
            std::ptr::addr_of!(_dispatch_main_q) as *mut _,
            raw as *mut _,
            trampoline,
        );
    }
}

// `clang` defines DISPATCH_TIME_NOW as 0; delta-based dispatch_after_f needs
// a time in the future, which is what the multiplication above builds.
const _: () = ();

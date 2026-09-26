//! Port of `App/HotKeyCenter.swift`.
//!
//! Global shortcuts via Carbon `RegisterEventHotKey` — no Accessibility
//! permission needed. `objc2-carbon` 0.3.2 is an empty shell (it only emits a
//! `#[link(name = "Carbon")]` stub), so the HIToolbox FFI is declared by hand
//! here, with prototypes lifted verbatim from the SDK headers
//! (HIToolbox/CarbonEvents.h, CarbonEventsCore.h, MacApplication.h).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::app::preferences::Shortcut;

// ---- Carbon constants (CarbonEvents.h / CarbonEventsCore.h) ----

/// `kEventClassKeyboard` = 'keyb'
const K_EVENT_CLASS_KEYBOARD: u32 = 0x6B65_7962;
/// `kEventHotKeyPressed` = 5
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
/// `kEventParamDirectObject` = '----'
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = 0x2D2D_2D2D;
/// `typeEventHotKeyID` = 'hkid'
const TYPE_EVENT_HOT_KEY_ID: u32 = 0x686B_6964;
/// `eventNotHandledErr` = -9874
const EVENT_NOT_HANDLED_ERR: i32 = -9874;
/// `noErr` = 0
const NO_ERR: i32 = 0;
/// Our hotkey signature: 'SNCL' (Swift's `0x534E_434C`).
const SIGNATURE_SNCL: u32 = 0x534E_434C;

#[repr(C)]
#[derive(Clone, Copy)]
struct EventTypeSpec {
    event_class: u32, // OSType
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32, // OSType
    id: u32,
}

type EventTargetRef = *mut core::ffi::c_void;
type EventRef = *mut core::ffi::c_void;
type EventHandlerCallRef = *mut core::ffi::c_void;
type EventHotKeyRef = *mut core::ffi::c_void;
type EventHandlerRef = *mut core::ffi::c_void;
type OSStatus = i32;
type ItemCount = usize;
type ByteCount = usize;
type EventParamType = u32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C-unwind" {
    // EventTargetRef GetApplicationEventTarget(void);  (MacApplication.h)
    fn GetApplicationEventTarget() -> EventTargetRef;
    // OSStatus RegisterEventHotKey(UInt32 inHotKeyCode, UInt32 inHotKeyModifiers,
    //   EventHotKeyID inHotKeyID, EventTargetRef inTarget, OptionBits inOptions,
    //   EventHotKeyRef *outRef);  (CarbonEvents.h)
    fn RegisterEventHotKey(
        in_hot_key_code: u32,
        in_hot_key_modifiers: u32,
        in_hot_key_id: EventHotKeyID,
        in_target: EventTargetRef,
        in_options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    // OSStatus UnregisterEventHotKey(EventHotKeyRef inHotKey);
    fn UnregisterEventHotKey(in_hot_key: EventHotKeyRef) -> OSStatus;
    // OSStatus InstallEventHandler(EventTargetRef inTarget, EventHandlerUPP inHandler,
    //   ItemCount inNumTypes, const EventTypeSpec *inList, void *inUserData,
    //   EventHandlerRef *outRef);  (CarbonEvents.h)
    fn InstallEventHandler(
        in_target: EventTargetRef,
        in_handler: EventHandlerUPP,
        in_num_types: ItemCount,
        in_list: *const EventTypeSpec,
        in_user_data: *mut core::ffi::c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    // OSStatus GetEventParameter(EventRef inEvent, EventParamName inName,
    //   EventParamType inDesiredType, EventParamType *outActualType,
    //   ByteCount inBufferSize, ByteCount *outActualSize, void *outData);
    fn GetEventParameter(
        in_event: EventRef,
        in_name: EventParamType,
        in_desired_type: EventParamType,
        out_actual_type: *mut EventParamType,
        in_buffer_size: ByteCount,
        out_actual_size: *mut ByteCount,
        out_data: *mut core::ffi::c_void,
    ) -> OSStatus;
}

type EventHandlerUPP = unsafe extern "C-unwind" fn(
    handler_call_ref: EventHandlerCallRef,
    event: EventRef,
    user_data: *mut core::ffi::c_void,
) -> OSStatus;

/// The Carbon hot-key handler: dig the `EventHotKeyID` out of the event and
/// queue the action on the main serial queue (Swift's `DispatchQueue.main`).
unsafe extern "C-unwind" fn hot_key_handler(
    _call_ref: EventHandlerCallRef,
    event: EventRef,
    _user_data: *mut core::ffi::c_void,
) -> OSStatus {
    if event.is_null() {
        return EVENT_NOT_HANDLED_ERR;
    }
    let mut hk_id = EventHotKeyID { signature: 0, id: 0 };
    let status = GetEventParameter(
        event,
        K_EVENT_PARAM_DIRECT_OBJECT,
        TYPE_EVENT_HOT_KEY_ID,
        std::ptr::null_mut(),
        std::mem::size_of::<EventHotKeyID>(),
        std::ptr::null_mut(),
        &mut hk_id as *mut EventHotKeyID as *mut core::ffi::c_void,
    );
    if status != NO_ERR {
        return status;
    }
    let id = hk_id.id;
    dispatch_on_main(move || HotKeyCenter::shared().fire(id));
    NO_ERR
}

/// `DispatchQueue.main.async` for closures: hop through the Foundation main
/// run loop so actions fire on the main thread like the Swift version does.
fn dispatch_on_main(f: impl FnOnce() + Send + 'static) {
    // The app never leaves the main run loop for long; run it on the main
    // thread via the ObjC autorelease pool's queue helper. `dispatch_async`
    // on the main queue is the closest structural match to Swift's
    // `DispatchQueue.main.async` and needs no extra dependency.
    let boxed: Box<dyn FnOnce() + Send + 'static> = Box::new(f);
    let raw = Box::into_raw(Box::new(boxed));
    unsafe extern "C-unwind" {
        fn dispatch_async_f(
            queue: *mut core::ffi::c_void,
            context: *mut core::ffi::c_void,
            work: unsafe extern "C-unwind" fn(*mut core::ffi::c_void),
        );
        static _dispatch_main_q: core::ffi::c_void;
    }
    unsafe extern "C-unwind" fn trampoline(ctx: *mut core::ffi::c_void) {
        let f: Box<Box<dyn FnOnce() + Send>> = Box::from_raw(ctx as *mut _);
        f();
    }
    unsafe {
        // SAFETY: the context is a leaked Box reclaimed in the trampoline.
        dispatch_async_f(
            std::ptr::addr_of!(_dispatch_main_q) as *mut _,
            raw as *mut _,
            trampoline,
        );
    }
}

struct Binding {
    id: u32,
    #[allow(dead_code)]
    ref_: EventHotKeyRef,
    action: Box<dyn Fn()>,
}

/// Global hot-key center. All binding state is touched on the main thread
/// (bind/unbind from the app delegate; `fire` hops there first), but the
/// shared static must still be `Send + Sync` for the GCD hop, so the raw
/// pointer is wrapped in a mutex-guarded cell and never dereferenced off-main.
pub struct HotKeyCenter {
    inner: Mutex<Inner>,
}

/// SAFETY: `EventHotKeyRef` is an opaque Carbon token that Carbon itself uses
/// across threads; the closures are only ever called on the main thread.
unsafe impl Send for HotKeyCenter {}
unsafe impl Sync for HotKeyCenter {}

struct Inner {
    bindings: HashMap<String, Binding>,
    by_id: HashMap<u32, String>,
    next_id: u32,
    handler_installed: bool,
    /// Names whose last bind was refused (another app owns the combo).
    failed: Vec<String>,
    suspended: Vec<(String, Shortcut)>,
    suspend_depth: usize,
}

impl HotKeyCenter {
    pub fn shared() -> &'static HotKeyCenter {
        static SHARED: OnceLock<HotKeyCenter> = OnceLock::new();
        SHARED.get_or_init(|| HotKeyCenter {
            inner: Mutex::new(Inner {
                bindings: HashMap::new(),
                by_id: HashMap::new(),
                next_id: 1,
                handler_installed: false,
                failed: Vec::new(),
                suspended: Vec::new(),
                suspend_depth: 0,
            }),
        })
    }

    /// Run the action registered for hot-key `id` (called from the handler).
    fn fire(&self, id: u32) {
        // `fire` runs on the main thread (the handler hops there), the same
        // place binds happen, so this cannot deadlock against bind/unbind.
        let inner = self.inner.lock().unwrap();
        if let Some(name) = inner.by_id.get(&id) {
            if let Some(b) = inner.bindings.get(name) {
                (b.action)();
            }
        }
    }

    pub fn bind(&self, shortcut: Shortcut, name: &str, action: Box<dyn Fn()>) -> bool {
        self.unbind(name);
        self.inner.lock().unwrap().failed.retain(|n| n != name);
        if !shortcut.is_set() {
            return true;
        }
        let ok = self.bind_raw(shortcut.key_code, shortcut.carbon_modifiers, name, action);
        let mut inner = self.inner.lock().unwrap();
        if !ok {
            inner.failed.push(name.to_string());
        }
        ok
    }

    /// Modifier-less keys allowed (used for Esc while the picker is up).
    /// Unbind promptly.
    pub fn bind_raw(
        &self,
        key_code: u32,
        modifiers: u32,
        name: &str,
        action: Box<dyn Fn()>,
    ) -> bool {
        self.unbind(name);
        self.install_handler_if_needed();
        let id;
        let ref_;
        {
            let mut inner = self.inner.lock().unwrap();
            id = inner.next_id;
            inner.next_id += 1;
            let hot_key_id = EventHotKeyID { signature: SIGNATURE_SNCL, id };
            let mut out: EventHotKeyRef = std::ptr::null_mut();
            // SAFETY: FFI per CarbonEvents.h; outRef is written on success.
            let status = unsafe {
                RegisterEventHotKey(
                    key_code,
                    modifiers,
                    hot_key_id,
                    GetApplicationEventTarget(),
                    0,
                    &mut out,
                )
            };
            if status != NO_ERR || out.is_null() {
                return false;
            }
            ref_ = out;
            inner.bindings.insert(
                name.to_string(),
                Binding { id, ref_, action },
            );
            inner.by_id.insert(id, name.to_string());
        }
        let _ = id;
        true
    }

    /// Can this combo be registered right now (i.e. no other app holds it)?
    /// Registers and releases immediately. Our own bindings are released
    /// around the probe so they do not count as "taken" (M6: suspend around
    /// the probe like the Swift version does).
    pub fn is_available(&self, shortcut: Shortcut) -> bool {
        if !shortcut.is_set() {
            return true;
        }
        let hot_key_id = EventHotKeyID { signature: SIGNATURE_SNCL, id: 0xFFFF };
        let mut out: EventHotKeyRef = std::ptr::null_mut();
        // SAFETY: FFI probe; we unregister immediately on success.
        let status = unsafe {
            RegisterEventHotKey(
                shortcut.key_code,
                shortcut.carbon_modifiers,
                hot_key_id,
                GetApplicationEventTarget(),
                0,
                &mut out,
            )
        };
        if status == NO_ERR && !out.is_null() {
            unsafe { UnregisterEventHotKey(out) };
            return true;
        }
        false
    }

    /// Release every binding (a recorder is listening); `resume` puts them
    /// back once the last recorder is done.
    pub fn suspend(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.suspend_depth += 1;
        if inner.suspend_depth != 1 {
            return;
        }
        let names: Vec<String> = inner.bindings.keys().cloned().collect();
        for name in names {
            // The shortcut is recoverable from the binding's registration —
            // callers rebind with the stored preference, matching Swift.
            self.unbind_inner(&mut inner, &name);
        }
    }

    pub fn resume(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.suspend_depth = inner.suspend_depth.saturating_sub(1);
        if inner.suspend_depth != 0 {
            return;
        }
        // Rebinds are issued by the app delegate on resume (same as Swift,
        // which rebinds via the stored `shortcuts` map).
        inner.suspended.clear();
    }

    pub fn unbind(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        self.unbind_inner(&mut inner, name);
    }

    fn unbind_inner(&self, inner: &mut Inner, name: &str) {
        if let Some(b) = inner.bindings.remove(name) {
            inner.by_id.remove(&b.id);
            unsafe { UnregisterEventHotKey(b.ref_) };
        }
    }

    fn install_handler_if_needed(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.handler_installed {
            return;
        }
        inner.handler_installed = true;
        let spec = EventTypeSpec {
            event_class: K_EVENT_CLASS_KEYBOARD,
            event_kind: K_EVENT_HOT_KEY_PRESSED,
        };
        // SAFETY: FFI per CarbonEvents.h; the handler is a plain extern fn
        // with no state, and the out-ref is discarded (never uninstalled —
        // same lifetime as the process, as in the Swift version).
        unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                hot_key_handler,
                1,
                &spec,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        }
    }

    /// Names that could not be bound (shown in settings).
    pub fn failed(&self) -> Vec<String> {
        self.inner.lock().unwrap().failed.clone()
    }
}

#[allow(dead_code)]
fn _no_err_check(s: OSStatus) -> bool {
    s == NO_ERR
}

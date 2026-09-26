

// MARK: M6 keys (updater / notes / welcome / export / language switch)

impl Preferences {
    /// Daily update check against GitHub Releases (the app's only network
    /// request). On by default.
    pub fn check_for_updates(&self) -> bool {
        self.defaults()
            .objectForKey(ns_string_id("checkForUpdates"))
            .map(|v| m6_object_bool(&v))
            .unwrap_or(true)
    }

    pub fn set_check_for_updates(&self, v: bool) {
        self.defaults().setBool_forKey(v, ns_string_id("checkForUpdates"));
    }

    /// Swift stores an NSDate in UserDefaults (Preferences.swift:161); an
    /// NSDate does NOT respond to doubleValue — sending it crashes with
    /// NSInvalidArgumentException. Read `timeIntervalSince1970`, which
    /// NSNumber also implements, so both historical NSDate values and any
    /// NSNumber values work. Write as NSNumber-d double (epoch); Swift's
    /// `as? Date` cast then reads nil (should-check), never crashes.
    pub fn last_update_check(&self) -> Option<f64> {
        self.defaults()
            .objectForKey(ns_string_id("lastUpdateCheck"))
            .map(|v| m6_object_time_interval(&v))
    }

    pub fn set_last_update_check(&self, v: f64) {
        self.defaults()
            .setDouble_forKey(v, ns_string_id("lastUpdateCheck"));
    }

    pub fn skipped_version(&self) -> Option<String> {
        self.defaults()
            .stringForKey(ns_string_id("skippedVersion"))
            .map(|s| s.to_string())
    }

    pub fn set_skipped_version(&self, v: &str) {
        unsafe {
            self.defaults().setObject_forKey(
                Some(&*objc2_foundation::NSString::from_str(v)),
                ns_string_id("skippedVersion"),
            );
        }
    }

    /// First launch on this Mac: the shelf opens once by itself.
    pub fn did_welcome(&self) -> bool {
        self.defaults().boolForKey(ns_string_id("didWelcome"))
    }

    pub fn set_did_welcome(&self, v: bool) {
        self.defaults().setBool_forKey(v, ns_string_id("didWelcome"));
    }

    /// The 「Pastory 怎么用」 card has been seeded once.
    pub fn did_seed_how_to(&self) -> bool {
        self.defaults().boolForKey(ns_string_id("didSeedHowTo"))
    }

    pub fn set_did_seed_how_to(&self, v: bool) {
        self.defaults().setBool_forKey(v, ns_string_id("didSeedHowTo"));
    }

    /// Copy version of the seeded card; a newer build refreshes it in place.
    pub fn how_to_version(&self) -> i64 {
        self.defaults().integerForKey(ns_string_id("howToVersion")) as i64
    }

    pub fn set_how_to_version(&self, v: i64) {
        self.defaults().setInteger_forKey(v as isize, ns_string_id("howToVersion"));
    }

    /// Notes on the desktop: [{id, x, y, w, h, top}] in screen coordinates.
    pub fn desktop_notes_raw(&self) -> Vec<(String, f64, f64, Option<(f64, f64)>, bool)> {
        let raw = self
            .defaults()
            .arrayForKey(ns_string_id("desktopNotes"));
        let Some(arr) = raw else { return Vec::new() };
        let mut out = Vec::new();
        for entry in arr.iter() {
            let d = unsafe { Retained::cast_unchecked::<objc2_foundation::NSDictionary<objc2_foundation::NSString, objc2::runtime::AnyObject>>(entry) };
            let get = |k: &str| { d.objectForKey(ns_string_id(k)) };
            let id = get("id").map(|v| unsafe { Retained::cast_unchecked::<objc2_foundation::NSString>(v) }.to_string());
            let num = |k: &str| -> Option<f64> {
                get(k).map(|v| m6_object_double(&v))
            };
            let top = get("top").map(|v| m6_object_bool(&v)).unwrap_or(true);
            let (Some(id), Some(x), Some(y)) = (id, num("x"), num("y")) else { continue };
            let size = num("w").and_then(|w| num("h").map(|h| (w, h)));
            out.push((id, x, y, size, top));
        }
        out
    }

    pub fn set_desktop_notes_raw(&self, notes: &[(String, f64, f64, Option<(f64, f64)>, bool)]) {
        use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
        let mut entries: Vec<Retained<NSDictionary<NSString, objc2::runtime::AnyObject>>> = Vec::new();
        for (id, x, y, size, top) in notes {
            let id_s = NSString::from_str(id);
            let xs = NSNumber::new_f64(*x);
            let ys = NSNumber::new_f64(*y);
            let (ws, hs) = match size {
                Some((w, h)) => (NSNumber::new_f64(*w), NSNumber::new_f64(*h)),
                None => (NSNumber::new_f64(0.0), NSNumber::new_f64(0.0)),
            };
            let ts = NSNumber::new_bool(*top);
            let keys: [
                &NSString; 6
            ] = [
                ns_string!("id"),
                ns_string!("x"),
                ns_string!("y"),
                ns_string!("w"),
                ns_string!("h"),
                ns_string!("top"),
            ];
            let id_obj: &objc2::runtime::AnyObject = unsafe { &*(Retained::as_ptr(&id_s) as *const objc2::runtime::AnyObject) };
            let x_obj: &objc2::runtime::AnyObject = unsafe { &*(Retained::as_ptr(&xs) as *const objc2::runtime::AnyObject) };
            let y_obj: &objc2::runtime::AnyObject = unsafe { &*(Retained::as_ptr(&ys) as *const objc2::runtime::AnyObject) };
            let w_obj: &objc2::runtime::AnyObject = unsafe { &*(Retained::as_ptr(&ws) as *const objc2::runtime::AnyObject) };
            let h_obj: &objc2::runtime::AnyObject = unsafe { &*(Retained::as_ptr(&hs) as *const objc2::runtime::AnyObject) };
            let t_obj: &objc2::runtime::AnyObject = unsafe { &*(Retained::as_ptr(&ts) as *const objc2::runtime::AnyObject) };
            let entries_pair: [&objc2::runtime::AnyObject; 6] = [id_obj, x_obj, y_obj, w_obj, h_obj, t_obj];
            let d = NSDictionary::from_slices(&keys, &entries_pair);
            entries.push(d);
        }
        let refs: Vec<&NSDictionary<NSString, objc2::runtime::AnyObject>> = entries.iter().map(|e| &**e).collect();
        let arr = NSArray::from_slice(&refs);
        unsafe {
            self.defaults().setObject_forKey(
                Some(&*(Retained::as_ptr(&arr) as *const objc2::runtime::AnyObject)),
                ns_string_id("desktopNotes"),
            );
        }
    }

    /// The onboarding checklist: which of the two shortcuts has been tried.
    pub fn welcome_tried(&self) -> std::collections::HashSet<String> {
        self.defaults()
            .stringArrayForKey(ns_string_id("welcomeTried"))
            .map(|a| a.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn set_welcome_tried(&self, tried: &std::collections::HashSet<String>) {
        use objc2_foundation::{NSArray, NSString};
        let strings: Vec<Retained<NSString>> = tried.iter().map(|s| NSString::from_str(s)).collect();
        let refs: Vec<&NSString> = strings.iter().map(|s| &**s).collect();
        let arr = NSArray::from_slice(&refs);
        unsafe {
            self.defaults().setObject_forKey(
                Some(&*(Retained::as_ptr(&arr) as *const objc2::runtime::AnyObject)),
                ns_string_id("welcomeTried"),
            );
        }
    }

    /// ~/Downloads unless overridden. Created on demand.
    pub fn custom_export_dir(&self) -> Option<String> {
        self.defaults()
            .stringForKey(ns_string_id("exportDir"))
            .map(|s| s.to_string())
    }

    pub fn set_custom_export_dir(&self, v: Option<&str>) {
        match v {
            Some(s) if !s.is_empty() => unsafe {
                self.defaults().setObject_forKey(
                    Some(&*objc2_foundation::NSString::from_str(s)),
                    ns_string_id("exportDir"),
                );
            },
            _ => self.defaults().removeObjectForKey(ns_string_id("exportDir")),
        }
    }

    pub fn export_directory(&self) -> std::path::PathBuf {
        let dir = match self.custom_export_dir() {
            Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
            _ => {
                let home = std::env::var_os("HOME").expect("HOME");
                std::path::PathBuf::from(home).join("Downloads")
            }
        };
        if !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
        }
        dir
    }

    pub fn set_record_password_managers(&self, v: bool) {
        self.defaults().setBool_forKey(v, ns_string_id("recordPasswordManagers"));
    }

    pub fn set_paste_mode(&self, v: &str) {
        unsafe {
            self.defaults().setObject_forKey(
                Some(&*objc2_foundation::NSString::from_str(v)),
                ns_string_id("pasteMode"),
            );
        }
    }

    /// "system" (follow macOS), "zh" or "en"; the switch bumps the whole
    /// shelf through `language_changed()` (Swift's `L.languageChanged()`).
    pub fn set_language_pref(&self, v: &str) {
        unsafe {
            self.defaults().setObject_forKey(
                Some(&*objc2_foundation::NSString::from_str(v)),
                ns_string_id("language"),
            );
        }
        crate::app::localization::language_changed();
    }
}

/// NSNumber doubleValue on the erased UserDefaults value.
pub fn m6_object_double(v: &AnyObject) -> f64 {
    unsafe { objc2::msg_send![v, doubleValue] }
}

/// Epoch seconds from either an NSDate or an NSNumber. NSNumber does NOT
/// implement `timeIntervalSince1970`, and NSDate does NOT implement
/// `doubleValue` — dispatch on class (`isKindOfClass`).
pub fn m6_object_time_interval(v: &AnyObject) -> f64 {
    unsafe {
        if objc2::msg_send![v, isKindOfClass: objc2::class!(NSDate)] {
            objc2::msg_send![v, timeIntervalSince1970]
        } else {
            objc2::msg_send![v, doubleValue]
        }
    }
}

/// NSNumber boolValue on the erased UserDefaults value.
fn m6_object_bool(v: &AnyObject) -> bool {
    unsafe { objc2::msg_send![v, boolValue] }
}

use objc2::runtime::AnyObject;
use crate::app::preferences::{ns_string_id, Preferences};
use objc2::rc::Retained;
use objc2_foundation::ns_string;

// MARK: Launch at login (SMAppService mainApp)

/// ServiceManagement.framework carries SMAppService; nothing links it by
/// default (M2's QL pattern from panel.rs), so it loads once here.
fn ensure_service_management_loaded() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let path = NSString::from_str("/System/Library/Frameworks/ServiceManagement.framework");
        if let Some(bundle) = NSBundle::bundleWithPath(&path) {
            unsafe { let _ = bundle.load(); }
        }
    });
}

/// `SMAppService.mainApp.status == .enabled` through the ObjC bridge
/// (preferences row 登录时启动). ServiceManagement has no binding crate in
/// the tree, so the class is resolved by name and message-sent once.
pub fn launch_at_login() -> bool {
    ensure_service_management_loaded();
    unsafe { smapp_main_app_status() == 1 }
}

pub fn set_launch_at_login(want: bool) {
    ensure_service_management_loaded();
    unsafe {
        let Some(cls) = AnyClass::get(c"SMAppService") else { return };
        let service: *mut AnyObject = objc2::msg_send![cls, mainAppService];
        let service = match Retained::retain(service) {
            Some(s) => s,
            None => return,
        };
        let mut error: *mut objc2_foundation::NSError = std::ptr::null_mut();
        if want {
            let _: bool = objc2::msg_send![&*service, registerAndReturnError: &mut error];
        } else {
            let _: () = objc2::msg_send![&*service, unregisterAndReturnError: &mut error];
        }
        let _ = error;
    }
}

/// SMAppService.Status: 0 notRegistered, 1 enabled, 2 requiresApproval, 3 notFound.
unsafe fn smapp_main_app_status() -> i64 {
    let Some(cls) = AnyClass::get(c"SMAppService") else { return 0 };
    let service: *mut AnyObject = objc2::msg_send![cls, mainAppService];
    let service = match Retained::retain(service) {
        Some(s) => s,
        None => return 0,
    };
    let status: i64 = objc2::msg_send![&*service, status];
    status
}

use objc2::runtime::AnyClass;
use objc2_foundation::{NSBundle, NSString};


pub fn toggle_launch_at_login() {
    set_launch_at_login(!launch_at_login());
}


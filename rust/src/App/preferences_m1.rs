//! Port of `App/Preferences.swift` (M1 additions: clipboard settings).
//!
//! Keys and defaults are the Swift 1.0.4 set; self-tests get the same
//! throwaway suite as the M0 keys.

use objc2_foundation::{ns_string, NSDictionary, NSString};

use crate::app::preferences::{ns_string_id, Preferences, BUNDLE_ID};

/// Preference keys (M1 slice; more arrive with their features).
pub mod key {
    pub const RETENTION_DAYS: &str = "retentionDays";
    pub const CLEANUP_HOUR: &str = "cleanupHour";
    pub const MONITORING_PAUSED: &str = "monitoringPaused";
    pub const RECORD_PASSWORD_MANAGERS: &str = "recordPasswordManagers";
    pub const IMAGE_STORAGE: &str = "imageStorage";
    pub const PASTE_MODE: &str = "pasteMode";
    /// Pre-1.0.6 boolean, still honored on read (Preferences.swift:153).
    pub const PASTE_ON_DOUBLE_CLICK: &str = "pasteOnDoubleClick";
    /// M5: recording codec — true = HEVC, false (the shipped default) = H.264.
    pub const RECORD_HEVC: &str = "recordHEVC";
}

pub fn register_m1_defaults(p: &Preferences) {
    let d = p.defaults();
    // Defaults (1.0.4): retention 0 = keep everything; cleanup at 04:00;
    // monitoring on; password managers not recorded; HEIC storage.
    let reg = <NSDictionary<NSString, NSString>>::from_slices(
        &[
            ns_string!(key::RETENTION_DAYS),
            ns_string!(key::CLEANUP_HOUR),
            ns_string!(key::MONITORING_PAUSED),
            ns_string!(key::RECORD_PASSWORD_MANAGERS),
            ns_string!(key::IMAGE_STORAGE),
        ],
        &[
            &*NSString::from_str("0"),
            &*NSString::from_str("4"),
            &*NSString::from_str("0"),
            &*NSString::from_str("0"),
            &*NSString::from_str("heic"),
        ],
    );
    // SAFETY: NSString values keyed by NSString — registerDefaults requires it.
    let any: objc2::rc::Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> =
        unsafe { objc2::rc::Retained::cast_unchecked(reg) };
    unsafe { d.registerDefaults(&any) };
    let _ = BUNDLE_ID; // the domain these live under
}

impl Preferences {
    /// Seconds the store keeps items; 0 = keep everything.
    pub fn retention_days(&self) -> i64 {
        self.defaults().integerForKey(ns_string_id(key::RETENTION_DAYS)) as i64
    }

    pub fn set_retention_days(&self, days: i64) {
        self.defaults()
            .setInteger_forKey(days as isize, ns_string_id(key::RETENTION_DAYS));
    }

    /// The hour of day the calendar-day sweep targets (default 4).
    pub fn cleanup_hour(&self) -> i64 {
        self.defaults().integerForKey(ns_string_id(key::CLEANUP_HOUR)) as i64
    }

    pub fn set_cleanup_hour(&self, hour: i64) {
        self.defaults()
            .setInteger_forKey(hour.clamp(0, 23) as isize, ns_string_id(key::CLEANUP_HOUR));
    }

    pub fn monitoring_paused(&self) -> bool {
        self.defaults()
            .boolForKey(ns_string_id(key::MONITORING_PAUSED))
    }

    pub fn set_monitoring_paused(&self, v: bool) {
        self.defaults()
            .setBool_forKey(v, ns_string_id(key::MONITORING_PAUSED));
    }

    /// Password-manager copies are secrets; recorded only when asked (default no).
    pub fn record_password_managers(&self) -> bool {
        self.defaults()
            .boolForKey(ns_string_id(key::RECORD_PASSWORD_MANAGERS))
    }

    /// "heic" (default) or "png" — what a stored screenshot keeps on disk.
    pub fn image_storage(&self) -> String {
        self.defaults()
            .stringForKey(ns_string_id(key::IMAGE_STORAGE))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "heic".into())
    }

    pub fn set_image_storage(&self, v: &str) {
        // SAFETY: NSString value for an NSString key.
        unsafe {
            self.defaults().setObject_forKey(
                Some(&*NSString::from_str(v)),
                ns_string_id(key::IMAGE_STORAGE),
            );
        }
    }

    pub fn stores_heic(&self) -> bool {
        self.image_storage() == "heic"
    }

    /// "off" / "double" / "return" — what happens after a copy from the
    /// shelf. Unset or invalid values fall back to the legacy boolean
    /// (default true → "double"), verbatim from Preferences.swift:150-156.
    pub fn paste_mode(&self) -> String {
        let d = self.defaults();
        if let Some(m) = d.stringForKey(ns_string_id(key::PASTE_MODE)) {
            let m = m.to_string();
            if ["off", "double", "return"].contains(&m.as_str()) {
                return m;
            }
        }
        let legacy = ns_string_id(key::PASTE_ON_DOUBLE_CLICK);
        let on = if d.objectForKey(legacy).is_some() {
            d.boolForKey(legacy)
        } else {
            true
        };
        if on {
            "double".into()
        } else {
            "off".into()
        }
    }

    /// M5 codec flag (`recordHEVC`; the shipped default is H.264 = false).
    pub fn record_hevc(&self) -> bool {
        self.defaults()
            .boolForKey(ns_string_id(key::RECORD_HEVC))
    }

    pub fn set_record_hevc(&self, v: bool) {
        self.defaults()
            .setBool_forKey(v, ns_string_id(key::RECORD_HEVC));
    }

    /// ⏎ pastes into the previous app only in "return" mode.
    /// 0 = keep everything forever (no sweep ever runs).
    pub fn never_cleans(&self) -> bool {
        self.retention_days() == 0
    }

    pub fn paste_on_return(&self) -> bool {
        self.paste_mode() == "return"
    }

    /// Double-click pastes too unless paste is off (Preferences.swift:157).
    pub fn paste_on_double_click(&self) -> bool {
        self.paste_mode() != "off"
    }
}

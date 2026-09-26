//! Port of `App/Preferences.swift` (M0 slice: the keys the shell needs).
//!
//! UserDefaults-backed settings. Self-tests (PASTORY_STORE set) get a
//! throwaway suite so they never touch the user's real preferences.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_foundation::{ns_string, NSDictionary, NSUserDefaults, NSString};

use crate::app::sandbox;

/// A Carbon-registerable shortcut (port of `Shortcut`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shortcut {
    pub key_code: u32,
    pub carbon_modifiers: u32,
}

// Carbon modifier bits (Events.h): cmdKey=1<<8, shiftKey=1<<9,
// optionKey=1<<11, controlKey=1<<12.
pub const CMD_KEY: u32 = 1 << 8;
pub const SHIFT_KEY: u32 = 1 << 9;
pub const OPTION_KEY: u32 = 1 << 11;
pub const CONTROL_KEY: u32 = 1 << 12;

impl Shortcut {
    pub const NONE: Shortcut = Shortcut { key_code: 0, carbon_modifiers: 0 };

    pub fn is_set(&self) -> bool {
        self.carbon_modifiers != 0
    }

    pub fn encoded(&self) -> String {
        format!("{}:{}", self.key_code, self.carbon_modifiers)
    }

    pub fn from_encoded(s: &str) -> Option<Shortcut> {
        let (k, m) = s.split_once(':')?;
        Some(Shortcut {
            key_code: k.parse().ok()?,
            carbon_modifiers: m.parse().ok()?,
        })
    }

    /// ⌃⌥⇧⌘ order + key name, as in the Swift version.
    pub fn display(&self) -> String {
        if !self.is_set() {
            return crate::app::localization::l("未设置");
        }
        let mut s = String::new();
        if self.carbon_modifiers & CONTROL_KEY != 0 { s.push('⌃'); }
        if self.carbon_modifiers & OPTION_KEY != 0 { s.push('⌥'); }
        if self.carbon_modifiers & SHIFT_KEY != 0 { s.push('⇧'); }
        if self.carbon_modifiers & CMD_KEY != 0 { s.push('⌘'); }
        s + key_code_name(self.key_code)
    }
}

/// Port of `KeyCodeNames`.
pub fn key_code_name(code: u32) -> &'static str {
    match code {
        0 => "A", 1 => "S", 2 => "D", 3 => "F", 4 => "H", 5 => "G", 6 => "Z",
        7 => "X", 8 => "C", 9 => "V", 11 => "B", 12 => "Q", 13 => "W",
        14 => "E", 15 => "R", 16 => "Y", 17 => "T", 31 => "O", 32 => "U",
        34 => "I", 35 => "P", 37 => "L", 38 => "J", 40 => "K", 45 => "N",
        46 => "M",
        18 => "1", 19 => "2", 20 => "3", 21 => "4", 23 => "5", 22 => "6",
        26 => "7", 28 => "8", 25 => "9", 29 => "0",
        36 => "↩", 48 => "⇥", 49 => "Space", 51 => "⌫", 53 => "Esc",
        123 => "←", 124 => "→", 125 => "↓", 126 => "↑",
        122 => "F1", 120 => "F2", 99 => "F3", 118 => "F4", 96 => "F5",
        97 => "F6", 98 => "F7", 100 => "F8", 101 => "F9", 109 => "F10",
        103 => "F11", 111 => "F12",
        _ => "",
    }
}

/// Preference keys (subset used so far; more arrive with their features).
pub mod key {
    pub const HOTKEY_CAPTURE: &str = "hotkeyCapture";
    pub const HOTKEY_SHELF: &str = "hotkeyShelf";
    pub const HOTKEY_SEARCH: &str = "hotkeySearch";
}

/// Bundle id is also the UserDefaults domain; red line 1 — never change it.
pub const BUNDLE_ID: &str = "com.cici.snipclip";

pub struct Preferences {
    d: Retained<NSUserDefaults>,
}

impl Preferences {
    pub fn shared() -> &'static Preferences {
        use std::sync::OnceLock;
        static SHARED: OnceLock<Preferences> = OnceLock::new();
        SHARED.get_or_init(Preferences::new)
    }

    fn new() -> Preferences {
        let d: Retained<NSUserDefaults> = if sandbox::launch().store().is_some() {
            // Self-tests get a throwaway suite, wiped on first use.
            let suite = NSUserDefaults::initWithSuiteName(
                NSUserDefaults::alloc(),
                Some(ns_string!("com.cici.snipclip.selftest")),
            )
            .expect("selftest defaults suite");
            suite.removePersistentDomainForName(ns_string!(
                "com.cici.snipclip.selftest"
            ));
            suite
        } else {
            NSUserDefaults::standardUserDefaults()
        };
        // Defaults (1.0.4): ⌥⌘S capture, ⇧⌘V shelf, ⌥⌘F search;
        // retention 0 = keep everything; cleanup at 4; monitoring on.
        {
            let reg = <NSDictionary<NSString, NSString>>::from_slices(
                &[
                    ns_string!(key::HOTKEY_CAPTURE),
                    ns_string!(key::HOTKEY_SHELF),
                    ns_string!(key::HOTKEY_SEARCH),
                ],
                &[
                    &*NSString::from_str(&Shortcut { key_code: 1, carbon_modifiers: OPTION_KEY | CMD_KEY }.encoded()),
                    &*NSString::from_str(&Shortcut { key_code: 9, carbon_modifiers: SHIFT_KEY | CMD_KEY }.encoded()),
                    &*NSString::from_str(&Shortcut { key_code: 3, carbon_modifiers: OPTION_KEY | CMD_KEY }.encoded()),
                ],
            );
        // registerDefaults takes NSDictionary<NSString, AnyObject>; the
        // encoded-shortcut strings are the right value types here, and
        // `cast_unchecked` is a no-op reinterpretation at the ObjC level.
        // SAFETY: cast_unchecked.
        let any: Retained<NSDictionary<NSString, AnyObject>> =
            unsafe { Retained::cast_unchecked(reg) };
        // SAFETY: the values are NSStrings keyed by NSString, which
        // registerDefaults requires.
        unsafe { d.registerDefaults(&any) };
        }
        let p = Preferences { d };
        // M1 keys (retention, monitoring, image storage) — same suite.
        crate::app::preferences_m1::register_m1_defaults(&p);
        p
    }

    /// The backing suite (M1 settings live on the same instance).
    pub fn defaults(&self) -> Retained<NSUserDefaults> {
        self.d.clone()
    }

    pub fn shortcut(&self, key: &str) -> Shortcut {
        let raw = self.d.stringForKey(ns_string_id(key));
        match raw {
            Some(s) => Shortcut::from_encoded(&s.to_string()).unwrap_or(Shortcut::NONE),
            None => Shortcut::NONE,
        }
    }

    pub fn set_shortcut(&self, key: &str, s: Shortcut) {
        // SAFETY: NSString value for an NSString key.
        unsafe {
            self.d.setObject_forKey(
                Some(&*objc2_foundation::NSString::from_str(&s.encoded())),
                ns_string_id(key),
            );
        }
    }

    /// "system" (follow macOS), "zh" or "en" (M6 adds the settings row).
    pub fn language(&self) -> &'static str {
        match self.d.stringForKey(ns_string_id("language")) {
            Some(s) => Box::leak(s.to_string().into_boxed_str()),
            None => "system",
        }
    }
}

pub fn ns_string_id(key: &str) -> &'static NSString {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<String, &'static NSString>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(s) = c.get(key) {
            return *s;
        }
        let s: &'static NSString = Box::leak(Box::new(NSString::from_str(key)));
        c.insert(key.to_string(), s);
        s
    })
}

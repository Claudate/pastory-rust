//! Layouts: the shelf's *form* as data.
//!
//! `theme_skin.rs` decides what the shelf looks like (colours, radii, faces);
//! this file decides what *shape* the shelf takes. The two axes are
//! orthogonal on purpose: the nine skins are colour recipes and apply to any
//! layout, so "晾衣绳 + 午夜" and "面板 + 暖纸" are both legitimate.
//!
//! A layout is not just geometry — it owns a whole interaction contract.
//! `panel` is the keyboard-driven drawer the app has always had (search box,
//! ⏎ paste, ⌘F, ⌘P, Space). `line` is a click-through strip along the top of
//! the screen that never takes focus (click copies, right-click for a menu).
//! Those contracts are mutually exclusive: a window that takes the keyboard
//! cannot also let every click pass through to the app behind it. That is why
//! the split lives here, at the top, and not inside a skin.
//!
//! Switching layouts rebuilds the panel's whole view tree *and* its frame
//! (`panel::relayout`), unlike a skin switch, which deliberately keeps the
//! frame so the shelf does not jump.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::app::preferences::{ns_string_id, Preferences};
use crate::app::sandbox;

/// Preference key holding the selected layout id (default "panel").
pub const LAYOUT_KEY: &str = "layout";

/// The layout the app falls back to: the drawer it has always shipped.
pub const DEFAULT_ID: &str = "panel";

/// One shelf form.
pub struct Layout {
    /// Stored in preferences; stable, never shown.
    pub id: &'static str,
    /// Shown in the settings picker (Chinese then English, as everywhere).
    pub name: &'static str,
    /// One-line description under the name.
    pub blurb: &'static str,
}

/// Every selectable layout, in picker order.
pub static LAYOUTS: &[Layout] = &[
    Layout {
        id: "panel",
        name: "面板",
        blurb: "底部抽屉，键盘与搜索齐全",
    },
    Layout {
        id: "line",
        name: "晾衣绳",
        blurb: "屏幕顶部挂一排照片，鼠标手势",
    },
];

/// Index into `LAYOUTS` of the selection.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

pub fn all() -> &'static [Layout] {
    LAYOUTS
}

pub fn by_id(id: &str) -> &'static Layout {
    LAYOUTS.iter().find(|l| l.id == id).unwrap_or(&LAYOUTS[0])
}

pub fn current() -> &'static Layout {
    &LAYOUTS[CURRENT.load(Ordering::Relaxed).min(LAYOUTS.len() - 1)]
}

pub fn current_id() -> &'static str {
    current().id
}

/// True when the 晾衣绳 form is in force — the one switch the rest of the app
/// asks about (the panel form is the default and needs no test).
pub fn is_line() -> bool {
    current_id() == "line"
}

/// `PASTORY_LAYOUT=<id>` overrides the stored choice in a self-test run, the
/// same way `PASTORY_SKIN` / `PASTORY_LANG` do, so the render commands can
/// walk every layout without touching the user's preference.
fn env_id() -> Option<String> {
    sandbox::self_test_var("PASTORY_LAYOUT")
}

/// The stored choice, or the default when unset / unknown.
pub fn stored_id() -> String {
    Preferences::shared()
        .defaults()
        .stringForKey(ns_string_id(LAYOUT_KEY))
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_ID.to_string())
}

/// Resolve the layout for this process.
pub fn init() {
    let id = env_id().unwrap_or_else(stored_id);
    let index = LAYOUTS.iter().position(|l| l.id == id).unwrap_or(0);
    CURRENT.store(index, Ordering::Relaxed);
}

/// Persist the selection (the caller has already applied it).
pub fn store(id: &str) {
    // SAFETY: NSString value for an NSString key.
    unsafe {
        Preferences::shared().defaults().setObject_forKey(
            Some(&*objc2_foundation::NSString::from_str(id)),
            ns_string_id(LAYOUT_KEY),
        );
    }
}

/// Select `id`; true when the selection actually changed. Does not touch the
/// window — `panel::relayout` is the caller's job, because the rebuild must
/// happen outside the click that asked for it (see `settings_pane::Choice`).
pub fn select(id: &str) -> bool {
    let Some(index) = LAYOUTS.iter().position(|l| l.id == id) else {
        return false;
    };
    if index == CURRENT.load(Ordering::Relaxed) {
        return false;
    }
    CURRENT.store(index, Ordering::Relaxed);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids are the stored value: a duplicate would make one layout unreachable.
    #[test]
    fn layout_ids_are_unique_and_the_default_exists() {
        let mut seen: Vec<&str> = Vec::new();
        for l in all() {
            assert!(!l.id.is_empty(), "a layout has no id");
            assert!(!seen.contains(&l.id), "duplicate layout id {}", l.id);
            seen.push(l.id);
            assert!(!l.name.is_empty() && !l.blurb.is_empty(), "{} is unlabelled", l.id);
        }
        assert_eq!(all()[0].id, DEFAULT_ID, "the first layout is the fallback");
        assert!(all().iter().any(|l| l.id == DEFAULT_ID));
        assert!(all().iter().any(|l| l.id == "line"), "the second form is missing");
    }

    /// An unknown / missing id must land on the default, never on a panic —
    /// first launch and a downgrade both hit this path.
    #[test]
    fn unknown_ids_fall_back_to_the_default() {
        assert_eq!(by_id("does-not-exist").id, DEFAULT_ID);
        assert_eq!(by_id("").id, DEFAULT_ID);
        assert_eq!(by_id("line").id, "line");
    }
}

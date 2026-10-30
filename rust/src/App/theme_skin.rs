//! Skins: the app's look as data.
//!
//! `theme.rs` is the only place that *draws*; this file is the only place
//! that decides *what* is drawn. A skin bundles a colour recipe (ground,
//! card faces, ink tiers, separator, accent), a geometry recipe (corner
//! radius, shadow weight), a type recipe (body face, brand face) and a
//! material (or none, for skins that paint their own ground).
//!
//! Everything the rest of the app asks for is resolved here against the
//! skin that is currently selected, so switching skins is a repaint and not
//! a rewrite: no call site knows a colour, a radius or a font family.
//!
//! ## Semantics
//!
//! - A cell is `Val::System` when the skin wants AppKit's semantic colour of
//!   that role (it then follows the system appearance and the user's accent
//!   choice), or `Val::Rgb` when the skin pins its own. "原生" is the skin
//!   that pins nothing — the native macOS look the app shipped with.
//! - `Tone` says which rendition a skin is designed for. A tinted skin that
//!   is drawn in the opposite appearance would fight the system (system ink
//!   resolves dark on a dark ground), so a pinned tone is applied to the app
//!   appearance when it launches; `Adaptive` follows the system instead and
//!   carries a second palette for dark.
//! - `material` is the `NSVisualEffectMaterial` raw value behind the panel
//!   (0 = none). With a material, `glass` says how much of the ground is left
//!   transparent so the material shows through; without one the ground is
//!   opaque, which is what keeps the colour-blocked skins legible.

use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::rc::Retained;
use objc2_app_kit::{
    NSAppearance, NSApplication, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSColor,
    NSFont, NSFontWeightBold, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold,
};
use objc2_foundation::NSString;

use crate::app::preferences::{ns_string_id, Preferences};
use crate::app::sandbox;

/// Preference key holding the selected skin id (default "native").
pub const SKIN_KEY: &str = "skin";

/// The skin the app falls back to: no pinned colour, no pinned tone.
pub const DEFAULT_ID: &str = "native";

/// One role's colour: the system's, or a fixed sRGB triple.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Val {
    System,
    Rgb(u8, u8, u8),
}

impl Val {
    pub fn is_system(&self) -> bool {
        matches!(self, Val::System)
    }

    pub fn rgb(&self) -> Option<(u8, u8, u8)> {
        match self {
            Val::System => None,
            Val::Rgb(r, g, b) => Some((*r, *g, *b)),
        }
    }
}

/// A skin's colour recipe for one appearance.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Palette {
    /// Panel ground, behind everything.
    pub bg: Val,
    /// Recessed ground (the sidebar band), painted translucent over the panel.
    pub bg_deep: Val,
    /// Card / sheet face.
    pub surface: Val,
    /// One step off the card face: toggle tracks, wells, unselected chips.
    pub surface_alt: Val,
    /// Primary ink.
    pub label: Val,
    /// Secondary copy.
    pub label_secondary: Val,
    /// Hints, disabled chrome, empty state.
    pub label_tertiary: Val,
    /// Hairlines and card edges.
    pub separator: Val,
    /// Alpha for the separator role (hairlines are rarely opaque).
    pub separator_alpha: f64,
    /// Selection, progress, badges.
    pub accent: Val,
    /// Ink drawn on an accent fill. `System` is white — the pairing AppKit
    /// itself uses on a filled accent surface — so a skin whose accent is
    /// pale (a light cyan-grey, say) must name its own dark ink here.
    pub ink_on_accent: Val,
}

/// A ground wash: two tones painted top to bottom behind everything. Kept
/// separate from `Palette` because it is optional — a skin without one keeps
/// the flat ground colour it already names in `bg`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gradient {
    /// Top of the panel.
    pub from: Val,
    /// Bottom of the panel.
    pub to: Val,
}

impl Palette {
    /// Every role on the system's semantic palette — the native look.
    pub const fn system() -> Palette {
        Palette {
            bg: Val::System,
            bg_deep: Val::System,
            surface: Val::System,
            surface_alt: Val::System,
            label: Val::System,
            label_secondary: Val::System,
            label_tertiary: Val::System,
            separator: Val::System,
            separator_alpha: 1.0,
            accent: Val::System,
            ink_on_accent: Val::System,
        }
    }
}

/// Which appearance a skin is drawn in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Pins light (a paper-coloured skin under a dark system would be a mess).
    Light,
    /// Pins dark.
    Dark,
    /// Follows the system, with `palette_dark` for the dark rendition.
    Adaptive,
}

/// Body-face weights, named so a skin can map them onto its own faces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
    Bold,
}

/// A whole look.
pub struct Skin {
    /// Stored in preferences; stable, never shown.
    pub id: &'static str,
    /// Shown in the settings picker (Chinese then English, as everywhere).
    pub name: &'static str,
    /// One-line description under the name.
    pub blurb: &'static str,
    pub tone: Tone,
    /// Corner-radius multiplier (1.0 = the app's `RADIUS_*` values).
    pub radius: f64,
    /// Shadow-weight multiplier.
    pub shadow: f64,
    /// Panel material raw value (0 = none).
    pub material: i64,
    /// How much of the ground is left transparent over the material.
    pub glass: f64,
    /// Body face per weight (PostScript name; "" = the system UI font).
    pub ui_regular: &'static str,
    pub ui_bold: &'static str,
    /// Brand wordmark face ("" = the bundled brand font, then the system).
    pub brand: &'static str,
    /// The optional ground wash, painted behind the whole panel.
    pub gradient: Option<Gradient>,
    /// The light rendition; also the only one when `palette_dark` is None.
    pub palette: Palette,
    /// The dark rendition, for `Adaptive` skins.
    pub palette_dark: Option<Palette>,
}

impl Skin {
    pub fn is_pinned(&self) -> bool {
        !matches!(self.tone, Tone::Adaptive)
    }
}

/// The body face for a weight, given the skin's faces.
fn face_for(skin: &Skin, w: Weight) -> &'static str {
    match w {
        Weight::Regular | Weight::Medium => skin.ui_regular,
        Weight::Semibold | Weight::Bold => {
            if skin.ui_bold.is_empty() {
                skin.ui_regular
            } else {
                skin.ui_bold
            }
        }
    }
}

use Weight::{Bold, Medium, Regular, Semibold};

/// Every selectable skin, in picker order.
pub static SKINS: &[Skin] = &[
    Skin {
        id: "native",
        name: "原生",
        blurb: "系统语义色与材质，跟随浅色 / 深色",
        gradient: None,
        tone: Tone::Adaptive,
        radius: 1.0,
        shadow: 1.0,
        material: 21, // underWindowBackground
        glass: 1.0,
        ui_regular: "",
        ui_bold: "",
        brand: "",
        palette: Palette::system(),
        palette_dark: Some(Palette::system()),
    },
    Skin {
        id: "papyrus",
        name: "暖纸",
        blurb: "羊皮纸底、石墨字、赭石强调",
        gradient: Some(Gradient { from: Val::Rgb(249, 243, 233), to: Val::Rgb(232, 221, 200) }),
        tone: Tone::Light,
        radius: 1.1,
        shadow: 1.15,
        material: 0,
        glass: 0.0,
        ui_regular: "",
        ui_bold: "",
        brand: "",
        palette: Palette {
            bg: Val::Rgb(243, 236, 222),
            bg_deep: Val::Rgb(234, 225, 207),
            surface: Val::Rgb(252, 249, 242),
            surface_alt: Val::Rgb(232, 223, 205),
            label: Val::Rgb(46, 40, 32),
            label_secondary: Val::Rgb(102, 92, 78),
            label_tertiary: Val::Rgb(152, 142, 126),
            separator: Val::Rgb(198, 186, 166),
            separator_alpha: 1.0,
            accent: Val::Rgb(146, 88, 38),
            ink_on_accent: Val::Rgb(255, 255, 255),
        },
        palette_dark: None,
    },
    Skin {
        id: "graphite",
        name: "墨玉",
        blurb: "炭灰底、白墨、青灰强调",
        gradient: Some(Gradient { from: Val::Rgb(38, 40, 45), to: Val::Rgb(19, 20, 23) }),
        tone: Tone::Dark,
        radius: 1.0,
        shadow: 1.3,
        material: 0,
        glass: 0.0,
        ui_regular: "",
        ui_bold: "",
        brand: "",
        palette: Palette {
            bg: Val::Rgb(28, 29, 32),
            bg_deep: Val::Rgb(22, 23, 26),
            surface: Val::Rgb(38, 40, 44),
            surface_alt: Val::Rgb(48, 50, 55),
            label: Val::Rgb(238, 240, 243),
            label_secondary: Val::Rgb(166, 171, 179),
            label_tertiary: Val::Rgb(118, 123, 131),
            separator: Val::Rgb(62, 65, 70),
            separator_alpha: 1.0,
            accent: Val::Rgb(126, 186, 214),
            ink_on_accent: Val::Rgb(18, 26, 32),
        },
        palette_dark: None,
    },
    Skin {
        id: "midnight",
        name: "午夜",
        blurb: "深蓝底、月光白、钴蓝强调",
        gradient: Some(Gradient { from: Val::Rgb(25, 35, 62), to: Val::Rgb(10, 14, 29) }),
        tone: Tone::Dark,
        radius: 1.05,
        shadow: 1.4,
        material: 0,
        glass: 0.0,
        ui_regular: "",
        ui_bold: "",
        brand: "",
        palette: Palette {
            bg: Val::Rgb(17, 24, 43),
            bg_deep: Val::Rgb(12, 18, 34),
            surface: Val::Rgb(25, 34, 56),
            surface_alt: Val::Rgb(35, 46, 71),
            label: Val::Rgb(228, 234, 248),
            label_secondary: Val::Rgb(157, 170, 199),
            label_tertiary: Val::Rgb(110, 122, 150),
            separator: Val::Rgb(45, 58, 86),
            separator_alpha: 1.0,
            accent: Val::Rgb(58, 102, 205),
            ink_on_accent: Val::Rgb(255, 255, 255),
        },
        palette_dark: None,
    },
    Skin {
        id: "mariner",
        name: "深海",
        blurb: "白瓷底、深蓝墨、黄铜点",
        gradient: Some(Gradient { from: Val::Rgb(247, 251, 255), to: Val::Rgb(220, 230, 241) }),
        tone: Tone::Light,
        radius: 0.9,
        shadow: 0.9,
        material: 0,
        glass: 0.0,
        ui_regular: "",
        ui_bold: "",
        brand: "",
        palette: Palette {
            bg: Val::Rgb(240, 244, 248),
            bg_deep: Val::Rgb(228, 234, 241),
            surface: Val::Rgb(252, 253, 255),
            surface_alt: Val::Rgb(226, 233, 241),
            label: Val::Rgb(20, 38, 58),
            label_secondary: Val::Rgb(80, 98, 120),
            label_tertiary: Val::Rgb(134, 150, 168),
            separator: Val::Rgb(196, 207, 220),
            separator_alpha: 1.0,
            accent: Val::Rgb(28, 92, 148),
            ink_on_accent: Val::Rgb(255, 255, 255),
        },
        palette_dark: None,
    },
    Skin {
        id: "turbine",
        name: "黄铜",
        blurb: "灰绿底、暗铜线、工业感",
        gradient: Some(Gradient { from: Val::Rgb(233, 233, 226), to: Val::Rgb(203, 206, 197) }),
        tone: Tone::Light,
        radius: 0.8,
        shadow: 1.2,
        material: 0,
        glass: 0.0,
        ui_regular: "",
        ui_bold: "",
        brand: "",
        palette: Palette {
            bg: Val::Rgb(222, 223, 216),
            bg_deep: Val::Rgb(207, 209, 201),
            surface: Val::Rgb(240, 240, 234),
            surface_alt: Val::Rgb(206, 208, 200),
            label: Val::Rgb(35, 37, 33),
            label_secondary: Val::Rgb(80, 84, 76),
            label_tertiary: Val::Rgb(136, 140, 131),
            separator: Val::Rgb(174, 176, 166),
            separator_alpha: 1.0,
            accent: Val::Rgb(150, 106, 44),
            ink_on_accent: Val::System,
        },
        palette_dark: None,
    },
    Skin {
        id: "terminal",
        name: "终端",
        blurb: "近黑底、等宽字、荧光绿",
        gradient: Some(Gradient { from: Val::Rgb(15, 22, 16), to: Val::Rgb(5, 8, 6) }),
        tone: Tone::Dark,
        radius: 0.5,
        shadow: 1.2,
        material: 0,
        glass: 0.0,
        ui_regular: "Menlo-Regular",
        ui_bold: "Menlo-Bold",
        brand: "Menlo-Bold",
        palette: Palette {
            bg: Val::Rgb(12, 14, 12),
            bg_deep: Val::Rgb(8, 10, 8),
            surface: Val::Rgb(18, 21, 18),
            surface_alt: Val::Rgb(26, 31, 26),
            label: Val::Rgb(216, 255, 220),
            label_secondary: Val::Rgb(140, 196, 148),
            label_tertiary: Val::Rgb(94, 140, 100),
            separator: Val::Rgb(40, 62, 44),
            separator_alpha: 1.0,
            accent: Val::Rgb(74, 222, 128),
            ink_on_accent: Val::Rgb(15, 20, 15),
        },
        palette_dark: None,
    },
    Skin {
        id: "patchwork",
        name: "拼布",
        blurb: "柔紫底、洋红点、圆润柔软",
        gradient: Some(Gradient { from: Val::Rgb(251, 247, 254), to: Val::Rgb(230, 219, 241) }),
        tone: Tone::Light,
        radius: 1.5,
        shadow: 1.1,
        material: 0,
        glass: 0.0,
        ui_regular: "",
        ui_bold: "",
        brand: "",
        palette: Palette {
            bg: Val::Rgb(244, 240, 247),
            bg_deep: Val::Rgb(234, 227, 241),
            surface: Val::Rgb(253, 251, 255),
            surface_alt: Val::Rgb(232, 224, 241),
            label: Val::Rgb(45, 36, 55),
            label_secondary: Val::Rgb(102, 89, 116),
            label_tertiary: Val::Rgb(150, 138, 162),
            separator: Val::Rgb(212, 201, 222),
            separator_alpha: 1.0,
            accent: Val::Rgb(178, 68, 152),
            ink_on_accent: Val::System,
        },
        palette_dark: None,
    },
    Skin {
        id: "sakura",
        name: "樱",
        blurb: "藕粉底、墨紫字、暖粉强调",
        gradient: Some(Gradient { from: Val::Rgb(255, 248, 250), to: Val::Rgb(241, 226, 232) }),
        tone: Tone::Light,
        radius: 1.3,
        shadow: 0.95,
        material: 0,
        glass: 0.0,
        ui_regular: "",
        ui_bold: "",
        brand: "",
        palette: Palette {
            bg: Val::Rgb(250, 242, 244),
            bg_deep: Val::Rgb(243, 231, 235),
            surface: Val::Rgb(255, 252, 253),
            surface_alt: Val::Rgb(243, 230, 235),
            label: Val::Rgb(54, 40, 48),
            label_secondary: Val::Rgb(116, 94, 104),
            label_tertiary: Val::Rgb(164, 142, 152),
            separator: Val::Rgb(224, 206, 214),
            separator_alpha: 1.0,
            accent: Val::Rgb(176, 60, 90),
            ink_on_accent: Val::Rgb(255, 255, 255),
        },
        palette_dark: None,
    },
];

/// Index into `SKINS` of the selection.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

pub fn all() -> &'static [Skin] {
    SKINS
}

pub fn by_id(id: &str) -> &'static Skin {
    SKINS
        .iter()
        .find(|s| s.id == id)
        .unwrap_or(&SKINS[0])
}

pub fn current() -> &'static Skin {
    &SKINS[CURRENT.load(Ordering::Relaxed).min(SKINS.len() - 1)]
}

pub fn current_id() -> &'static str {
    current().id
}

/// The stored choice, or the default when unset / unknown.
pub fn stored_id() -> String {
    Preferences::shared()
        .defaults()
        .stringForKey(ns_string_id(SKIN_KEY))
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_ID.to_string())
}

/// `PASTORY_SKIN=<id>` overrides the stored choice in a self-test run, the
/// same way `PASTORY_LANG` / `PASTORY_APPEARANCE` do — the render commands
/// walk every skin without touching the user's preference.
fn env_id() -> Option<String> {
    sandbox::self_test_var("PASTORY_SKIN")
}

/// Resolve and store the skin for this process.
pub fn init() {
    let id = env_id().unwrap_or_else(stored_id);
    let index = SKINS.iter().position(|s| s.id == id).unwrap_or(0);
    CURRENT.store(index, Ordering::Relaxed);
    apply_pinned_appearance();
}

/// A skin's preview swatch: ground, accent, primary ink and the ink that
/// stays readable on the accent, resolved for the settings picker. An
/// adaptive skin (原生) previews the rendition the pane is drawn in, so the
/// swatch does not promise a white ground under a dark system — and any
/// semantic role falls back to AppKit's own colour, which resolves as drawn.
pub fn swatch(skin: &Skin) -> (Retained<NSColor>, Retained<NSColor>, Retained<NSColor>, Retained<NSColor>) {
    let p = if skin.tone == Tone::Adaptive && dark_rendition() {
        skin.palette_dark.as_ref().unwrap_or(&skin.palette)
    } else {
        &skin.palette
    };
    (
        color(p.bg, || NSColor::windowBackgroundColor()),
        color(p.accent, || NSColor::controlAccentColor()),
        color(p.label, || NSColor::labelColor()),
        pick_ink_on_accent(p),
    )
}

/// Persist the selection (the caller has already applied it).
pub fn store(id: &str) {
    // SAFETY: NSString value for an NSString key.
    unsafe {
        Preferences::shared().defaults().setObject_forKey(
            Some(&*NSString::from_str(id)),
            ns_string_id(SKIN_KEY),
        );
    }
}

/// Select `id`; true when the selection actually changed.
pub fn select(id: &str) -> bool {
    let Some(index) = SKINS.iter().position(|s| s.id == id) else {
        return false;
    };
    if index == CURRENT.load(Ordering::Relaxed) {
        return false;
    }
    CURRENT.store(index, Ordering::Relaxed);
    apply_pinned_appearance();
    true
}

/// Pin the app appearance for a skin that is designed for one tone only, so
/// the parts AppKit draws itself (sidebar selection ink, text fields) match
/// the skin's palette. `Adaptive` clears the pin.
fn apply_pinned_appearance() {
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let token = match current().tone {
        Tone::Adaptive => None,
        Tone::Light => Some(unsafe { NSAppearanceNameAqua }),
        Tone::Dark => Some(unsafe { NSAppearanceNameDarkAqua }),
    };
    let appearance = token.and_then(NSAppearance::appearanceNamed);
    app.setAppearance(appearance.as_deref());
}

/// Whether this panel draws the dark rendition: a pinned tone decides it
/// outright, an adaptive skin follows the system.
pub fn dark_rendition() -> bool {
    match current().tone {
        Tone::Dark => true,
        Tone::Light => false,
        Tone::Adaptive => system_is_dark(),
    }
}

/// The system appearance, with `PASTORY_APPEARANCE=dark|light` honoured for
/// self-tests (any other value falls through to the system). A headless run
/// treats "no application" as light.
fn system_is_dark() -> bool {
    match sandbox::self_test_var("PASTORY_APPEARANCE").as_deref() {
        Some("dark") => return true,
        Some("light") => return false,
        _ => {}
    }
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let name = app.effectiveAppearance().name().to_string();
    name.contains("Dark")
}

/// The colour recipe in force for this panel.
pub fn palette() -> &'static Palette {
    let skin = current();
    if dark_rendition() {
        skin.palette_dark.as_ref().unwrap_or(&skin.palette)
    } else {
        &skin.palette
    }
}

/// Corner radius for a role, scaled by the skin.
pub fn radius(base: f64) -> f64 {
    base * current().radius
}

/// Shadow weight for a role, scaled by the skin.
pub fn shadow_alpha(a: f64) -> f64 {
    (a * current().shadow).clamp(0.0, 1.0)
}

/// The panel's material raw value (0 = none: the skin paints its ground).
pub fn material_code() -> i64 {
    current().material
}

/// How much of the ground stays transparent over the material.
pub fn glass() -> f64 {
    if material_code() == 0 {
        0.0
    } else {
        current().glass
    }
}

/// Resolve a role: the skin's own colour, or the system's.
pub fn color(v: Val, system: impl FnOnce() -> Retained<NSColor>) -> Retained<NSColor> {
    match v {
        Val::System => system(),
        Val::Rgb(r, g, b) => {
            NSColor::colorWithSRGBRed_green_blue_alpha(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0)
        }
    }
}

/// `color` at an explicit alpha. A semantic colour is returned untouched at
/// full alpha: `colorWithAlphaComponent` resolves a dynamic colour on the spot
/// and hands back a frozen one, which would pin the app to whichever
/// appearance happened to be current (the old skin's dark-mode bug).
pub fn color_alpha(v: Val, a: f64, system: impl FnOnce() -> Retained<NSColor>) -> Retained<NSColor> {
    match v {
        Val::Rgb(r, g, b) => srgb4(r, g, b, a),
        Val::System => {
            let c = system();
            if a >= 1.0 {
                c
            } else {
                c.colorWithAlphaComponent(a)
            }
        }
    }
}

/// Ink that stays readable on an accent fill — the label inside a selected
/// chip, the text on a filled button.
///
/// `System` is white: the ink AppKit pairs with a filled accent surface
/// (`alternateSelectedControlTextColor`). A skin whose accent is pale names
/// its own dark ink instead of leaving white on tan, and
/// `ink_on_accent_is_readable` holds every skin to 4.5:1 either way.
pub fn text_on_accent() -> Retained<NSColor> {
    pick_ink_on_accent(palette())
}

/// Ink for a palette's accent fill: the skin's own choice, else white.
pub fn pick_ink_on_accent(p: &Palette) -> Retained<NSColor> {
    match p.ink_on_accent {
        Val::Rgb(r, g, b) => srgb4(r, g, b, 1.0),
        Val::System => srgb4(255, 255, 255, 1.0),
    }
}

/// A fixed sRGB colour at an explicit alpha — the skins name their tones for
/// the current macOS surface look, which is deliberately translucent.
pub fn srgb4(r: u8, g: u8, b: u8, a: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        r as f64 / 255.0,
        g as f64 / 255.0,
        b as f64 / 255.0,
        a,
    )
}

/// The body font for a size and weight: the skin's face, or the system UI
/// font (which is what makes the app honour the user's text-size setting).
pub fn ui_font(size: f64, weight: Weight) -> Retained<NSFont> {
    let skin = current();
    let name = face_for(skin, weight);
    if !name.is_empty() {
        if let Some(f) = NSFont::fontWithName_size(&NSString::from_str(name), size) {
            return f;
        }
    }
    let w = match weight {
        Regular => unsafe { NSFontWeightRegular },
        Medium => unsafe { NSFontWeightMedium },
        Semibold => unsafe { NSFontWeightSemibold },
        Bold => unsafe { NSFontWeightBold },
    };
    NSFont::systemFontOfSize_weight(size, w)
}

/// The brand wordmark face: the skin's own, else "" so the caller falls back
/// to the bundled brand font and then the system face.
pub fn brand_face() -> &'static str {
    current().brand
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids are the stored value: a duplicate would make one skin unreachable.
    #[test]
    fn skin_ids_are_unique_and_the_default_exists() {
        let mut seen: Vec<&str> = Vec::new();
        for s in all() {
            assert!(!s.id.is_empty(), "a skin has no id");
            assert!(!seen.contains(&s.id), "duplicate skin id {}", s.id);
            seen.push(s.id);
            assert!(!s.name.is_empty() && !s.blurb.is_empty(), "{} is unlabelled", s.id);
        }
        assert_eq!(all()[0].id, DEFAULT_ID, "the first skin is the fallback");
        assert!(all().iter().any(|s| s.id == DEFAULT_ID));
    }

    /// A skin must carry both renditions when it follows the system, and a
    /// pinned skin must not need the other one.
    #[test]
    fn adaptive_skins_carry_a_dark_rendition() {
        for s in all() {
            match s.tone {
                Tone::Adaptive => assert!(s.palette_dark.is_some(), "{} is adaptive without a dark palette", s.id),
                Tone::Light | Tone::Dark => {}
            }
        }
    }

    /// Every skin must draw legible body text on its own ground: the whole
    /// point of pinning ink in a skin is that light-on-light cannot happen.
    #[test]
    fn ink_contrasts_with_the_ground() {
        for s in all() {
            let palettes = [Some(&s.palette), s.palette_dark.as_ref()];
            for (n, p) in palettes.into_iter().enumerate() {
                let Some(p) = p else { continue };
                let (Some(bg), Some(fg)) = (p.bg.rgb(), p.label.rgb()) else {
                    continue; // semantic: the system guarantees its own contrast
                };
                let ratio = contrast(bg, fg);
                assert!(
                    ratio >= 4.5,
                    "{} rendition {}: ink {:?} on ground {:?} is only {:.2}:1",
                    s.id,
                    n,
                    fg,
                    bg,
                    ratio
                );
            }
        }
    }

    /// Ink drawn on an accent fill must clear the same bar, for both the
    /// skin's explicit choice and the luminance-picked default.
    #[test]
    fn ink_on_accent_is_readable() {
        for s in all() {
            for p in [Some(&s.palette), s.palette_dark.as_ref()].into_iter().flatten() {
                let Some((ar, ag, ab)) = p.accent.rgb() else { continue };
                let ink = match p.ink_on_accent {
                    Val::Rgb(r, g, b) => (r, g, b),
                    Val::System => (255, 255, 255),
                };
                let ratio = contrast((ar, ag, ab), ink);
                assert!(ratio >= 4.5, "{}: ink {:?} on accent {:?} is {:.2}:1", s.id, ink, (ar, ag, ab), ratio);
            }
        }
    }

    /// Body copy sits directly on the panel ground, and a skin with a gradient
    /// paints two different grounds behind it — so ink has to clear the bar at
    /// *both* ends of the wash, not just against the flat `bg` it replaced.
    /// `label_tertiary` is exempt on purpose: it is the disabled / chrome role,
    /// which WCAG excludes, and every empty state uses `label_secondary`.
    #[test]
    fn ink_contrasts_with_both_ends_of_the_gradient() {
        for s in all() {
            let Some(g) = s.gradient else { continue };
            let (Some(from), Some(to)) = (g.from.rgb(), g.to.rgb()) else { continue };
            for p in [Some(&s.palette), s.palette_dark.as_ref()].into_iter().flatten() {
                let (Some(pri), Some(sec)) = (p.label.rgb(), p.label_secondary.rgb()) else { continue };
                for (end, ground) in [("from", from), ("to", to)] {
                    let rp = contrast(ground, pri);
                    assert!(
                        rp >= 4.5,
                        "{}: ink {:?} on gradient {} {:?} is only {:.2}:1",
                        s.id, pri, end, ground, rp
                    );
                    let rs = contrast(ground, sec);
                    assert!(
                        rs >= 4.5,
                        "{}: secondary {:?} on gradient {} {:?} is only {:.2}:1",
                        s.id, sec, end, ground, rs
                    );
                }
            }
        }
    }

    /// Radii and shadow weights have to stay usable: a zero-radius skin would
    /// square off every control, a huge multiplier would round cards into pills.
    #[test]
    fn geometry_stays_in_range() {
        for s in all() {
            assert!(s.radius >= 0.25 && s.radius <= 2.0, "{}: radius {}", s.id, s.radius);
            assert!(s.shadow >= 0.5 && s.shadow <= 2.0, "{}: shadow {}", s.id, s.shadow);
            assert!((0.0..=1.0).contains(&s.glass), "{}: glass {}", s.id, s.glass);
            if s.material == 0 {
                assert_eq!(s.glass, 0.0, "{}: no material means an opaque ground", s.id);
            }
        }
    }

    /// WCAG relative luminance of a fixed sRGB triple.
    fn relative(c: (u8, u8, u8)) -> f64 {
        let ch = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.0) + 0.7152 * ch(c.1) + 0.0722 * ch(c.2)
    }

    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (la, lb) = (relative(a), relative(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }
}

//! One place for the look: a native macOS visual language — dynamic
//! surfaces, system type, continuous-corner shapes.
//!
//! 1.0.7 replaces the stationery skin this file used to port from
//! `App/Theme.swift`. That skin was copied from the frozen Swift target
//! value for value (brown desk, cream paper, torn edges, handwritten
//! type), which made the two builds visually identical — the opposite of
//! what an independent product needs — and it had no dark mode at all.
//! `Sources/Pastory/` stays the behaviour baseline for geometry and
//! interaction; the *look* now lives here alone and is deliberately
//! different.
//!
//! The look itself is data and lives in [`crate::app::theme_skin`]: a skin
//! bundles a colour recipe, corner radii, shadow weight, body face and
//! panel material, and every accessor here resolves against whichever skin
//! is selected. Nothing outside this file (and its skin table) names a
//! colour, a radius or a font family, so adding a skin is a table row and a
//! skin switch is a repaint.
//!
//! The default skin ("原生") pins nothing: it asks AppKit for its semantic
//! palette (`labelColor`, `controlBackgroundColor`, `separatorColor`,
//! `controlAccentColor` …), each of which resolves against the current
//! appearance when it is drawn, so light and dark both work with no literal
//! colour values. Text is the system UI font, so it follows the user's
//! text-size setting. The brand face survives only as the wordmark and card
//! titles (`script` / `brand_font`).
//!
//! Naming note: `serif` and the `paper*` names are kept where a rename
//! would touch a hundred call sites with no behaviour change, but nothing
//! here is serif or paper any more — `serif` is the body face (system UI),
//! and the shapes have continuous corners.
//!
//! The panel rides on a real `NSVisualEffectView` material
//! (`shelf::view::build`), so the desktop tints it and the window reads as a
//! native macOS surface. A material is composited by the window server and
//! cannot appear in an offscreen snapshot, so `panel_material_alpha` is the
//! one switch that turns the translucency on for a live window and off for
//! the render self-tests — one drawing path serves both.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSCompositingOperation, NSFont, NSFontDescriptor, NSGradient,
    NSGraphicsContext, NSImage, NSImageRep, NSRectFill, NSRectFillUsingOperation, NSShadow, NSView,
    NSWorkspace,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGImage,
};
use objc2_foundation::{ns_string, NSDictionary, NSString};

use crate::app::theme_skin::{self, Weight};

/// Where resources live. A normal launch: the bundle's Resources. A dev run /
/// self-test (`cargo run`, the bare binary): `<repo>/rust/target/…/pastory`
/// three levels under `rust/target`, so fall back to the repo's `Resources/`.
fn dev_resources() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // <…>/rust/target/{debug,release}/pastory → walk up to the repo root.
    for ancestor in exe.ancestors() {
        let fonts = ancestor.join("Resources").join("Fonts");
        if fonts.is_dir() {
            return Some(ancestor.join("Resources"));
        }
    }
    None
}

fn bundle_resources() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.ancestors().nth(3)?; // …/PasteJar.app/Contents/MacOS/PasteJar
    if bundle.file_name()?.to_str()? == "Contents" {
        return Some(bundle.join("Resources"));
    }
    None
}

fn resource_path(name: &str) -> Option<PathBuf> {
    for base in [bundle_resources(), dev_resources()].into_iter().flatten() {
        let p = base.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// `NSImage(contentsOf:)` — None for a path that does not decode (same
/// shape as the Swift img := NSImage(contentsOfFile:)).
fn image_from_path(p: &PathBuf) -> Option<Retained<NSImage>> {
    let img = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&p.to_string_lossy()))?;
    // An unreadable file yields an empty NSImage, non-nil: it must not pass
    // downstream as "the image" (NULL class derefs in the draw paths).
    if img.size().width <= 0.0 || img.size().height <= 0.0 {
        None
    } else {
        Some(img)
    }
}

/// `Theme.resource(_:)` wrapped in a process-lifetime cache.
fn cached_named_image(cell: &OnceLock<Option<usize>>, name: &str) -> Option<Retained<NSImage>> {
    let ptr = *cell.get_or_init(|| {
        let img = resource_path(name).and_then(|p| image_from_path(&p))?;
        Some(Retained::into_raw(img) as usize)
    });
    // SAFETY: the reference was into_raw'd once above and never leaves the
    // cell; `retain` adds the caller's own retain (store.rs thumb_cache style).
    unsafe { Retained::retain(ptr? as *mut NSImage) }
}

/// Resources \<name\>.png on the resource path (contact QR codes).
pub fn resource_named(name: &str) -> Option<Retained<NSImage>> {
    resource_path(name).and_then(|p| image_from_path(&p))
}

/// Product logo (Resources/Logo.png).
pub fn logo() -> Option<Retained<NSImage>> {
    static CELL: OnceLock<Option<usize>> = OnceLock::new();
    cached_named_image(&CELL, "Logo.png")
}

/// Brand mark without the paper card (Resources/LogoMark.png): the same
/// watercolour glyph as an alpha-only mask, so the sidebar can tint it with
/// the skin's ink instead of pasting a cream tile on a cream background.
///
/// Compiled in rather than loaded from the bundle on purpose: the build
/// scripts copy a fixed whitelist out of `Resources/`, and a mark that only
/// exists on disk fails softly — the wordmark still draws, so a missing
/// brand mark would reach a release unnoticed.
pub fn logo_mark() -> Option<Retained<NSImage>> {
    static CELL: OnceLock<Option<usize>> = OnceLock::new();
    let ptr = *CELL.get_or_init(|| {
        let data = objc2_foundation::NSData::with_bytes(include_bytes!("../../../Resources/LogoMark.png"));
        let img = NSImage::initWithData(NSImage::alloc(), &data)?;
        if img.size().width <= 0.0 || img.size().height <= 0.0 {
            return None;
        }
        Some(Retained::into_raw(img) as usize)
    });
    // SAFETY: same contract as `cached_named_image` — the raw reference never
    // leaves this cell; `retain` hands the caller an owned reference.
    unsafe { Retained::retain(ptr? as *mut NSImage) }
}

/// The 晾衣绳's clothes pin (Resources/ClothesPin.png) — game-icons'
/// `clothespin` by Lorc, CC BY 3.0, baked to the clip's own size. Cached: a
/// rope of ten cards redraws it thirty times a second.
pub fn clothes_pin() -> Option<Retained<NSImage>> {
    static CELL: OnceLock<Option<usize>> = OnceLock::new();
    cached_named_image(&CELL, "ClothesPin.png")
}

/// Menu bar glyph (designer's folded-P asset, 1x + 2x). Template: macOS tints
/// it for light / dark menu bars — the app itself never switches colors.
pub fn menu_icon() -> Option<Retained<NSImage>> {
    static ICON: OnceLock<Option<usize>> = OnceLock::new();
    let ptr = *ICON.get_or_init(|| {
        let base = resource_path("MenuIcon.png")?;
        let img = image_from_path(&base)?;
        let icon = NSImage::initWithSize(
            NSImage::alloc(),
            objc2_core_foundation::CGSize::new(20.0, 20.0),
        );
        for rep in unsafe { base_reps(&img) } {
            icon.addRepresentation(&rep);
        }
        if let Some(hi) = resource_path("MenuIcon@2x.png").and_then(|p| image_from_path(&p)) {
            for rep in unsafe { base_reps(&hi) } {
                rep.setSize(objc2_core_foundation::CGSize::new(20.0, 20.0));
                icon.addRepresentation(&rep);
            }
        }
        icon.setTemplate(true);
        // Cached as a raw reference for the process lifetime (same shape as
        // `pushpin`/`logo`): the panel reads template-mode system tinting.
        Some(Retained::into_raw(icon) as usize)
    });
    let Some(raw) = ptr else { return None };
    unsafe { Retained::retain(raw as *mut NSImage) }
}

/// `NSImage.representations` — the NSBitmapImageReps of a loaded PNG.
/// SAFETY: `img` must be a valid NSImage; returns its representations.
unsafe fn base_reps(img: &NSImage) -> Vec<Retained<NSImageRep>> {
    let reps: Retained<objc2_foundation::NSArray<NSImageRep>> =
        objc2::msg_send![img, representations];
    reps.to_vec()
}

/// Brand typeface (Ysabeau Office, OFL) bundled in Resources/Fonts;
/// registered for this process on first use. True when any face registered —
/// the M0 spike for CTFontManagerRegisterFontsForURL.
pub fn register_brand_fonts() -> bool {
    static DONE: OnceLock<bool> = OnceLock::new();
    *DONE.get_or_init(|| {
        use objc2_core_foundation::{CFString, CFURL, CFURLPathStyle};
        use objc2_core_text::{CTFontManagerRegisterFontsForURL, CTFontManagerScope};
        let mut any = false;
        for name in ["YsabeauOffice.ttf"] {
            for dir in font_dirs() {
                let path = dir.join(name);
                if !path.is_file() {
                    continue;
                }
                // SAFETY: plain CF constructors; the registration is scoped to
                // this process, matching the Swift `.process` scope.
                unsafe {
                    let cf_path = CFString::from_str(&path.to_string_lossy());
                    if let Some(url) = CFURL::with_file_system_path(
                        None,
                        Some(&cf_path),
                        CFURLPathStyle::CFURLPOSIXPathStyle,
                        false,
                    ) {
                        if CTFontManagerRegisterFontsForURL(
                            &url,
                            CTFontManagerScope::Process,
                            std::ptr::null_mut(),
                        ) {
                            any = true;
                            break;
                        }
                    }
                }
            }
        }
        any
    })
}

/// Bundle Resources/Fonts first, then the source tree (self-tests / `cargo run`).
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(r) = bundle_resources() {
        dirs.push(r.join("Fonts"));
    }
    if let Some(d) = dev_resources() {
        dirs.push(d.join("Fonts"));
    }
    dirs
}

// MARK: SplitMix64 (Annotate/AnnotationRenderer.swift:241)

/// Tiny deterministic generator (SplitMix64) so a sketch's wobble never
/// changes between frames. Bit-identical with the Swift `Seeded`; the
/// annotation sketcher is its remaining user.
pub struct Seeded(pub u64);

impl Seeded {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

// MARK: Palette — resolved through the selected skin

/// sRGB from 0-255 octets (annotation palette constants).
pub fn srgb_octets(red: u8, green: u8, blue: u8) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        red as f64 / 255.0,
        green as f64 / 255.0,
        blue as f64 / 255.0,
        1.0,
    )
}

/// Legacy accent still used by the annotation palette's first swatch. The
/// annotation palette is a tool palette, not the app's skin: it keeps its
/// own fixed colours so a drawn stroke looks the same on any screenshot.
pub fn purple() -> Retained<NSColor> {
    srgb_octets(0xB5, 0xA3, 0xF2)
}

/// Panel / window ground, behind everything the user sees.
pub fn ground() -> Retained<NSColor> {
    theme_skin::color(theme_skin::palette().bg, || NSColor::windowBackgroundColor())
}

/// A recessed ground: the sidebar and other inset bands. Translucent so it
/// reads as depth over the panel material rather than a painted rectangle.
pub fn ground_deep() -> Retained<NSColor> {
    theme_skin::color_alpha(theme_skin::palette().bg_deep, 0.65, || {
        NSColor::underPageBackgroundColor()
    })
}

/// Card / sheet face.
pub fn surface() -> Retained<NSColor> {
    theme_skin::color(theme_skin::palette().surface, || NSColor::controlBackgroundColor())
}

/// One step off the card face: toggle tracks, unfilled chips, wells.
pub fn surface_alt() -> Retained<NSColor> {
    theme_skin::color(theme_skin::palette().surface_alt, || NSColor::underPageBackgroundColor())
}

/// Primary text and glyphs.
pub fn label() -> Retained<NSColor> {
    theme_skin::color(theme_skin::palette().label, || NSColor::labelColor())
}

/// Secondary copy, captions, inactive glyphs.
pub fn label_secondary() -> Retained<NSColor> {
    theme_skin::color(theme_skin::palette().label_secondary, || NSColor::secondaryLabelColor())
}

/// Faintest tier: hints, disabled chrome, empty-state copy.
pub fn label_tertiary() -> Retained<NSColor> {
    theme_skin::color(theme_skin::palette().label_tertiary, || NSColor::tertiaryLabelColor())
}

/// Hairlines, card edges, dividers.
pub fn separator() -> Retained<NSColor> {
    let p = theme_skin::palette();
    theme_skin::color_alpha(p.separator, p.separator_alpha, || NSColor::separatorColor())
}

/// The selection accent: fills, borders, badges, progress.
pub fn accent() -> Retained<NSColor> {
    theme_skin::color(theme_skin::palette().accent, || NSColor::controlAccentColor())
}

/// A soft accent wash for fills behind text — the accent at low alpha so the
/// same value works as a pale tint in light mode and a deep tint in dark.
pub fn accent_tint() -> Retained<NSColor> {
    accent().colorWithAlphaComponent(0.18)
}

/// Ink for text drawn on an [`accent`] fill (selected chips, filled
/// buttons): white with the system accent, or the skin's own dark ink where
/// the accent is pale enough that white would disappear into it.
pub fn text_on_accent() -> Retained<NSColor> {
    theme_skin::text_on_accent()
}

/// Card corner radius, on the panel's nested scale (scaled by the skin).
pub fn radius_card() -> f64 {
    theme_skin::radius(10.0)
}
/// Panel corner radius (the shelf's top corners).
pub fn radius_panel() -> f64 {
    theme_skin::radius(12.0)
}
/// Chips, wells and other small controls.
pub fn radius_control() -> f64 {
    theme_skin::radius(6.0)
}

// MARK: Fonts

/// One process-wide cache for every Theme font (Swift `fontCache`).
fn cached_font(key: &str, make: impl FnOnce() -> Retained<NSFont>) -> Retained<NSFont> {
    static CACHE: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("font cache poisoned");
    if let Some(&ptr) = guard.get(key) {
        // SAFETY: inserted below and never removed; adds a retain for the caller.
        return unsafe { Retained::retain(ptr as *mut NSFont) }.expect("cached font");
    }
    let f = make();
    guard.insert(key.to_string(), Retained::into_raw(f.clone()) as usize);
    f
}

/// Swift string interpolation of a CGFloat cache key (`"serif\(size)"` →
/// `serif14.0`). `{:?}` on f64 prints the shortest round-trip form with the
/// decimal point, which matches Swift's Double interpolation for these sizes.
fn size_key(size: f64) -> String {
    format!("{:?}", size)
}

/// Body face. Kept under its old name because a hundred call sites say
/// `serif`; there is no serif left in this design. The face comes from the
/// skin: the system UI font for the default look — which is what makes the
/// UI honour the user's text-size setting — or the face the skin names.
/// The cache key carries the skin, so a switch cannot hand back the last
/// skin's face.
pub fn serif(size: f64, bold: bool) -> Retained<NSFont> {
    let key = format!("body{}:{}:{}", if bold { "b" } else { "" }, theme_skin::current_id(), size_key(size));
    cached_font(&key, || {
        theme_skin::ui_font(size, if bold { Weight::Bold } else { Weight::Regular })
    })
}

/// Semibold body face — section headings and emphasised labels.
pub fn semibold(size: f64) -> Retained<NSFont> {
    let key = format!("semibold{}:{}", theme_skin::current_id(), size_key(size));
    cached_font(&key, || theme_skin::ui_font(size, Weight::Semibold))
}

/// `NSFontDescriptor(fontAttributes:)` — the attributes dictionary types as
/// `NSDictionary<NSFontDescriptorAttributeName, AnyObject>`; the keys are
/// NSString constants, so build over NSString keys and reinterpret.
fn descriptor_with_attributes(
    pairs: &[(&'static NSString, Retained<AnyObject>)],
) -> Retained<NSFontDescriptor> {
    let keys: Vec<&NSString> = pairs.iter().map(|(k, _)| *k).collect();
    let vals: Vec<&AnyObject> = pairs.iter().map(|(_, v)| &**v).collect();
    let dict: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&keys, &vals);
    // SAFETY: NSFontDescriptorAttributeName is an NSString subclass; the
    // generic parameters are compile-time markers only.
    let dict = unsafe {
        Retained::cast_unchecked::<NSDictionary<objc2_app_kit::NSFontDescriptorAttributeName, AnyObject>>(dict)
    };
    // SAFETY: attributes dictionary of the documented shape.
    unsafe { NSFontDescriptor::fontDescriptorWithFontAttributes(Some(&dict)) }
}

/// Brand wordmark face. A skin that names one gets it; otherwise Ysabeau
/// Office when it registered, else the system medium weight. The bundled
/// file is a variable font, so it resolves through its FAMILY name — the
/// PostScript name "YsabeauOffice-Regular" matches no face and would
/// silently fall back to the system font. CJK glyphs land on the system face
/// through the normal cascade.
pub fn script(size: f64) -> Retained<NSFont> {
    let skin_face = theme_skin::brand_face();
    let key = format!("brand:{}:{}", theme_skin::current_id(), size_key(size));
    if !skin_face.is_empty() {
        return cached_font(&key, || theme_skin::ui_font(size, Weight::Bold));
    }
    register_brand_fonts();
    cached_font(&key, || {
        let d = descriptor_with_attributes(&[(
            ns_string!("NSFontNameAttribute"),
            any_from_nsstring("Ysabeau Office"),
        )]);
        let face = NSFont::fontWithDescriptor_size(&d, size).filter(|f| {
            f.familyName().map(|n| n.to_string()).as_deref() == Some("Ysabeau Office")
        });
        face.unwrap_or_else(|| theme_skin::ui_font(size, Weight::Medium))
    })
}

/// `NSString` value for an attributes dictionary, erased to `id`.
fn any_from_nsstring(s: &str) -> Retained<AnyObject> {
    let s = NSString::from_str(s);
    // SAFETY: upcast to the root object type for the value slot.
    unsafe { Retained::cast_unchecked::<AnyObject>(s) }
}

/// Brand wordmark (`Theme.brandFont`).
pub fn brand_font(size: f64) -> Retained<NSFont> {
    script(size)
}

// MARK: Surfaces

/// Draw into a fresh offscreen bitmap (app-icon greying, snapshot fixtures).
/// The Swift `NSImage(size:flipped:drawingHandler:)` idiom, baked to 1x.
fn offscreen_cg(size: CGSize, draw: impl FnOnce(CGRect)) -> Option<objc2_core_foundation::CFRetained<CGImage>> {
    // Device RGB matches the Swift contexts (CGColorSpaceCreateDeviceRGB).
    let space = CGColorSpace::new_device_rgb();
    // SAFETY: `data` is null (the context allocates its own buffer);
    // bitmapInfo 1 is premultipliedLast, as in the Swift contexts.
    let ctx = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            size.width as usize,
            size.height as usize,
            8,
            0,
            space.as_deref(),
            1,
        )?
    };
    let gc = NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, false);
    let prev = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&gc));
    draw(CGRect::new(CGPoint::ZERO, size));
    NSGraphicsContext::setCurrentContext(prev.as_deref());
    CGBitmapContextCreateImage(Some(&ctx))
}

/// A sheet / card / panel face: solid fill plus a hairline edge.
pub fn draw_surface(path: &NSBezierPath, fill: &NSColor) {
    fill.setFill();
    path.fill();
    separator().setStroke();
    path.setLineWidth(1.0);
    path.stroke();
}

/// Capture chrome and other floating bars: ground fill, hairline edge.
pub fn draw_desk(path: &NSBezierPath) {
    NSGraphicsContext::saveGraphicsState_class();
    path.addClip();
    ground().setFill();
    NSRectFill(path.bounds());
    NSGraphicsContext::restoreGraphicsState_class();
    separator().setStroke();
    path.setLineWidth(1.0);
    path.stroke();
}

/// The panel ground (window grounds); the live shelf adds a material behind
/// this in `shelf::panel`.
pub fn draw_ground(r: CGRect) {
    draw_panel_ground(r);
}

/// The panel's ground, in order of who owns it: the user's own background
/// image blended over the skin's colour scheme (only in the content column —
/// `photo_rect`, right of the sidebar — the sidebar keeps the skin's own
/// recessed band), else the skin's gradient wash, else the flat colour the
/// skin names.
///
/// The photo's opacity (`background::dim`) is how much of it covers the
/// skin's ground; 0.0 paints no photo at all. A photo and a gradient are
/// painted opaque on purpose: both are deliberate choices about the panel's
/// surface, and neither is offered by a skin that wants its material to
/// show through (原生, the only one with glass).
pub fn draw_panel_ground(r: CGRect) {
    draw_panel_ground_in(r, r);
}

/// The same ground with the photo limited to `photo_rect` (a sub-rect of `r`);
/// the sidebar passes the full bounds for the skin, and the content column
/// for the photo.
pub fn draw_panel_ground_in(r: CGRect, photo_rect: CGRect) {
    // The skin always owns the whole surface first: the photo blends onto it.
    if let Some(g) = gradient() {
        g.drawInRect_angle(r, 90.0);
    } else {
        ground().setFill();
        NSRectFill(r);
    }
    let Some(img) = crate::app::background::active_image() else {
        return;
    };
    // The photo at the user's opacity, over the skin's ground — nothing
    // veils it afterwards (a full-alpha veil would erase the photo, not
    // soften it). A 0.0 step paints nothing; the strip never offers it.
    let a = crate::app::background::dim().clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    crate::app::background::draw_cover_alpha(&img, photo_rect, a);
}

/// The skin's ground wash, or None on a skin that paints one flat colour.
fn gradient() -> Option<Retained<NSGradient>> {
    let g = theme_skin::current().gradient.as_ref()?;
    let from = theme_skin::color(g.from, || NSColor::windowBackgroundColor());
    let to = theme_skin::color(g.to, || NSColor::windowBackgroundColor());
    NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &from, &to)
}

/// How much of the panel's material shows through the ground: 0.0 paints the
/// ground opaque (a snapshot), 1.0 leaves it almost entirely to the material
/// (the live shelf). See the module note on why this exists.
static PANEL_MATERIAL_ALPHA: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

pub fn panel_material_alpha() -> f64 {
    f32::from_bits(PANEL_MATERIAL_ALPHA.load(std::sync::atomic::Ordering::Relaxed)) as f64
}

/// Turn the panel's glass on (a window) or off (a snapshot).
pub fn set_panel_material_alpha(a: f64) {
    PANEL_MATERIAL_ALPHA.store((a as f32).to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The shelf's ground where a material sits behind it. Opaque while the glass
/// is off, so nothing changes for the render self-tests and the capture
/// chrome, which all call `draw_ground` instead. A skin with no material (or
/// one that wants a solid ground) paints itself opaque regardless.
pub fn ground_over_material() -> Retained<NSColor> {
    ground().colorWithAlphaComponent(1.0 - 0.85 * theme_skin::glass() * panel_material_alpha())
}

/// Card / sheet layer: one radius, one soft shadow, weighted by the skin.
pub fn card_layer(v: &NSView, radius: f64) {
    v.setWantsLayer(true);
    if let Some(layer) = v.layer() {
        // SAFETY: ordinary property setter on the view's layer.
        let () = unsafe { objc2::msg_send![&*layer, setCornerRadius: radius] };
    }
    let s = NSShadow::new();
    s.setShadowColor(Some(&NSColor::colorWithCalibratedWhite_alpha(
        0.0,
        theme_skin::shadow_alpha(0.22),
    )));
    s.setShadowBlurRadius(10.0);
    s.setShadowOffset(CGSize::new(0.0, -2.0));
    v.setShadow(Some(&s));
}

/// Round a view's own layer and clip its subviews to it. The panel needs
/// this: a full-bleed backing (the material) would otherwise square off the
/// window's rounded silhouette.
pub fn clip_to_radius(v: &NSView, radius: f64) {
    v.setWantsLayer(true);
    if let Some(layer) = v.layer() {
        // SAFETY: ordinary property setters on the view's layer.
        unsafe {
            let () = objc2::msg_send![&*layer, setCornerRadius: radius];
            let () = objc2::msg_send![&*layer, setMasksToBounds: true];
        }
    }
}

// MARK: Shapes

/// Which corners a rounded path rounds.
#[derive(Clone, Copy, Debug)]
pub struct Corners {
    pub top_left: bool,
    pub top_right: bool,
    pub bottom_left: bool,
    pub bottom_right: bool,
}

impl Corners {
    pub const ALL: Corners = Corners {
        top_left: true,
        top_right: true,
        bottom_left: true,
        bottom_right: true,
    };
    pub const TOP: Corners = Corners {
        top_left: true,
        top_right: true,
        bottom_left: false,
        bottom_right: false,
    };
}

/// A rounded rectangle with per-corner control, in flipped (y-down)
/// coordinates. Continuous corners are the base shape of this skin; the
/// quarter-arcs below are the same proven calls the old sheet used, so the
/// geometry is byte-verifiable (`rounded_rect_corners_geometry`).
pub fn rounded_rect_corners(r: CGRect, radius: f64, c: Corners) -> Retained<NSBezierPath> {
    let (x0, y0) = (r.min().x, r.min().y);
    let (x1, y1) = (r.max().x, r.max().y);
    let p = NSBezierPath::new();
    // Left edge, starting just below the top-left corner.
    p.moveToPoint(CGPoint::new(x0, if c.top_left { y0 + radius } else { y0 }));
    if c.top_left {
        // left edge → top edge
        p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            CGPoint::new(x0 + radius, y0 + radius),
            radius,
            180.0,
            270.0,
            false,
        );
    }
    p.lineToPoint(CGPoint::new(if c.top_right { x1 - radius } else { x1 }, y0));
    if c.top_right {
        // top edge → right edge
        p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            CGPoint::new(x1 - radius, y0 + radius),
            radius,
            -90.0,
            0.0,
            false,
        );
    }
    p.lineToPoint(CGPoint::new(x1, if c.bottom_right { y1 - radius } else { y1 }));
    if c.bottom_right {
        // right edge → bottom edge
        p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            CGPoint::new(x1 - radius, y1 - radius),
            radius,
            0.0,
            90.0,
            false,
        );
    }
    p.lineToPoint(CGPoint::new(if c.bottom_left { x0 + radius } else { x0 }, y1));
    if c.bottom_left {
        // bottom edge → left edge
        p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            CGPoint::new(x0 + radius, y1 - radius),
            radius,
            90.0,
            180.0,
            false,
        );
    }
    p.closePath();
    p
}

/// A rounded rectangle with all four corners rounded.
pub fn rounded_rect(r: CGRect, radius: f64) -> Retained<NSBezierPath> {
    rounded_rect_corners(r, radius, Corners::ALL)
}

// MARK: Card icons (Theme.swift:74-94)

/// App icons for the cards, desaturated once and cached per bundle id; a
/// Launch Services lookup per body was the lag. Nil when the bundle id is nil
/// or Launch Services can't resolve it (cached either way). Our own app gets
/// the product logo instead.
pub fn card_icon(bundle_id: Option<&str>) -> Option<Retained<NSImage>> {
    let bundle_id = bundle_id?;
    if bundle_id == "com.cici.snipclip" {
        return logo();
    }
    static CACHE: OnceLock<Mutex<HashMap<String, Option<usize>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("icon cache poisoned");
    if let Some(hit) = guard.get(bundle_id) {
        // SAFETY: pointers were into_raw'd below and never leave the map;
        // adds a retain for the caller.
        return hit.and_then(|p| unsafe { Retained::retain(p as *mut NSImage) });
    }
    let made = make_card_icon(bundle_id);
    let raw = made.as_ref().map(|i| Retained::into_raw(i.clone()) as usize);
    guard.insert(bundle_id.to_string(), raw);
    made
}

/// The Launch Services lookup + grey-down, run once per bundle id.
fn make_card_icon(bundle_id: &str) -> Option<Retained<NSImage>> {
    let ws = NSWorkspace::sharedWorkspace();
    let url = ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
    let path = url.path()?;
    let src = ws.iconForFile(&path);
    src.setSize(CGSize::new(44.0, 44.0));
    let size = src.size();
    let cg = offscreen_cg(size, |r| {
        src.drawInRect(r);
        // Grey it down so the card stays quiet, keep the alpha.
        NSColor::colorWithCalibratedWhite_alpha(0.45, 1.0).set();
        NSRectFillUsingOperation(r, NSCompositingOperation::Color);
        src.drawInRect_fromRect_operation_fraction(
            r,
            CGRect::ZERO,
            NSCompositingOperation::DestinationIn,
            1.0,
        );
    })?;
    Some(NSImage::initWithCGImage_size(NSImage::alloc(), &cg, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_app_kit::NSBezierPathElement;

    // ── SplitMix64 reference values generated from the Swift runtime ────────
    //     struct Seeded … (AnnotationRenderer.swift verbatim); swift /tmp/t.swift
    //     Seeded(20260912).next() ×3.

    #[test]
    fn seeded_matches_swift_vectors() {
        let mut g = Seeded(20260912);
        assert_eq!(g.next(), 3821998546305730818);
        assert_eq!(g.next(), 1209654006528025633);
        assert_eq!(g.next(), 1409108709278482661);
    }

    /// Element stream of an NSBezierPath as (kind, points) pairs.
    fn elements(p: &NSBezierPath) -> Vec<(NSBezierPathElement, Vec<CGPoint>)> {
        let mut out = Vec::new();
        for i in 0..p.elementCount() {
            let kind = p.elementAtIndex(i);
            let mut pts = [CGPoint::ZERO; 3];
            // SAFETY: 3 NSPoints is the documented buffer size for
            // associated points (curveTo needs all three).
            unsafe {
                p.elementAtIndex_associatedPoints(i, pts.as_mut_ptr());
            }
            let n = if kind == NSBezierPathElement::ClosePath {
                0
            } else if kind == NSBezierPathElement::CubicCurveTo {
                3
            } else {
                1
            };
            out.push((kind, pts[..n].to_vec()));
        }
        out
    }

    fn pt(x: f64, y: f64) -> CGPoint {
        CGPoint::new(x, y)
    }

    /// Arc endpoints are computed through trigonometry, so a point lands
    /// within a hair of the exact value rather than on it.
    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// The card and panel shapes are what replaced the ticket / torn-paper
    /// paths: assert the four corner arcs land on the geometry the drawing
    /// code depends on (bounds exact, corner cuts at the radius, sharp
    /// corners left square).
    #[test]
    fn rounded_rect_corners_geometry() {
        let r = CGRect::new(pt(0.0, 0.0), CGSize::new(100.0, 80.0));
        let p = rounded_rect_corners(r, 10.0, Corners::ALL);
        let els = elements(&p);
        let b = el_bounds(&els);
        assert!(
            near(b.min().x, 0.0) && near(b.min().y, 0.0) && near(b.max().x, 100.0) && near(b.max().y, 80.0),
            "bounds {:?} escaped the rect",
            b
        );
        // One quarter arc per corner, and AppKit approximates an arc of at
        // most 90° with a single cubic segment — so four corners is four
        // curves, plus the closing edge.
        let curves = els
            .iter()
            .filter(|(k, _)| *k == NSBezierPathElement::CubicCurveTo)
            .count();
        assert_eq!(curves, 4, "one cubic per rounded corner");
        // Start point sits on the left edge, below the top-left corner (the
        // y coordinate is a sine away from 10.0, hence the tolerance).
        assert_eq!(els[0], (NSBezierPathElement::MoveTo, vec![pt(0.0, 10.0)]));
        // Every drawn point stays inside the rect (the arcs bow inwards).
        for (_, pts) in &els {
            for p in pts {
                assert!(p.x >= -0.001 && p.x <= 100.001, "x {} escaped the rect", p.x);
                assert!(p.y >= -0.001 && p.y <= 80.001, "y {} escaped the rect", p.y);
            }
        }
    }

    /// A square-cornered rect draws no arcs at all and still closes.
    #[test]
    fn rounded_rect_corners_none_is_square() {
        let r = CGRect::new(pt(4.0, 6.0), CGSize::new(20.0, 20.0));
        let p = rounded_rect_corners(
            r,
            8.0,
            Corners {
                top_left: false,
                top_right: false,
                bottom_left: false,
                bottom_right: false,
            },
        );
        let els = elements(&p);
        let curves = els
            .iter()
            .filter(|(k, _)| *k == NSBezierPathElement::CubicCurveTo)
            .count();
        assert_eq!(curves, 0);
        let b = el_bounds(&els);
        assert!(
            near(b.min().x, 4.0) && near(b.min().y, 6.0) && near(b.max().x, 24.0) && near(b.max().y, 26.0),
            "bounds {:?}",
            b
        );
    }

    /// Top-only rounding (the shelf panel) keeps the bottom corners square.
    #[test]
    fn rounded_rect_corners_top_only() {
        let r = CGRect::new(pt(0.0, 0.0), CGSize::new(200.0, 100.0));
        let p = rounded_rect_corners(r, 12.0, Corners::TOP);
        let els = elements(&p);
        let b = el_bounds(&els);
        assert!(
            near(b.min().x, 0.0) && near(b.min().y, 0.0) && near(b.max().x, 200.0) && near(b.max().y, 100.0),
            "bounds {:?} escaped the rect",
            b
        );
        let curves = els
            .iter()
            .filter(|(k, _)| *k == NSBezierPathElement::CubicCurveTo)
            .count();
        assert_eq!(curves, 2, "one cubic per rounded corner");
    }

    fn el_bounds(els: &[(NSBezierPathElement, Vec<CGPoint>)]) -> CGRect {
        let (mut x0, mut y0) = (f64::MAX, f64::MAX);
        let (mut x1, mut y1) = (f64::MIN, f64::MIN);
        for (_, pts) in els {
            for p in pts {
                x0 = x0.min(p.x);
                y0 = y0.min(p.y);
                x1 = x1.max(p.x);
                y1 = y1.max(p.y);
            }
        }
        CGRect::new(pt(x0, y0), CGSize::new(x1 - x0, y1 - y0))
    }

    /// Resolve a colour to sRGB bytes by actually painting it under a given
    /// appearance. A semantic colour is an `NSDynamicSystemColor`: it has no
    /// components of its own and refuses every conversion selector, because it
    /// only resolves while a drawing appearance is current. Painting into a
    /// 1x1 bitmap is therefore both the only route and the honest one — it
    /// tests the mechanism the UI depends on, not a colour object's internals.
    fn srgb_under(appearance: &objc2_app_kit::NSAppearance, c: &NSColor) -> (u8, u8, u8) {
        use objc2_app_kit::{
            NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext,
        };
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                1,
                1,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                NSBitmapFormat(0),
                4,
                32,
            )
        }
        .expect("1x1 RGBA bitmap");

        let prev = NSGraphicsContext::currentContext();
        let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)
            .expect("bitmap graphics context");
        let out = std::cell::Cell::new((0u8, 0u8, 0u8));
        let block = block2::RcBlock::new(|| {
            NSGraphicsContext::setCurrentContext(Some(&ctx));
            NSGraphicsContext::saveGraphicsState_class();
            c.setFill();
            objc2_app_kit::NSRectFill(CGRect::new(CGPoint::ZERO, CGSize::new(1.0, 1.0)));
            NSGraphicsContext::restoreGraphicsState_class();
            NSGraphicsContext::setCurrentContext(prev.as_deref());
            // Non-planar 8-bit RGBA is laid out R, G, B, A.
            let px = rep.bitmapData();
            out.set(unsafe { (*px, *px.add(1), *px.add(2)) });
        });
        let dyn_block: &block2::DynBlock<dyn Fn()> = &block;
        appearance.performAsCurrentDrawingAppearance(dyn_block);
        out.get()
    }

    /// The skin's whole point is that light and dark both work from one code
    /// path: every semantic colour must actually resolve differently under
    /// darkAqua, and the ground must go dark there (the old stationery skin
    /// had no dark mode at all).
    #[test]
    fn semantic_colours_follow_the_appearance() {
        use objc2_app_kit::NSAppearanceNameDarkAqua;
        let Some(light) = objc2_app_kit::NSAppearance::appearanceNamed(unsafe {
            objc2_app_kit::NSAppearanceNameAqua
        }) else {
            return; // headless CI without an appearance server
        };
        let Some(dark) = objc2_app_kit::NSAppearance::appearanceNamed(unsafe {
            NSAppearanceNameDarkAqua
        }) else {
            return;
        };
        for (name, c) in [
            ("ground", ground()),
            ("surface", surface()),
            ("label", label()),
            ("separator", separator()),
        ] {
            let l = srgb_under(&light, &c);
            let d = srgb_under(&dark, &c);
            assert_ne!(l, d, "{} must resolve differently in dark mode (got {:?})", name, l);
        }
        // Ground is light in light mode and dark in dark mode — the check
        // that would have caught the old paper-on-brown skin.
        let (gr, gg, gb) = srgb_under(&light, &ground());
        let (dr, dg, db) = srgb_under(&dark, &ground());
        assert!(gr > 200 && gg > 200 && gb > 200, "light ground is light (got {:?})", (gr, gg, gb));
        assert!(dr < 60 && dg < 60 && db < 60, "dark ground is dark (got {:?})", (dr, dg, db));
        // Text stays legible against the ground in both.
        let (lr, lg, lb) = srgb_under(&light, &label());
        assert!(lr < 90 && lg < 90 && lb < 90, "light-mode label is dark ink");
        let (dr2, dg2, db2) = srgb_under(&dark, &label());
        assert!(dr2 > 190 && dg2 > 190 && db2 > 190, "dark-mode label is light ink");
    }

    #[test]
    fn body_face_is_the_system_font() {
        // The skin is system type: no bundled serif, no handwriting.
        let f = serif(14.0, false);
        assert_eq!(f.pointSize(), 14.0);
        let family = f.familyName().map(|n| n.to_string()).unwrap_or_default();
        let system = NSFont::systemFontOfSize_weight(14.0, unsafe { objc2_app_kit::NSFontWeightRegular });
        assert_eq!(family, system.familyName().map(|n| n.to_string()).unwrap_or_default());
        let b = serif(14.0, true);
        assert_ne!(
            b.fontDescriptor().symbolicTraits().0,
            0,
            "bold resolves to a real weight"
        );
        assert_eq!(semibold(13.0).pointSize(), 13.0);
    }

    #[test]
    fn brand_face_registers_or_falls_back() {
        // Brand fonts live under Resources/Fonts in the repo; registration is
        // what gates the Ysabeau descriptor. Either way a face must come back.
        let registered = register_brand_fonts();
        let s = script(16.0);
        assert_eq!(s.pointSize(), 16.0);
        if registered {
            assert_eq!(
                s.familyName().map(|n| n.to_string()).as_deref(),
                Some("Ysabeau Office")
            );
        }
    }

    #[test]
    fn card_icon_greys_non_pastory_apps() {
        // Any installed app resolves through Launch Services; nil ids and
        // misses come back None, our own id is the product logo.
        let Some(icon) = card_icon(Some("com.apple.finder")) else {
            return; // Launch Services unavailable (headless CI)
        };
        assert_eq!(icon.size(), CGSize::new(44.0, 44.0));
        assert!(card_icon(Some("com.apple.finder")).is_some()); // cache hit
        assert!(card_icon(Some("com.pastory.no-such-bundle")).is_none());
        assert!(card_icon(None).is_none());
        assert_eq!(
            card_icon(Some("com.cici.snipclip")).map(|i| Retained::as_ptr(&i) as usize),
            logo().map(|i| Retained::as_ptr(&i) as usize),
        );
    }

    /// Renders the new shapes to PNGs for eyeball comparison. Opt-in:
    /// `PASTORY_THEME_SNAPSHOT_DIR=/tmp xargs cargo test`.
    #[test]
    fn snapshot_fixtures() {
        let Some(dir) = std::env::var_os("PASTORY_THEME_SNAPSHOT_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        objc2::rc::autoreleasepool(|_| {
            let card = CGRect::new(pt(0.0, 0.0), CGSize::new(288.0, 420.0));
            if let Some(cg) = offscreen_cg(card.size, |_| {
                ground().setFill();
                NSRectFill(CGRect::new(pt(-40.0, -40.0), CGSize::new(card.size.width + 80.0, card.size.height + 80.0)));
                let t = rounded_rect(CGRect::new(pt(16.0, 16.0), CGSize::new(256.0, 388.0)), radius_card());
                draw_surface(&t, &surface());
                let chip = rounded_rect(CGRect::new(pt(32.0, 360.0), CGSize::new(120.0, 26.0)), radius_control());
                accent_tint().setFill();
                chip.fill();
            }) {
                let png = crate::capture::screenshotter::png_data(&cg).unwrap();
                std::fs::write(dir.join("card.png"), png).unwrap();
            }
            if let Some(cg) = offscreen_cg(CGSize::new(320.0, 120.0), |r| {
                draw_ground(r);
                let p = rounded_rect_corners(CGRect::new(pt(10.0, 15.0), CGSize::new(300.0, 90.0)), radius_panel(), Corners::TOP);
                draw_surface(&p, &surface());
            }) {
                let png = crate::capture::screenshotter::png_data(&cg).unwrap();
                std::fs::write(dir.join("panel.png"), png).unwrap();
            }
        });
    }
}

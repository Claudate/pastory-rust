//! Port of `App/Theme.swift` (M2 slice A: the full paper theme) plus the two
//! shape helpers from `Shelf/ShelfView.swift` (`TornPaper`, `RuledBox`) and
//! `Shelf/ClipCardView.swift` (`TicketShape`, `Line`).
//!
//! One place for the look: brown desk, cream paper, light-blue accent, ink
//! type. Values are copied one-for-one from Swift; do not eyeball-tune.
//!
//! Coordinate note: the Swift shelf draws these shapes in SwiftUI (y grows
//! downwards). NSBezierPath itself carries no flip state — the geometry here
//! matches the Swift path coordinates point for point, and the shelf slice
//! decides flip at draw time. Arc direction (`clockwise:`) needs no remap:
//! `Path.addArc` and `NSBezierPath.appendBezierPathWithArcWithCenter:` take the
//! same degrees, same origin at +x, same clockwise sense — verified by building
//! TicketShape both ways and diffing every CGPath element (curve control
//! points included): identical except NSBezierPath appends one degenerate
//! trailing moveTo after close. See `ticket_matches_swift_reference` below.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSCompositingOperation, NSFont, NSFontDescriptor, NSFontWeightBold,
    NSFontWeightMedium, NSFontWeightRegular, NSGraphicsContext, NSImage, NSImageRep, NSRectFill,
    NSRectFillUsingOperation, NSShadow, NSView, NSWorkspace,
};
use objc2_core_foundation::{CFData, CFRetained, CGRect, CGPoint, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGBitmapInfo, CGColorRenderingIntent,
    CGColorSpace, CGContext, CGDataProvider, CGImage, CGImageAlphaInfo,
};
use objc2_foundation::{ns_string, NSArray, NSDictionary, NSString};

/// Where resources live. A normal launch: the bundle's Resources. A dev run /
/// self-test (`cargo run`, the bare binary): `<repo>/rust/target/…/Pastory`
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
    let bundle = exe.ancestors().nth(3)?; // …/Pastory.app/Contents/MacOS/Pastory
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

/// The one piece of stationery: a pink pushpin on the card that is currently
/// on the clipboard.
pub fn pushpin() -> Option<Retained<NSImage>> {
    static CELL: OnceLock<Option<usize>> = OnceLock::new();
    cached_named_image(&CELL, "Pushpin.png")
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

/// Brand typeface (Ysabeau Office, OFL; Caveat for the script) bundled in
/// Resources/Fonts; registered for this process on first use. True when any
/// face registered — the M0 spike for CTFontManagerRegisterFontsForURL.
pub fn register_brand_fonts() -> bool {
    static DONE: OnceLock<bool> = OnceLock::new();
    *DONE.get_or_init(|| {
        use objc2_core_foundation::{CFString, CFURL, CFURLPathStyle};
        use objc2_core_text::{CTFontManagerRegisterFontsForURL, CTFontManagerScope};
        let mut any = false;
        for name in ["YsabeauOffice.ttf", "Caveat.ttf"] {
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

/// Tiny deterministic generator (SplitMix64) so a shape's wobble never changes
/// between frames. Bit-identical with the Swift `Seeded`.
pub struct Seeded(pub u64);

impl Seeded {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `TornPaper.jitter` / `RuledBox.j`: the modulo lands on the integer
    /// first, exactly like Swift's `Double(g.next() % 1000) / 1000.0 - 0.5`.
    fn jitter(&mut self, amplitude: f64) -> f64 {
        ((self.next() % 1000) as f64 / 1000.0 - 0.5) * 2.0 * amplitude
    }
}

// MARK: Palette (Theme.swift:7-18,140-141)

fn srgb(red: f64, green: f64, blue: f64, alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, alpha)
}

/// sRGB from 0-255 octets (annotation palette constants).
pub fn srgb_octets(red: u8, green: u8, blue: u8) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        red as f64 / 255.0,
        green as f64 / 255.0,
        blue as f64 / 255.0,
        1.0,
    )
}

/// Legacy accent still used by the annotation palette's first swatch.
pub fn purple() -> Retained<NSColor> {
    srgb(0.71, 0.64, 0.95, 1.0) // #B5A3F2
}

/// Paper theme ground: dark brown.
pub fn brown() -> Retained<NSColor> {
    srgb(0.17, 0.13, 0.12, 1.0) // #2B211E
}

pub fn brown_deep() -> Retained<NSColor> {
    srgb(0.14, 0.11, 0.10, 1.0) // #241C19
}

pub fn paper() -> Retained<NSColor> {
    srgb(0.95, 0.93, 0.89, 1.0) // #F2EDE3
}

pub fn paper_dim() -> Retained<NSColor> {
    srgb(0.90, 0.87, 0.82, 1.0) // #E6DFD1
}

pub fn paper_blue() -> Retained<NSColor> {
    srgb(0.74, 0.84, 0.90, 1.0) // #BDD6E5
}

pub fn paper_blue_deep() -> Retained<NSColor> {
    srgb(0.50, 0.65, 0.74, 1.0) // #7FA5BD
}

pub fn ink() -> Retained<NSColor> {
    srgb(0.16, 0.14, 0.13, 1.0) // #2A2521
}

pub fn ink_muted() -> Retained<NSColor> {
    srgb(0.43, 0.40, 0.37, 1.0) // #6E665F
}

/// Text on the brown ground.
pub fn on_brown() -> Retained<NSColor> {
    srgb(0.93, 0.90, 0.86, 1.0)
}

pub fn on_brown_muted() -> Retained<NSColor> {
    srgb(0.68, 0.63, 0.59, 1.0)
}

/// Faint ink outline on paper.
pub fn paper_line() -> Retained<NSColor> {
    srgb(0.16, 0.14, 0.13, 0.28)
}

/// Card corner radius (`Theme.paperRadius`).
pub const PAPER_RADIUS: f64 = 6.0;

// MARK: Fonts (Theme.swift:21-39, Annotate/Annotation.swift:44)

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

/// Serif for the paper theme (Songti SC covers CJK and Latin). Swift signature
/// is `serif(size:bold:)` with `bold = false` by default.
pub fn serif(size: f64, bold: bool) -> Retained<NSFont> {
    let key = format!("serif{}{}", if bold { "b" } else { "" }, size_key(size));
    cached_font(&key, || {
        let name = if bold { "STSongti-SC-Bold" } else { "STSongti-SC-Regular" };
        match NSFont::fontWithName_size(&NSString::from_str(name), size) {
            Some(f) => f,
            None => NSFont::systemFontOfSize_weight(
                size,
                // SAFETY: stable extern constants.
                unsafe {
                    if bold { NSFontWeightBold } else { NSFontWeightRegular }
                },
            ),
        }
    })
}

/// 翩翩体 covers Latin too (Annotate HandFont); Theme's fallback when the
/// Caveat descriptor resolves to nothing.
fn hand_font(size: f64) -> Retained<NSFont> {
    for name in ["HanziPenSC-W5", "HannotateSC-W5"] {
        if let Some(f) = NSFont::fontWithName_size(&NSString::from_str(name), size) {
            return f;
        }
    }
    // SAFETY: stable extern constant.
    NSFont::systemFontOfSize_weight(size, unsafe { NSFontWeightMedium })
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

/// Handwritten script: Caveat for Latin, falling back to 翩翩体 for CJK.
pub fn script(size: f64) -> Retained<NSFont> {
    register_brand_fonts();
    let key = format!("script{}", size_key(size));
    cached_font(&key, || {
        let cjk = descriptor_with_attributes(&[(ns_string!("NSFontNameAttribute"), any_from_nsstring("HanziPenSC-W5"))]);
        let cascade: Retained<NSArray<NSFontDescriptor>> = NSArray::from_slice(&[&*cjk]);
        let d = descriptor_with_attributes(&[
            (ns_string!("NSFontNameAttribute"), any_from_nsstring("Caveat-Regular")),
            // SAFETY: upcasting an NSArray object to AnyObject for the erased
            // value slot — ObjC objects share layout; nothing is converted.
            (
                ns_string!("NSFontCascadeListAttribute"),
                unsafe { Retained::cast_unchecked::<AnyObject>(cascade) },
            ),
        ]);
        match NSFont::fontWithDescriptor_size(&d, size) {
            Some(f) => f,
            None => hand_font(size),
        }
    })
}

/// `NSString` value for an attributes dictionary, erased to `id`.
fn any_from_nsstring(s: &str) -> Retained<AnyObject> {
    let s = NSString::from_str(s);
    // SAFETY: upcast to the root object type for the value slot.
    unsafe { Retained::cast_unchecked::<AnyObject>(s) }
}

/// Brand wordmark: the handwritten script (`Theme.brandFont`).
pub fn brand_font(size: f64) -> Retained<NSFont> {
    script(size)
}

// MARK: Grain (Theme.swift:42-71,143-194)

/// Tile edge in pixels — `noiseTile` is 96×96.
const NOISE_N: usize = 96;

/// `CGImageAlphaInfo.premultipliedLast` for the offscreen bitmaps.
const PREMULTIPLIED_LAST: u32 = 1;

/// Faint grain, tiled over paper and ground so nothing looks flat.
pub fn noise_tile() -> Retained<NSImage> {
    static CELL: OnceLock<usize> = OnceLock::new();
    let ptr = *CELL.get_or_init(|| {
        let img = make_noise_tile().expect("noise tile CGImageCreate of static bytes");
        Retained::into_raw(img) as usize
    });
    // SAFETY: the reference was into_raw'd once above; adds the caller's retain.
    unsafe { Retained::retain(ptr as *mut NSImage) }.expect("noise tile cached")
}

/// `Theme.noiseTile`: 96×96 grey noise from `Seeded(20260912)`, one channel
/// replicated, alpha 255.
fn make_noise_tile() -> Option<Retained<NSImage>> {
    let mut bytes = vec![0u8; NOISE_N * NOISE_N * 4];
    let mut g = Seeded(20260912);
    let mut i = 0;
    while i < bytes.len() {
        let v = (g.next() % 256) as u8;
        bytes[i] = v;
        bytes[i + 1] = v;
        bytes[i + 2] = v;
        bytes[i + 3] = 255;
        i += 4;
    }
    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let space = CGColorSpace::new_device_rgb();
    // SAFETY: plain CG constructor over the byte buffer; parameters mirror
    // the Swift CGImage(width:…) call exactly.
    let cg = unsafe {
        CGImage::new(
            NOISE_N,
            NOISE_N,
            8,
            32,
            NOISE_N * 4,
            space.as_deref(),
            CGBitmapInfo(CGImageAlphaInfo::NoneSkipLast.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )?
    };
    Some(NSImage::initWithCGImage_size(
        NSImage::alloc(),
        &cg,
        CGSize::new(NOISE_N as f64, NOISE_N as f64),
    ))
}

/// Draw into a fresh offscreen bitmap and hand back what the Swift
/// `NSImage(size:flipped:drawingHandler:)` idiom produced: a 1x bitmap-backed
/// image. The Swift handler versions are NSCustomImageReps that re-render into
/// the destination's coordinate space on every use (verified:
/// `pixelsWide == 0`). A frozen bitmap cannot reproduce one thing: AppKit
/// renders the custom rep y-down, so the handler's CG pattern grain anchors
/// at the visual top, while a direct y-up CGBitmapContext render anchors the
/// same phase at the buffer bottom. The pipe's pattern phase lives in CG base
/// space (the NSGraphicsContext flipped flag does not move it), so the baked
/// tiles correct this with one explicit flip pass — see `baked_tile`. Only
/// the tiles bother: a fill is uniform and the grain is directionless grey,
/// but byte-identical costs one redraw.
fn offscreen_image(size: CGSize, draw: impl FnOnce(CGRect)) -> Option<Retained<NSImage>> {
    let cg = offscreen_cg(size, draw)?;
    Some(NSImage::initWithCGImage_size(NSImage::alloc(), &cg, size))
}

fn offscreen_cg(size: CGSize, draw: impl FnOnce(CGRect)) -> Option<CFRetained<CGImage>> {
    // Device RGB matches the Swift contexts (CGColorSpaceCreateDeviceRGB).
    let space = CGColorSpace::new_device_rgb();
    // SAFETY: `data` is null (the context allocates its own buffer).
    let ctx = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            size.width as usize,
            size.height as usize,
            8,
            0,
            space.as_deref(),
            PREMULTIPLIED_LAST,
        )?
    };
    let gc = NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, false);
    let prev = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&gc));
    draw(CGRect::new(CGPoint::ZERO, size));
    NSGraphicsContext::setCurrentContext(prev.as_deref());
    CGBitmapContextCreateImage(Some(&ctx))
}

/// `Theme.bakedTile` — paper / desk with the grain already multiplied in.
/// SwiftUI tiles these as `ImagePaint`; a live `blendMode(.multiply)` per card
/// forced an offscreen pass for every card on every frame.
fn baked_tile(color: &NSColor, grain: f64) -> Retained<NSImage> {
    let size = CGSize::new(NOISE_N as f64, NOISE_N as f64);
    let cg = offscreen_cg(size, |r| {
        color.setFill();
        NSRectFill(r);
        draw_grain(r, grain);
    })
    .expect("offscreen bake of a static 96×96 tile");
    let cg = vflip(size, &cg).expect("same-size redraw of a CGImage");
    NSImage::initWithCGImage_size(NSImage::alloc(), &cg, size)
}

/// Vertical mirror via a CTM flip (CTM moves images, unlike pattern phase):
/// makes the frozen bake byte-identical to Swift's y-down custom-rep render.
fn vflip(size: CGSize, image: &CGImage) -> Option<CFRetained<CGImage>> {
    let space = CGColorSpace::new_device_rgb();
    // SAFETY: `data` is null (the context allocates its own buffer).
    let ctx = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            size.width as usize,
            size.height as usize,
            8,
            0,
            space.as_deref(),
            PREMULTIPLIED_LAST,
        )?
    };
    CGContext::translate_ctm(Some(&ctx), 0.0, size.height);
    CGContext::scale_ctm(Some(&ctx), 1.0, -1.0);
    CGContext::draw_image(Some(&ctx), CGRect::new(CGPoint::ZERO, size), Some(image));
    CGBitmapContextCreateImage(Some(&ctx))
}

fn cached_tile(cell: &OnceLock<usize>, color: &NSColor, grain: f64) -> Retained<NSImage> {
    let ptr = *cell.get_or_init(|| Retained::into_raw(baked_tile(color, grain)) as usize);
    // SAFETY: the reference was into_raw'd once above; adds the caller's retain.
    unsafe { Retained::retain(ptr as *mut NSImage) }.expect("tile cached")
}

/// `Theme.paperTile` (grain 0.11).
pub fn paper_tile() -> Retained<NSImage> {
    static CELL: OnceLock<usize> = OnceLock::new();
    cached_tile(&CELL, &paper(), 0.11)
}

/// `Theme.paperBlueTile` (grain 0.11).
pub fn paper_blue_tile() -> Retained<NSImage> {
    static CELL: OnceLock<usize> = OnceLock::new();
    cached_tile(&CELL, &paper_blue(), 0.11)
}

/// `Theme.deskTile` (grain 0.22).
pub fn desk_tile() -> Retained<NSImage> {
    static CELL: OnceLock<usize> = OnceLock::new();
    cached_tile(&CELL, &brown(), 0.22)
}

/// Grain: multiply the noise tile over whatever was just painted. Draws into
/// the current NSGraphicsContext (shelf cards and window grounds call this
/// inside their drawRect).
pub fn draw_grain(r: CGRect, opacity: f64) {
    NSGraphicsContext::saveGraphicsState_class();
    if let Some(gc) = NSGraphicsContext::currentContext() {
        gc.setCompositingOperation(NSCompositingOperation::Multiply);
        let cg = gc.CGContext();
        CGContext::set_alpha(Some(&cg), opacity);
    }
    NSColor::colorWithPatternImage(&noise_tile()).setFill();
    NSBezierPath::bezierPathWithRect(r).fill();
    NSGraphicsContext::restoreGraphicsState_class();
}

/// A sheet of paper: fill, grain, hairline ink edge — clipped to `path`.
/// Swift's default fill is `paper`.
pub fn draw_paper(path: &NSBezierPath, fill: &NSColor) {
    NSGraphicsContext::saveGraphicsState_class();
    path.addClip();
    fill.setFill();
    NSRectFill(path.bounds());
    draw_grain(path.bounds(), 0.045);
    NSGraphicsContext::restoreGraphicsState_class();
    paper_line().setStroke();
    path.setLineWidth(1.0);
    path.stroke();
}

/// Capture chrome: the same brown frosted ground as the shelf, cut into a
/// strip, with a faint light edge.
pub fn draw_desk(path: &NSBezierPath) {
    NSGraphicsContext::saveGraphicsState_class();
    path.addClip();
    brown().setFill();
    NSRectFill(path.bounds());
    draw_grain(path.bounds(), 0.16);
    NSGraphicsContext::restoreGraphicsState_class();
    on_brown().colorWithAlphaComponent(0.22).setStroke();
    path.setLineWidth(1.0);
    path.stroke();
}

/// The brown desk the paper sits on (window grounds).
pub fn draw_ground(r: CGRect) {
    brown().setFill();
    NSRectFill(r);
    draw_grain(r, 0.16);
}

/// Paper card with a soft shadow on its layer; `draw_paper` paints the face.
/// Swift default radius is `paperRadius` (6).
pub fn paper_sheet(v: &NSView, radius: f64) {
    v.setWantsLayer(true);
    if let Some(layer) = v.layer() {
        // SAFETY: ordinary property setter on the view's layer.
        let () = unsafe { objc2::msg_send![&*layer, setCornerRadius: radius] };
    }
    let s = NSShadow::new();
    s.setShadowColor(Some(&NSColor::colorWithCalibratedWhite_alpha(0.0, 0.4)));
    s.setShadowBlurRadius(8.0);
    s.setShadowOffset(CGSize::new(1.0, -4.0));
    v.setShadow(Some(&s));
}

// MARK: Shapes (ShelfView.swift:290-359, ClipCardView.swift:368-401)

/// A rectangle whose chosen edges are torn: small irregular teeth, fixed per
/// `seed` so it never shimmers. Swift `TornPaper` defaults: seed 7,
/// amplitude 3, step 7. The RNG draws are in the Swift order — do not re-sort.
#[allow(clippy::too_many_arguments)]
pub fn torn_paper_path(
    r: CGRect,
    top: bool,
    right: bool,
    bottom: bool,
    left: bool,
    seed: u64,
    amplitude: f64,
    step: f64,
) -> Retained<NSBezierPath> {
    let mut g = Seeded(seed);
    let (min_x, min_y) = (r.min().x, r.min().y);
    let (max_x, max_y) = (r.max().x, r.max().y);
    let p = NSBezierPath::new();
    // top-left → top-right
    p.moveToPoint(CGPoint::new(min_x, min_y));
    if top {
        let mut x = min_x + step;
        while x < max_x {
            p.lineToPoint(CGPoint::new(x, min_y + g.jitter(amplitude)));
            x += step;
        }
    }
    p.lineToPoint(CGPoint::new(max_x, min_y));
    if right {
        let mut y = min_y + step;
        while y < max_y {
            p.lineToPoint(CGPoint::new(max_x + g.jitter(amplitude), y));
            y += step;
        }
    }
    p.lineToPoint(CGPoint::new(max_x, max_y));
    if bottom {
        let mut x = max_x - step;
        while x > min_x {
            p.lineToPoint(CGPoint::new(x, max_y + g.jitter(amplitude)));
            x -= step;
        }
    }
    p.lineToPoint(CGPoint::new(min_x, max_y));
    if left {
        let mut y = max_y - step;
        while y > min_y {
            p.lineToPoint(CGPoint::new(min_x + g.jitter(amplitude), y));
            y -= step;
        }
    }
    p.closePath();
    p
}

/// Three sides of a box drawn by hand: down the left, along the bottom, up the
/// right. No top; the subpath stays open, like Swift's `RuledBox`. Swift
/// defaults: seed 1, amplitude 0.9, advance 6.
pub fn ruled_box_path(r: CGRect, seed: u64, amplitude: f64) -> Retained<NSBezierPath> {
    let mut g = Seeded(seed);
    let (min_x, min_y) = (r.min().x, r.min().y);
    let (max_x, max_y) = (r.max().x, r.max().y);
    let p = NSBezierPath::new();
    p.moveToPoint(CGPoint::new(min_x, min_y));
    let mut y = min_y + 6.0;
    while y < max_y {
        p.lineToPoint(CGPoint::new(min_x + g.jitter(amplitude), y));
        y += 6.0;
    }
    p.lineToPoint(CGPoint::new(min_x, max_y));
    let mut x = min_x + 6.0;
    while x < max_x {
        p.lineToPoint(CGPoint::new(x, max_y + g.jitter(amplitude)));
        x += 6.0;
    }
    p.lineToPoint(CGPoint::new(max_x, max_y));
    let mut y = max_y - 6.0;
    while y > min_y {
        p.lineToPoint(CGPoint::new(max_x + g.jitter(amplitude), y));
        y -= 6.0;
    }
    p.lineToPoint(CGPoint::new(max_x, min_y));
    p
}

/// Rounded rectangle with a half-circle notch cut into each side,
/// `notch_from_bottom` up from the bottom edge (odd cards are punched, even
/// cards are plain). Swift `TicketShape` defaults: radius 5, notch 11.
///
/// The Swift `Path.addArc` calls translate one-to-one to
/// `appendBezierPathWithArcWithCenter:` with the same angles and the same
/// `clockwise` flags — the two API surfaces agree on angle orientation
/// (verified element-for-element against the Swift runtime; see module docs).
pub fn ticket_path(
    r: CGRect,
    notch_from_bottom: Option<f64>,
    radius: f64,
    notch: f64,
) -> Retained<NSBezierPath> {
    let y = notch_from_bottom.map(|v| r.max().y - v);
    let (min_x, min_y) = (r.min().x, r.min().y);
    let (max_x, max_y) = (r.max().x, r.max().y);
    let p = NSBezierPath::new();
    p.moveToPoint(CGPoint::new(min_x + radius, min_y));
    p.lineToPoint(CGPoint::new(max_x - radius, min_y));
    p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
        CGPoint::new(max_x - radius, min_y + radius),
        radius,
        -90.0,
        0.0,
        false,
    );
    if let Some(y) = y {
        p.lineToPoint(CGPoint::new(max_x, y - notch));
        p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            CGPoint::new(max_x, y),
            notch,
            -90.0,
            90.0,
            true,
        );
    }
    p.lineToPoint(CGPoint::new(max_x, max_y - radius));
    p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
        CGPoint::new(max_x - radius, max_y - radius),
        radius,
        0.0,
        90.0,
        false,
    );
    p.lineToPoint(CGPoint::new(min_x + radius, max_y));
    p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
        CGPoint::new(min_x + radius, max_y - radius),
        radius,
        90.0,
        180.0,
        false,
    );
    if let Some(y) = y {
        p.lineToPoint(CGPoint::new(min_x, y + notch));
        p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            CGPoint::new(min_x, y),
            notch,
            90.0,
            -90.0,
            true,
        );
    }
    p.lineToPoint(CGPoint::new(min_x, min_y + radius));
    p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
        CGPoint::new(min_x + radius, min_y + radius),
        radius,
        180.0,
        270.0,
        false,
    );
    p.closePath();
    p
}

/// Perforation guide: the mid horizontal (`Line` in ClipCardView.swift).
pub fn line_path(r: CGRect) -> Retained<NSBezierPath> {
    let p = NSBezierPath::new();
    p.moveToPoint(CGPoint::new(r.min().x, r.mid().y));
    p.lineToPoint(CGPoint::new(r.max().x, r.mid().y));
    p
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
    offscreen_image(src.size(), |r| {
        src.drawInRect(r);
        // Grey it down so the paper stays quiet, keep the alpha.
        NSColor::colorWithCalibratedWhite_alpha(0.45, 1.0).set();
        NSRectFillUsingOperation(r, NSCompositingOperation::Color);
        src.drawInRect_fromRect_operation_fraction(
            r,
            CGRect::ZERO,
            NSCompositingOperation::DestinationIn,
            1.0,
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_app_kit::NSBezierPathElement;

    // ── SplitMix64 reference values generated from the Swift runtime ────────
    //     struct Seeded … (AnnotationRenderer.swift verbatim); swift /tmp/t.swift
    //     Seeded(20260912).next() ×3 and TornPaper/RuledBox mods for seeds 7 / 1.

    #[test]
    fn seeded_matches_swift_vectors() {
        let mut g = Seeded(20260912);
        assert_eq!(g.next(), 3821998546305730818);
        assert_eq!(g.next(), 1209654006528025633);
        assert_eq!(g.next(), 1409108709278482661);
        // noiseTile's first pixel: v = next() % 256.
        let mut n = Seeded(20260912);
        assert_eq!(n.next() % 256, 2);
    }

    #[test]
    fn jitter_matches_swift() {
        // Swift: Seeded(7).next() % 1000 → 487, 804, 346 …
        // jitter(3) = (mod/1000 − 0.5)·6 → −0.078, 1.824, −0.924
        let mut g = Seeded(7);
        assert_eq!(g.next() % 1000, 487);
        assert_eq!(g.next() % 1000, 804);
        assert_eq!(g.next() % 1000, 346);
        let mut g = Seeded(7);
        assert_eq!(g.jitter(3.0), -0.07800000000000007);
        assert_eq!(g.jitter(3.0), 1.8240000000000003);
        assert_eq!(g.jitter(3.0), -0.9240000000000002);
        // RuledBox seed 1.
        let mut g = Seeded(1);
        assert_eq!(g.next() % 1000, 465);
        assert_eq!(g.next() % 1000, 519);
        assert_eq!(g.next() % 1000, 590);
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
            let n = if kind == NSBezierPathElement::ClosePath { 0 } else if kind == NSBezierPathElement::CubicCurveTo { 3 } else { 1 };
            out.push((kind, pts[..n].to_vec()));
        }
        out
    }

    fn pt(x: f64, y: f64) -> CGPoint {
        CGPoint::new(x, y)
    }

    #[test]
    fn torn_paper_first_teeth_match_swift() {
        let r = CGRect::new(pt(0.0, 0.0), CGSize::new(100.0, 80.0));
        let p = torn_paper_path(r, true, true, true, true, 7, 3.0, 7.0);
        let els = elements(&p);
        assert_eq!(els[0], (NSBezierPathElement::MoveTo, vec![pt(0.0, 0.0)]));
        // Top edge: x = 7, 14, 21; y = 0 ± jitter.
        assert_eq!(els[1], (NSBezierPathElement::LineTo, vec![pt(7.0, -0.07800000000000007)]));
        assert_eq!(els[2], (NSBezierPathElement::LineTo, vec![pt(14.0, 1.8240000000000003)]));
        assert_eq!(els[3], (NSBezierPathElement::LineTo, vec![pt(21.0, -0.9240000000000002)]));
        // Closed; NSBezierPath may keep one degenerate moveTo after the close
        // (draws nothing) — the same trailing element seen on the Swift side.
        let closers = els.iter().filter(|e| e.0 == NSBezierPathElement::ClosePath).count();
        assert_eq!(closers, 1);
        assert!(
            els[els.len() - 1].0 == NSBezierPathElement::ClosePath
                || (els[els.len() - 2].0 == NSBezierPathElement::ClosePath
                    && els[els.len() - 1].0 == NSBezierPathElement::MoveTo)
        );
    }

    #[test]
    fn ruled_box_is_three_open_sides() {
        let r = CGRect::new(pt(0.0, 0.0), CGSize::new(100.0, 80.0));
        let p = ruled_box_path(r, 1, 0.9);
        let els = elements(&p);
        assert_eq!(els[0], (NSBezierPathElement::MoveTo, vec![pt(0.0, 0.0)]));
        // First left-side tooth: seed 1 mod 465 → j = (0.465−0.5)·2·0.9 (value
        // confirmed against the Swift runtime, bit-identical).
        assert_eq!(els[1], (NSBezierPathElement::LineTo, vec![pt(-0.06299999999999996, 6.0)]));
        // Ends top-right and is never closed.
        assert_eq!(els.last().unwrap().0, NSBezierPathElement::LineTo);
        assert_eq!(els.last().unwrap().1, vec![pt(100.0, 0.0)]);
    }

    // Reference table captured from the Swift runtime: TicketShape (SwiftUI
    // Path) vs NSBezierPath built with the same arc calls — every CGPath
    // element (with curve control points) compares equal, which is why this
    // port passes the arguments through unchanged. The only divergence at
    // CGPath level is one degenerate trailing moveTo NSBezierPath adds after
    // close (draws nothing).
    #[test]
    fn ticket_matches_swift_reference() {
        let r = CGRect::new(pt(10.0, 20.0), CGSize::new(100.0, 80.0));
        let p = ticket_path(r, Some(30.0), 5.0, 11.0);
        let els = elements(&p);
        let b = el_bounds(&els);
        let expect: [(u8, &[(f64, f64)]); 22] = [
            (0, &[(15.0, 20.0)]),                                    // M
            (1, &[(105.0, 20.0)]),                                   // L
            (1, &[(105.0, 20.0)]),                                   // L (auto-connect)
            (2, &[(107.7614, 20.0), (110.0, 22.2386), (110.0, 25.0)]),   // corner
            (1, &[(110.0, 59.0)]),
            (1, &[(110.0, 59.0)]),
            (2, &[(103.9249, 59.0), (99.0, 63.9249), (99.0, 70.0)]),     // notch in
            (2, &[(99.0, 76.0751), (103.9249, 81.0), (110.0, 81.0)]),    // notch out
            (1, &[(110.0, 95.0)]),
            (1, &[(110.0, 95.0)]),
            (2, &[(110.0, 97.7614), (107.7614, 100.0), (105.0, 100.0)]), // corner
            (1, &[(15.0, 100.0)]),
            (1, &[(15.0, 100.0)]),
            (2, &[(12.2386, 100.0), (10.0, 97.7614), (10.0, 95.0)]),     // corner
            (1, &[(10.0, 81.0)]),
            (1, &[(10.0, 81.0)]),
            (2, &[(16.0751, 81.0), (21.0, 76.0751), (21.0, 70.0)]),      // notch in
            (2, &[(21.0, 63.9249), (16.0751, 59.0), (10.0, 59.0)]),      // notch out
            (1, &[(10.0, 25.0)]),
            (1, &[(10.0, 25.0)]),
            (2, &[(10.0, 22.2386), (12.2386, 20.0), (15.0, 20.0)]),      // corner
            (3, &[]),                                                  // close
        ];
        assert!(els.len() >= expect.len());
        for (i, (kind, pts)) in expect.iter().enumerate() {
            assert_eq!(els[i].0.0, *kind as usize, "element {i} kind");
            assert_eq!(els[i].1.len(), pts.len(), "element {i} arity");
            for (got, (ex, ey)) in els[i].1.iter().zip(pts.iter()) {
                // Swift reference printed to 4 decimals.
                assert!((got.x - ex).abs() < 5e-5, "element {i} x: {} vs {ex}", got.x);
                assert!((got.y - ey).abs() < 5e-5, "element {i} y: {} vs {ey}", got.y);
            }
        }
        assert_eq!((b.min().x, b.min().y, b.max().x, b.max().y), (10.0, 20.0, 110.0, 100.0));
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

    #[test]
    fn ticket_without_notch_has_no_mid_arcs() {
        let r = CGRect::new(pt(10.0, 20.0), CGSize::new(100.0, 80.0));
        let p = ticket_path(r, None, 5.0, 11.0);
        let els = elements(&p);
        let curves = els.iter().filter(|(k, _)| *k == NSBezierPathElement::CubicCurveTo).count();
        assert_eq!(curves, 4, "four rounded corners only");
    }

    #[test]
    fn fonts_resolve() {
        // Songti ships with macOS.
        let f = serif(14.0, false);
        assert_eq!(f.fontName().to_string(), "STSongti-SC-Regular");
        let b = serif(14.0, true);
        assert_eq!(b.fontName().to_string(), "STSongti-SC-Bold");
        assert_eq!(f.pointSize(), 14.0);
        // Brand fonts live under Resources/Fonts in the repo; registration is
        // what gates the Caveat descriptor.
        assert!(register_brand_fonts());
        let s = script(16.0);
        assert_eq!(s.familyName().map(|n| n.to_string()).as_deref(), Some("Caveat"));
    }

    #[test]
    fn noise_tile_builds() {
        let t = noise_tile();
        assert_eq!(t.size(), CGSize::new(96.0, 96.0));
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

    /// Renders the shapes + textures to PNGs for eyeball comparison with the
    /// Swift build. Opt-in: `PASTORY_THEME_SNAPSHOT_DIR=/tmp xargs cargo test`.
    #[test]
    fn snapshot_fixtures() {
        let Some(dir) = std::env::var_os("PASTORY_THEME_SNAPSHOT_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        objc2::rc::autoreleasepool(|_| {
            let card = CGRect::new(pt(0.0, 0.0), CGSize::new(288.0, 420.0));
            // A whole card: desk ground, ticket punched like an odd card.
            if let Some(cg) = offscreen_cg(card.size, |_| {
                draw_ground(CGRect::new(pt(-40.0, -40.0), CGSize::new(card.size.width + 80.0, card.size.height + 80.0)));
                let t = ticket_path(
                    CGRect::new(pt(16.0, 16.0), CGSize::new(256.0, 388.0)),
                    Some(220.0),
                    5.0,
                    11.0,
                );
                draw_paper(&t, &paper());
                ruled_box_path(CGRect::new(pt(32.0, 360.0), CGSize::new(120.0, 12.0)), 1, 0.9).stroke();
            }) {
                let png = crate::capture::screenshotter::png_data(&cg).unwrap();
                std::fs::write(dir.join("card.png"), png).unwrap();
            }
            if let Some(cg) = offscreen_cg(CGSize::new(320.0, 120.0), |r| {
                draw_ground(r);
                let t = torn_paper_path(CGRect::new(pt(10.0, 15.0), CGSize::new(300.0, 90.0)), true, true, true, true, 7, 3.0, 7.0);
                draw_paper(&t, &paper_blue());
            }) {
                let png = crate::capture::screenshotter::png_data(&cg).unwrap();
                std::fs::write(dir.join("torn.png"), png).unwrap();
            }
            if let Some(cg) = offscreen_cg(CGSize::new(288.0, 30.0), |r| {
                draw_ground(r);
                NSColor::colorWithPatternImage(&paper_tile()).setFill();
                NSRectFill(CGRect::new(pt(8.0, 5.0), CGSize::new(130.0, 20.0)));
                NSColor::colorWithPatternImage(&paper_blue_tile()).setFill();
                NSRectFill(CGRect::new(pt(142.0, 5.0), CGSize::new(64.0, 20.0)));
                NSColor::colorWithPatternImage(&desk_tile()).setFill();
                NSRectFill(CGRect::new(pt(210.0, 5.0), CGSize::new(70.0, 20.0)));
            }) {
                let png = crate::capture::screenshotter::png_data(&cg).unwrap();
                std::fs::write(dir.join("tiles.png"), png).unwrap();
            }
            // Bake content drawn at exact tile size + a grid-aligned tiling.
            if let Some(cg) = offscreen_cg(CGSize::new(96.0, 96.0), |r| {
                paper_tile().drawInRect(r);
            }) {
                let png = crate::capture::screenshotter::png_data(&cg).unwrap();
                std::fs::write(dir.join("bake.png"), png).unwrap();
            }
            if let Some(cg) = offscreen_cg(CGSize::new(192.0, 96.0), |r| {
                NSColor::colorWithPatternImage(&paper_tile()).setFill();
                NSRectFill(r);
            }) {
                let png = crate::capture::screenshotter::png_data(&cg).unwrap();
                std::fs::write(dir.join("aligned.png"), png).unwrap();
            }
            // Off-origin paper patches with live grain — the shelf's per-card case.
            if let Some(cg) = offscreen_cg(CGSize::new(220.0, 110.0), |r| {
                draw_ground(r);
                let t = torn_paper_path(
                    CGRect::new(pt(16.3, 22.7), CGSize::new(150.0, 60.0)),
                    true, true, true, true, 11, 3.0, 7.0,
                );
                draw_paper(&t, &paper());
                let t2 = ticket_path(
                    CGRect::new(pt(37.5, 41.25), CGSize::new(120.0, 50.0)),
                    Some(25.0),
                    5.0,
                    11.0,
                );
                draw_paper(&t2, &paper_blue());
            }) {
                let png = crate::capture::screenshotter::png_data(&cg).unwrap();
                std::fs::write(dir.join("patch.png"), png).unwrap();
            }
        });
    }
}

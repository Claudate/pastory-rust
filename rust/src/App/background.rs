//! The user's own panel background: pick an image, remember it, paint it.
//!
//! The photo is a *ground* like the skin's `bg`: drawn behind the content
//! column (right of the sidebar — the sidebar keeps the skin's own recessed
//! band), at the user's chosen **opacity** (`dim` — the key keeps its name
//! and its stored tenths: 0 = invisible, 1 = full strength), blended over
//! the skin's ground colour so the type on top keeps its contrast no matter
//! which photo was picked.
//!
//! The photo and the skin's own colour scheme are mutually exclusive
//! (`enabled`): selecting a skin pauses the photo, choosing a photo (or
//! pressing 启用 again) resumes it. The remembered file is never thrown
//! away by the switch, so 背景 and 皮肤 stay a toggle, not a choice that
//! forgets.
//!
//! Three things are remembered: a security-scoped bookmark (the sandbox can
//! only reopen a user-picked file through one), the plain path (what the
//! settings pane prints, and what the non-sandboxed build resolves), and
//! the on/off switch above.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_app_kit::{NSCompositingOperation, NSImage};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSString, NSURL};

use crate::app::preferences::{ns_string_id, Preferences};

/// The bookmark blob (sandbox-safe handle on the picked file).
const BOOKMARK_KEY: &str = "panelBackgroundBookmark";
/// The plain path: what the pane displays and the direct build resolves.
///
/// Public so `--selftest background` can drop it and force the bookmark route, which
/// is the one a sandboxed relaunch actually takes.
pub const PATH_KEY: &str = "panelBackgroundPath";
/// Veil strength over the image, 0.0 (invisible) … 1.0 (full strength).
const DIM_KEY: &str = "panelBackgroundDim";
/// Whether the photo is in force at all. The skin's colour scheme and the
/// photo are mutually exclusive: selecting a skin stores `false`, choosing
/// a photo stores `true`. The file itself stays remembered either way, so
/// flipping the switch back restores it.
const ENABLED_KEY: &str = "panelBackgroundEnabled";

/// Photo opacity before the user touches it: full strength over the skin's
/// ground — the skin's colour already keeps the type readable, so there is
/// nothing to tune back by default.
pub const DEFAULT_DIM: f64 = 1.0;

/// The opacities the settings row offers, in percent: 淡 / 中 / 浓 / 全亮.
/// A fixed strip rather than a slider — the row is 44pt tall and four steps
/// cover the useful range (0 hides the photo entirely, 1 paints it over the
/// skin's ground).
pub const DIM_STEPS: [(i64, &str); 4] = [(3, "淡"), (6, "中"), (8, "浓"), (10, "全亮")];

/// The step the current strength sits on, for the row's selected chip.
pub fn dim_step() -> i64 {
    let d = dim();
    DIM_STEPS
        .iter()
        .map(|(v, _)| *v)
        .min_by(|a, b| ((*a as f64 / 10.0) - d).abs().partial_cmp(&((*b as f64 / 10.0) - d).abs()).unwrap())
        .unwrap_or(10)
}

/// The decoded image, resolved once per pick.
///
/// An `NSImage` is neither `Send` nor `Sync`, so it is held as a raw pointer
/// for the process lifetime exactly like the other long-lived images in
/// `theme.rs`: `load` into_raw's it once, `image()` hands the caller a fresh
/// retain, and nothing else ever sees the pointer.
static CACHE: OnceLock<Mutex<Option<Option<usize>>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<Option<usize>>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Drop the decoded image so the next lookup re-reads the stored path. Called
/// after the picker (or a clear) changes which file is remembered.
pub fn invalidate() {
    if let Ok(mut c) = cache().lock() {
        *c = None;
    }
}

/// The remembered file, if it is readable right now. A stored path whose file
/// has since moved is a memory to drop, not an error to raise: the panel
/// simply falls back to the skin's own ground.
fn load(path: &str) -> Option<Retained<NSImage>> {
    let img = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path));
    if img.is_none() {
        return None;
    }
    let img = img?;
    // A zero-size image would divide by zero in the cover fit below.
    let s = img.size();
    if s.width <= 0.0 || s.height <= 0.0 {
        return None;
    }
    Some(img)
}

/// The background image in force, decoded on first use.
pub fn image() -> Option<Retained<NSImage>> {
    let path = Preferences::shared().panel_background_path()?;
    if let Ok(mut c) = cache().lock() {
        match *c {
            Some(Some(ptr)) => {
                // SAFETY: the reference was into_raw'd once below and never
                // leaves the cell; adds the caller's own retain.
                return unsafe { Retained::retain(ptr as *mut NSImage) };
            }
            Some(None) => return None, // already known unreadable
            None => {}
        }
        let img = load(&path);
        let raw = img.as_ref().map(|i| Retained::into_raw(i.clone()) as usize);
        *c = Some(raw);
        return img;
    }
    load(&path)
}

/// Whether a background image is set (and readable), regardless of the
/// switch. The settings row uses this: a paused photo still shows its name
/// and offers 启用 back.
pub fn has_image() -> bool {
    image().is_some()
}

/// The photo in force right now — `active()`'s image, the only thing the
/// paint path may draw. `None` while the switch is off, so a paused photo
/// stays remembered but paints nothing.
pub fn active_image() -> Option<Retained<NSImage>> {
    if !active() {
        return None;
    }
    image()
}

/// Whether the mutual-exclusion switch is on (a remembered-but-paused photo
/// paints nothing). The self-test override (`PASTORY_BACKGROUND`) is always
/// in force — the render is there to exercise the painted ground.
pub fn enabled() -> bool {
    if crate::app::sandbox::self_test_var("PASTORY_BACKGROUND").is_some() {
        return true;
    }
    Preferences::shared().panel_background_enabled()
}

/// Whether the photo is in force right now: the mutual-exclusion switch on
/// **and** a readable file. The paint path checks only this, so a paused
/// photo stays remembered but paints nothing.
pub fn active() -> bool {
    enabled() && has_image()
}

/// Veil strength over the image.
pub fn dim() -> f64 {
    Preferences::shared().panel_background_dim()
}

/// Paint `img` so it covers `rect` completely (centre-cropped, never
/// letterboxed): a ground must not leave bare corners behind the cards.
pub fn draw_cover(img: &NSImage, rect: CGRect) {
    draw_cover_alpha(img, rect, 1.0);
}

/// `draw_cover` at `alpha` — how the photo blends over the skin's ground
/// (the 透明度 the settings row offers). A photo cropped bigger than the
/// rect must not darken outside it, so the draw is clipped to `rect`.
pub fn draw_cover_alpha(img: &NSImage, rect: CGRect, alpha: f64) {
    if alpha <= 0.0 || rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return;
    }
    let s = img.size();
    if s.width <= 0.0 || s.height <= 0.0 {
        return;
    }
    let scale = (rect.size.width / s.width).max(rect.size.height / s.height);
    let (w, h) = (s.width * scale, s.height * scale);
    let dest = CGRect::new(
        CGPoint::new(
            rect.min().x + (rect.size.width - w) / 2.0,
            rect.min().y + (rect.size.height - h) / 2.0,
        ),
        CGSize::new(w, h),
    );
    objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
    objc2_app_kit::NSBezierPath::bezierPathWithRect(rect).addClip();
    crate::shelf::card::draw_image(
        img,
        dest,
        NSCompositingOperation::SourceOver,
        alpha,
    );
    objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
}

impl Preferences {
    /// Remember `url` as the panel background: a security-scoped bookmark so
    /// the sandbox can reopen it next launch, plus the plain path for the
    /// settings row. Replaces whatever was picked before.
    pub fn set_panel_background(&self, url: &NSURL) {
        let blob = crate::app::preferences_export::file_bookmark_blob(url);
        match blob {
            Some(blob) => unsafe {
                // SAFETY: `blob` is an NSData and UserDefaults holds any object.
                self.defaults().setObject_forKey(
                    Some(&*(Retained::as_ptr(&blob) as *const AnyObject)),
                    ns_string_id(BOOKMARK_KEY),
                );
            },
            // Nothing usable came back from the panel: drop the old handle
            // rather than keep pointing at the file the user replaced.
            None => self.defaults().removeObjectForKey(ns_string_id(BOOKMARK_KEY)),
        }
        self.set_panel_background_path(url.path().map(|p| p.to_string()).as_deref());
        // A fresh pick resumes the photo over the skin (the mutual exclusion).
        self.set_panel_background_enabled(true);
        invalidate();
    }

    /// Forget the background; the panel goes back to the skin's own ground.
    /// The switch goes with the file: nothing remembered means nothing to
    /// enable, and the next pick starts from on.
    pub fn clear_panel_background(&self) {
        self.defaults().removeObjectForKey(ns_string_id(BOOKMARK_KEY));
        self.defaults().removeObjectForKey(ns_string_id(PATH_KEY));
        self.set_panel_background_enabled(false);
        invalidate();
    }

    fn set_panel_background_path(&self, v: Option<&str>) {
        match v {
            Some(s) if !s.is_empty() => unsafe {
                // SAFETY: NSString value for an NSString key.
                self.defaults().setObject_forKey(
                    Some(&*NSString::from_str(s)),
                    ns_string_id(PATH_KEY),
                );
            },
            _ => self.defaults().removeObjectForKey(ns_string_id(PATH_KEY)),
        }
    }

    /// The picked file's path: the plain key when it is there, else the
    /// bookmark resolved back to a URL (the sandboxed relaunch).
    ///
    /// `PASTORY_BACKGROUND=<file>` overrides it for a self-test run, the same way
    /// `PASTORY_SKIN` / `PASTORY_LANG` do — a render can then exercise the painted
    /// ground without the user's own preference being touched or read.
    pub fn panel_background_path(&self) -> Option<String> {
        if let Some(p) = crate::app::sandbox::self_test_var("PASTORY_BACKGROUND") {
            return std::path::Path::new(&p).is_file().then_some(p);
        }
        if let Some(p) = self.defaults().stringForKey(ns_string_id(PATH_KEY)) {
            let p = p.to_string();
            if !p.is_empty() && std::path::Path::new(&p).is_file() {
                return Some(p);
            }
        }
        let blob = self.defaults().dataForKey(ns_string_id(BOOKMARK_KEY))?;
        let url = crate::app::preferences_export::resolve_file_blob(&blob)?;
        let path = url.path().map(|p| p.to_string())?;
        std::path::Path::new(&path).is_file().then_some(path)
    }

    /// Veil strength, clamped to the usable range (a stored value from an
    /// older build must never make the panel unreadable or fully opaque
    /// without the user asking for it). `PASTORY_BACKGROUND_DIM` overrides it
    /// in a self-test run.
    pub fn panel_background_dim(&self) -> f64 {
        if let Some(v) = crate::app::sandbox::self_test_var("PASTORY_BACKGROUND_DIM") {
            if let Ok(n) = v.parse::<f64>() {
                return n.clamp(0.0, 1.0);
            }
        }
        match self.defaults().objectForKey(ns_string_id(DIM_KEY)) {
            Some(v) => {
                let n: f64 = unsafe { objc2::msg_send![&*v, doubleValue] };
                n.clamp(0.0, 1.0)
            }
            None => DEFAULT_DIM,
        }
    }

    pub fn set_panel_background_dim(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        unsafe {
            // SAFETY: NSNumber value for an NSString key.
            let n = objc2_foundation::NSNumber::numberWithDouble(v);
            self.defaults().setObject_forKey(
                Some(&*(Retained::as_ptr(&n) as *const AnyObject)),
                ns_string_id(DIM_KEY),
            );
        }
    }

    /// Whether the photo is in force (the mutual-exclusion switch). Unset
    /// means off: the skin's own colour scheme owns the panel until the user
    /// picks a photo.
    pub fn panel_background_enabled(&self) -> bool {
        if let Some(v) = self.defaults().objectForKey(ns_string_id(ENABLED_KEY)) {
            // SAFETY: NSNumber value for an NSString key.
            return unsafe { objc2::msg_send![&*v, boolValue] };
        }
        false
    }

    pub fn set_panel_background_enabled(&self, on: bool) {
        unsafe {
            // SAFETY: NSNumber value for an NSString key.
            let n = objc2_foundation::NSNumber::numberWithBool(on);
            self.defaults().setObject_forKey(
                Some(&*(Retained::as_ptr(&n) as *const AnyObject)),
                ns_string_id(ENABLED_KEY),
            );
        }
    }
}

/// The tint the photo blends with: the skin's own ground, so the photo at a
/// partial opacity sits on the skin rather than on bare black. (Kept for
/// callers that want the colour itself; the paint path blends by alpha.)
#[allow(dead_code)]
pub fn dim_color() -> Option<Retained<objc2_app_kit::NSColor>> {
    Some(crate::app::theme::ground())
}

/// Where the picked file lives, for the settings row (the plain path when
/// stored, else "" — the row shows a placeholder rather than the bookmark).
pub fn display_path() -> String {
    Preferences::shared()
        .panel_background_path()
        .unwrap_or_default()
}

/// The file's name only, which is all the settings row has room for.
pub fn display_name() -> String {
    PathBuf::from(display_path())
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

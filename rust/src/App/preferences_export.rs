//! The remembered 「保存到本地」 folder, as a security-scoped bookmark.
//!
//! The Swift original stored a plain path under `UserDefaults.exportDir`. In
//! the sandbox that memory dies with the launch: the same path comes back as a
//! folder the app is no longer allowed to write (docs/MAS_PLAN.md §3). A
//! bookmark created with `WithSecurityScope` survives it. The plain path is
//! still written alongside, so the settings pane can print it and the
//! non-sandboxed direct build keeps its old behaviour byte for byte.

use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2_foundation::{
    NSData, NSString, NSURL, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions,
};

use crate::app::preferences::{ns_string_id, Preferences};

/// The security-scoped bookmark blob.
const BOOKMARK_KEY: &str = "exportDirBookmark";
/// The plain path: Swift's key, still the source of what the pane displays and
/// what the non-sandboxed build resolves.
const PATH_KEY: &str = "exportDir";

/// The export folder resolved during this launch. Holding the URL is what
/// keeps its security scope open, so this static owns it and the path is read
/// back out of it — one source of truth for "where do we save".
static OPEN_EXPORT_DIR: OnceLock<Mutex<Option<Retained<NSURL>>>> = OnceLock::new();

fn open_dir() -> &'static Mutex<Option<Retained<NSURL>>> {
    OPEN_EXPORT_DIR.get_or_init(|| Mutex::new(None))
}

/// Drop the folder held open for this launch, so the next lookup reads the
/// bookmark again. For `--selftest exportdir`, which changes the stored blob
/// underneath a live process.
pub(crate) fn forget_open_export_dir() {
    close_open_dir();
}

/// Usable folder to save into: create it when the user deleted it (Swift did
/// the same on every export). Outside the sandbox a fresh folder under a
/// granted parent still works; a path the app cannot even create is a memory
/// to drop rather than an error to keep.
fn ensure_dir(dir: &std::path::Path) -> bool {
    if dir.is_dir() {
        return true;
    }
    std::fs::create_dir_all(dir).is_ok() && dir.is_dir()
}

/// Release the folder we had open.
fn close_open_dir() {
    if let Ok(mut slot) = open_dir().lock() {
        if let Some(url) = slot.take() {
            // SAFETY: every URL that reaches the slot came from a successful
            // `startAccessingSecurityScopedResource`, so this balances it.
            unsafe { url.stopAccessingSecurityScopedResource() };
        }
    }
}

/// Bookmark blob for `url`: security-scoped when this process can make one,
/// plain otherwise.
///
/// Only a sandboxed process can mint a security-scoped bookmark — outside the
/// sandbox the framework refuses with `NSCocoaErrorDomain 256` ("Could not
/// open() the item"), which is exactly the build that needs no scope at all.
/// Both cases must still land a usable memory, so the plain bookmark is the
/// fallback: it round-trips a path just as well when there is no sandbox to
/// cross. Shared with the panel background, which remembers a file the same
/// way.
pub(crate) fn file_bookmark_blob(url: &NSURL) -> Option<Retained<NSData>> {
    url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
        NSURLBookmarkCreationOptions::WithSecurityScope,
        None,
        None,
    )
    .ok()
    .or_else(|| {
        url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
            NSURLBookmarkCreationOptions::empty(),
            None,
            None,
        )
        .ok()
    })
}

/// Resolve a stored blob back to a URL, preferring the security-scoped reading
/// and falling back to the plain one. The returned URL has not had its scope
/// started — the caller decides whether to.
///
/// Start the scope for a security-scoped blob (or the read fails under the
/// sandbox); a plain blob, and the direct build, need none. Nothing closes it:
/// a user-picked file stays reachable for the rest of the launch, which is the
/// same lifetime the export folder keeps.
pub(crate) fn resolve_file_blob(blob: &NSData) -> Option<Retained<NSURL>> {
    let url = resolve_blob_to_url(blob)?;
    unsafe { url.startAccessingSecurityScopedResource() };
    Some(url)
}

fn resolve_blob_to_url(blob: &NSData) -> Option<Retained<NSURL>> {
    let scoped = NSURLBookmarkResolutionOptions::WithSecurityScope
        | NSURLBookmarkResolutionOptions::WithoutImplicitStartAccessing
        | NSURLBookmarkResolutionOptions::WithoutUI
        | NSURLBookmarkResolutionOptions::WithoutMounting;
    let plain = NSURLBookmarkResolutionOptions::WithoutUI
        | NSURLBookmarkResolutionOptions::WithoutMounting;
    for options in [scoped, plain] {
        let url = unsafe {
            // SAFETY: a null `is_stale` is the documented "do not care" form.
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                blob,
                options,
                None,
                std::ptr::null_mut::<Bool>(),
            )
        };
        if let Ok(url) = url {
            return Some(url);
        }
    }
    None
}

impl Preferences {
    /// The stored bookmark blob, if the folder was ever picked.
    fn export_bookmark(&self) -> Option<Retained<NSData>> {
        self.defaults().dataForKey(ns_string_id(BOOKMARK_KEY))
    }

    /// Remember `url` as the export folder: a security-scoped bookmark so the
    /// sandbox can reopen it next launch, plus the plain path for display.
    /// Replaces the folder remembered before.
    pub fn set_custom_export_dir(&self, url: &NSURL) {
        close_open_dir();
        match file_bookmark_blob(url) {
            Some(blob) => unsafe {
                // SAFETY: `blob` is an NSData and UserDefaults holds any object.
                self.defaults().setObject_forKey(
                    Some(&*(Retained::as_ptr(&blob) as *const AnyObject)),
                    ns_string_id(BOOKMARK_KEY),
                );
            },
            // No blob at all (the panel handed back something unusable): drop
            // the old one rather than reopen a folder the user replaced.
            None => self.defaults().removeObjectForKey(ns_string_id(BOOKMARK_KEY)),
        }
        self.set_export_path(url.path().map(|p| p.to_string()).as_deref());
    }

    /// The plain path key, spot-checked by Swift builds of the same app.
    fn set_export_path(&self, v: Option<&str>) {
        match v {
            Some(s) if !s.is_empty() => unsafe {
                self.defaults().setObject_forKey(
                    Some(&*NSString::from_str(s)),
                    ns_string_id(PATH_KEY),
                );
            },
            _ => self.defaults().removeObjectForKey(ns_string_id(PATH_KEY)),
        }
    }

    /// Where 「保存到本地」 opens next: the remembered folder, its security
    /// scope already open. Falls back to ~/Downloads — and drops a folder this
    /// launch cannot reach, so a stale memory never blocks a save.
    pub fn export_directory(&self) -> std::path::PathBuf {
        self.remembered_export_dir().unwrap_or_else(|| self.reset_export_dir())
    }

    /// The remembered folder when it is reachable right now. Opens the scope
    /// on the first call and keeps the URL alive for the rest of the launch.
    fn remembered_export_dir(&self) -> Option<std::path::PathBuf> {
        if let Ok(slot) = open_dir().lock() {
            if let Some(url) = slot.as_ref() {
                return url.path().map(|p| std::path::PathBuf::from(p.to_string()));
            }
        }
        if let Some(url) = self.resolve_export_bookmark() {
            let path = url.path().map(|p| std::path::PathBuf::from(p.to_string()));
            if let Some(path) = path.filter(|p| ensure_dir(p)) {
                if let Ok(mut slot) = open_dir().lock() {
                    *slot = Some(url);
                }
                return Some(path);
            }
            return None;
        }
        // No bookmark — the non-sandboxed build, or an install from before the
        // bookmark existed. The plain path needs no permission there.
        self.custom_export_dir()
            .map(std::path::PathBuf::from)
            .filter(|d| ensure_dir(d))
    }

    /// Resolve the blob and open its scope.
    fn resolve_export_bookmark(&self) -> Option<Retained<NSURL>> {
        let blob = self.export_bookmark()?;
        let url = resolve_blob_to_url(&blob)?;
        // Best effort on purpose: outside the sandbox (the direct build, and
        // the self-test suite) this returns false while the folder is wide
        // open, so gating on it would throw away a perfectly good memory.
        // Inside the sandbox a failed start surfaces as a failed write below.
        // SAFETY: a scope opened here is closed by `close_open_dir`, and at
        // the latest by the process exiting.
        unsafe { url.startAccessingSecurityScopedResource() };
        Some(url)
    }

    /// Forget the memory and land on ~/Downloads (created on demand).
    fn reset_export_dir(&self) -> std::path::PathBuf {
        close_open_dir();
        self.defaults().removeObjectForKey(ns_string_id(BOOKMARK_KEY));
        self.defaults().removeObjectForKey(ns_string_id(PATH_KEY));
        let home = std::env::var_os("HOME").expect("HOME");
        let dir = std::path::PathBuf::from(home).join("Downloads");
        if !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
        }
        dir
    }
}

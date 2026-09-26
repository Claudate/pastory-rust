//! Port of `Clipboard/ClipStore.swift` — the in-memory index over
//! `pastory.sqlite` plus the payload files under the store root.
//!
//! ```text
//! ~/Library/Application Support/Pastory/
//!   pastory.sqlite   the index (SQLite, WAL)
//!   items/<id>.<ext> payload (txt / png / json list of paths); <id>.rtf alongside when rich text
//!   thumbs/<id>.heic shelf thumbnail for images and recordings (older stores: .png)
//!   share/<id>/      a human-named hard link for the pasteboard
//! ```
//!
//! The Swift type is `@MainActor` + `@Observable`. The Rust port keeps the
//! same single global instance behind a mutex; background work (thumbnail
//! decoding, OCR in M4) locks it briefly to commit results.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSDate, NSDateFormatter, NSString, NSUUID};

use crate::app::preferences::Preferences;
use crate::capture::{pasteboard_writer, screenshotter};
use crate::clipboard::db::{self, ClipDB};
use crate::clipboard::item::{now, ClipItem, ClipKind};
use crate::clipboard::retention;

/// Max size of a text payload (Swift `maxTextBytes`).
pub const MAX_TEXT_BYTES: usize = 20 * 1024 * 1024;

/// Marker in `sourceAppName` for imported items (stored as-is in the DB;
/// shown localized). Never rename (red line 4).
pub const IMPORT_SOURCE_NAME: &str = "导入";

// CommonCrypto ships in libSystem — one extern instead of a sha2 dependency.
mod cc {
    extern "C" {
        pub fn CC_SHA256(data: *const u8, len: u32, md: *mut u8) -> *mut u8;
    }
}

/// `stableHash` (ClipStore.swift:6): SHA256 of the payload, first 8 digest
/// bytes load little-endian into an Int. Image items always hash the original
/// PNG, whatever the storage format (red line 9).
pub fn stable_hash(data: &[u8]) -> i64 {
    let mut out = [0u8; 32];
    // SAFETY: plain C function over caller-owned buffers; CC_SHA256 never
    // fails and writes exactly 32 bytes.
    unsafe {
        cc::CC_SHA256(data.as_ptr(), data.len() as u32, out.as_mut_ptr());
    }
    i64::from_le_bytes(out[..8].try_into().expect("8 of 32 digest bytes"))
}

/// `ClipStore.Source` — where a copy came from.
#[derive(Clone, Debug, Default)]
pub struct Source {
    pub bundle_id: Option<String>,
    pub name: Option<String>,
}

impl Source {
    /// `CaptureCoordinator.source` — our own app (captures, edits).
    pub fn pastory() -> Source {
        Source {
            bundle_id: Some("com.cici.snipclip".into()),
            name: Some("Pastory".into()),
        }
    }

    /// The frontmost app right now.
    pub fn frontmost() -> Source {
        use objc2_app_kit::NSWorkspace;
        let app = NSWorkspace::sharedWorkspace().frontmostApplication();
        Source {
            bundle_id: app
                .as_ref()
                .and_then(|a| a.bundleIdentifier())
                .map(|s| s.to_string()),
            name: app.and_then(|a| a.localizedName()).map(|s| s.to_string()),
        }
    }
}

/// One import entry (the background-computable half of an import).
pub enum ImportPayload {
    Text(String),
    Image(Vec<u8>),
}

pub struct ImportEntry {
    pub payload: ImportPayload,
    pub created_at: f64,
    pub pinned: bool,
    pub title: Option<String>,
}

/// Everything about an entry that can be computed away from the store:
/// bytes to write, thumbnail, hash.
pub struct PreparedEntry {
    pub kind: ClipKind,
    pub payload: Vec<u8>,
    pub ext: String,
    pub thumb: Option<Vec<u8>>,
    pub snippet: String,
    pub pixel_width: Option<i64>,
    pub pixel_height: Option<i64>,
    pub hash: i64,
    pub created_at: f64,
    pub pinned: bool,
    pub title: Option<String>,
}

pub struct ClipStore {
    pub items: Vec<ClipItem>,
    pub root: PathBuf,
    items_dir: PathBuf,
    thumbs_dir: PathBuf,
    share_dir: PathBuf,
    db: Option<ClipDB>,
    /// Set when the index could not be read at launch. While it is set
    /// nothing is ever written back, because a replace-all save from an empty
    /// in-memory list would wipe the table.
    pub load_failed: bool,
    /// True after a write failed (full disk, unplugged volume). Retention
    /// holds off until a save succeeds again.
    pub last_save_failed: bool,
    warned_save_failure: bool,
    /// Set when a one-row write failed: the table no longer matches memory,
    /// so the next write rewrites the whole table.
    needs_full_save: bool,
    /// Content hashes of deleted items, loaded with the index; consulted by imports.
    buried: HashSet<i64>,
    /// Bumped on every successful write; views cache derived lists against it.
    pub version: u64,
    /// Decoded thumbnails, `Retained<CGImage>` kept as raw pointers (CG images
    /// are immutable and CF refcounting is thread-safe; the mutex serializes
    /// every access). `cache_remove` is the only release path.
    thumb_cache: HashMap<String, usize>,
    /// Insertion order for the >100 eviction of the oldest third.
    thumb_order: Vec<String>,
    thumb_loading: HashSet<String>,
    /// Decoding was tried and there is no usable file: show a glyph, do not retry.
    thumb_missing: HashSet<String>,
    /// Bumped when a thumbnail finishes decoding; cards re-render.
    pub thumb_tick: u64,
}

impl Drop for ClipStore {
    fn drop(&mut self) {
        let ptrs: Vec<usize> = self.thumb_cache.drain().map(|(_, p)| p).collect();
        for p in ptrs {
            // SAFETY: each pointer was Retained::into_raw on insert and leaves
            // the cache here exactly once.
            let _ = unsafe { CFRetained::from_raw(std::ptr::NonNull::new_unchecked(p as *mut CGImage)) };
        }
    }
}

/// `~/Library/Application Support/Pastory/` — unless PASTORY_STORE is set, in
/// which case that sandbox is "default" too, so self-tests (relocate back to
/// default included) can never touch the user's data.
pub fn default_root() -> PathBuf {
    if let Some(env) = crate::app::sandbox::launch().store() {
        return PathBuf::from(env);
    }
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Pastory")
}

/// The process-wide store (Swift `ClipStore.shared`). Created on first use
/// from `default_root()`; poisoned-mutex panics are intended — a store write
/// that panicked must not silently half-apply on the next call.
pub fn shared() -> &'static Mutex<Option<ClipStore>> {
    static SHARED: std::sync::OnceLock<&'static Mutex<Option<ClipStore>>> =
        std::sync::OnceLock::new();
    SHARED.get_or_init(|| {
        let cell: &'static Mutex<Option<ClipStore>> = &*Box::leak(Box::new(Mutex::new(None)));
        let mut guard = cell.lock().unwrap();
        if guard.is_none() {
            *guard = Some(ClipStore::new(default_root()));
        }
        cell
    })
}

/// Typed lock guard over the store; the `Option` unwrap is the
/// "store initialized" invariant.
pub struct StoreGuard(std::sync::MutexGuard<'static, Option<ClipStore>>);

impl std::ops::Deref for StoreGuard {
    type Target = ClipStore;
    fn deref(&self) -> &ClipStore {
        self.0.as_ref().expect("store initialized")
    }
}

impl std::ops::DerefMut for StoreGuard {
    fn deref_mut(&mut self) -> &mut ClipStore {
        self.0.as_mut().expect("store initialized")
    }
}

/// Lock the store; panics if poisoned (half-applied writes must be loud).
pub fn with<R>(f: impl FnOnce(&mut ClipStore) -> R) -> R {
    let guard = shared().lock().expect("clip store poisoned");
    let mut g = StoreGuard(guard);
    f(&mut g)
}

/// Same, read-only.
pub fn read<R>(f: impl FnOnce(&ClipStore) -> R) -> R {
    with(|s| f(s))
}

/// Bulk removal by id list (Retention sweep, "remove all imported" in M6).
pub fn remove_where(ids: &[String]) {
    with(|s| s.remove_ids(ids));
}

/// `ClipStore.shared.purgeTombstones(olderThan:)` from other subsystems.
pub fn purge_tombstones(older_than_days: f64) {
    with(|s| s.purge_tombstones(older_than_days));
}

fn new_uuid() -> String {
    NSUUID::new().UUIDString().to_string()
}

/// `data.write(to:options:.atomic)`: temp file in the same directory + rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".pastory-tmp");
    let tmp = PathBuf::from(tmp_os);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl ClipStore {
    pub fn new(root: PathBuf) -> ClipStore {
        let mut s = ClipStore {
            items: Vec::new(),
            items_dir: root.join("items"),
            thumbs_dir: root.join("thumbs"),
            share_dir: root.join("share"),
            root,
            db: None,
            load_failed: false,
            last_save_failed: false,
            warned_save_failure: false,
            needs_full_save: false,
            buried: HashSet::new(),
            version: 0,
            thumb_cache: HashMap::new(),
            thumb_order: Vec::new(),
            thumb_loading: HashSet::new(),
            thumb_missing: HashSet::new(),
            thumb_tick: 0,
        };
        s.ensure_dirs();
        s.load();
        s
    }

    fn ensure_dirs(&self) {
        for d in [&self.root, &self.items_dir, &self.thumbs_dir, &self.share_dir] {
            if !d.exists() {
                let _ = std::fs::create_dir_all(d);
            }
        }
    }

    fn db_url(&self) -> PathBuf {
        self.root.join("pastory.sqlite")
    }

    // MARK: Persistence

    fn load(&mut self) {
        let path = self.db_url().to_string_lossy().into_owned();
        match ClipDB::open(&path).and_then(|d| d.load_all().map(|rows| (d, rows))) {
            Ok((d, rows)) => {
                self.buried = d
                    .tombstone_hashes()
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                self.db = Some(d);
                self.items = rows;
                self.load_failed = false;
            }
            Err(e) => {
                self.db = None;
                self.items = Vec::new();
                self.load_failed = true;
                self.last_save_failed = true;
                self.report_storage_failure(&e.to_string());
            }
        }
    }

    /// Whole table (bulk changes: clear, import, migration). `persist` /
    /// `unpersist` are the one-row versions.
    fn save(&mut self) -> bool {
        let items = self.items.clone();
        self.write_op(|db| db.save_all(&items))
    }

    fn persist(&mut self, item: &ClipItem) -> bool {
        self.write_op(|db| db.upsert(item))
    }

    fn unpersist(&mut self, id: &str) -> bool {
        self.write_op(|db| db.delete(id))
    }

    fn write_op(&mut self, op: impl FnOnce(&ClipDB) -> rusqlite::Result<()>) -> bool {
        // Never write over a table we could not read.
        if self.load_failed || self.db.is_none() {
            self.last_save_failed = true;
            return false;
        }
        self.version += 1;
        let op = if self.needs_full_save {
            self.needs_full_save = false;
            let items = self.items.clone();
            Box::new(move |db: &ClipDB| db.save_all(&items)) as Box<dyn FnOnce(&ClipDB) -> rusqlite::Result<()>>
        } else {
            Box::new(op) as Box<dyn FnOnce(&ClipDB) -> rusqlite::Result<()>>
        };
        let result = op(self.db.as_ref().expect("db checked above"));
        match result {
            Ok(()) => {
                let recovered = self.last_save_failed;
                self.last_save_failed = false;
                if recovered {
                    retention::reschedule();
                }
                true
            }
            Err(e) => {
                self.last_save_failed = true;
                self.needs_full_save = true;
                self.report_storage_failure(&e.to_string());
                false
            }
        }
    }

    /// Shown once, and never inline: a modal inside the singleton's
    /// initializer can re-enter `shared` and deadlock (Swift comment verbatim).
    fn report_storage_failure(&mut self, msg: &str) {
        if self.warned_save_failure {
            return;
        }
        self.warned_save_failure = true;
        let path = self.root.display().to_string();
        let msg = msg.to_string();
        crate::app::delegate::dispatch_main_after(0.0, Box::new(move || {
            use objc2_app_kit::{NSAlert, NSApplication};
            let Some(mtm) = objc2::MainThreadMarker::new() else {
                return;
            };
            let app = NSApplication::sharedApplication(mtm);
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(&crate::app::localization::l(
                "Pastory 读写不了存储目录",
            )));
            let detail = format!(
                "{}\n\n{}\n\n{}",
                path,
                msg,
                crate::app::localization::l(
                    "在修好之前不会写入任何改动，也不会清理。检查磁盘空间后重新打开 Pastory。"
                )
            );
            alert.setInformativeText(&NSString::from_str(&detail));
            // Matches the Swift baseline (ClipStore.reportStorageFailure).
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
            alert.runModal();
        }));
    }

    // MARK: Files

    pub fn payload_url(&self, item: &ClipItem) -> PathBuf {
        self.items_dir.join(item.file_name())
    }

    pub fn rtf_url(&self, item: &ClipItem) -> PathBuf {
        self.items_dir.join(format!("{}.rtf", item.id))
    }

    /// Thumbnails are display-only, so they are lossy HEIC (≈ a quarter of a
    /// PNG). Older stores still have .png ones.
    pub fn thumb_url(&self, item: &ClipItem) -> PathBuf {
        let heic = self.thumbs_dir.join(format!("{}.heic", item.id));
        if heic.exists() {
            return heic;
        }
        let png = self.thumbs_dir.join(format!("{}.png", item.id));
        if png.exists() {
            png
        } else {
            heic
        }
    }

    fn thumb_data(t: &CGImage) -> Option<Vec<u8>> {
        screenshotter::heic_data(t, 0.8).or_else(|| screenshotter::png_data(t))
    }

    /// A human-named file for the pasteboard ("Rec 2026-09-11 16.10.23.mp4"),
    /// kept under share/<id>/ so it can always be found and removed with the
    /// item. Hard link when the volume allows, copy otherwise.
    pub fn share_url(&self, item: &ClipItem) -> PathBuf {
        let df = NSDateFormatter::new();
        df.setDateFormat(Some(&NSString::from_str("yyyy-MM-dd HH.mm.ss")));
        let date = NSDate::dateWithTimeIntervalSince1970(item.created_at);
        let stamp = df.stringFromDate(&date);
        // Only recordings are shared as files.
        let dir = self.share_dir.join(&item.id);
        let url = dir.join(format!("Rec {}.{}", stamp, item.ext));
        if !url.exists() {
            let _ = std::fs::create_dir_all(&dir);
            let payload = self.payload_url(item);
            if std::fs::hard_link(&payload, &url).is_err() {
                let _ = std::fs::copy(&payload, &url);
            }
        }
        url
    }

    // MARK: Insert

    pub fn insert_text(&mut self, text: &str, rtf: Option<&[u8]>, source: &Source) -> Option<ClipItem> {
        if text.trim().is_empty() {
            return None;
        }
        let data = text.as_bytes();
        if data.len() > MAX_TEXT_BYTES {
            return None;
        }
        let hash = stable_hash(data);
        if let Some(dup) = self.dedupe(hash, &[ClipKind::Text, ClipKind::Url]) {
            return Some(dup);
        }
        let kind = if ClipItem::is_url_text(text) {
            ClipKind::Url
        } else {
            ClipKind::Text
        };
        let item = ClipItem::new(
            new_uuid(),
            kind,
            now(),
            source.bundle_id.clone(),
            source.name.clone(),
            ClipItem::snippet_of_text(text),
            None,
            false,
            "txt".into(),
            rtf.is_some(),
            None,
            None,
            data.len() as i64,
            None,
            None,
            hash,
            None,
        );
        if write_atomic(&self.payload_url(&item), data).is_err() {
            return None;
        }
        if let Some(r) = rtf {
            if write_atomic(&self.rtf_url(&item), r).is_err() {
                return None;
            }
        }
        self.prepend(item.clone());
        Some(item)
    }

    pub fn insert_image(&mut self, png: &[u8], source: &Source, ocr_text: Option<String>) -> Option<ClipItem> {
        // The hash stays that of the PNG, so dedupe works on either storage
        // format (red line 9).
        let hash = stable_hash(png);
        if let Some(dup) = self.dedupe(hash, &[ClipKind::Image]) {
            return Some(dup);
        }
        let cg = screenshotter::image_from_png(png)?;
        let (stored, ext) = screenshotter::stored_image(png, &cg, Preferences::shared().stores_heic());
        let item = ClipItem::new(
            new_uuid(),
            ClipKind::Image,
            now(),
            source.bundle_id.clone(),
            source.name.clone(),
            format!("{}×{}", CGImage::width(Some(&cg)), CGImage::height(Some(&cg))),
            ocr_text,
            false,
            ext,
            false,
            Some(CGImage::width(Some(&cg)) as i64),
            Some(CGImage::height(Some(&cg)) as i64),
            stored.len() as i64,
            None,
            None,
            hash,
            None,
        );
        if write_atomic(&self.payload_url(&item), &stored).is_err() {
            return None;
        }
        if let Some(t) = screenshotter::thumbnail(&cg, 900) {
            if let Some(td) = Self::thumb_data(&t) {
                let _ = write_atomic(&self.thumb_url(&item), &td);
            }
        }
        let needs_ocr = item.ocr_text.is_none();
        let id = item.id.clone();
        self.prepend(item.clone());
        if needs_ocr {
            let _ = id;
            // M4: OCR.recognize(cg) on a utility thread, then
            // set_ocr(text, for: id, if_hash: hash) back on the store.
        }
        Some(item)
    }

    pub fn insert_files(&mut self, urls: &[PathBuf], source: &Source) -> Option<ClipItem> {
        let paths: Vec<String> = urls
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if paths.is_empty() {
            return None;
        }
        let data = json_encode_strings(&paths);
        let hash = stable_hash(&data);
        if let Some(dup) = self.dedupe(hash, &[ClipKind::Files]) {
            return Some(dup);
        }
        let names: Vec<String> = urls
            .iter()
            .map(|u| {
                u.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        let snippet = if names.len() <= 3 {
            names.join("\n")
        } else {
            format!(
                "{}\n{}",
                names[..3].join("\n"),
                crate::app::localization::l("… 共 %d 项")
                    .replacen("%d", &names.len().to_string(), 1)
            )
        };
        let item = ClipItem::new(
            new_uuid(),
            ClipKind::Files,
            now(),
            source.bundle_id.clone(),
            source.name.clone(),
            snippet,
            None,
            false,
            "json".into(),
            false,
            None,
            None,
            data.len() as i64,
            None,
            None,
            hash,
            None,
        );
        if write_atomic(&self.payload_url(&item), &data).is_err() {
            return None;
        }
        self.prepend(item.clone());
        Some(item)
    }

    /// Move a finished recording (mp4 / gif) into the store.
    pub fn insert_video(
        &mut self,
        temp_file: &Path,
        poster: Option<&CGImage>,
        duration: f64,
        source: &Source,
    ) -> Option<ClipItem> {
        let ext = temp_file
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let size = std::fs::metadata(temp_file).map(|m| m.len()).unwrap_or(0);
        let secs = duration.round() as i64;
        let dims = poster
            .map(|p| format!(" · {}×{}", CGImage::width(Some(p)), CGImage::height(Some(p))))
            .unwrap_or_default();
        let snippet = crate::app::localization::l("%@ · %d 秒%@")
            .replacen("%@", &ext.to_uppercase(), 1)
            .replacen("%d", &secs.to_string(), 1)
            .replacen("%@", &dims, 1);
        // Random hash: recordings never dedupe.
        let hash = random_i64();
        let item = ClipItem::new(
            new_uuid(),
            ClipKind::Video,
            now(),
            source.bundle_id.clone(),
            source.name.clone(),
            snippet,
            None,
            false,
            ext,
            false,
            poster.map(|p| CGImage::width(Some(p)) as i64),
            poster.map(|p| CGImage::height(Some(p)) as i64),
            size as i64,
            Some(duration),
            None,
            hash,
            None,
        );
        let dest = self.payload_url(&item);
        if std::fs::rename(temp_file, &dest).is_err() {
            std::fs::copy(temp_file, &dest).ok()?;
            let _ = std::fs::remove_file(temp_file);
        }
        if let Some(p) = poster {
            if let Some(t) = screenshotter::thumbnail(p, 900) {
                if let Some(td) = Self::thumb_data(&t) {
                    let _ = write_atomic(&self.thumb_url(&item), &td);
                }
            }
        }
        self.prepend(item.clone());
        Some(item)
    }

    /// Edited text: rewrite the payload, keep id / pin / position. If the item
    /// is gone meanwhile, keep the work as a new one.
    pub fn update_text(&mut self, id: &str, text: &str) {
        let Some(i) = self.items.iter().position(|it| it.id == id) else {
            self.insert_text(text, None, &Source::pastory());
            return;
        };
        let data = text.as_bytes();
        if write_atomic(&self.payload_url(&self.items[i]), data).is_err() {
            return;
        }
        let _ = std::fs::remove_file(self.rtf_url(&self.items[i]).clone());
        let item = &mut self.items[i];
        item.has_rtf = false;
        item.modified_at = now();
        item.snippet = ClipItem::snippet_of_text(text);
        item.byte_count = data.len() as i64;
        item.content_hash = stable_hash(data);
        item.kind = if ClipItem::is_url_text(text) {
            ClipKind::Url
        } else {
            ClipKind::Text
        };
        let item = item.clone();
        self.persist(&item);
    }

    /// Edited image: new PNG + thumbnail, OCR again in the background. If the
    /// item is gone meanwhile, keep the work as a new one.
    pub fn update_image(&mut self, id: &str, png: &[u8]) {
        let Some(cg) = screenshotter::image_from_png(png) else {
            return;
        };
        let Some(i) = self.items.iter().position(|it| it.id == id) else {
            self.insert_image(png, &Source::pastory(), None);
            return;
        };
        let (stored, ext) = screenshotter::stored_image(png, &cg, Preferences::shared().stores_heic());
        let old = self.items[i].clone();
        self.items[i].ext = ext;
        if write_atomic(&self.payload_url(&self.items[i]).clone(), &stored).is_err() {
            self.items[i].ext = old.ext;
            return;
        }
        if old.ext != self.items[i].ext {
            let _ = std::fs::remove_file(self.items_dir.join(old.file_name()));
        }
        if let Some(t) = screenshotter::thumbnail(&cg, 900) {
            if let Some(td) = Self::thumb_data(&t) {
                let _ = write_atomic(&self.thumb_url(&self.items[i]).clone(), &td);
            }
        }
        self.cache_remove(id);
        self.thumb_missing.remove(id);
        let hash = stable_hash(png);
        {
            let item = &mut self.items[i];
            item.modified_at = now();
            item.snippet = format!("{}×{}", CGImage::width(Some(&cg)), CGImage::height(Some(&cg)));
            item.pixel_width = Some(CGImage::width(Some(&cg)) as i64);
            item.pixel_height = Some(CGImage::height(Some(&cg)) as i64);
            item.byte_count = stored.len() as i64;
            item.content_hash = hash;
        }
        let item = self.items[i].clone();
        self.persist(&item);
        let _ = hash;
        // M4: background OCR → set_ocr(text, for: id, if_hash: hash).
    }

    /// Same payload anywhere in the history (same kind family) → bring that
    /// card to the front instead of making a twin; its title and pin come along.
    fn dedupe(&mut self, hash: i64, kinds: &[ClipKind]) -> Option<ClipItem> {
        let hit = self
            .items
            .iter()
            .find(|it| it.content_hash == hash && kinds.contains(&it.kind))?;
        let id = hit.id.clone();
        self.bump(&id);
        self.items.first().cloned()
    }

    fn prepend(&mut self, item: ClipItem) {
        self.items.insert(0, item.clone());
        self.persist(&item);
        retention::item_added();
    }

    // MARK: Import

    /// Hashing, decoding, HEIC re-encoding and thumbnails for a whole batch —
    /// runnable off the main thread (Swift `nonisolated static prepareImport`).
    pub fn prepare_import(entries: Vec<ImportEntry>, store_heic: bool) -> Vec<PreparedEntry> {
        let mut out = Vec::new();
        for e in entries {
            match e.payload {
                ImportPayload::Text(text) => {
                    if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
                        continue;
                    }
                    let snippet = ClipItem::snippet_of_text(&text);
                    let kind = if ClipItem::is_url_text(&text) {
                        ClipKind::Url
                    } else {
                        ClipKind::Text
                    };
                    let payload = text.into_bytes();
                    out.push(PreparedEntry {
                        kind,
                        hash: stable_hash(&payload),
                        payload,
                        ext: "txt".into(),
                        thumb: None,
                        snippet,
                        pixel_width: None,
                        pixel_height: None,
                        created_at: e.created_at,
                        pinned: e.pinned,
                        title: e.title,
                    });
                }
                ImportPayload::Image(png) => {
                    let Some(cg) = screenshotter::image_from_png(&png) else {
                        continue;
                    };
                    let heic = if store_heic {
                        screenshotter::heic_data(&cg, 0.9)
                    } else {
                        None
                    };
                    let thumb = screenshotter::thumbnail(&cg, 900)
                        .and_then(|t| Self::thumb_data(&t));
                    let hash = stable_hash(&png);
                    out.push(PreparedEntry {
                        kind: ClipKind::Image,
                        payload: heic.clone().unwrap_or(png),
                        ext: if heic.is_some() { "heic".into() } else { "png".into() },
                        thumb,
                        snippet: format!("{}×{}", CGImage::width(Some(&cg)), CGImage::height(Some(&cg))),
                        pixel_width: Some(CGImage::width(Some(&cg)) as i64),
                        pixel_height: Some(CGImage::height(Some(&cg)) as i64),
                        hash,
                        created_at: e.created_at,
                        pinned: e.pinned,
                        title: e.title,
                    });
                }
            }
        }
        out
    }

    /// Bulk insert from another store. Skips anything whose payload is already
    /// here (or repeated in the batch) and saves once. Imported history always
    /// sorts behind everything Pastory captured itself: the batch keeps its own
    /// internal order, shifted back so its newest entry is older than our
    /// oldest item. Returns how many were added.
    pub fn import_entries(&mut self, entries: Vec<ImportEntry>) -> usize {
        let prepared = Self::prepare_import(entries, Preferences::shared().stores_heic());
        self.commit_import(prepared)
    }

    /// Main-actor half of an import: write files, build items, save once.
    pub fn commit_import(&mut self, prepared: Vec<PreparedEntry>) -> usize {
        let mut entries = prepared;
        entries.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let oldest_own = self.items.iter().map(|i| i.created_at).reduce(f64::min);
        if let (Some(oldest_own), Some(newest_import)) =
            (oldest_own, entries.first().map(|e| e.created_at))
        {
            let shift = newest_import - oldest_own + 1.0;
            if shift > 0.0 {
                for e in &mut entries {
                    e.created_at -= shift;
                }
            }
        }
        // What you deleted here stays deleted.
        let mut seen: HashSet<i64> = self.items.iter().map(|i| i.content_hash).collect();
        seen.extend(self.buried.iter().copied());
        let mut added: Vec<ClipItem> = Vec::new();
        for e in entries {
            if !seen.insert(e.hash) {
                continue;
            }
            let item = ClipItem::new(
                new_uuid(),
                e.kind,
                e.created_at,
                None,
                Some(IMPORT_SOURCE_NAME.into()),
                e.snippet,
                None,
                e.pinned,
                e.ext,
                false,
                e.pixel_width,
                e.pixel_height,
                e.payload.len() as i64,
                None,
                e.title,
                e.hash,
                None,
            );
            if write_atomic(&self.payload_url(&item), &e.payload).is_err() {
                continue;
            }
            if let Some(t) = &e.thumb {
                let _ = write_atomic(&self.thumb_url(&item), t);
            }
            added.push(item);
        }
        if added.is_empty() {
            return 0;
        }
        let mut merged = self.items.clone();
        merged.extend(added.iter().cloned());
        merged.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.items = merged;
        if !self.save() {
            // Disk said no: take the files back out so nothing half-exists.
            let ids: HashSet<&str> = added.iter().map(|a| a.id.as_str()).collect();
            self.items.retain(|it| !ids.contains(it.id.as_str()));
            for a in &added {
                let _ = std::fs::remove_file(self.payload_url(a));
                let _ = std::fs::remove_file(self.thumb_url(a));
            }
            return 0;
        }
        retention::reschedule();
        added.len()
    }

    // MARK: Mutations

    pub fn bump(&mut self, id: &str) {
        let i = match self.items.iter().position(|it| it.id == id) {
            Some(i) if i != 0 => i,
            _ => return,
        };
        let mut item = self.items.remove(i);
        let t = now();
        item.created_at = t;
        item.modified_at = t;
        self.persist(&item);
        self.items.insert(0, item);
    }

    /// `welcome: false` = a pin the app did on the user's behalf (seeding,
    /// desktop notes); it must not tick the welcome checklist (M2/M6).
    pub fn toggle_pin(&mut self, id: &str, welcome: bool) {
        let Some(i) = self.items.iter().position(|it| it.id == id) else {
            return;
        };
        self.items[i].pinned = !self.items[i].pinned;
        self.items[i].modified_at = now();
        let item = self.items[i].clone();
        // UI must not claim a pin the disk does not have.
        if !self.persist(&item) {
            self.items[i].pinned = !self.items[i].pinned;
            return;
        }
        if !self.items[i].pinned {
            // An un-pinned old item may be the next to expire.
            retention::reschedule();
        } else if welcome {
            // M2: ShelfPanelController…noteWelcomeTried("pin")
        }
    }

    /// Self-test only.
    pub fn debug_set_date(&mut self, date: f64, id: &str) {
        let Some(i) = self.items.iter().position(|it| it.id == id) else {
            return;
        };
        self.items[i].created_at = date;
        let item = self.items[i].clone();
        self.persist(&item);
    }

    pub fn set_title(&mut self, title: Option<&str>, id: &str) {
        let Some(i) = self.items.iter().position(|it| it.id == id) else {
            return;
        };
        let t = title.map(|t| t.trim()).unwrap_or("");
        self.items[i].title = if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        };
        self.items[i].modified_at = now();
        let item = self.items[i].clone();
        self.persist(&item);
    }

    /// Only applies if the item still holds the image the OCR ran on.
    pub fn set_ocr(&mut self, text: Option<String>, id: &str, if_hash: i64) {
        let Some(i) = self
            .items
            .iter()
            .position(|it| it.id == id && it.content_hash == if_hash)
        else {
            return;
        };
        self.items[i].ocr_text = text;
        self.items[i].modified_at = now();
        let item = self.items[i].clone();
        self.persist(&item);
    }

    pub fn remove(&mut self, id: &str) {
        let Some(i) = self.items.iter().position(|it| it.id == id) else {
            return;
        };
        let item = self.items.remove(i);
        // Index first; files only once the index agrees.
        if !self.unpersist(&item.id) {
            self.items.insert(i, item);
            return;
        }
        self.bury(std::slice::from_ref(&item));
        self.delete_files(&item);
        if let Some(d) = &self.db {
            d.checkpoint();
        }
        crate::shelf::desktop_notes::items_gone(std::slice::from_ref(&item.id));
    }

    pub fn remove_ids(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        let gone: Vec<ClipItem> = self
            .items
            .iter()
            .filter(|it| set.contains(it.id.as_str()))
            .cloned()
            .collect();
        if gone.is_empty() {
            return;
        }
        let before = self.items.clone();
        self.items.retain(|it| !set.contains(it.id.as_str()));
        if !self.save() {
            self.items = before;
            return;
        }
        self.bury(&gone);
        for g in &gone {
            self.delete_files(g);
        }
        if let Some(d) = &self.db {
            d.checkpoint();
        }
        crate::shelf::desktop_notes::items_gone(ids);
    }

    /// Deleted is deleted: remember the id and content hash so an import (or,
    /// one day, a sync) cannot resurrect it.
    fn bury(&mut self, gone: &[ClipItem]) {
        let Some(d) = &self.db else { return };
        if gone.is_empty() {
            return;
        }
        let _ = d.add_tombstones(gone, db::now_ts());
        for g in gone {
            self.buried.insert(g.content_hash);
        }
    }

    /// Tombstones older than this are forgotten; a fresh copy of the same
    /// content is a new item anyway.
    pub fn purge_tombstones(&mut self, older_than_days: f64) {
        let Some(d) = &self.db else { return };
        let cutoff = db::now_ts() - older_than_days * 86400.0;
        let _ = d.purge_tombstones_before(cutoff);
        if let Ok(hashes) = d.tombstone_hashes() {
            self.buried = hashes.into_iter().collect();
        }
    }

    fn delete_files(&mut self, item: &ClipItem) {
        let _ = std::fs::remove_file(self.payload_url(item));
        let _ = std::fs::remove_file(self.rtf_url(item));
        let _ = std::fs::remove_file(self.thumb_url(item));
        let _ = std::fs::remove_dir_all(self.share_dir.join(&item.id));
        self.cache_remove(&item.id);
        self.thumb_missing.remove(&item.id);
    }

    // MARK: Read

    pub fn text(&self, item: &ClipItem) -> Option<String> {
        if item.kind != ClipKind::Text && item.kind != ClipKind::Url {
            return None;
        }
        std::fs::read_to_string(self.payload_url(item)).ok()
    }

    pub fn rtf(&self, item: &ClipItem) -> Option<Vec<u8>> {
        if item.has_rtf {
            std::fs::read(self.rtf_url(item)).ok()
        } else {
            None
        }
    }

    /// PNG bytes of an image item, whatever is on disk (HEIC-stored items are
    /// decoded and re-wrapped losslessly, same pixels, same color space).
    pub fn png(&self, item: &ClipItem) -> Option<Vec<u8>> {
        if item.kind != ClipKind::Image {
            return None;
        }
        let data = std::fs::read(self.payload_url(item)).ok()?;
        if item.ext == "png" {
            return Some(data);
        }
        let cg = screenshotter::decode_any(&data)?;
        screenshotter::png_data(&cg)
    }

    pub fn file_urls(&self, item: &ClipItem) -> Vec<PathBuf> {
        if item.kind != ClipKind::Files {
            return Vec::new();
        }
        let Ok(data) = std::fs::read(self.payload_url(item)) else {
            return Vec::new();
        };
        json_decode_strings(&data)
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .collect()
    }

    /// Cached thumbnail, or nil while it decodes in the background (the card
    /// shows a placeholder for a frame or two).
    pub fn thumbnail(&mut self, item: &ClipItem) -> Option<CFRetained<CGImage>> {
        let _ = self.thumb_tick;
        if let Some(ptr) = self.thumb_cache.get(&item.id) {
            // SAFETY: retained on insert and alive until `cache_remove`; this
            // adds another retain for the caller.
            return Some(unsafe {
                CFRetained::retain(std::ptr::NonNull::new_unchecked(*ptr as *mut CGImage))
            });
        }
        if item.kind != ClipKind::Image && item.kind != ClipKind::Video {
            return None;
        }
        self.warm_thumbnail(item);
        None
    }

    /// Self-tests wait on this before snapshotting the shelf.
    pub fn is_thumbnail_cached(&self, id: &str) -> bool {
        self.thumb_cache.contains_key(id)
    }

    pub fn thumbnail_missing(&self, id: &str) -> bool {
        self.thumb_missing.contains(id)
    }

    fn cache_remove(&mut self, id: &str) {
        if let Some(ptr) = self.thumb_cache.remove(id) {
            // SAFETY: the pointer was CFRetained::into_raw on insert and leaves
            // the cache here exactly once.
            let _ = unsafe {
                CFRetained::from_raw(std::ptr::NonNull::new_unchecked(ptr as *mut CGImage))
            };
        }
    }

    /// Start decoding an item's thumbnail off the main thread; no-op when
    /// cached or already in flight.
    pub fn warm_thumbnail(&mut self, item: &ClipItem) {
        if item.kind != ClipKind::Image && item.kind != ClipKind::Video {
            return;
        }
        if self.thumb_cache.contains_key(&item.id)
            || self.thumb_loading.contains(&item.id)
            || self.thumb_missing.contains(&item.id)
        {
            return;
        }
        self.thumb_loading.insert(item.id.clone());
        let url = self.thumb_url(item);
        let id = item.id.clone();
        std::thread::spawn(move || {
            let image = screenshotter::image_from_file(&url);
            with(|s| {
                s.thumb_loading.remove(&id);
                let Some(image) = image else {
                    s.thumb_missing.insert(id);
                    s.thumb_tick += 1;
                    return;
                };
                // ~2 MB decoded each: drop the oldest third, not everything on screen.
                if s.thumb_cache.len() > 100 {
                    let n = s.thumb_order.len().min(34);
                    let old: Vec<String> = s.thumb_order.drain(..n).collect();
                    for o in old {
                        s.cache_remove(&o);
                    }
                }
                let ptr = CFRetained::into_raw(image).as_ptr() as usize;
                s.thumb_cache.insert(id.clone(), ptr);
                s.thumb_order.push(id);
                s.thumb_tick += 1;
            });
        });
    }

    // MARK: Actions

    /// Put the item back on the pasteboard (the monitor then bumps it).
    pub fn copy_to_pasteboard(&mut self, item: &ClipItem) {
        match item.kind {
            ClipKind::Text | ClipKind::Url => {
                let Some(s) = self.text(item) else { return };
                let rtf = self.rtf(item);
                pasteboard_writer::write_text(&s, rtf.as_deref(), &item.id);
            }
            ClipKind::Image => {
                let Some(png) = self.png(item) else { return };
                pasteboard_writer::write_image(&png, &item.id);
            }
            ClipKind::Files => {
                pasteboard_writer::write_files(&self.file_urls(item), &item.id);
            }
            ClipKind::Video => {
                if item.ext == "gif" {
                    if let Ok(data) = std::fs::read(self.payload_url(item)) {
                        pasteboard_writer::write_gif(&data, &item.id);
                    }
                } else {
                    pasteboard_writer::write_files(&[self.share_url(item)], &item.id);
                }
            }
        }
        self.bump(&item.id);
    }
}

/// SplitMix64-based unique i64 for recording hashes: recordings never dedupe,
/// the hash only has to not collide with anything else in the store.
fn random_i64() -> i64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0x2026_0912_5EED_0001);
    let mut z = STATE.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) as i64
}

/// `[String]` JSON — Files payloads are read by both the Swift and the Rust
/// build of the app, so encode/decode has to be exact in semantics (byte-level
/// formatting may differ; both decoders accept both encoders).
pub fn json_encode_strings(values: &[String]) -> Vec<u8> {
    let mut s = String::with_capacity(values.iter().map(|v| v.len() + 8).sum::<usize>() + 2);
    s.push('[');
    for (i, v) in values.iter().enumerate() {
        if i != 0 {
            s.push(',');
        }
        s.push('"');
        for c in v.chars() {
            match c {
                '"' => s.push_str("\\\""),
                '\\' => s.push_str("\\\\"),
                '\n' => s.push_str("\\n"),
                '\r' => s.push_str("\\r"),
                '\t' => s.push_str("\\t"),
                c if (c as u32) < 0x20 => s.push_str(&format!("\\u{:04x}", c as u32)),
                c => s.push(c),
            }
        }
        s.push('"');
    }
    s.push(']');
    s.into_bytes()
}

/// Tolerant counterpart of `json_encode_strings`: full JSON string escapes
/// including \uXXXX surrogate pairs.
pub fn json_decode_strings(data: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(data).ok()?;
    let mut chars = text.chars().peekable();
    fn skip_ws(chars: &mut std::iter::Peekable<std::str::Chars>) {
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
    }
    fn parse_string(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
        if chars.next() != Some('"') {
            return None;
        }
        let mut out = String::new();
        loop {
            match chars.next()? {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex4 = |c: &mut std::iter::Peekable<std::str::Chars>| -> Option<u32> {
                            let mut v = 0u32;
                            for _ in 0..4 {
                                v = v * 16 + c.next()?.to_digit(16)?;
                            }
                            Some(v)
                        };
                        let hi = hex4(chars)?;
                        if (0xD800..0xDC00).contains(&hi) {
                            // High surrogate: a low one must follow.
                            if chars.next() != Some('\\') || chars.next() != Some('u') {
                                return None;
                            }
                            let lo = hex4(chars)?;
                            if !(0xDC00..0xE000).contains(&lo) {
                                return None;
                            }
                            let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                            out.push(char::from_u32(cp)?);
                        } else {
                            out.push(char::from_u32(hi)?);
                        }
                    }
                    _ => return None,
                },
                c => out.push(c),
            }
        }
    }
    let mut out = Vec::new();
    skip_ws(&mut chars);
    if chars.next() != Some('[') {
        return None;
    }
    skip_ws(&mut chars);
    if chars.peek() == Some(&']') {
        chars.next();
        return Some(out);
    }
    loop {
        skip_ws(&mut chars);
        out.push(parse_string(&mut chars)?);
        skip_ws(&mut chars);
        match chars.next()? {
            ',' => continue,
            ']' => return Some(out),
            _ => return None,
        }
    }
}

//! Port of `Clipboard/ClipSearchIndex.swift` — the disposable full-text
//! cache. File I/O, normalization and scans run off the UI thread; the
//! clipboard database and payload files remain the source of truth.
//!
//! The Swift type is an `actor`; here the entries sit behind a Mutex (the
//! store singleton uses the same arrangement). Searches run on background
//! threads spawned by the shelf model.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2_foundation::{NSNotFound, NSString, NSStringCompareOptions};

use crate::clipboard::item::{ClipItem, ClipKind};

/// A stale search was stopped (Swift `CancellationError`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// What an entry was built from; equal fingerprints reuse the cached text
/// (Swift `Document`, Equatable). Pin and timestamps are deliberately absent:
/// focusing, pinning and copying keep the full-text cache.
#[derive(Clone, PartialEq, Eq)]
struct Document {
    kind: ClipKind,
    hash: i64,
    ext: String,
    snippet: String,
    ocr: String,
    source: String,
    title: String,
}

impl Document {
    fn new(item: &ClipItem) -> Document {
        Document {
            kind: item.kind,
            hash: item.content_hash,
            ext: item.ext.clone(),
            snippet: item.snippet.clone(),
            ocr: item.ocr_text.clone().unwrap_or_default(),
            source: item.source_app_name.clone().unwrap_or_default(),
            title: item.title.clone().unwrap_or_default(),
        }
    }
}

struct Entry {
    document: Document,
    /// Lower-cased, precomposed; the only copy kept (Swift `literalText`).
    literal_text: Retained<NSString>,
}

struct IndexInner {
    entries: HashMap<String, Entry>,
    cached_directory: Option<PathBuf>,
    /// Used by the search self-test to verify that focusing, pinning and
    /// copying do not reread payloads.
    payload_read_count: u64,
}

/// Disposable memory cache; the database and payload files are the truth.
pub struct ClipSearchIndex {
    inner: Mutex<IndexInner>,
}

// SAFETY: NSString is immutable (toll-free bridged to CFString, whose
// immutable instances are documented thread-safe), and every access to the
// entries — lookup, insert, release — is serialized by the Mutex. This is
// the same guarantee the Swift actor provides.
unsafe impl Send for ClipSearchIndex {}
unsafe impl Sync for ClipSearchIndex {}

impl Default for ClipSearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipSearchIndex {
    pub fn new() -> ClipSearchIndex {
        ClipSearchIndex {
            inner: Mutex::new(IndexInner {
                entries: HashMap::new(),
                cached_directory: None,
                payload_read_count: 0,
            }),
        }
    }

    /// `normalizedQuery`: trim Unicode whitespace (Foundation `.whitespaces`
    /// is the White_Space property — the set `str::trim` uses), then
    /// lowercase + precomposed. Both steps go through NSString itself, so the
    /// Rust and Swift builds share one normalization engine.
    pub fn normalized_query(query: &str) -> String {
        NSString::from_str(query.trim())
            .lowercaseString()
            .precomposedStringWithCanonicalMapping()
            .to_string()
    }

    pub fn payload_read_count(&self) -> u64 {
        self.inner
            .lock()
            .expect("search index poisoned")
            .payload_read_count
    }

    /// `search(_:items:directory:)` with `task.isCancelled` checks at the
    /// same points as Swift's `Task.checkCancellation()` (entry, per item,
    /// after the payload read, at the end). An empty normalized query warms
    /// the cache without scanning for matches; cancellation preserves
    /// completed entries so a new query can reuse that work.
    pub fn search(
        &self,
        query: &str,
        items: &[ClipItem],
        directory: &Path,
        cancel: &AtomicBool,
    ) -> Result<HashSet<String>, Cancelled> {
        fn check(cancel: &AtomicBool) -> Result<(), Cancelled> {
            if cancel.load(Ordering::SeqCst) {
                Err(Cancelled)
            } else {
                Ok(())
            }
        }
        check(cancel)?;
        let query = Self::normalized_query(query);
        let inner = &mut *self.inner.lock().expect("search index poisoned");
        if inner.cached_directory.as_deref() != Some(directory) {
            inner.entries.clear();
            inner.cached_directory = Some(directory.to_path_buf());
        }
        let live: HashSet<&str> = items.iter().map(|i| i.id.as_str()).collect();
        inner.entries.retain(|id, _| live.contains(id.as_str()));
        let mut matches = HashSet::new();
        let needle = NSString::from_str(&query);
        for item in items {
            check(cancel)?;
            let document = Document::new(item);
            let scratch: Entry; // non-cached fallback, dropped at iteration end
            let entry: &Entry = if matches!(
                inner.entries.get(&item.id),
                Some(cached) if cached.document == document
            ) {
                inner.entries.get(&item.id).expect("hit checked above")
            } else {
                let body: String;
                let mut cacheable = true;
                if item.kind == ClipKind::Text || item.kind == ClipKind::Url {
                    inner.payload_read_count += 1;
                    match std::fs::read_to_string(directory.join(item.file_name())) {
                        Ok(text) => body = text,
                        Err(_) => {
                            body = item.snippet.clone();
                            cacheable = false; // retry a missing/unreadable payload on the next search
                        }
                    }
                } else {
                    body = item.snippet.clone();
                }
                check(cancel)?;
                // One pasted log file must not sit in memory for the life of
                // the app: the first 200k characters are searchable. Swift
                // `prefix(200_000)` counts grapheme clusters; `chars()`
                // counts scalars — the cap is a memory guard (200000 is far
                // past any card), so the scalar variant is equivalent here.
                let head: String = body.chars().take(200_000).collect();
                let joined = [head, document.ocr.clone(), document.source.clone(), document.title.clone()]
                    .join("\n");
                let text = NSString::from_str(&joined)
                    .lowercaseString()
                    .precomposedStringWithCanonicalMapping();
                let built = Entry {
                    document,
                    literal_text: text,
                };
                if cacheable {
                    inner.entries.insert(item.id.clone(), built);
                    inner.entries.get(&item.id).expect("inserted above")
                } else {
                    scratch = built;
                    &scratch
                }
            };
            if query.is_empty() {
                continue;
            }
            // Literal match on precomposed text. Deliberately not
            // grapheme-strict: 👍 should find 👍🏽 and 👩 should find 👨‍👩‍👧,
            // which a cluster-by-cluster comparison refuses. It is also the
            // fast path.
            let found = entry
                .literal_text
                .rangeOfString_options(&needle, NSStringCompareOptions::LiteralSearch);
            if found.location != NSNotFound as usize {
                matches.insert(item.id.clone());
            }
        }
        check(cancel)?;
        Ok(matches)
    }
}

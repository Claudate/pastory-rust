//! Port of `Clipboard/ClipItem.swift`.
//!
//! One history entry: a kind, the payload on disk (`items/<id>.<ext>`), and
//! the row that indexes it in `pastory.sqlite`.

use objc2_foundation::{NSCalendar, NSDate};

/// Payload kinds, stored as these raw strings in the DB (`kind` column).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ClipKind {
    Text,
    Url,
    Image,
    Files,
    Video,
}

impl ClipKind {
    pub fn raw(&self) -> &'static str {
        match self {
            ClipKind::Text => "text",
            ClipKind::Url => "url",
            ClipKind::Image => "image",
            ClipKind::Files => "files",
            ClipKind::Video => "video",
        }
    }

    pub fn from_raw(s: &str) -> Option<ClipKind> {
        match s {
            "text" => Some(ClipKind::Text),
            "url" => Some(ClipKind::Url),
            "image" => Some(ClipKind::Image),
            "files" => Some(ClipKind::Files),
            "video" => Some(ClipKind::Video),
            _ => None,
        }
    }

    /// Card corner label (「文本」…); the Chinese literal is the storage form,
    /// `.l` maps it for English UI.
    pub fn label(&self) -> String {
        crate::app::localization::l(match self {
            ClipKind::Text => "文本",
            ClipKind::Url => "链接",
            ClipKind::Image => "图片",
            ClipKind::Files => "文件",
            ClipKind::Video => "录屏",
        })
    }
}

/// A history entry. Field order and meaning mirror `ClipItem` field for field.
/// (`Equatable` in Swift.)
#[derive(Clone, Debug, PartialEq)]
pub struct ClipItem {
    pub id: String,
    pub kind: ClipKind,
    /// Seconds since 1970 (Swift `Date.timeIntervalSince1970`).
    pub created_at: f64,
    pub source_bundle_id: Option<String>,
    pub source_app_name: Option<String>,
    /// Card preview: first lines of text, file names, or "1280×720".
    pub snippet: String,
    pub ocr_text: Option<String>,
    pub pinned: bool,
    /// Extension of `items/<id>.<ext>`: txt / png / json / mp4 / gif
    pub ext: String,
    pub has_rtf: bool,
    pub pixel_width: Option<i64>,
    pub pixel_height: Option<i64>,
    pub byte_count: i64,
    /// Seconds, for recordings.
    pub duration: Option<f64>,
    /// User-given name ("翻译 prompt"), shown above the content and searchable.
    pub title: Option<String>,
    /// Hash of the payload, for de-duplicating back-to-back copies.
    pub content_hash: i64,
    /// Last change to any field (pin, title, OCR, edit, bump). Sync merges on
    /// this; equals created_at for old rows.
    pub modified_at: f64,
}

impl ClipItem {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        kind: ClipKind,
        created_at: f64,
        source_bundle_id: Option<String>,
        source_app_name: Option<String>,
        snippet: String,
        ocr_text: Option<String>,
        pinned: bool,
        ext: String,
        has_rtf: bool,
        pixel_width: Option<i64>,
        pixel_height: Option<i64>,
        byte_count: i64,
        duration: Option<f64>,
        title: Option<String>,
        content_hash: i64,
        modified_at: Option<f64>,
    ) -> ClipItem {
        ClipItem {
            id,
            kind,
            created_at,
            source_bundle_id,
            source_app_name,
            snippet,
            ocr_text,
            pinned,
            ext,
            has_rtf,
            pixel_width,
            pixel_height,
            byte_count,
            duration,
            title,
            content_hash,
            modified_at: modified_at.unwrap_or(created_at),
        }
    }

    pub fn file_name(&self) -> String {
        format!("{}.{}", self.id, self.ext)
    }

    /// One http(s) link on its own line, nothing else.
    pub fn is_url_text(text: &str) -> bool {
        let t = text.trim();
        if t.contains('\n') {
            return false;
        }
        // Swift `URL(string:)` accepts a very wide set; what matters here is
        // that an http/https scheme prefixes something non-empty and the
        // whole trimmed string has no whitespace.
        if t.is_empty() || t.contains(char::is_whitespace) {
            return false;
        }
        match t.split_once("://") {
            Some((scheme, rest)) => {
                (scheme == "http" || scheme == "https") && !rest.is_empty()
            }
            None => false,
        }
    }

    /// Card preview of a text payload: trimmed, first 400 chars.
    pub fn snippet_of_text(s: &str) -> String {
        s.trim().chars().take(400).collect()
    }
}

/// `now` as Swift `Date()` — seconds since 1970.
pub fn now() -> f64 {
    NSDate::now().timeIntervalSince1970()
}

/// `Calendar.current.startOfDay(for:)` in seconds since 1970.
pub fn start_of_day(ts: f64) -> f64 {
    let date = NSDate::dateWithTimeIntervalSince1970(ts);
    let cal = NSCalendar::currentCalendar();
    cal.startOfDayForDate(&date).timeIntervalSince1970()
}

//! Form-agnostic item actions: everything that can be done to one clip.
//!
//! The 面板 form and the 晾衣绳 form each build their own context menu, but the
//! *actions* behind the rows are the same product behaviour and must not drift
//! apart. This module owns them once; a menu only names a row and calls in here.
//! Before it existed the 晾衣绳 menu offered copy / copy-and-close / reveal and
//! nothing else, because every other action lived inside the panel's private
//! menu handler.
//!
//! Two rules, both learned the hard way in this codebase:
//!
//! * Never hold a model borrow across anything that reads the model again (an
//!   editor, Quick Look, the pin-delete confirm, the save panel): a second
//!   `borrow_mut` on the same `RefCell` aborts the process. Each function below
//!   drops the borrow before it calls out.
//! * A row is offered only when it applies (a title can only be removed when
//!   there is one; the OCR row only when there is recognised text), so the menu
//!   and the action agree.

use objc2_foundation::{NSArray, NSString};

use crate::clipboard::item::{ClipItem, ClipKind};
use crate::shelf::panel::with_model_mut;

/// Copy the clip and stay (both forms keep the shelf up).
pub fn copy(item: &ClipItem) {
    with_model_mut(|m| m.copy(item));
}

/// Copy, then close the shelf; `paste` also hands the ⌘V back to the app the
/// user was in (the double-click path).
pub fn copy_and_close(item: &ClipItem, paste: bool) {
    with_model_mut(|m| m.copy_and_close(item, paste));
    crate::shelf::panel::finish_pending_close();
}

/// Write or clear the clip's title. The *UI* that asks for it is the form's own
/// (the panel edits inline on the card; the 晾衣绳 uses a small modal), so this
/// is only the data half, shared by both.
pub fn set_title(item: &ClipItem, title: Option<&str>) {
    with_model_mut(|m| m.with_store_mut(|s| s.set_title(title, &item.id)));
}

/// Whether the clip has a title to remove.
pub fn has_title(item: &ClipItem) -> bool {
    item.title.is_some()
}

/// The clip's title, if any.
pub fn title(item: &ClipItem) -> Option<String> {
    item.title.clone()
}

/// Open the clip in its editor (text / image) or Quick Look (anything else).
/// The editor and the preview read the model themselves, so the select happens
/// in its own borrow and is released first.
pub fn edit(item: &ClipItem) {
    with_model_mut(|m| m.select_for_action(item));
    open_editor_or_preview(item);
}

/// `edit` and `preview` are the same dialog in this app; kept as two names so a
/// menu can spell either row.
pub fn preview(item: &ClipItem) {
    edit(item);
}

fn open_editor_or_preview(item: &ClipItem) {
    match item.kind {
        ClipKind::Text | ClipKind::Url => crate::shelf::text_editor::TextEditorWindow::open(item),
        ClipKind::Image => crate::shelf::image_editor::ImageEditorWindow::open(item),
        _ => crate::shelf::panel::toggle_quick_look(),
    }
}

/// Flip the pin. Pinned clips survive retention; the store owns the rule.
pub fn toggle_pin(item: &ClipItem) {
    with_model_mut(|m| m.with_store_mut(|s| s.toggle_pin(&item.id, true)));
}

/// Save the clip to a file the user picks (modal: run with no borrow held).
pub fn save(item: &ClipItem) {
    crate::shelf::exporter::export(item);
}

/// Delete the clip. `panel::delete_item` raises the pinned-delete confirm
/// outside the borrow.
pub fn delete(item: &ClipItem) {
    crate::shelf::panel::delete_item(item);
}

/// Put the clip on the desktop as a note (or raise it if it is already there).
pub fn stick(item: &ClipItem) {
    if crate::shelf::desktop_notes::is_on_desktop(&item.id) {
        crate::shelf::desktop_notes::bring_to_front(&item.id);
    } else {
        crate::shelf::desktop_notes::place(&item.id, None);
    }
}

/// Take the clip's desktop note away.
pub fn unstick(item: &ClipItem) {
    crate::shelf::desktop_notes::close(&item.id);
}

/// Select the file-backed clip(s) in the Finder.
pub fn show_in_finder(item: &ClipItem) {
    let urls = with_model_mut(|m| m.with_store(|s| s.file_urls(item)));
    if urls.is_empty() {
        return;
    }
    let ns: Vec<objc2::rc::Retained<objc2_foundation::NSURL>> = urls
        .iter()
        .map(|p| {
            objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()))
        })
        .collect();
    let refs: Vec<&objc2_foundation::NSURL> = ns.iter().map(|u| &**u).collect();
    let arr = NSArray::from_slice(&refs);
    objc2_app_kit::NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&arr);
}

/// Open the clip's payload with the default app (video / file).
pub fn open(item: &ClipItem) {
    let path = with_model_mut(|m| m.with_store(|s| s.payload_url(item)));
    if !path.exists() {
        return;
    }
    let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
}

/// Copy the image's recognised text and put it on the clipboard, then take the
/// shelf down (the panel's OCR row behaviour).
pub fn copy_ocr(item: &ClipItem) {
    let Some(text) = item.ocr_text.clone().filter(|t| !t.is_empty()) else {
        return;
    };
    let source = crate::clipboard::store::Source::pastory();
    let inserted = crate::clipboard::store::with(|s| s.insert_text(&text, None, &source));
    crate::capture::pasteboard_writer::write_text(
        &text,
        None,
        inserted.as_ref().map(|i| i.id.as_str()).unwrap_or(""),
    );
    crate::shelf::panel::hide();
}

// MARK: Row applicability (the menu asks these; the action agrees)

/// Whether the clip is a picture with recognised text.
pub fn has_ocr(item: &ClipItem) -> bool {
    item.kind == ClipKind::Image
        && item.ocr_text.as_deref().map(|t| !t.is_empty()).unwrap_or(false)
}

/// Whether the clip is backed by a file (image / video / file list).
pub fn is_file_backed(item: &ClipItem) -> bool {
    matches!(item.kind, ClipKind::Image | ClipKind::Video | ClipKind::Files)
}

/// Whether the clip can be opened by an editor / preview.
pub fn can_edit(item: &ClipItem) -> bool {
    matches!(item.kind, ClipKind::Text | ClipKind::Url | ClipKind::Image)
}

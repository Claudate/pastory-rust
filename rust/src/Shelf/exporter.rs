//! Port of `Shelf/Exporter.swift` — "保存到本地": always ask where, remember
//! the folder for next time (`Exporter.export`).
//!
//! Saved files are always what they would have been as a paste: image → PNG
//! (HEIC decoded), video → its own ext, text/url → .txt, file lists copied to
//! a chosen folder. The write only lands through `replaceItemAt`, like Swift.


use objc2::{MainThreadMarker};
use objc2_app_kit::{NSOpenPanel, NSSavePanel};
use objc2_foundation::{NSString, NSURL};

use crate::app::localization::l;
use crate::app::preferences::Preferences;
use crate::clipboard::item::{ClipItem, ClipKind};

/// `Exporter.export(_:)` — the panel rides `withDialog` so the shelf lowers
/// itself under the system's save sheet.
pub fn export(item: &ClipItem) {
    let item = item.clone();
    crate::shelf::panel::with_dialog(|| run(&item));
}

fn run(item: &ClipItem) {
    let export_dir = Preferences::shared().export_directory();
    let export_url = NSURL::fileURLWithPath(&NSString::from_str(&export_dir.to_string_lossy()));
    match item.kind {
        ClipKind::Image | ClipKind::Text | ClipKind::Url | ClipKind::Video => {
            let mtm = MainThreadMarker::new().expect("main thread");
            let panel = NSSavePanel::savePanel(mtm);
            panel.setDirectoryURL(Some(&export_url));
            panel.setCanCreateDirectories(true);
            panel.setExtensionHidden(false);
            let (name, uti) = match item.kind {
                ClipKind::Image => (format!("Pastory {}.png", stamp(item)), "public.png"),
                ClipKind::Video => (
                    format!("Rec {}.{}", stamp(item), item.ext),
                    if item.ext == "gif" { "com.compuserve.gif" } else { "public.mpeg-4" },
                ),
                _ => (format!("Clip {}.txt", stamp(item)), "public.plain-text"),
            };
            panel.setNameFieldStringValue(&NSString::from_str(&name));
            unsafe {
                // SAFETY: one UTI string on the standard save panel contract.
                let uti_s = NSString::from_str(uti);
                let arr = objc2_foundation::NSArray::from_slice(&[&*uti_s]);
                let _: () = objc2::msg_send![&panel, setAllowedContentTypes: &*arr];
            }
            panel.setPrompt(Some(&NSString::from_str(&l("保存"))));
            if panel.runModal() != objc2_app_kit::NSModalResponseOK {
                return;
            }
            let Some(url) = panel.URL() else { return };
            let Some(path) = url.path().map(|p| p.to_string()) else { return };
            let out = std::path::PathBuf::from(path);
            if write_payload(item, &out) {
                Preferences::shared().set_custom_export_dir(Some(
                    &out.parent().map(|p| p.to_string_lossy()).unwrap_or_default(),
                ));
                reveal(&[out]);
            } else {
                unsafe {
        let _: () = objc2::msg_send![objc2::class!(NSSound), beep];
    }
            }
        }
        ClipKind::Files => {
            let mtm = MainThreadMarker::new().expect("main thread");
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseDirectories(true);
            panel.setCanChooseFiles(false);
            panel.setCanCreateDirectories(true);
            panel.setDirectoryURL(Some(&export_url));
            panel.setPrompt(Some(&NSString::from_str(&l("保存到这里"))));
            panel.setMessage(Some(&NSString::from_str(&l("选择一个文件夹，把这些文件复制过去"))));
            if panel.runModal() != objc2_app_kit::NSModalResponseOK {
                return;
            }
            let Some(url) = panel.URL() else { return };
            let Some(path) = url.path().map(|p| p.to_string()) else { return };
            let dir = std::path::PathBuf::from(path);
            let mut out: Vec<std::path::PathBuf> = Vec::new();
            crate::clipboard::store::with(|s| {
                for src in s.file_urls(item) {
                    if !src.is_file() {
                        continue;
                    }
                    let name = src
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let stem = src
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let ext = src.extension().map(|e| e.to_string_lossy().into_owned());
                    let mut dst = dir.join(&name);
                    let mut n = 2;
                    while dst.exists() {
                        dst = dir.join(match &ext {
                            Some(e) if !e.is_empty() => format!("{stem} {n}.{e}"),
                            _ => format!("{stem} {n}"),
                        });
                        n += 1;
                    }
                    if std::fs::copy(&src, &dst).is_ok() {
                        out.push(dst);
                    }
                }
            });
            Preferences::shared().set_custom_export_dir(Some(&dir.to_string_lossy()));
            if !out.is_empty() {
                reveal(&out);
            }
        }
    }
}

/// NSSound.beep — matches the export failure alert.
fn msg_send_beep() {
    unsafe {
        let _: () = objc2::msg_send![objc2::class!(NSSound), beep];
    }
}

/// yyyy-MM-dd HH.mm.ss of the card's own creation time (NSDate "current
/// calendar" = local time zone, like Swift's DateFormatter).
fn stamp(item: &ClipItem) -> String {
    let date = NSDate::dateWithTimeIntervalSince1970(item.created_at);
    let comp = NSCalendar::currentCalendar().components_fromDate(
        NSCalendarUnit::Year | NSCalendarUnit::Month | NSCalendarUnit::Day
            | NSCalendarUnit::Hour | NSCalendarUnit::Minute | NSCalendarUnit::Second,
        &date,
    );
    format!(
        "{:04}-{:02}-{:02} {:02}.{:02}.{:02}",
        comp.year(),
        comp.month(),
        comp.day(),
        comp.hour(),
        comp.minute(),
        comp.second(),
    )
}

use objc2_foundation::{NSCalendar, NSCalendarUnit, NSDate};


/// The bytes as they would have pasted: PNG for anything image (HEIC decoded),
/// everything else verbatim. Tmp sibling first, swap only once complete.
fn write_payload(item: &ClipItem, out: &std::path::Path) -> bool {
    let tmp = out.with_extension("pastory-tmp");
    let _ = std::fs::remove_file(&tmp);
    let ok = crate::clipboard::store::with(|s| {
        if item.kind == ClipKind::Image && item.ext != "png" {
            match s.png(item) {
                Some(png) => std::fs::write(&tmp, png).is_ok(),
                None => false,
            }
        } else {
            std::fs::copy(s.payload_url(item), &tmp).is_ok()
        }
    });
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    let url_tmp = NSURL::fileURLWithPath(&NSString::from_str(&tmp.to_string_lossy()));
    let url_out = NSURL::fileURLWithPath(&NSString::from_str(&out.to_string_lossy()));
    let mut resulting: Option<objc2::rc::Retained<NSURL>> = None;
    let fm = NSFileManager::defaultManager();
    let result = fm.replaceItemAtURL_withItemAtURL_backupItemName_options_resultingItemURL_error(
        &url_out,
        &url_tmp,
        None,
        objc2_foundation::NSFileManagerItemReplacementOptions(0),
        Some(&mut resulting),
    );
    result.is_ok()
}

/// `NSWorkspace.activateFileViewerSelecting` — show the result in Finder.
fn reveal(paths: &[std::path::PathBuf]) {
    let urls: Vec<objc2::rc::Retained<NSURL>> = paths
        .iter()
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())))
        .collect();
    let refs: Vec<&NSURL> = urls.iter().map(|u| &**u).collect();
    let arr = NSArray::from_slice(&refs);
    objc2_app_kit::NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&arr);
}

use objc2_foundation::{NSArray, NSFileManager};

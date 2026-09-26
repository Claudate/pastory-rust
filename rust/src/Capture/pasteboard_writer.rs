//! Port of `Capture/PasteboardWriter.swift`.
//!
//! Everything Pastory puts on the pasteboard carries `marker` = item id so the
//! monitor bumps the existing item instead of recording a duplicate.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardType};
use objc2_foundation::{NSArray, NSData, NSString, NSURL};

use crate::capture::screenshotter;
use crate::clipboard::item::ClipItem;

/// `NSPasteboardType` is a plain NSString alias, so the "custom" types are
/// just string literals.
fn type_from_str(s: &str) -> Retained<NSPasteboardType> {
    NSString::from_str(s)
}

/// The marker type, leaked once (process-lifetime pasteboard constant).
pub(crate) fn marker() -> &'static NSPasteboardType {
    // A static of an ObjC reference is !Sync, so keep the raw pointer instead.
    static MARKER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *MARKER.get_or_init(|| {
        Retained::into_raw(type_from_str("com.cici.snipclip.marker")) as usize
    });
    // SAFETY: the pointer is a Retained we intentionally never release (a
    // constant for the life of the process).
    unsafe { &*(p as *const NSPasteboardType) }
}

macro_rules! static_type {
    ($name:ident, $konst:path) => {
        pub(crate) fn $name() -> &'static NSPasteboardType {
            // SAFETY: framework constants, stable for the process lifetime.
            unsafe { $konst }
        }
    };
}

static_type!(t_png, objc2_app_kit::NSPasteboardTypePNG);
static_type!(t_tiff, objc2_app_kit::NSPasteboardTypeTIFF);
static_type!(t_string, objc2_app_kit::NSPasteboardTypeString);
static_type!(t_rtf, objc2_app_kit::NSPasteboardTypeRTF);
static_type!(t_url, objc2_app_kit::NSPasteboardTypeURL);

pub fn write_image(png: &[u8], item_id: &str) {
    let item = NSPasteboardItem::new();
    item.setData_forType(&NSData::with_bytes(png), t_png());
    if let Some(cg) = screenshotter::image_from_png(png) {
        if let Some(tiff) = screenshotter::tiff_data(&cg) {
            item.setData_forType(&NSData::with_bytes(&tiff), t_tiff());
        }
    }
    item.setString_forType(&NSString::from_str(item_id), marker());
    commit(&item);
}

pub fn write_text(text: &str, rtf: Option<&[u8]>, item_id: &str) {
    let item = NSPasteboardItem::new();
    item.setString_forType(&NSString::from_str(text), t_string());
    if let Some(rtf) = rtf {
        item.setData_forType(&NSData::with_bytes(rtf), t_rtf());
    }
    if ClipItem::is_url_text(text) {
        item.setString_forType(&NSString::from_str(text), t_url());
    }
    item.setString_forType(&NSString::from_str(item_id), marker());
    commit(&item);
}

/// Animated GIF as image data (chat apps paste it as a moving picture, not an attachment).
pub fn write_gif(data: &[u8], item_id: &str) {
    let item = NSPasteboardItem::new();
    let t = type_from_str("com.compuserve.gif");
    item.setData_forType(&NSData::with_bytes(data), &t);
    item.setString_forType(&NSString::from_str(item_id), marker());
    commit(&item);
}

/// Files the way Finder copies them: NSURL objects plus the legacy filenames
/// list. Chat apps (WeChat included) look for the latter; a bare
/// public.file-url pastes as text there.
pub fn write_files(urls: &[std::path::PathBuf], item_id: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let ns_urls: Vec<Retained<NSURL>> = urls
        .iter()
        .map(|p| {
            NSURL::fileURLWithPath_isDirectory(
                &NSString::from_str(&p.to_string_lossy()),
                p.is_dir(),
            )
        })
        .collect();
    let objects: Vec<&ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>> = ns_urls
        .iter()
        .map(|u| ProtocolObject::from_ref(&**u))
        .collect();
    let arr = NSArray::from_slice(&objects);
    if !pb.writeObjects(&arr) {
        println!("pasteboard writeObjects(files) failed");
    }
    // Legacy filenames list: property list of path strings.
    let paths: Vec<Retained<NSString>> = urls
        .iter()
        .map(|p| NSString::from_str(&p.to_string_lossy()))
        .collect();
    let path_arr = NSArray::from_retained_slice(&paths);
    let filenames_type = type_from_str("NSFilenamesPboardType");
    // SAFETY: an NSArray of NSString is a valid property list object.
    let plist = unsafe { Retained::cast_unchecked::<objc2::runtime::AnyObject>(path_arr) };
    let ok = unsafe { pb.setPropertyList_forType(&plist, &filenames_type) };
    if !ok {
        println!("pasteboard setPropertyList(NSFilenamesPboardType) failed");
    }
    pb.setString_forType(&NSString::from_str(item_id), marker());
}

fn commit(item: &NSPasteboardItem) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let objects: [&ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>; 1] =
        [ProtocolObject::from_ref(item)];
    let arr = NSArray::from_slice(&objects);
    if !pb.writeObjects(&arr) {
        println!("pasteboard writeObjects failed");
    }
}

//! Port of `Capture/CaptureTarget.swift`: the three capture targets and
//! `ShareableSnapshot` (SCShareableContent + our own windows tagged by pid),
//! including the CGWindowList front-to-back ordering fix-up (contract #27).
//!
//! `fetch` is the only async piece: SCK answers on its own queue and the
//! completion hops to the main queue, mirroring Swift's `@MainActor` await.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSScreen;
use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_foundation::{ns_string, NSDictionary, NSError, NSNumber, NSString};
use objc2_screen_capture_kit::{
    SCDisplay, SCRunningApplication, SCShareableContent, SCWindow,
};

/// `CaptureTarget` — region rects are display-local points, origin top-left.
#[derive(Clone)]
pub enum CaptureTarget {
    Display(Retained<SCDisplay>),
    Region(Retained<SCDisplay>, CGRect),
    Window(Retained<SCWindow>),
}

/// One frozen look at the shareable content (`ShareableSnapshot`).
pub struct ShareableSnapshot {
    pub content: Retained<SCShareableContent>,
    /// Our own windows (pid match) — excluded from display captures.
    pub own_windows: Vec<Retained<SCWindow>>,
}

impl ShareableSnapshot {
    /// `ShareableSnapshot.fetch()`. The completion runs on the main thread;
    /// `None` is Swift's `catch` path.
    pub fn fetch(completion: Box<dyn FnOnce(Option<ShareableSnapshot>) + Send + 'static>) {
        let completion = std::sync::Arc::new(std::sync::Mutex::new(Some(completion)));
        let block = block2::RcBlock::new(
            move |content: *mut SCShareableContent, error: *mut NSError| {
                if !error.is_null() {
                    let err = unsafe { &*error };
                    eprintln!(
                        "shareable content failed ({}): {}",
                        err.code(),
                        err.localizedDescription()
                    );
                }
                // SAFETY: a non-null completion parameter is retained here;
                // SC window/display snapshots are immutable and safe to hand
                // to the main thread. `retain` maps null to None.
                let snap = unsafe { Retained::retain(content) };
                let snap = snap.map(|content| {
                    let pid = std::process::id() as i32;
                    let own_windows = unsafe { content.windows() }
                        .iter()
                        .filter(|w| unsafe {
                            w.owningApplication()
                                .map(|a: Retained<SCRunningApplication>| a.processID() == pid)
                                .unwrap_or(false)
                        })
                        .collect();
                    ShareableSnapshot { content, own_windows }
                });
                let cb = completion.lock().unwrap().take().expect("fires once");
                // SAFETY: the pointer is reclaimed exactly once on the main
                // thread; SC snapshots are immutable.
                let raw = Box::into_raw(Box::new(snap)) as usize;
                crate::app::delegate::dispatch_main_async(Box::new(move || {
                    let snap = unsafe { Box::from_raw(raw as *mut Option<ShareableSnapshot>) };
                    cb(*snap);
                }));
            },
        );
        // SAFETY: the block is copied by SCK; parameters match the generated
        // signature.
        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                false,
                true,
                &*block,
            );
        }
    }

    /// `display(for: screen)` — NSScreen ↔ SCDisplay via NSScreenNumber.
    pub fn display_for_screen(&self, screen: &NSScreen) -> Option<Retained<SCDisplay>> {
        let id = screen_display_id(screen)?;
        unsafe { self.content.displays() }
            .iter()
            .find(|d| unsafe { d.displayID() } == id)
    }

    /// `screen(for: display)` — the reverse lookup (stateless; asks AppKit).
    pub fn screen_for_display(&self, display: &SCDisplay) -> Option<Retained<NSScreen>> {
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        let id = unsafe { display.displayID() };
        NSScreen::screens(mtm)
            .iter()
            .find(|s| screen_display_id(s) == Some(id))
    }

    /// `pickableWindows(also:)` — on-screen windows ≥40pt, layer 0, not ours
    /// (plus explicitly named ones like the shelf, which floats above the
    /// normal layer), ordered front-to-back per the window server list
    /// (contract #27: SCShareableContent does not promise an order).
    pub fn pickable_windows(&self, also: &[u32]) -> Vec<Retained<SCWindow>> {
        let pid = std::process::id() as i32;
        let mut visible: Vec<Retained<SCWindow>> = unsafe { self.content.windows() }
            .iter()
            .filter(|w| unsafe {
                let frame = w.frame();
                w.isOnScreen()
                    && frame.size.width > 40.0
                    && frame.size.height > 40.0
                    && ((w.owningApplication()
                        .map(|a| a.processID() != pid)
                        .unwrap_or(true)
                        && w.windowLayer() == 0)
                        || also.contains(&w.windowID()))
            })
            .collect();
        let order = window_server_order();
        visible.sort_by_key(|w| {
            order
                .iter()
                .position(|id| Some(*id) == unsafe { Some(w.windowID()) })
                .unwrap_or(usize::MAX)
        });
        visible
    }
}

/// `screen.deviceDescription["NSScreenNumber"]` as a display id.
pub fn screen_display_id(screen: &NSScreen) -> Option<u32> {
    let desc = screen.deviceDescription();
    // SAFETY: same dictionary; only the key type is re-marked.
    let desc: Retained<NSDictionary<NSString, AnyObject>> =
        unsafe { Retained::cast_unchecked(desc) };
    let any = desc.objectForKey(ns_string!("NSScreenNumber"))?;
    let number: &NSNumber = any.downcast_ref().expect("NSScreenNumber is an NSNumber");
    Some(number.unsignedIntValue())
}

/// Front-to-back on-screen window ids from the window server list.
fn window_server_order() -> Vec<u32> {
    let Some(list) = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        0, // kCGNullWindowID
    ) else {
        return Vec::new();
    };
    // The binding types the elements as opaque; they are CFDictionary per
    // C's contract, so re-mark the element type before touching them.
    let list: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(list) };
    let key = CFString::from_static_str("kCGWindowNumber");
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for i in 0..list.len() {
        let Some(elem) = list.get(i) else { continue };
        // SAFETY: CGWindowListCopyWindowInfo returns an array of CFDictionary.
        let info: CFRetained<CFDictionary<CFString, CFType>> =
            unsafe { CFRetained::cast_unchecked(elem) };
        let Some(value) = info.get(&key) else { continue };
        // SAFETY: kCGWindowNumber is always a CFNumber (kSInt32).
        let number: CFRetained<CFNumber> = unsafe { CFRetained::cast_unchecked(value) };
        let Some(id) = number.as_i32() else { continue };
        if seen.insert(id as u32) {
            out.push(id as u32);
        }
    }
    out
}

//! Self-test drawing helpers.

use objc2_app_kit::NSView;

/// Force one `drawRect:` through the bitmap path and drop the pixels.
///
/// The self-test host is a nonactivating panel whose window is often fully
/// occluded (`occlusionState` reports Other, not Visible) — and AppKit skips
/// every display pass for an occluded window: `setNeedsDisplay` +
/// `displayIfNeeded` clear the flag without ever running `drawRect:`, so a
/// pane built on the live panel records no hotspots and the click test reads
/// a blank sheet. Flaky by desktop state, not by code. Drawing into a bitmap
/// rep does not ask the window server, so it always runs.
pub(crate) fn force_draw(view: &NSView) {
    let bounds = view.bounds();
    let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(bounds) else {
        return;
    };
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
}

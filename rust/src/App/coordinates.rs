//! Port of `CoordinateSpace` (`Capture/CaptureTarget.swift:52-66`).
//!
//! The three spaces every capture/window rect lives in:
//! - CG global: top-left origin (ScreenCaptureKit, CGWindowList),
//! - Cocoa global: bottom-left origin (NSScreen/NSEvent.mouseLocation),
//! - display-local: top-left points inside one display (region rects, crops).
//!
//! The math is pure points; AppKit lookup happens only in `primary_height`.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

/// `CoordinateSpace.primaryHeight` — the first (main) screen's height, the
/// pivot between CG's top-left and Cocoa's bottom-left global origins.
pub fn primary_height() -> f64 {
    let mtm = objc2::MainThreadMarker::new().expect("main thread");
    objc2_app_kit::NSScreen::screens(mtm)
        .firstObject()
        .map(|s| s.frame().size.height)
        .unwrap_or(0.0)
}

/// CG global (top-left origin) → Cocoa global (bottom-left origin).
pub fn cocoa_rect_from_cg(r: CGRect) -> CGRect {
    CGRect::new(
        CGPoint::new(r.origin.x, primary_height() - r.origin.y - r.size.height),
        r.size,
    )
}

/// `CoordinateSpace.cocoaRect(fromCG:)` with the height passed in — the same
/// formula, testable without a screen (the selftest's round-trips use this).
pub fn cocoa_rect_from_cg_with_primary(r: CGRect, primary_height: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(r.origin.x, primary_height - r.origin.y - r.size.height),
        r.size,
    )
}

/// Overlay-view rect (NSView y-up, covers exactly its screen) → display-local
/// top-left points (`CoordinateSpace.displayLocalRect(viewRect:screen:)`).
pub fn display_local_rect(view_rect: CGRect, screen_height: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(
            view_rect.origin.x.round(),
            (screen_height - (view_rect.origin.y + view_rect.size.height)).round(),
        ),
        CGSize::new(view_rect.size.width.round(), view_rect.size.height.round()),
    )
}

/// `CGRect.integral` — floor the min corner, ceil the max corner.
pub fn integral_rect(r: CGRect) -> CGRect {
    let x = r.origin.x.floor();
    let y = r.origin.y.floor();
    let max_x = (r.origin.x + r.size.width).ceil();
    let max_y = (r.origin.y + r.size.height).ceil();
    CGRect::new(CGPoint::new(x, y), CGSize::new(max_x - x, max_y - y))
}

/// `CGRect.insetBy` — shrink (negative grows) symmetrically.
pub fn inset_rect(r: CGRect, dx: f64, dy: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(r.origin.x + dx, r.origin.y + dy),
        CGSize::new(r.size.width - 2.0 * dx, r.size.height - 2.0 * dy),
    )
}

/// `CGRect.intersection` — the overlap (zero size when disjoint).
pub fn intersect_rect(a: CGRect, b: CGRect) -> CGRect {
    let x0 = a.origin.x.max(b.origin.x);
    let y0 = a.origin.y.max(b.origin.y);
    let x1 = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    CGRect::new(
        CGPoint::new(x0, y0),
        CGSize::new((x1 - x0).max(0.0), (y1 - y0).max(0.0)),
    )
}

/// `CGRect.intersects`.
pub fn intersects_rect(a: CGRect, b: CGRect) -> bool {
    intersection_area(a, b) > 0.0
}

/// `CGRect.contains(point)`.
pub fn contains_pt(r: CGRect, p: CGPoint) -> bool {
    p.x >= r.origin.x
        && p.x < r.origin.x + r.size.width
        && p.y >= r.origin.y
        && p.y < r.origin.y + r.size.height
}

/// Intersection area (Swift's private `CGRect.area`): 0 for null/empty.
pub fn intersection_area(a: CGRect, b: CGRect) -> f64 {
    let x0 = a.origin.x.max(b.origin.x);
    let y0 = a.origin.y.max(b.origin.y);
    let x1 = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    if x1 <= x0 || y1 <= y0 {
        0.0
    } else {
        (x1 - x0) * (y1 - y0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn cocoa_from_cg_flips_y() {
        // Primary 1080: CG rect at the top of the screen lands at the top in
        // Cocoa too (bottom-left origin).
        let cg = r(100.0, 0.0, 400.0, 300.0);
        let cocoa = cocoa_rect_from_cg_with_primary(cg, 1080.0);
        assert_eq!(cocoa.origin, CGPoint::new(100.0, 780.0));
        assert_eq!(cocoa.size, cg.size);
    }

    #[test]
    fn cocoa_from_cg_roundtrip_is_identity() {
        // The flip is an involution: applying it twice returns the original.
        let cg = r(-50.0, 123.5, 640.25, 480.75);
        let cocoa = cocoa_rect_from_cg_with_primary(cg, 1440.0);
        let back = cocoa_rect_from_cg_with_primary(cocoa, 1440.0);
        assert_eq!(back, cg);
    }

    #[test]
    fn display_local_rect_measures_from_top() {
        // An overlay view rect with its top edge 100pt below the screen top
        // is display-local y=100, sizes rounded to whole points.
        let screen_h = 900.0;
        let view = r(10.2, 800.0, 500.5, 100.0); // y-up: top at 800+100=900
        let local = display_local_rect(view, screen_h);
        assert_eq!(local, r(10.0, 0.0, 501.0, 100.0));
    }

    #[test]
    fn display_local_rect_full_screen() {
        let screen_h = 1080.0;
        let view = r(0.0, 0.0, 1920.0, 1080.0);
        assert_eq!(display_local_rect(view, screen_h), view);
    }

    #[test]
    fn intersection_area_matches_cgrect_semantics() {
        assert_eq!(intersection_area(r(0.0, 0.0, 10.0, 10.0), r(5.0, 5.0, 10.0, 10.0)), 25.0);
        assert_eq!(intersection_area(r(0.0, 0.0, 4.0, 4.0), r(5.0, 5.0, 2.0, 2.0)), 0.0);
        assert_eq!(intersection_area(r(0.0, 0.0, 10.0, 10.0), r(2.0, 2.0, 4.0, 4.0)), 16.0);
    }
}

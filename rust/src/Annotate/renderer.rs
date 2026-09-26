//! Port of `Annotate/AnnotationRenderer.swift`.
//!
//! Excalidraw-flavoured drawing: every stroke is flattened to points, nudged
//! by smooth low-frequency noise and drawn twice, so shapes look hand-drawn
//! but stay legible. Contexts are y-down and measured in canvas points;
//! `render` scales that up to image pixels (preview == export, contract #13).

use objc2_app_kit::{NSBezierPath, NSColor, NSGraphicsContext};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage,
    CGLineCap, CGLineJoin, CGInterpolationQuality, CGMutablePath, CGPath,
};

use crate::annotate::annotation::{
    AnnotateTool, Annotation, StrokeSize,
};
use crate::app::{coordinates, theme};

/// premultipliedLast (the Swift contexts' bitmapInfo).
const PREMULTIPLIED_LAST: u32 = 1;

/// `AnnotationRenderer.render(_:annotations:canvasSize:)` — the flattened
/// export: the image itself when there is nothing drawn.
pub fn render(
    image: &CGImage,
    annotations: &[Annotation],
    canvas_size: CGSize,
) -> Option<objc2_core_foundation::CFRetained<CGImage>> {
    let (w, h) = (
        objc2_core_graphics::CGImage::width(Some(image)),
        objc2_core_graphics::CGImage::height(Some(image)),
    );
    let space = CGImage::color_space(Some(image))
        .or_else(|| CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceSRGB })))?;
    let ctx = unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&space), PREMULTIPLIED_LAST)
    }?;
    CGContext::draw_image(
        Some(&ctx),
        CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64)),
        Some(image),
    );
    let scale = w as f64 / canvas_size.width;
    CGContext::translate_ctm(Some(&ctx), 0.0, h as f64);
    CGContext::scale_ctm(Some(&ctx), scale, -scale);
    let gc = NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, true);
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&gc));
    for a in annotations {
        draw(a, &ctx, image, scale);
    }
    NSGraphicsContext::restoreGraphicsState_class();
    CGBitmapContextCreateImage(Some(&ctx))
}

/// `AnnotationRenderer.draw(_:in:source:pixelsPerPoint:)`.
pub fn draw(a: &Annotation, ctx: &CGContext, source: &CGImage, pixels_per_point: f64) {
    CGContext::save_g_state(Some(ctx));
    CGContext::set_stroke_color_with_color(Some(ctx), Some(&a.color.CGColor()));
    CGContext::set_fill_color_with_color(Some(ctx), Some(&a.color.CGColor()));
    CGContext::set_line_width(Some(ctx), a.size.line_width());
    CGContext::set_line_cap(Some(ctx), CGLineCap::Round);
    CGContext::set_line_join(Some(ctx), CGLineJoin::Round);
    let mut rng = theme::Seeded(a.seed);
    match a.tool {
        AnnotateTool::Rect => {
            let r = a.rect();
            let radius = 12.0f64.min(r.size.width.min(r.size.height) * 0.2);
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, radius, radius);
            sketch(&path, true, a.size, &mut rng, ctx);
        }
        AnnotateTool::Ellipse => {
            let path = NSBezierPath::bezierPathWithOvalInRect(a.rect());
            sketch(&path, true, a.size, &mut rng, ctx);
        }
        AnnotateTool::Line | AnnotateTool::Arrow => {
            if a.points.len() >= 2 {
                let p0 = a.points[0];
                let p1 = a.points[a.points.len() - 1];
                let path = NSBezierPath::new();
                path.moveToPoint(p0);
                path.lineToPoint(p1);
                sketch(&path, false, a.size, &mut rng, ctx);
                if a.tool == AnnotateTool::Arrow {
                    arrow_head(p0, p1, a.size, &mut rng, ctx);
                }
            }
        }
        AnnotateTool::Pen => {
            if a.points.len() > 1 {
                CGContext::add_path(Some(ctx), Some(&smooth_path(&a.points)));
                CGContext::stroke_path(Some(ctx));
            }
        }
        AnnotateTool::Text => {
            if let Some(p) = a.points.first() {
                if !a.text.is_empty() {
                    a.text_layout().draw_at(*p);
                }
            }
        }
        AnnotateTool::Mosaic => {
            pixelate(a.rect(), a.size.mosaic_block(), source, pixels_per_point, ctx);
        }
    }
    CGContext::restore_g_state(Some(ctx));
}

// MARK: Hand-drawn strokes

/// Swift's `Int.random(in: a...b, using: &Seeded)` — parity is visual only
/// (preview==export is guaranteed by reusing the same seed on both paths;
/// §8.3 does not byte-compare against the Swift render).
fn rand_int(rng: &mut theme::Seeded, a: u64, b: u64) -> u64 {
    a + rng.next() % (b - a + 1)
}

/// Swift's `CGFloat.random(in: 0...(2π), using: &Seeded)`.
fn rand_pi2(rng: &mut theme::Seeded) -> f64 {
    (rng.next() as f64 / u64::MAX as f64) * 2.0 * std::f64::consts::PI
}

/// Two jittered passes over the flattened path (`sketch`).
fn sketch(path: &NSBezierPath, closed: bool, size: StrokeSize, rng: &mut theme::Seeded, ctx: &CGContext) {
    // `path.flatness = 0.3`: the flatten() below uses the same tol.
    let pts = flatten(path, 0.3);
    if pts.len() <= 1 {
        return;
    }
    let amp = 0.9 + size.line_width() * 0.25;
    for pass in 0..2 {
        let f1 = (if closed { rand_int(rng, 2, 3) } else { rand_int(rng, 1, 2) }) as f64;
        let f2 = (if closed { rand_int(rng, 5, 7) } else { rand_int(rng, 3, 5) }) as f64;
        let (ph1, ph2, ph3) = (rand_pi2(rng), rand_pi2(rng), rand_pi2(rng));
        let scale_amp = if pass == 0 { amp } else { amp * 0.8 };
        let out = CGMutablePath::new();
        let n_pts = pts.len();
        for (i, p) in pts.iter().enumerate() {
            let t = i as f64 / (n_pts - 1) as f64;
            let mut n = (2.0 * std::f64::consts::PI * f1 * t + ph1).sin() * 0.6
                + (2.0 * std::f64::consts::PI * f2 * t + ph2).sin() * 0.4;
            let mut m = (2.0 * std::f64::consts::PI * f1 * t + ph3).cos() * 0.5;
            if !closed {
                let taper = (t * std::f64::consts::PI).sin();
                n *= taper;
                m *= taper;
            }
            let q = CGPoint::new(p.x + n * scale_amp, p.y + m * scale_amp);
            if i == 0 {
                unsafe { CGMutablePath::move_to_point(Some(&out), std::ptr::null(), q.x, q.y) };
            } else {
                unsafe { CGMutablePath::add_line_to_point(Some(&out), std::ptr::null(), q.x, q.y) };
            }
        }
        if closed {
            CGMutablePath::close_subpath(Some(&out));
        }
        let out_path: CFRetained<CGPath> = unsafe { objc2_core_foundation::CFRetained::cast_unchecked(out) };
        CGContext::add_path(Some(ctx), Some(&out_path));
        CGContext::stroke_path(Some(ctx));
    }
}

/// Flatten a bezier at `tol` and densify long segments so the noise has
/// something to bend (`flatten`).
fn flatten(path: &NSBezierPath, tol: f64) -> Vec<CGPoint> {
    path.setFlatness(tol);
    let flat = path.bezierPathByFlatteningPath();
    let mut pts: Vec<CGPoint> = Vec::new();
    let count = flat.elementCount();
    for i in 0..count {
        let mut buf = [CGPoint::ZERO; 3];
        let el = unsafe { flat.elementAtIndex_associatedPoints(i, buf.as_mut_ptr()) };
        match el {
            objc2_app_kit::NSBezierPathElement::MoveTo | objc2_app_kit::NSBezierPathElement::LineTo => {
                pts.push(buf[0]);
            }
            objc2_app_kit::NSBezierPathElement::ClosePath => {
                if let Some(f) = pts.first() {
                    let f = *f;
                    pts.push(f);
                }
            }
            _ => {}
        }
    }
    // Densify long segments.
    let mut dense: Vec<CGPoint> = Vec::new();
    for (i, p) in pts.iter().enumerate() {
        if i > 0 {
            let q = pts[i - 1];
            let d = ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt();
            let n = (d / 6.0) as usize;
            if n > 1 {
                for k in 1..n {
                    let t = k as f64 / n as f64;
                    dense.push(CGPoint::new(
                        q.x + (p.x - q.x) * t,
                        q.y + (p.y - q.y) * t,
                    ));
                }
            }
        }
        dense.push(*p);
    }
    dense
}

/// Open V head, like Excalidraw's default arrow (`arrowHead`).
fn arrow_head(a: CGPoint, b: CGPoint, size: StrokeSize, rng: &mut theme::Seeded, ctx: &CGContext) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let (ux, uy) = (dx / len, dy / len);
    let head = (len * 0.6).min(12.0 + size.line_width() * 3.5);
    let ang = 0.42f64;
    for s in [1.0f64, -1.0f64] {
        let vx = ux * ang.cos() - s * uy * ang.sin();
        let vy = s * ux * ang.sin() + uy * ang.cos();
        let p = CGPoint::new(b.x - vx * head, b.y - vy * head);
        let path = NSBezierPath::new();
        path.moveToPoint(p);
        path.lineToPoint(b);
        sketch(&path, false, size, rng, ctx);
    }
}

/// Quadratic curve through midpoints: smooth freehand without over-rounding
/// (`smoothPath`).
fn smooth_path(pts: &[CGPoint]) -> CFRetained<CGPath> {
    let path = CGMutablePath::new();
    unsafe { CGMutablePath::move_to_point(Some(&path), std::ptr::null(), pts[0].x, pts[0].y) };
    if pts.len() == 2 {
        unsafe { CGMutablePath::add_line_to_point(Some(&path), std::ptr::null(), pts[1].x, pts[1].y) };
        return unsafe { objc2_core_foundation::CFRetained::cast_unchecked(path) };
    }
    for i in 1..pts.len() - 1 {
        let mid = CGPoint::new((pts[i].x + pts[i + 1].x) / 2.0, (pts[i].y + pts[i + 1].y) / 2.0);
        unsafe {
            CGMutablePath::add_quad_curve_to_point(
                Some(&path),
                std::ptr::null(),
                pts[i].x,
                pts[i].y,
                mid.x,
                mid.y,
            )
        };
    }
    let last = pts[pts.len() - 1];
    unsafe { CGMutablePath::add_line_to_point(Some(&path), std::ptr::null(), last.x, last.y) };
    unsafe { objc2_core_foundation::CFRetained::cast_unchecked(path) }
}

/// Block-average the region: shrink to a few cells, blow back up without
/// interpolation (`pixelate`).
fn pixelate(rect: CGRect, block_points: f64, source: &CGImage, pixels_per_point: f64, ctx: &CGContext) {
    let px = coordinates::integral_rect(CGRect::new(
        CGPoint::new(rect.min().x * pixels_per_point, rect.min().y * pixels_per_point),
        CGSize::new(rect.size.width * pixels_per_point, rect.size.height * pixels_per_point),
    ));
    if px.size.width < 1.0 || px.size.height < 1.0 {
        return;
    }
    let Some(crop) = CGImage::with_image_in_rect(Some(source), px) else {
        return;
    };
    let block = 4.0f64.max(block_points * pixels_per_point);
    let cw = (px.size.width / block) as usize;
    let ch = (px.size.height / block) as usize;
    let (cw, ch) = (cw.max(1), ch.max(1));
    let space = CGImage::color_space(Some(source))
        .or_else(|| CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceSRGB })));
    let Some(space) = space else { return };
    let Some(small) = (unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), cw, ch, 8, 0, Some(&space), PREMULTIPLIED_LAST)
    }) else {
        return;
    };
    CGContext::set_interpolation_quality(Some(&small), CGInterpolationQuality::Medium);
    CGContext::draw_image(
        Some(&small),
        CGRect::new(CGPoint::ZERO, CGSize::new(cw as f64, ch as f64)),
        Some(&crop),
    );
    let Some(tiny) = CGBitmapContextCreateImage(Some(&small)) else {
        return;
    };
    CGContext::save_g_state(Some(ctx));
    CGContext::set_interpolation_quality(Some(ctx), CGInterpolationQuality::None);
    CGContext::translate_ctm(Some(ctx), 0.0, rect.max().y);
    CGContext::scale_ctm(Some(ctx), 1.0, -1.0);
    CGContext::draw_image(
        Some(ctx),
        CGRect::new(
            CGPoint::new(rect.min().x, 0.0),
            CGSize::new(rect.size.width, rect.size.height),
        ),
        Some(&tiny),
    );
    CGContext::restore_g_state(Some(ctx));
}

// MARK: Selection chrome (live view only)

/// `handleRadius`.
pub const HANDLE_RADIUS: f64 = 4.5;
/// How far the outline sits from a text box's glyphs.
pub const TEXT_INSET: f64 = 4.0;
/// `deleteRadius`.
pub const DELETE_RADIUS: f64 = 10.0;

/// `selectionHandles` — text grips sit on the outline, clear of the glyphs;
/// model handles describe the text box itself.
pub fn selection_handles(a: &Annotation) -> Vec<CGPoint> {
    if a.tool != AnnotateTool::Text {
        return a.handles();
    }
    let r = coordinates::inset_rect(a.bounds(), -TEXT_INSET, -TEXT_INSET);
    let (min, max, mid) = (r.min(), r.max(), r.mid());
    vec![
        CGPoint::new(min.x, min.y),
        CGPoint::new(max.x, min.y),
        CGPoint::new(min.x, max.y),
        CGPoint::new(max.x, max.y),
        CGPoint::new(min.x, mid.y),
        CGPoint::new(max.x, mid.y),
        CGPoint::new(mid.x, min.y),
        CGPoint::new(mid.x, max.y),
    ]
}

/// Where the delete button sits for a selected annotation.
pub fn delete_center(a: &Annotation) -> CGPoint {
    if a.tool == AnnotateTool::Text {
        // Text: the chip sits on the outline's own top-right corner, so it
        // stays with the box and inside the picture even when the text hugs
        // the top or right edge.
        let r = coordinates::inset_rect(a.bounds(), -TEXT_INSET, -TEXT_INSET);
        return CGPoint::new(r.max().x, r.min().y);
    }
    // Shapes keep theirs just off the corner, clear of the corner grip.
    let r = coordinates::inset_rect(a.bounds(), -8.0, -8.0);
    CGPoint::new(r.max().x + 6.0, r.min().y - 6.0)
}

pub fn delete_rect(a: &Annotation) -> CGRect {
    let c = delete_center(a);
    CGRect::new(
        CGPoint::new(c.x - DELETE_RADIUS, c.y - DELETE_RADIUS),
        CGSize::new(2.0 * DELETE_RADIUS, 2.0 * DELETE_RADIUS),
    )
}

/// `drawSelection` — dashed box for boxes/text/pen, handles, delete bubble.
pub fn draw_selection(a: &Annotation, ctx: &CGContext) {
    CGContext::save_g_state(Some(ctx));
    if a.tool != AnnotateTool::Arrow && a.tool != AnnotateTool::Line {
        // Rounded outline drawn twice: a soft dark line, then cream dashes on
        // top. One of the two always shows, on a white page as well as on a
        // dark one.
        let inset = if a.tool == AnnotateTool::Text { TEXT_INSET } else { 8.0 };
        let box_r = coordinates::inset_rect(a.bounds(), -inset, -inset);
        let path = round_rect_path(box_r, 5.0, 5.0);
        CGContext::add_path(Some(ctx), Some(&path));
        CGContext::set_stroke_color_with_color(
            Some(ctx),
            Some(&theme::ink().colorWithAlphaComponent(0.45).CGColor()),
        );
        CGContext::set_line_width(Some(ctx), 1.5);
        CGContext::stroke_path(Some(ctx));
        CGContext::add_path(Some(ctx), Some(&path));
        CGContext::set_stroke_color_with_color(Some(ctx), Some(&theme::paper().CGColor()));
        CGContext::set_line_width(Some(ctx), 1.5);
        let lengths: [objc2_core_foundation::CGFloat; 2] = [5.0, 4.0];
        unsafe { CGContext::set_line_dash(Some(ctx), 0.0, lengths.as_ptr(), lengths.len()) };
        CGContext::stroke_path(Some(ctx));
        unsafe { CGContext::set_line_dash(Some(ctx), 0.0, std::ptr::null(), 0) };
    }
    // Grips: none on text (its edges and corners drag, and the pointer says
    // so). Shapes and lines get small dots.
    if a.tool != AnnotateTool::Text {
        CGContext::set_fill_color_with_color(Some(ctx), Some(&theme::paper().CGColor()));
        CGContext::set_stroke_color_with_color(
            Some(ctx),
            Some(&theme::ink().colorWithAlphaComponent(0.6).CGColor()),
        );
        CGContext::set_line_width(Some(ctx), 1.0);
        let g = 3.0;
        for p in selection_handles(a) {
            let d = CGRect::new(CGPoint::new(p.x - g, p.y - g), CGSize::new(2.0 * g, 2.0 * g));
            CGContext::fill_ellipse_in_rect(Some(ctx), d);
            CGContext::stroke_ellipse_in_rect(Some(ctx), d);
        }
    }
    // Delete: a small dark chip with a light ×. Quieter than an outlined
    // button, and it reads on any picture.
    let c = delete_center(a);
    let chip = coordinates::inset_rect(delete_rect(a), 2.0, 2.0);
    CGContext::save_g_state(Some(ctx));
    CGContext::set_shadow_with_color(
        Some(ctx),
        CGSize::new(0.0, 1.0),
        2.0,
        Some(&NSColor::colorWithCalibratedWhite_alpha(0.0, 0.25).CGColor()),
    );
    CGContext::set_fill_color_with_color(
        Some(ctx),
        Some(&theme::ink().colorWithAlphaComponent(0.85).CGColor()),
    );
    CGContext::fill_ellipse_in_rect(Some(ctx), chip);
    CGContext::restore_g_state(Some(ctx));
    CGContext::set_stroke_color_with_color(Some(ctx), Some(&theme::paper().CGColor()));
    CGContext::set_line_width(Some(ctx), 1.4);
    CGContext::set_line_cap(Some(ctx), CGLineCap::Round);
    let k = 2.6;
    CGContext::move_to_point(Some(ctx), c.x - k, c.y - k);
    CGContext::add_line_to_point(Some(ctx), c.x + k, c.y + k);
    CGContext::move_to_point(Some(ctx), c.x + k, c.y - k);
    CGContext::add_line_to_point(Some(ctx), c.x - k, c.y + k);
    CGContext::stroke_path(Some(ctx));
    CGContext::restore_g_state(Some(ctx));
}

/// `CGPath(roundedRect:cornerWidth:cornerHeight:transform:nil)`.
fn round_rect_path(r: CGRect, w: f64, h: f64) -> CFRetained<CGPath> {
    unsafe { CGPath::with_rounded_rect(r, w, h, std::ptr::null()) }
}

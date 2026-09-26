//! Port of `Annotate/Annotation.swift`: the tool set, stroke sizes, the hand
//! font, one drawn thing (`Annotation`), the palette, and `isLight`.
//!
//! One drawn thing lives in canvas points (origin top-left); `seed` pins the
//! hand-drawn jitter so redraws and export look identical (contract #13).

use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSFont};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

use crate::annotate::text_layout::AnnotationTextLayout;
use crate::app::{coordinates, localization::l, theme};

/// `AnnotateTool: CaseIterable` (order = toolbar order).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnnotateTool {
    Rect,
    Ellipse,
    Arrow,
    Line,
    Pen,
    Text,
    Mosaic,
}

pub const ALL_TOOLS: [AnnotateTool; 7] = [
    AnnotateTool::Rect,
    AnnotateTool::Ellipse,
    AnnotateTool::Arrow,
    AnnotateTool::Line,
    AnnotateTool::Pen,
    AnnotateTool::Text,
    AnnotateTool::Mosaic,
];

impl AnnotateTool {
    pub fn tip(self) -> String {
        match self {
            AnnotateTool::Rect => l("矩形  R"),
            AnnotateTool::Ellipse => l("椭圆  O"),
            AnnotateTool::Arrow => l("箭头  A"),
            AnnotateTool::Line => l("直线  L"),
            AnnotateTool::Pen => l("画笔  P"),
            AnnotateTool::Text => l("文字  T"),
            AnnotateTool::Mosaic => l("马赛克  M"),
        }
    }

    /// Single-key shortcut (Excalidraw-style).
    pub fn key(self) -> char {
        match self {
            AnnotateTool::Rect => 'r',
            AnnotateTool::Ellipse => 'o',
            AnnotateTool::Arrow => 'a',
            AnnotateTool::Line => 'l',
            AnnotateTool::Pen => 'p',
            AnnotateTool::Text => 't',
            AnnotateTool::Mosaic => 'm',
        }
    }

    pub fn from_key(ch: char) -> Option<AnnotateTool> {
        ALL_TOOLS.iter().copied().find(|t| t.key() == ch)
    }
}

/// `StrokeSize` — one size control for everything: stroke width for shapes,
/// font size for text, block size for mosaic, dot in the size picker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrokeSize {
    S = 1,
    M = 2,
    L = 3,
}

pub const ALL_SIZES: [StrokeSize; 3] = [StrokeSize::S, StrokeSize::M, StrokeSize::L];

impl StrokeSize {
    pub fn from_raw(raw: i64) -> StrokeSize {
        match raw {
            1 => StrokeSize::S,
            3 => StrokeSize::L,
            _ => StrokeSize::M,
        }
    }
    pub fn raw(self) -> i64 {
        self as i64
    }
    pub fn line_width(self) -> f64 {
        [2.6, 4.2, 6.5][self.raw() as usize - 1]
    }
    pub fn font_size(self) -> f64 {
        [18.0, 24.0, 34.0][self.raw() as usize - 1]
    }
    /// Mosaic cell size in canvas points.
    pub fn mosaic_block(self) -> f64 {
        [8.0, 12.0, 18.0][self.raw() as usize - 1]
    }
    /// Diameter of the dot shown in the size picker.
    pub fn dot_diameter(self) -> f64 {
        [5.0, 8.0, 11.0][self.raw() as usize - 1]
    }
}

/// `HandFont` — 翩翩体 covers Latin too, close to Excalidraw's
/// Xiaolai/Virgil feel. Falls back to the system font.
pub fn hand_font(size: f64) -> Retained<NSFont> {
    for name in ["HanziPenSC-W5", "HannotateSC-W5"] {
        if let Some(f) = NSFont::fontWithName_size(&NSString::from_str(name), size) {
            return f;
        }
    }
    NSFont::systemFontOfSize_weight(size, unsafe { objc2_app_kit::NSFontWeightMedium })
}

pub type AnnotationId = u64;

/// One drawn thing, in canvas points (origin top-left).
#[derive(Clone)]
pub struct Annotation {
    pub id: AnnotationId,
    pub tool: AnnotateTool,
    pub color: Retained<NSColor>,
    pub size: StrokeSize,
    /// rect / ellipse / arrow / line / mosaic: [start, end]. pen: the whole
    /// path. text: [anchor].
    pub points: Vec<CGPoint>,
    pub text: String,
    /// User-sized text box. Height grows as needed so wrapping never hides
    /// any text.
    pub text_box_size: Option<CGSize>,
    /// Fixes the hand-drawn jitter so redraws and the export look identical.
    pub seed: u64,
}

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn fresh_seed() -> u64 {
    // Swift UUID().random seed: the value itself is invisible; the same
    // entropy source style (time-mixed) is fine.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0x853C_49E6_748F_EA9B);
    let mut g = theme::Seeded(t ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let s = g.next();
    if s == 0 { 1 } else { s }
}

impl Annotation {
    pub fn new(tool: AnnotateTool, color: Retained<NSColor>, size: StrokeSize, points: Vec<CGPoint>) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            tool,
            color,
            size,
            points,
            text: String::new(),
            text_box_size: None,
            seed: fresh_seed(),
        }
    }

    pub fn rect(&self) -> CGRect {
        let (Some(a), Some(b)) = (self.points.first(), self.points.last()) else {
            return CGRect::ZERO;
        };
        CGRect::new(
            CGPoint::new(a.x.min(b.x), a.y.min(b.y)),
            CGSize::new((a.x - b.x).abs(), (a.y - b.y).abs()),
        )
    }

    pub fn text_layout(&self) -> AnnotationTextLayout {
        AnnotationTextLayout::new(
            &if self.text.is_empty() {
                " ".to_string()
            } else {
                self.text.clone()
            },
            self.size,
            &self.color,
            self.text_width(),
        )
    }

    fn text_width(&self) -> f64 {
        self.text_box_size
            .map(|s| s.width)
            .unwrap_or_else(|| text_natural_width(&self.text, self.size) + 6.0)
            .max(32.0)
    }

    /// Bounding box in canvas points, used for selection and hit testing.
    pub fn bounds(&self) -> CGRect {
        match self.tool {
            AnnotateTool::Pen => {
                let Some(f) = self.points.first() else { return CGRect::ZERO };
                let (mut x0, mut y0, mut x1, mut y1) = (f.x, f.y, f.x, f.y);
                for p in &self.points {
                    x0 = x0.min(p.x);
                    y0 = y0.min(p.y);
                    x1 = x1.max(p.x);
                    y1 = y1.max(p.y);
                }
                CGRect::new(CGPoint::new(x0, y0), CGSize::new(x1 - x0, y1 - y0))
            }
            AnnotateTool::Text => {
                let Some(p) = self.points.first() else { return CGRect::ZERO };
                let h = self
                    .text_box_size
                    .map(|s| s.height)
                    .unwrap_or(0.0)
                    .max(self.text_layout().height());
                CGRect::new(*p, CGSize::new(self.text_width(), h))
            }
            _ => self.rect(),
        }
    }

    pub fn translate(&mut self, d: CGPoint) {
        for p in &mut self.points {
            p.x += d.x;
            p.y += d.y;
        }
    }

    /// Hit test near the stroke (so you can still start a new shape inside
    /// an old rectangle); mosaic and text hit anywhere inside.
    pub fn hit(&self, p: CGPoint) -> bool {
        let slop = 8.0;
        match self.tool {
            AnnotateTool::Arrow | AnnotateTool::Line => {
                if self.points.len() < 2 {
                    return false;
                }
                distance(p, &self.points[0], &self.points[self.points.len() - 1]) <= slop
            }
            AnnotateTool::Pen => {
                for i in 1..self.points.len().max(1) {
                    if distance(p, &self.points[i - 1], &self.points[i]) <= slop {
                        return true;
                    }
                }
                false
            }
            AnnotateTool::Rect => {
                let r = self.rect();
                coordinates::contains_pt(coordinates::inset_rect(r, -slop, -slop), p)
                    && !coordinates::contains_pt(coordinates::inset_rect(r, slop, slop), p)
            }
            AnnotateTool::Ellipse => {
                let r = self.rect();
                let a = (r.size.width / 2.0).max(1.0);
                let b = (r.size.height / 2.0).max(1.0);
                let mid = r.mid();
                let dx = (p.x - mid.x) / a;
                let dy = (p.y - mid.y) / b;
                let d = (dx * dx + dy * dy).sqrt();
                (d - 1.0).abs() * a.min(b) <= slop
            }
            AnnotateTool::Mosaic | AnnotateTool::Text => {
                coordinates::contains_pt(coordinates::inset_rect(self.bounds(), -slop, -slop), p)
            }
        }
    }

    /// Endpoints for lines, corners for boxes, corners and edge midpoints
    /// for text.
    pub fn handles(&self) -> Vec<CGPoint> {
        match self.tool {
            AnnotateTool::Arrow | AnnotateTool::Line => {
                if self.points.len() < 2 {
                    Vec::new()
                } else {
                    vec![self.points[0], self.points[self.points.len() - 1]]
                }
            }
            AnnotateTool::Rect | AnnotateTool::Ellipse | AnnotateTool::Mosaic => {
                let r = self.rect();
                let (min, max) = (r.min(), r.max());
                vec![
                    CGPoint::new(min.x, min.y),
                    CGPoint::new(max.x, min.y),
                    CGPoint::new(min.x, max.y),
                    CGPoint::new(max.x, max.y),
                ]
            }
            AnnotateTool::Text => {
                let r = self.bounds();
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
            AnnotateTool::Pen => Vec::new(),
        }
    }

    /// Move handle `i` to `p`; the opposite side stays put.
    pub fn set_handle(&mut self, i: usize, p: CGPoint) {
        match self.tool {
            AnnotateTool::Arrow | AnnotateTool::Line => {
                if self.points.len() >= 2 {
                    if i == 0 {
                        self.points[0] = p;
                    } else {
                        let last = self.points.len() - 1;
                        self.points[last] = p;
                    }
                }
            }
            AnnotateTool::Rect | AnnotateTool::Ellipse | AnnotateTool::Mosaic => {
                let h = self.handles();
                if h.len() != 4 || i >= 4 {
                    return;
                }
                let opposite = h[3 - i];
                self.points = vec![opposite, p];
            }
            AnnotateTool::Text => {
                if i >= 8 {
                    return;
                }
                let r = self.bounds();
                let (mut left, mut right, mut top, mut bottom) =
                    (r.min().x, r.max().x, r.min().y, r.max().y);
                if [0usize, 2, 4].contains(&i) {
                    left = p.x.min(right - 32.0);
                }
                if [1usize, 3, 5].contains(&i) {
                    right = p.x.max(left + 32.0);
                }
                let min_height = self
                    .text_layout_with_width(right - left)
                    .height();
                if [0usize, 1, 6].contains(&i) {
                    top = p.y.min(bottom - min_height);
                }
                if [2usize, 3, 7].contains(&i) {
                    bottom = p.y.max(top + min_height);
                }
                self.points = vec![CGPoint::new(left, top)];
                self.text_box_size = Some(CGSize::new(
                    right - left,
                    min_height.max(bottom - top),
                ));
            }
            AnnotateTool::Pen => {}
        }
    }

    fn text_layout_with_width(&self, width: f64) -> AnnotationTextLayout {
        AnnotationTextLayout::new(
            &if self.text.is_empty() {
                " ".to_string()
            } else {
                self.text.clone()
            },
            self.size,
            &self.color,
            width,
        )
    }
}

pub fn distance(p: CGPoint, a: &CGPoint, b: &CGPoint) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (0.0f64).max(1.0f64.min(((p.x - a.x) * dx + (p.y - a.y) * dy) / len2))
    } else {
        0.0
    };
    let (qx, qy) = (a.x + t * dx, a.y + t * dy);
    ((p.x - qx).powi(2) + (p.y - qy).powi(2)).sqrt()
}

/// Single-line width of `text` in the hand font at `size` (`ceil`ed, like
/// the Swift `size(withAttributes:).width`).
pub fn text_natural_width(text: &str, size: StrokeSize) -> f64 {
    measure_hand_text(text, size)
}

pub fn measure_hand_text(text: &str, size: StrokeSize) -> f64 {
    let font = hand_font(size.font_size());
    let dict = crate::shelf::card::attrs(&font, &theme::ink(), None);
    unsafe {
        use objc2_app_kit::NSStringDrawing;
        NSString::from_str(text).sizeWithAttributes(Some(&dict)).width
    }
    .ceil()
}

/// Low-saturation set; purple first and default (`AnnotatePalette`).
pub fn palette() -> Vec<Retained<NSColor>> {
    vec![
        theme::purple(),
        theme::srgb_octets(0xE9, 0x63, 0x1A),
        theme::srgb_octets(0xC5, 0x6F, 0x8C),
        theme::srgb_octets(0xA9, 0xC2, 0xE0),
        theme::srgb_octets(0x59, 0x38, 0x2C),
        theme::srgb_octets(0x1E, 0x15, 0x1C),
        theme::srgb_octets(0xEB, 0xEB, 0xDF),
    ]
}

/// `AnnotatePalette.accent`.
pub fn palette_accent() -> Retained<NSColor> {
    theme::paper_blue_deep()
}

/// `NSColor.isLight` — luma past 0.7 in sRGB.
pub fn is_light(color: &NSColor) -> bool {
    let Some(c) = color.colorUsingColorSpace(&objc2_app_kit::NSColorSpace::sRGBColorSpace()) else {
        return false;
    };
    0.299 * c.redComponent() + 0.587 * c.greenComponent() + 0.114 * c.blueComponent() > 0.7
}

/// Quick NSColor equality via CGColor (palette circles compare `c == color`).
pub fn same_color(a: &NSColor, b: &NSColor) -> bool {
    a.CGColor() == b.CGColor()
}

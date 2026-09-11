//! Conversions between dxpdf renderer types and Skia types.

use crate::render::geometry::{PtLineSegment, PtOffset, PtRect, PtSize};
use crate::render::resolve::color::RgbColor;

pub fn to_point(o: PtOffset) -> skia_safe::Point {
    skia_safe::Point::new(f32::from(o.x), f32::from(o.y))
}

pub fn to_size(s: PtSize) -> skia_safe::Size {
    skia_safe::Size::new(f32::from(s.width), f32::from(s.height))
}

pub fn to_rect(r: PtRect) -> skia_safe::Rect {
    skia_safe::Rect::from_xywh(
        f32::from(r.origin.x),
        f32::from(r.origin.y),
        f32::from(r.size.width),
        f32::from(r.size.height),
    )
}

pub fn to_color4f(c: RgbColor) -> skia_safe::Color4f {
    const MAX: f32 = u8::MAX as f32;
    skia_safe::Color4f::new(c.r as f32 / MAX, c.g as f32 / MAX, c.b as f32 / MAX, 1.0)
}

pub fn to_line(l: PtLineSegment) -> (skia_safe::Point, skia_safe::Point) {
    (to_point(l.start), to_point(l.end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::dimension::Pt;
    use crate::render::geometry::PtOffset;

    #[test]
    fn offset_and_size_keep_their_axes() {
        // Asymmetric values throughout, so a transposed x/y or width/height
        // shows up rather than cancelling out.
        let p = to_point(PtOffset::new(Pt::new(3.0), Pt::new(-7.5)));
        assert_eq!((p.x, p.y), (3.0, -7.5));
        let s = to_size(PtSize::new(Pt::new(11.0), Pt::new(22.0)));
        assert_eq!((s.width, s.height), (11.0, 22.0));
    }

    /// `PtRect` is origin + size; `Rect` is left/top/right/bottom. The
    /// conversion has to add, and this is the only place that does it.
    #[test]
    fn rect_converts_origin_plus_size_to_edges() {
        let r = to_rect(PtRect::from_xywh(
            Pt::new(10.0),
            Pt::new(20.0),
            Pt::new(30.0),
            Pt::new(40.0),
        ));
        assert_eq!((r.left, r.top), (10.0, 20.0));
        assert_eq!(
            (r.right, r.bottom),
            (40.0, 60.0),
            "right/bottom are origin + size, not size"
        );
        assert_eq!((r.width(), r.height()), (30.0, 40.0));
    }

    /// 0–255 channels map to 0.0–1.0, and alpha is always opaque —
    /// `RgbColor` carries no alpha channel to lose.
    #[test]
    fn color_scales_bytes_to_unit_range_and_is_opaque() {
        let c = to_color4f(RgbColor {
            r: 255,
            g: 0,
            b: 51,
        });
        assert_eq!((c.r, c.g, c.b, c.a), (1.0, 0.0, 0.2, 1.0));
    }

    #[test]
    fn line_maps_start_then_end() {
        let (a, b) = to_line(PtLineSegment::new(
            PtOffset::new(Pt::new(1.0), Pt::new(2.0)),
            PtOffset::new(Pt::new(3.0), Pt::new(4.0)),
        ));
        assert_eq!(((a.x, a.y), (b.x, b.y)), ((1.0, 2.0), (3.0, 4.0)));
    }
}

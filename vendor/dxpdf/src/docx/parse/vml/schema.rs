//! Serde schema for VML (§14.1) — `<w:pict>` picture containers with shape
//! types, shapes, text boxes, and related child elements.
//!
//! XML structure goes through serde. Attribute-level string sub-grammars
//! (CSS-like `style`, `path` commands, `adj` / `coordsize` numeric lists,
//! color names/hex) are reused from `vml::{style, color, path_commands,
//! formulas}` as pure `&str` → value parsers.

#![allow(dead_code, clippy::large_enum_variant)]

use crate::model::Dup;
use serde::Deserialize;

use crate::docx::model::{
    Block, Pict, RelId, VmlArc, VmlCommonAttrs, VmlConnectType, VmlCurve, VmlDashStyle,
    VmlExtHandling, VmlFormula, VmlGroup, VmlImage, VmlImageData, VmlJoinStyle, VmlLine, VmlLock,
    VmlOval, VmlPath, VmlPoint, VmlPolyLine, VmlPrimitive, VmlRect, VmlRoundRect, VmlShape,
    VmlShapeId, VmlShapeType, VmlStroke, VmlTextBox, VmlTextBoxInset, VmlVector2D, VmlWrap,
    VmlWrapSide, VmlWrapType,
};
use crate::docx::parse::body_schema::BlockChildXml;

use super::color::parse_color;
use super::formulas::parse_formula;
use super::path_commands::{parse_adj, parse_path_commands, parse_vector2d};
use super::style::{parse_length, parse_style};

// ── pict ──────────────────────────────────────────────────────────────────

/// `<w:pict>` — legacy VML picture container. Per ECMA-376 Part 4
/// §14.1.1, the children form a sequence: optional `<v:shapetype>`
/// definitions followed by any of the primitive shape elements
/// (`<v:shape>`, `<v:rect>`, `<v:roundrect>`, `<v:oval>`, `<v:line>`,
/// `<v:polyline>`, `<v:arc>`, `<v:curve>`, `<v:image>`, `<v:group>`).
/// We capture every child via `$value` and dispatch through
/// [`VmlPrimitiveXml`].
#[derive(Deserialize)]
pub(crate) struct PictXml {
    #[serde(rename = "shapetype", default)]
    pub shape_type: Vec<ShapeTypeXml>,
    #[serde(rename = "$value", default)]
    pub primitives: Vec<VmlPrimitiveXml>,
}

impl PictXml {
    /// Convert to the model; `ctx` resolves embedded drawings nested inside
    /// VML text box content (same iterator threading as shape).
    pub(crate) fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> Pict {
        Pict {
            shape_type: Dup::from(self.shape_type).into_value().map(Into::into),
            primitives: self
                .primitives
                .into_iter()
                .filter_map(|p| p.into_model(ctx))
                .collect(),
        }
    }
}

/// VML §14.1.2 shape choice — a `<w:pict>` admits any of these as
/// children. `ShapeType` here is *not* a primitive but appears in
/// the same sibling list; serde routes it into this variant so we
/// don't have to special-case `<v:shapetype>` in the deserializer
/// (the dedicated `PictXml::shape_type` field also captures it).
/// `Other` absorbs unknown VML elements (or extension namespaces) so
/// parsing never fails on a strange child.
#[derive(Deserialize)]
pub(crate) enum VmlPrimitiveXml {
    #[serde(rename = "shape")]
    Shape(Box<ShapeXml>),
    #[serde(rename = "rect")]
    Rect(RectXml),
    #[serde(rename = "roundrect")]
    RoundRect(RoundRectXml),
    #[serde(rename = "oval")]
    Oval(OvalXml),
    #[serde(rename = "line")]
    Line(LineXml),
    #[serde(rename = "polyline")]
    PolyLine(PolyLineXml),
    #[serde(rename = "arc")]
    Arc(ArcXml),
    #[serde(rename = "curve")]
    Curve(CurveXml),
    #[serde(rename = "image")]
    Image(ImageXml),
    #[serde(rename = "group")]
    Group(Box<GroupXml>),
    /// `<v:shapetype>` is captured separately on `PictXml.shape_type`,
    /// but `$value` collects *every* child — this variant lets serde
    /// absorb the duplicate match without erroring.
    #[serde(rename = "shapetype")]
    ShapeType(Box<ShapeTypeXml>),
    /// Unknown / unsupported VML element. Dropped at conversion time.
    #[serde(other)]
    Other,
}

impl VmlPrimitiveXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> Option<VmlPrimitive> {
        Some(match self {
            VmlPrimitiveXml::Shape(s) => VmlPrimitive::Shape(s.into_model(ctx)),
            VmlPrimitiveXml::Rect(r) => VmlPrimitive::Rect(r.into_model(ctx)),
            VmlPrimitiveXml::RoundRect(r) => VmlPrimitive::RoundRect(r.into_model(ctx)),
            VmlPrimitiveXml::Oval(o) => VmlPrimitive::Oval(o.into_model(ctx)),
            VmlPrimitiveXml::Line(l) => VmlPrimitive::Line(l.into_model(ctx)),
            VmlPrimitiveXml::PolyLine(p) => VmlPrimitive::PolyLine(p.into_model(ctx)),
            VmlPrimitiveXml::Arc(a) => VmlPrimitive::Arc(a.into_model(ctx)),
            VmlPrimitiveXml::Curve(c) => VmlPrimitive::Curve(c.into_model(ctx)),
            VmlPrimitiveXml::Image(i) => VmlPrimitive::Image(i.into_model(ctx)),
            VmlPrimitiveXml::Group(g) => VmlPrimitive::Group(Box::new(g.into_model(ctx))),
            // Captured separately on `PictXml.shape_type`; drop the duplicate.
            VmlPrimitiveXml::ShapeType(_) | VmlPrimitiveXml::Other => return None,
        })
    }
}

/// Common attributes + child elements shared by every VML primitive
/// (§14.1.2.18 CoreAttributes plus `<v:stroke>` / `<v:textbox>` /
/// `<v:wrap>` / `<v:imagedata>` / `<v:fill>`).
///
/// **A carrier, never a deserialization target.** Every primitive declares
/// these nine fields itself and builds one of these to convert; this type owns
/// only the mapping — `parse_style`, `parse_color`, the [`Dup`] collapse, and
/// the `ctx` threading a text box needs — so that logic exists once rather
/// than ten times. It deliberately does not derive `Deserialize`.
///
/// It used to, embedded by seven primitives via `#[serde(flatten)]`, and that
/// boundary made two things fatal that are fine everywhere else:
///
/// * A **repeated child**. Elsewhere a duplicable child is `Vec<T>` collapsed
///   at the read, so repeating one cannot fail the parse (see
///   [`crate::docx::parse::primitives::duplicates`]). Flatten buffers the
///   element into a map and serde's `FlatMapDeserializer` cannot collect
///   repeated keys into a sequence, so these five had to stay `Option<T>` and
///   a second `<v:stroke>` failed the document with *"duplicate field"*.
/// * **Nested `<v:textbox><w:txbxContent>`**. The same buffering fails on the
///   inner block sequence with *"invalid type: map, expected a sequence"* —
///   the failure [`RectXml`] was inlined to escape, on XML that parses
///   correctly the moment the boundary is gone.
///
/// Both are covered by `every_primitive_tolerates_a_repeated_stroke` and
/// `every_primitive_keeps_its_textbox_content`. Do not reintroduce the
/// flatten to save the field declarations; the declarations are the price.
#[derive(Default)]
pub(crate) struct CommonAttrsXml {
    pub id: Option<String>,
    pub style: Option<String>,
    pub fillcolor: Option<String>,
    pub stroked: Option<VmlBool>,
    pub stroke: Vec<StrokeXml>,
    pub textbox: Vec<TextBoxXml>,
    pub wrap: Vec<WrapXml>,
    pub imagedata: Vec<ImageDataXml>,
    pub fill: Vec<FillXml>,
}

/// Move a primitive's inlined common fields into the shared carrier.
///
/// Every primitive declares the nine fields itself — that is what keeps them
/// off a flatten boundary, and [`CommonAttrsXml`] says why that matters. The
/// declarations are duplicated on purpose; only the forwarding is shared, so
/// a tenth common child cannot be wired into one primitive and silently
/// missed on another.
macro_rules! common_attrs {
    ($s:ident) => {
        CommonAttrsXml {
            id: $s.id,
            style: $s.style,
            fillcolor: $s.fillcolor,
            stroked: $s.stroked,
            stroke: $s.stroke,
            textbox: $s.textbox,
            wrap: $s.wrap,
            imagedata: $s.imagedata,
            fill: $s.fill,
        }
    };
}

/// VML §14.1.2.5 `<v:fill>` — every primitive can carry one. Fields
/// are admitted as attributes, not children.
#[derive(Deserialize, Default)]
pub(crate) struct FillXml {
    #[serde(rename = "@type", default)]
    pub fill_type: Option<String>,
    #[serde(rename = "@color", default)]
    pub color: Option<String>,
    #[serde(rename = "@color2", default)]
    pub color2: Option<String>,
    #[serde(rename = "@opacity", default)]
    pub opacity: Option<String>,
    #[serde(rename = "@src", default)]
    pub src: Option<String>,
    #[serde(rename = "@id", default)]
    pub rel_id: Option<String>,
}

impl From<FillXml> for crate::docx::model::VmlFill {
    fn from(x: FillXml) -> Self {
        use crate::docx::model::{VmlFill, VmlFillType};
        let fill_type = match x.fill_type.as_deref() {
            Some("solid") | None => VmlFillType::Solid,
            Some("gradient") | Some("gradientCenter") | Some("gradientUnscaled") => {
                VmlFillType::Gradient
            }
            Some("gradientRadial") => VmlFillType::GradientRadial,
            Some("tile") => VmlFillType::Tile,
            Some("frame") => VmlFillType::Frame,
            Some("pattern") => VmlFillType::Pattern,
            // Unknown type → treat as solid; the renderer logs a warn
            // when it can't honor a fill, never panics.
            Some(_) => VmlFillType::Solid,
        };
        VmlFill {
            fill_type,
            color: x.color.as_deref().and_then(parse_color),
            color2: x.color2.as_deref().and_then(parse_color),
            opacity: x.opacity.as_deref().and_then(|s| {
                // VML opacity admits "0.5" or "32768f" (fixed-point fraction
                // of 65536). For phase C we accept the float form.
                s.parse::<f32>().ok()
            }),
            src: x.src,
            rel_id: x.rel_id.map(crate::docx::model::RelId::new),
        }
    }
}

impl CommonAttrsXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlCommonAttrs {
        VmlCommonAttrs {
            id: self.id.map(VmlShapeId::new),
            style: parse_style(self.style),
            fill_color: self.fillcolor.as_deref().and_then(parse_color),
            stroked: self.stroked.map(|b| b.0),
            stroke: Dup::from(self.stroke).into_value().map(Into::into),
            text_box: Dup::from(self.textbox)
                .into_value()
                .map(|t| t.into_model(ctx)),
            wrap: Dup::from(self.wrap).into_value().map(Into::into),
            image_data: Dup::from(self.imagedata).into_value().map(Into::into),
            fill: Dup::from(self.fill).into_value().map(Into::into),
        }
    }
}

// ── shapetype ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct ShapeTypeXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@coordsize", default)]
    pub coordsize: Option<String>,
    #[serde(rename = "@spt", default)]
    pub spt: Option<String>,
    #[serde(rename = "@adj", default)]
    pub adj: Option<String>,
    #[serde(rename = "@path", default)]
    pub path: Option<String>,
    #[serde(rename = "@filled", default)]
    pub filled: Option<VmlBool>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,

    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "path", default)]
    pub vml_path: Vec<PathXml>,
    #[serde(rename = "formulas", default)]
    pub formulas: Vec<FormulasXml>,
    #[serde(rename = "lock", default)]
    pub lock: Vec<LockXml>,
}

impl From<ShapeTypeXml> for VmlShapeType {
    fn from(x: ShapeTypeXml) -> Self {
        Self {
            id: x.id.map(VmlShapeId::new),
            coord_size: parse_vector2d(x.coordsize),
            spt: x.spt.and_then(|s| s.parse::<f32>().ok()),
            adj: parse_adj(x.adj),
            path: parse_path_commands(x.path),
            filled: x.filled.map(|b| b.0),
            stroked: x.stroked.map(|b| b.0),
            stroke: Dup::from(x.stroke).into_value().map(Into::into),
            vml_path: Dup::from(x.vml_path).into_value().map(Into::into),
            formulas: Dup::from(x.formulas)
                .into_value()
                .map(|f| {
                    f.entries
                        .into_iter()
                        .filter_map(|e| parse_formula(&e.eqn))
                        .collect()
                })
                .unwrap_or_default(),
            lock: Dup::from(x.lock).into_value().map(Into::into),
        }
    }
}

// ── shape ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct ShapeXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    /// Reference like `"#_x0000_t202"` — strip the leading `#`.
    #[serde(rename = "@type", default)]
    pub ty: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,

    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "path", default)]
    pub vml_path: Vec<PathXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
}

impl ShapeXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlShape {
        VmlShape {
            shape_type_ref: self
                .ty
                .map(|s| VmlShapeId::new(s.strip_prefix('#').unwrap_or(&s))),
            vml_path: Dup::from(self.vml_path).into_value().map(Into::into),
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

// ── primitive shapes (rect / roundrect / oval / line / polyline /
//    arc / curve / image / group) ──────────────────────────────────────

/// VML §14.1.2.16 `<v:rect>`.
///
/// Common attrs are inlined here, as on every other primitive — this was the
/// first one to need it, because quick-xml's serde could not read the
/// deeply-nested `<v:textbox><w:txbxContent>` while the field carrying
/// `<v:textbox>` lived behind a `#[serde(flatten)]` boundary. See
/// [`CommonAttrsXml`] for the full account and the second failure it caused.
#[derive(Deserialize)]
pub(crate) struct RectXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
}

impl RectXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlRect {
        VmlRect {
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

/// VML §14.1.2.17 `<v:roundrect>`.
#[derive(Deserialize)]
pub(crate) struct RoundRectXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
    /// `@arcsize` — corner radius as a fraction (e.g. "10923f" = ~16.7%
    /// in the spec's fixed-point format, or a plain decimal). We accept
    /// floats; downstream layout clamps to [0, 1].
    #[serde(rename = "@arcsize", default)]
    pub arcsize: Option<f32>,
}

impl RoundRectXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlRoundRect {
        VmlRoundRect {
            arcsize: self.arcsize,
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

/// VML §14.1.2.13 `<v:oval>`.
#[derive(Deserialize)]
pub(crate) struct OvalXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
}

impl OvalXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlOval {
        VmlOval {
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

/// VML §14.1.2.12 `<v:line>` — endpoints in `@from` / `@to` as
/// comma-separated lengths (e.g. "10pt,20pt").
#[derive(Deserialize)]
pub(crate) struct LineXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
    #[serde(rename = "@from", default)]
    pub from: Option<String>,
    #[serde(rename = "@to", default)]
    pub to: Option<String>,
}

impl LineXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlLine {
        VmlLine {
            from: self.from.as_deref().and_then(parse_vml_point),
            to: self.to.as_deref().and_then(parse_vml_point),
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

/// VML §14.1.2.15 `<v:polyline>`. `@points` is a space-and-comma
/// separated list of x,y pairs.
#[derive(Deserialize)]
pub(crate) struct PolyLineXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
    #[serde(rename = "@points", default)]
    pub points: Option<String>,
}

impl PolyLineXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlPolyLine {
        VmlPolyLine {
            points: self
                .points
                .as_deref()
                .map(parse_vml_points)
                .unwrap_or_default(),
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

/// VML §14.1.2.3 `<v:arc>`.
#[derive(Deserialize)]
pub(crate) struct ArcXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
    #[serde(rename = "@startangle", default)]
    pub start_angle: Option<f32>,
    #[serde(rename = "@endangle", default)]
    pub end_angle: Option<f32>,
}

impl ArcXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlArc {
        VmlArc {
            start_angle: self.start_angle,
            end_angle: self.end_angle,
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

/// VML §14.1.2.7 `<v:curve>` — cubic Bezier with two control points.
#[derive(Deserialize)]
pub(crate) struct CurveXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
    #[serde(rename = "@from", default)]
    pub from: Option<String>,
    #[serde(rename = "@control1", default)]
    pub control1: Option<String>,
    #[serde(rename = "@control2", default)]
    pub control2: Option<String>,
    #[serde(rename = "@to", default)]
    pub to: Option<String>,
}

impl CurveXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlCurve {
        VmlCurve {
            from: self.from.as_deref().and_then(parse_vml_point),
            control1: self.control1.as_deref().and_then(parse_vml_point),
            control2: self.control2.as_deref().and_then(parse_vml_point),
            to: self.to.as_deref().and_then(parse_vml_point),
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

/// VML §14.1.2.10 `<v:image>` — image element. `@src` carries the
/// path; rels (`<v:imagedata r:id>`) are still picked up via the
/// inlined `imagedata` child, so either form works.
#[derive(Deserialize)]
pub(crate) struct ImageXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "stroke", default)]
    pub stroke: Vec<StrokeXml>,
    #[serde(rename = "textbox", default)]
    pub textbox: Vec<TextBoxXml>,
    #[serde(rename = "wrap", default)]
    pub wrap: Vec<WrapXml>,
    #[serde(rename = "imagedata", default)]
    pub imagedata: Vec<ImageDataXml>,
    #[serde(rename = "fill", default)]
    pub fill: Vec<FillXml>,
    #[serde(rename = "@src", default)]
    pub src: Option<String>,
}

impl ImageXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlImage {
        VmlImage {
            src: self.src,
            common: common_attrs!(self).into_model(ctx),
        }
    }
}

/// VML §14.1.2.9 `<v:group>` — recursive shape grouping.
///
/// The only primitive that carries **no** common child elements: `$value`
/// claims every child for the recursive primitive list, so a `<v:stroke>` on a
/// group lands in [`VmlPrimitiveXml::Other`] and is dropped. That is why this
/// one builds [`VmlCommonAttrs`] directly from its four attributes instead of
/// going through [`CommonAttrsXml`] like the other nine. Inlining was
/// originally forced here for a different reason — `#[serde(flatten)]` fought
/// `$value` for child capture, routing `<rect>`/`<oval>`/etc. through the
/// flattened struct, which rejected them as unknown fields and dropped them.
#[derive(Deserialize)]
pub(crate) struct GroupXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@fillcolor", default)]
    pub fillcolor: Option<String>,
    #[serde(rename = "@stroked", default)]
    pub stroked: Option<VmlBool>,
    #[serde(rename = "@coordsize", default)]
    pub coord_size: Option<String>,
    #[serde(rename = "@coordorigin", default)]
    pub coord_origin: Option<String>,
    /// Recursive: a group's children are themselves primitives.
    #[serde(rename = "$value", default)]
    pub children: Vec<VmlPrimitiveXml>,
}

impl GroupXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlGroup {
        VmlGroup {
            common: VmlCommonAttrs {
                id: self.id.map(VmlShapeId::new),
                style: parse_style(self.style),
                fill_color: self.fillcolor.as_deref().and_then(parse_color),
                stroked: self.stroked.map(|b| b.0),
                ..VmlCommonAttrs::default()
            },
            coord_size: parse_vector2d(self.coord_size),
            coord_origin: parse_vector2d(self.coord_origin),
            children: self
                .children
                .into_iter()
                .filter_map(|c| c.into_model(ctx))
                .collect(),
        }
    }
}

/// Parse a single VML 2D point from a comma-separated `"x,y"` string,
/// honoring CSS-like length units. Returns `None` on parse error so
/// callers can drop malformed values without aborting the whole parse.
fn parse_vml_point(s: &str) -> Option<VmlPoint> {
    let s = s.trim();
    let (xs, ys) = s.split_once(',')?;
    let x = parse_length(xs.trim())?;
    let y = parse_length(ys.trim())?;
    Some(VmlPoint {
        x: pt_value(&x),
        y: pt_value(&y),
    })
}

/// Parse a `<v:polyline>`-style points list: pairs separated by
/// whitespace and/or commas. Skips malformed pairs rather than failing.
fn parse_vml_points(s: &str) -> Vec<VmlPoint> {
    let toks: Vec<&str> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    toks.chunks_exact(2)
        .filter_map(|pair| {
            let x = parse_length(pair[0])?;
            let y = parse_length(pair[1])?;
            Some(VmlPoint {
                x: pt_value(&x),
                y: pt_value(&y),
            })
        })
        .collect()
}

/// Convert a parsed `VmlLength` to a plain `f32` in points, honoring
/// the unit. Used by primitive coord parsers; the layout layer keeps
/// the typed `VmlLength` for shape geometry but points/curves are
/// scalar enough that a flat `f32` suffices.
fn pt_value(len: &crate::docx::model::VmlLength) -> f32 {
    // Absolute units come from the shared table on `VmlLength`. The units it
    // leaves unresolved (`%`, `em`, and a bare number) degrade to the raw
    // value here: on a primitive's coordinate attributes a bare number is in
    // the shape's *local* coordinate system, not EMU as it would be in a
    // `style` measurement, and the relative units aren't expected at all.
    len.to_absolute_points().unwrap_or(len.value as f32)
}

// ── textbox ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct TextBoxXml {
    #[serde(rename = "@style", default)]
    pub style: Option<String>,
    #[serde(rename = "@inset", default)]
    pub inset: Option<String>,
    #[serde(rename = "txbxContent", default)]
    pub content: Vec<TxbxContentXml>,
}

#[derive(Deserialize, Default)]
pub(crate) struct TxbxContentXml {
    #[serde(rename = "$value", default)]
    pub children: Vec<BlockChildXml>,
}

impl TextBoxXml {
    fn into_model(self, ctx: &mut crate::docx::parse::body::ConvertCtx) -> VmlTextBox {
        let content: Vec<Block> = Dup::from(self.content)
            .into_value()
            .map(|c| {
                let (blocks, _) = crate::docx::parse::body::convert_container(c.children, ctx);
                blocks
            })
            .unwrap_or_default();
        VmlTextBox {
            style: parse_style(self.style),
            inset: self.inset.and_then(parse_inset),
            content,
        }
    }
}

/// Parse comma-separated `left,top,right,bottom` CSS lengths.
fn parse_inset(s: String) -> Option<VmlTextBoxInset> {
    let parts: Vec<&str> = s.split(',').collect();
    Some(VmlTextBoxInset {
        left: parts.first().and_then(|v| parse_length(v)),
        top: parts.get(1).and_then(|v| parse_length(v)),
        right: parts.get(2).and_then(|v| parse_length(v)),
        bottom: parts.get(3).and_then(|v| parse_length(v)),
    })
}

// ── stroke ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct StrokeXml {
    #[serde(rename = "@dashstyle", default)]
    pub dashstyle: Option<String>,
    #[serde(rename = "@joinstyle", default)]
    pub joinstyle: Option<String>,
}

impl From<StrokeXml> for VmlStroke {
    fn from(x: StrokeXml) -> Self {
        Self {
            dash_style: x.dashstyle.as_deref().and_then(parse_dash_style),
            join_style: x.joinstyle.as_deref().and_then(parse_join_style),
        }
    }
}

fn parse_dash_style(s: &str) -> Option<VmlDashStyle> {
    match s {
        "solid" => Some(VmlDashStyle::Solid),
        "shortdash" => Some(VmlDashStyle::ShortDash),
        "shortdot" => Some(VmlDashStyle::ShortDot),
        "shortdashdot" => Some(VmlDashStyle::ShortDashDot),
        "shortdashdotdot" => Some(VmlDashStyle::ShortDashDotDot),
        "dot" => Some(VmlDashStyle::Dot),
        "dash" => Some(VmlDashStyle::Dash),
        "longdash" => Some(VmlDashStyle::LongDash),
        "dashdot" => Some(VmlDashStyle::DashDot),
        "longdashdot" => Some(VmlDashStyle::LongDashDot),
        "longdashdotdot" => Some(VmlDashStyle::LongDashDotDot),
        _ => None,
    }
}

fn parse_join_style(s: &str) -> Option<VmlJoinStyle> {
    match s {
        "round" => Some(VmlJoinStyle::Round),
        "bevel" => Some(VmlJoinStyle::Bevel),
        "miter" => Some(VmlJoinStyle::Miter),
        _ => None,
    }
}

// ── path ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct PathXml {
    #[serde(rename = "@gradientshapeok", default)]
    pub gradient_shape_ok: Option<VmlBool>,
    #[serde(rename = "@connecttype", default)]
    pub connect_type: Option<String>,
    #[serde(rename = "@extrusionok", default)]
    pub extrusion_ok: Option<VmlBool>,
}

impl From<PathXml> for VmlPath {
    fn from(x: PathXml) -> Self {
        Self {
            gradient_shape_ok: x.gradient_shape_ok.map(|b| b.0),
            connect_type: x.connect_type.as_deref().and_then(|s| match s {
                "none" => Some(VmlConnectType::None),
                "rect" => Some(VmlConnectType::Rect),
                "segments" => Some(VmlConnectType::Segments),
                "custom" => Some(VmlConnectType::Custom),
                _ => None,
            }),
            extrusion_ok: x.extrusion_ok.map(|b| b.0),
        }
    }
}

// ── formulas ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub(crate) struct FormulasXml {
    #[serde(rename = "f", default)]
    pub entries: Vec<FormulaEntryXml>,
}

#[derive(Deserialize)]
pub(crate) struct FormulaEntryXml {
    #[serde(rename = "@eqn", default)]
    pub eqn: String,
}

// Re-exported so VmlFormula is reachable without silencing `unused` warnings.
#[doc(hidden)]
pub(crate) type _KeepFormulaAlive = VmlFormula;

// ── lock ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct LockXml {
    #[serde(rename = "@aspectratio", default)]
    pub aspect_ratio: Option<VmlBool>,
    #[serde(rename = "@ext", default)]
    pub ext: Option<String>,
}

impl From<LockXml> for VmlLock {
    fn from(x: LockXml) -> Self {
        let ext = x.ext.as_deref().and_then(|s| match s {
            "edit" => Some(VmlExtHandling::Edit),
            "view" => Some(VmlExtHandling::View),
            "backwardCompatible" => Some(VmlExtHandling::BackwardCompatible),
            _ => None,
        });
        Self {
            aspect_ratio: x.aspect_ratio.map(|b| b.0),
            ext,
        }
    }
}

// ── imagedata ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct ImageDataXml {
    #[serde(rename = "@id", default)]
    pub id: Option<String>,
    #[serde(rename = "@title", default)]
    pub title: Option<String>,
}

impl From<ImageDataXml> for VmlImageData {
    fn from(x: ImageDataXml) -> Self {
        Self {
            rel_id: x.id.map(RelId::new),
            title: x.title,
        }
    }
}

// ── wrap ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct WrapXml {
    #[serde(rename = "@type", default)]
    pub ty: Option<String>,
    #[serde(rename = "@side", default)]
    pub side: Option<String>,
}

impl From<WrapXml> for VmlWrap {
    fn from(x: WrapXml) -> Self {
        let wrap_type = x.ty.as_deref().and_then(|s| match s {
            "topAndBottom" => Some(VmlWrapType::TopAndBottom),
            "square" => Some(VmlWrapType::Square),
            "none" => Some(VmlWrapType::None),
            "tight" => Some(VmlWrapType::Tight),
            "through" => Some(VmlWrapType::Through),
            _ => None,
        });
        let side = x.side.as_deref().and_then(|s| match s {
            "both" => Some(VmlWrapSide::Both),
            "left" => Some(VmlWrapSide::Left),
            "right" => Some(VmlWrapSide::Right),
            "largest" => Some(VmlWrapSide::Largest),
            _ => None,
        });
        Self { wrap_type, side }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

/// VML boolean attribute — `"t"`/`"true"` → true, `"f"`/`"false"` → false,
/// anything else silently ignored.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VmlBool(pub bool);

impl<'de> Deserialize<'de> for VmlBool {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Self(matches!(s.as_str(), "t" | "true")))
    }
}

// Keep VmlVector2D reachable so it doesn't trip dead-code lint when the
// only user is via parse_vector2d.
#[doc(hidden)]
pub(crate) fn _keep_vmlvector_alive(v: VmlVector2D) -> VmlVector2D {
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::model::VmlColor;

    fn parse(xml: &str) -> Pict {
        let wrapped = format!(
            r#"<wrap xmlns:v="urn:v" xmlns:o="urn:o" xmlns:w="urn:w" xmlns:r="urn:r">{}</wrap>"#,
            xml
        );
        #[derive(Deserialize)]
        struct Wrap {
            pict: PictXml,
        }
        let w: Wrap = quick_xml::de::from_str(&wrapped).unwrap();
        let mut ctx = crate::docx::parse::body::ConvertCtx::new();
        w.pict.into_model(&mut ctx)
    }

    fn shapes(p: &Pict) -> Vec<&VmlShape> {
        p.shapes().collect()
    }

    #[test]
    fn empty_pict() {
        let p = parse(r#"<pict/>"#);
        assert!(p.shape_type.is_none());
        assert!(p.primitives.is_empty());
    }

    #[test]
    fn minimal_shape_with_fill_and_type_ref() {
        let p = parse(
            r##"<pict>
                <shape id="s1" type="#_x0000_t202"
                       style="position:absolute;left:0;top:0;width:100pt;height:50pt"
                       fillcolor="#ff0000" stroked="f"/>
            </pict>"##,
        );
        let shapes = shapes(&p);
        assert_eq!(shapes.len(), 1);
        let s = shapes[0];
        assert_eq!(s.common.id.as_ref().map(|v| v.as_str()), Some("s1"));
        assert_eq!(
            s.shape_type_ref.as_ref().map(|v| v.as_str()),
            Some("_x0000_t202")
        );
        assert_eq!(s.common.stroked, Some(false));
        assert!(matches!(
            s.common.fill_color,
            Some(VmlColor::Rgb(0xFF, 0, 0))
        ));
    }

    #[test]
    fn shapetype_with_stroke_and_path() {
        let p = parse(
            r#"<pict>
                <shapetype id="t1" coordsize="21600,21600" adj="5400,5400"
                           filled="t" stroked="t" path="m 0,0 l 10,10 x e">
                    <stroke dashstyle="dash" joinstyle="miter"/>
                    <path gradientshapeok="t" connecttype="rect" extrusionok="f"/>
                    <lock aspectratio="t" ext="edit"/>
                </shapetype>
            </pict>"#,
        );
        let st = p.shape_type.expect("shapetype parsed");
        assert_eq!(st.filled, Some(true));
        assert_eq!(st.stroked, Some(true));
        assert_eq!(st.adj, vec![5400, 5400]);
        assert_eq!(st.coord_size, Some(VmlVector2D { x: 21600, y: 21600 }));
        assert!(!st.path.is_empty());
        let stroke = st.stroke.unwrap();
        assert_eq!(stroke.dash_style, Some(VmlDashStyle::Dash));
        assert_eq!(stroke.join_style, Some(VmlJoinStyle::Miter));
        assert_eq!(
            st.vml_path.unwrap().connect_type,
            Some(VmlConnectType::Rect)
        );
        let lock = st.lock.unwrap();
        assert_eq!(lock.aspect_ratio, Some(true));
        assert_eq!(lock.ext, Some(VmlExtHandling::Edit));
    }

    #[test]
    fn shape_with_imagedata() {
        let p = parse(
            r##"<pict>
                <shape id="img1" type="#_x0000_t75" style="width:100pt;height:50pt">
                    <imagedata r:id="rId7" o:title="Logo"/>
                </shape>
            </pict>"##,
        );
        let shapes = shapes(&p);
        let id = shapes[0].common.image_data.as_ref().unwrap();
        assert_eq!(id.rel_id.as_ref().map(|r| r.as_str()), Some("rId7"));
        assert_eq!(id.title.as_deref(), Some("Logo"));
    }

    #[test]
    fn shape_with_textbox_no_content() {
        let p = parse(
            r#"<pict>
                <shape id="tb" style="width:100pt;height:50pt">
                    <textbox style="mso-fit-shape-to-text:t" inset="1pt,2pt,3pt,4pt">
                        <txbxContent/>
                    </textbox>
                </shape>
            </pict>"#,
        );
        let shapes = shapes(&p);
        let tb = shapes[0].common.text_box.as_ref().unwrap();
        assert!(tb.content.is_empty());
        assert!(tb.inset.is_some());
    }

    #[test]
    fn shape_with_textbox_containing_paragraph() {
        let p = parse(
            r#"<pict>
                <shape id="tb" style="width:100pt">
                    <textbox>
                        <txbxContent>
                            <w:p><w:r><w:t>Hello VML</w:t></w:r></w:p>
                        </txbxContent>
                    </textbox>
                </shape>
            </pict>"#,
        );
        let shapes = shapes(&p);
        let tb = shapes[0].common.text_box.as_ref().unwrap();
        assert_eq!(tb.content.len(), 1);
        match &tb.content[0] {
            Block::Paragraph(_) => (),
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn shape_with_wrap() {
        let p = parse(
            r#"<pict>
                <shape id="wr" style="width:100pt">
                    <wrap type="square" side="both"/>
                </shape>
            </pict>"#,
        );
        let shapes = shapes(&p);
        let w = shapes[0].common.wrap.unwrap();
        assert_eq!(w.wrap_type, Some(VmlWrapType::Square));
        assert_eq!(w.side, Some(VmlWrapSide::Both));
    }

    #[test]
    fn multiple_shapes_preserve_order() {
        let p = parse(
            r#"<pict>
                <shape id="a" style="width:10pt"/>
                <shape id="b" style="width:20pt"/>
                <shape id="c" style="width:30pt"/>
            </pict>"#,
        );
        let shapes = shapes(&p);
        assert_eq!(shapes.len(), 3);
        assert_eq!(shapes[0].common.id.as_ref().unwrap().as_str(), "a");
        assert_eq!(shapes[2].common.id.as_ref().unwrap().as_str(), "c");
    }

    #[test]
    fn style_is_parsed_via_existing_helper() {
        let p = parse(
            r#"<pict>
                <shape id="s" style="position:absolute;left:10pt;top:20pt;width:100pt;height:50pt;z-index:5"/>
            </pict>"#,
        );
        let shapes = shapes(&p);
        let st = &shapes[0].common.style;
        assert!(st.position.is_some());
        assert!(st.left.is_some());
        assert_eq!(st.z_index, Some(5));
    }

    // ── §14.1.2 primitive shapes ────────────────────────────────────────

    #[test]
    fn rect_parses_into_rect_primitive() {
        let p = parse(
            r##"<pict>
                <rect id="r1"
                      style="position:absolute;margin-left:-.5pt;margin-top:0;width:596.5pt;height:40.5pt"
                      fillcolor="#888b8d" stroked="f"/>
            </pict>"##,
        );
        assert_eq!(p.primitives.len(), 1);
        let VmlPrimitive::Rect(r) = &p.primitives[0] else {
            panic!("expected VmlPrimitive::Rect, got {:?}", p.primitives[0]);
        };
        assert_eq!(r.common.id.as_ref().map(|v| v.as_str()), Some("r1"),);
        assert!(matches!(
            r.common.fill_color,
            Some(VmlColor::Rgb(0x88, 0x8B, 0x8D))
        ));
        assert_eq!(r.common.stroked, Some(false));
        assert!(r.common.style.width.is_some());
    }

    #[test]
    fn rect_and_shape_in_same_pict_preserve_order() {
        let p = parse(
            r##"<pict>
                <rect id="r1" style="width:10pt;height:10pt"/>
                <shape id="s1" style="width:20pt;height:20pt"/>
                <rect id="r2" style="width:30pt;height:30pt"/>
            </pict>"##,
        );
        assert_eq!(p.primitives.len(), 3);
        assert!(matches!(p.primitives[0], VmlPrimitive::Rect(_)));
        assert!(matches!(p.primitives[1], VmlPrimitive::Shape(_)));
        assert!(matches!(p.primitives[2], VmlPrimitive::Rect(_)));
    }

    #[test]
    fn roundrect_carries_arcsize() {
        let p = parse(
            r##"<pict>
                <roundrect id="rr" arcsize="0.25" style="width:50pt;height:30pt" fillcolor="#ff0000"/>
            </pict>"##,
        );
        let VmlPrimitive::RoundRect(rr) = &p.primitives[0] else {
            panic!();
        };
        assert!((rr.arcsize.unwrap() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn oval_parses_into_oval_primitive() {
        let p = parse(r#"<pict><oval id="o" style="width:40pt;height:20pt"/></pict>"#);
        assert!(matches!(p.primitives[0], VmlPrimitive::Oval(_)));
    }

    #[test]
    fn line_carries_from_and_to_points() {
        let p = parse(
            r#"<pict>
                <line id="l" from="10pt,20pt" to="100pt,120pt" stroked="t"/>
            </pict>"#,
        );
        let VmlPrimitive::Line(l) = &p.primitives[0] else {
            panic!();
        };
        let from = l.from.unwrap();
        let to = l.to.unwrap();
        assert!((from.x - 10.0).abs() < 1e-3);
        assert!((from.y - 20.0).abs() < 1e-3);
        assert!((to.x - 100.0).abs() < 1e-3);
        assert!((to.y - 120.0).abs() < 1e-3);
    }

    #[test]
    fn polyline_collects_points_from_attribute() {
        let p = parse(
            r#"<pict>
                <polyline id="pl" points="0pt,0pt 10pt,20pt,30pt,40pt"/>
            </pict>"#,
        );
        let VmlPrimitive::PolyLine(pl) = &p.primitives[0] else {
            panic!();
        };
        assert_eq!(pl.points.len(), 3);
        assert!((pl.points[2].x - 30.0).abs() < 1e-3);
    }

    #[test]
    fn arc_carries_start_and_end_angles() {
        let p = parse(
            r#"<pict>
                <arc id="a" startangle="0" endangle="90" style="width:50pt;height:50pt"/>
            </pict>"#,
        );
        let VmlPrimitive::Arc(a) = &p.primitives[0] else {
            panic!();
        };
        assert_eq!(a.start_angle, Some(0.0));
        assert_eq!(a.end_angle, Some(90.0));
    }

    #[test]
    fn curve_parses_all_four_points() {
        let p = parse(
            r#"<pict>
                <curve id="c"
                       from="0pt,0pt"
                       control1="10pt,10pt"
                       control2="20pt,30pt"
                       to="40pt,50pt"/>
            </pict>"#,
        );
        let VmlPrimitive::Curve(c) = &p.primitives[0] else {
            panic!();
        };
        assert!(c.from.is_some());
        assert!(c.control1.is_some());
        assert!(c.control2.is_some());
        assert!(c.to.is_some());
    }

    #[test]
    fn image_carries_src_and_image_data() {
        let p = parse(
            r#"<pict>
                <image id="i" src="photo.png" style="width:100pt;height:80pt">
                    <imagedata r:id="rId9"/>
                </image>
            </pict>"#,
        );
        let VmlPrimitive::Image(img) = &p.primitives[0] else {
            panic!();
        };
        assert_eq!(img.src.as_deref(), Some("photo.png"));
        assert_eq!(
            img.common
                .image_data
                .as_ref()
                .and_then(|d| d.rel_id.as_ref().map(|r| r.as_str())),
            Some("rId9"),
        );
    }

    #[test]
    fn group_recursively_parses_children() {
        let p = parse(
            r##"<pict>
                <group id="g" coordsize="21600,21600" coordorigin="0,0">
                    <rect id="r" style="width:10pt;height:10pt" fillcolor="#ff0000"/>
                    <oval id="o" style="width:20pt;height:20pt"/>
                </group>
            </pict>"##,
        );
        let VmlPrimitive::Group(g) = &p.primitives[0] else {
            panic!();
        };
        assert_eq!(g.children.len(), 2);
        assert!(matches!(g.children[0], VmlPrimitive::Rect(_)));
        assert!(matches!(g.children[1], VmlPrimitive::Oval(_)));
        assert_eq!(g.coord_size.as_ref().map(|v| v.x), Some(21600));
    }

    #[test]
    fn rect_textbox_content_is_populated() {
        // Regression for the gray-bar text bug: `<v:rect>` with a
        // `<v:textbox><w:txbxContent>` should reach the model with
        // its inner paragraphs intact, just like `<v:shape>` does.
        // The `flatten` indirection on `RectXml.common` was dropping
        // the inner content when this test was first added.
        use crate::docx::model::VmlPrimitive;
        let p = parse(
            r#"<pict>
                <rect id="r" style="width:100pt;height:50pt">
                    <textbox>
                        <txbxContent>
                            <w:p><w:r><w:t>Inside the rect</w:t></w:r></w:p>
                        </txbxContent>
                    </textbox>
                </rect>
            </pict>"#,
        );
        let VmlPrimitive::Rect(r) = &p.primitives[0] else {
            panic!();
        };
        let tb = r.common.text_box.as_ref().expect("textbox parsed");
        assert_eq!(tb.content.len(), 1, "textbox should contain one paragraph");
    }

    /// The elements that used to reach the model through a
    /// `#[serde(flatten)]` on `CommonAttrsXml`. Flatten buffers children into
    /// a map, which cost these two things that `<v:rect>` and `<v:shape>`
    /// never lost — hence the two tests below.
    const FLATTENED_PRIMITIVES: [&str; 7] = [
        "roundrect",
        "oval",
        "line",
        "polyline",
        "arc",
        "curve",
        "image",
    ];

    /// A repeated child the VML schema allows once must not fail the parse,
    /// and must resolve to the last occurrence — the policy
    /// `primitives::duplicates` records for the `w:` property bags. Behind a
    /// flatten boundary serde's `FlatMapDeserializer` cannot collect repeated
    /// keys into a sequence, so these five children stayed `Option` and a
    /// second `<v:stroke>` failed the whole conversion.
    #[test]
    fn every_primitive_tolerates_a_repeated_stroke() {
        for element in FLATTENED_PRIMITIVES {
            let p = parse(&format!(
                r#"<pict>
                    <{element} id="x" style="width:50pt;height:30pt">
                        <stroke dashstyle="dot"/>
                        <stroke dashstyle="dash"/>
                    </{element}>
                </pict>"#
            ));
            let stroke = p.primitives[0]
                .common()
                .stroke
                .as_ref()
                .unwrap_or_else(|| panic!("<v:{element}> dropped its stroke"));
            assert_eq!(
                stroke.dash_style,
                Some(VmlDashStyle::Dash),
                "<v:{element}>: the last occurrence must win"
            );
        }
    }

    /// The same boundary also broke deeply-nested
    /// `<v:textbox><w:txbxContent>` — the failure `RectXml` was inlined to
    /// escape, pinned for `<v:rect>` by `rect_textbox_content_is_populated`.
    /// Behind flatten the identical inner XML fails with *"invalid type: map,
    /// expected a sequence"*, so a text box on any of these seven was fatal to
    /// the document, not merely dropped.
    #[test]
    fn every_primitive_keeps_its_textbox_content() {
        for element in FLATTENED_PRIMITIVES {
            let p = parse(&format!(
                r#"<pict>
                    <{element} id="x" style="width:100pt;height:50pt">
                        <textbox>
                            <txbxContent>
                                <w:p><w:r><w:t>Inside the {element}</w:t></w:r></w:p>
                            </txbxContent>
                        </textbox>
                    </{element}>
                </pict>"#
            ));
            let tb = p.primitives[0]
                .common()
                .text_box
                .as_ref()
                .unwrap_or_else(|| panic!("<v:{element}> dropped its textbox"));
            assert_eq!(
                tb.content.len(),
                1,
                "<v:{element}>: textbox content must survive"
            );
        }
    }

    #[test]
    fn rect_with_fill_child_carries_fill_type_and_color() {
        // §14.1.2.5: `<v:fill>` overrides `@fillcolor`. The model
        // carries both — the renderer's fill resolver picks the
        // child when present.
        use crate::docx::model::{VmlFillType, VmlPrimitive};
        let p = parse(
            r##"<pict>
                <rect id="r" style="width:50pt;height:30pt" fillcolor="#ff0000">
                    <fill type="solid" color="#00ff00"/>
                </rect>
            </pict>"##,
        );
        let VmlPrimitive::Rect(r) = &p.primitives[0] else {
            panic!();
        };
        assert!(matches!(
            r.common.fill_color,
            Some(VmlColor::Rgb(0xFF, 0, 0))
        ));
        let fill = r.common.fill.as_ref().expect("fill child parsed");
        assert_eq!(fill.fill_type, VmlFillType::Solid);
        assert!(matches!(fill.color, Some(VmlColor::Rgb(0, 0xFF, 0))));
    }

    #[test]
    fn fill_type_gradient_and_tile_are_modeled() {
        use crate::docx::model::{VmlFillType, VmlPrimitive};
        for (input, expected) in [
            ("gradient", VmlFillType::Gradient),
            ("gradientRadial", VmlFillType::GradientRadial),
            ("tile", VmlFillType::Tile),
            ("frame", VmlFillType::Frame),
            ("pattern", VmlFillType::Pattern),
        ] {
            let xml = format!(
                r##"<pict>
                    <rect id="r" style="width:10pt;height:10pt">
                        <fill type="{input}" color="#aabbcc"/>
                    </rect>
                </pict>"##
            );
            let p = parse(&xml);
            let VmlPrimitive::Rect(r) = &p.primitives[0] else {
                panic!();
            };
            assert_eq!(
                r.common.fill.as_ref().unwrap().fill_type,
                expected,
                "fill type {input} should map to {expected:?}"
            );
        }
    }

    #[test]
    fn fill_with_image_source_carries_src_attribute() {
        use crate::docx::model::VmlPrimitive;
        let p = parse(
            r#"<pict>
                <rect id="r" style="width:10pt;height:10pt">
                    <fill type="frame" src="watermark.png"/>
                </rect>
            </pict>"#,
        );
        let VmlPrimitive::Rect(r) = &p.primitives[0] else {
            panic!();
        };
        let fill = r.common.fill.as_ref().unwrap();
        assert_eq!(fill.src.as_deref(), Some("watermark.png"));
    }

    #[test]
    fn unknown_vml_element_falls_into_other_and_is_dropped() {
        let p = parse(
            r#"<pict>
                <rect id="r" style="width:10pt;height:10pt"/>
                <unknownVmlThing foo="bar"/>
            </pict>"#,
        );
        // Only the rect survives; the unknown element is silently
        // discarded by `VmlPrimitiveXml::Other` → `into_model = None`.
        assert_eq!(p.primitives.len(), 1);
        assert!(matches!(p.primitives[0], VmlPrimitive::Rect(_)));
    }
}

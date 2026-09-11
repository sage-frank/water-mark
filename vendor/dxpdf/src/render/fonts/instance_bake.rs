//! Baking a variable font's named-instance coordinates into a static SFNT —
//! issue #113.
//!
//! Skia's `Typeface::clone_with_arguments` (`super::apply_variation`) makes an
//! instanced typeface measure and paint correctly, but the bytes a PDF
//! ultimately embeds for it are still the font's *default* location — neither
//! `Typeface::to_font_data` nor a table copy can bake a design-space location
//! into `glyf`/`gvar`. This module is the fix: it evaluates every glyph at the
//! pinned location through `fontcull-skrifa`'s outline and metrics providers
//! (which already resolve composite glyphs and IUP-interpolated deltas
//! internally — that is exactly the risk a hand-rolled interpolator would
//! reintroduce) and writes a fresh, static `glyf`/`loca`/`hmtx` set with
//! `fontcull-write-fonts`, patching `head`'s bbox/`indexToLocFormat` and
//! `hhea.numberOfHMetrics` to match.
//!
//! An `MVAR` table, when the font has one, carries deltas for metrics that
//! live *outside* `glyf` — `hhea` ascender/descender/line gap, `OS/2` win
//! ascent/descent/cap height/x-height/strikeout, `post` underline
//! position/thickness — and dropping `MVAR` without applying those first
//! would leave a baked instance's own metric tables at the *default*
//! location's values: wrong in the same way an un-baked `glyf` would be, just
//! harder to notice, since nothing painted by this renderer reads them back
//! (layout measures off the live `clone_with_arguments` typeface, never the
//! embedded bytes). Each tag's delta is applied straight to its own field via
//! `Mvar::metric_delta`, not through a higher-level "resolved metrics" API:
//! such an API picks one of `hhea` or `OS/2`'s typo metrics as *the* line
//! height, which is useful for a renderer but wrong for a byte-accurate patch
//! that must not conflate which table a delta belongs to.
//!
//! Everything else — `cmap`, `name`, layout tables, and every table this
//! module does not itself touch — carries over unchanged. The
//! variable-specific tables (`fvar`, `gvar`, `avar`, `HVAR`, `VVAR`, `MVAR`,
//! `STAT`, `cvar`) are dropped, since the result no longer varies; `MVAR`'s
//! own remaining tags go with it unapplied — vertical metrics (`vhea`,
//! meaningless here: this renderer has no vertical-text layout to feed them),
//! caret slope/offset (consulted only by an interactive editor's cursor,
//! never a static PDF page), subscript/superscript size and offset (`dxpdf`
//! positions `vertAlign` text itself, at layout time, rather than asking the
//! embedded font's own hinting for it), and `gasp` (a rasterizer's
//! grid-fitting hint). None of these affect what a PDF viewer draws or
//! measures.
//!
//! # What this does not handle
//!
//! Named, not silently skipped — [`InstanceBakeFailure`] states why:
//!
//! - **CFF2-flavored variable fonts.** `fontcull-write-fonts` has no general
//!   CFF charstring builder to target; converting CFF2 outlines to CFF is a
//!   separate boundary this route does not cross (issue #113's own text).
//! - **WOFF/WOFF2-compressed source bytes.** Unwrapping WOFF2 today only
//!   exists in the top-level, `subset-fonts`-gated `fontcull` crate — pulling
//!   that in here would make the one fix meant to work in *every* build
//!   configuration depend on an optional feature for one input shape. DOCX
//!   never embeds fonts as WOFF2 (checked against how
//!   `FontRegistry::register_embedded` stores bytes: no unwrap happens there
//!   either), so in practice this only bites a WOFF2-flavored **host** font.
//! - **A `spec_next` cubic-outline glyph.** `glyf` is quadratic-only outside
//!   an unstabilized spec extension. Real fonts do not produce these today;
//!   this exists for totality, not because it is expected to fire.
//!
//! Baking always covers the *entire* glyph set — never scoped to the
//! codepoints a render actually used. `fontcull`'s own (still-optional)
//! codepoint subsetting runs on the result exactly as it does for any other
//! static face; duplicating its composite-closure-correct glyph trimming here
//! would be unmeasured, premature work.

use fontcull_font_types::{FWord, Fixed, Tag};
use fontcull_read_fonts::tables::mvar::tags as mvar_tags;
use fontcull_read_fonts::{FontRef, TableProvider};
use fontcull_skrifa::{
    instance::Size,
    outline::{DrawSettings, OutlinePen},
    setting::VariationSetting,
    GlyphId, MetadataProvider,
};
use fontcull_write_fonts::{
    from_obj::ToOwnedTable,
    tables::{
        glyf::{Glyf, GlyfLocaBuilder, Glyph, SimpleGlyph},
        hmtx::{Hmtx, LongMetric},
        loca::{Loca, LocaFormat},
    },
    FontBuilder,
};
use kurbo::BezPath;

use super::opentype::FontFormat;
use super::VariationInstance;

/// Why `bake_instance` could not produce baked bytes for an instance.
///
/// A named variant per cause, not a `String` — see this module's doc for what
/// each one means and why it is a real, permanent boundary rather than a gap
/// to close later (except [`Failed`](Self::Failed), which is genuinely
/// unexpected: a parse, instancing, or serialization error this route did not
/// anticipate).
#[derive(Clone, Debug, PartialEq)]
pub enum InstanceBakeFailure {
    Cff2NotSupported,
    CompressedSourceUnsupported,
    CubicOutlineUnsupported,
    Failed(String),
}

impl std::fmt::Display for InstanceBakeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cff2NotSupported => write!(f, "CFF2 outlines are not instanced by this route"),
            Self::CompressedSourceUnsupported => {
                write!(f, "the source bytes are WOFF/WOFF2-compressed")
            }
            Self::CubicOutlineUnsupported => {
                write!(f, "the font uses spec_next cubic glyf outlines")
            }
            Self::Failed(message) => write!(f, "{message}"),
        }
    }
}

/// Bake `instance`'s coordinates into a standalone, static SFNT.
///
/// `bytes` is the *un-instanced* source — a plain SFNT or a TrueType
/// Collection, selected by `collection_index` (0 for a non-collection font).
/// The result is always a single-face, non-variable SFNT: glyph IDs, `cmap`,
/// and every table this function does not itself rebuild are unchanged from
/// the source face, so a caller that already resolved codepoints/advances
/// against the original numbering does not need to re-resolve anything.
pub(crate) fn bake_instance(
    bytes: &[u8],
    collection_index: u32,
    instance: &VariationInstance,
) -> Result<Vec<u8>, InstanceBakeFailure> {
    if matches!(FontFormat::detect(bytes), Ok(FontFormat::Woff(_))) {
        return Err(InstanceBakeFailure::CompressedSourceUnsupported);
    }

    let font = FontRef::from_index(bytes, collection_index)
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?;

    if font.data_for_tag(Tag::new(b"CFF2")).is_some() {
        return Err(InstanceBakeFailure::Cff2NotSupported);
    }

    let location = font.axes().location(
        instance
            .coords
            .iter()
            .map(|c| VariationSetting::new(Tag::new(&c.axis.0), c.value)),
    );

    let num_glyphs = font
        .maxp()
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .num_glyphs();
    let outlines = font.outline_glyphs();
    let metrics = font.glyph_metrics(Size::unscaled(), &location);

    let mut glyphs: Vec<Glyph> = Vec::with_capacity(num_glyphs as usize);
    let mut font_bbox: Option<(i16, i16, i16, i16)> = None;
    for gid in 0..num_glyphs {
        let glyph_id = GlyphId::from(gid);
        let mut pen = PathPen::default();
        if let Some(outline) = outlines.get(glyph_id) {
            outline
                .draw(
                    DrawSettings::unhinted(Size::unscaled(), &location),
                    &mut pen,
                )
                .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?;
        }
        if pen.cubic {
            return Err(InstanceBakeFailure::CubicOutlineUnsupported);
        }

        let glyph = if pen.path.elements().is_empty() {
            Glyph::Empty
        } else {
            let simple = SimpleGlyph::from_bezpath(&pen.path)
                .map_err(|e| InstanceBakeFailure::Failed(format!("{e:?}")))?;
            let b = simple.bbox;
            font_bbox = Some(match font_bbox {
                None => (b.x_min, b.y_min, b.x_max, b.y_max),
                Some((x0, y0, x1, y1)) => (
                    x0.min(b.x_min),
                    y0.min(b.y_min),
                    x1.max(b.x_max),
                    y1.max(b.y_max),
                ),
            });
            Glyph::Simple(simple)
        };
        glyphs.push(glyph);
    }

    let mut glyf_builder = GlyfLocaBuilder::new();
    for glyph in &glyphs {
        glyf_builder
            .add_glyph(glyph)
            .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?;
    }
    let (glyf, loca, loca_format): (Glyf, Loca, LocaFormat) = glyf_builder.build();

    // Every glyph gets an explicit `hmtx` entry — no attempt to reproduce the
    // source's trailing-run compression. Correct either way; the source's own
    // compression choice has no bearing on a table this function rebuilds
    // from scratch.
    let h_metrics: Vec<LongMetric> = (0..num_glyphs)
        .map(|gid| {
            let advance = metrics
                .advance_width(GlyphId::from(gid))
                .unwrap_or(0.0)
                .round() as u16;
            let side_bearing = match &glyphs[gid as usize] {
                Glyph::Simple(s) => s.bbox.x_min,
                _ => 0,
            };
            LongMetric {
                advance,
                side_bearing,
            }
        })
        .collect();
    let hmtx = Hmtx {
        h_metrics,
        left_side_bearings: Vec::new(),
    };

    let mut head: fontcull_write_fonts::tables::head::Head = font
        .head()
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .to_owned_table();
    head.index_to_loc_format = match loca_format {
        LocaFormat::Short => 0,
        LocaFormat::Long => 1,
    };
    if let Some((x_min, y_min, x_max, y_max)) = font_bbox {
        head.x_min = x_min;
        head.y_min = y_min;
        head.x_max = x_max;
        head.y_max = y_max;
    }

    // An MVAR table, when present, carries deltas for metric fields outside
    // `glyf` — applied here, per tag, straight to the field it names. Absent
    // a delta (no `MVAR` table, or a tag it doesn't carry), `mvar_delta`
    // reads as zero, so every field below is simply left at the value it
    // already had.
    let mvar = font.mvar().ok();
    let coords = location.coords();
    let mvar_delta = |tag: Tag| -> i32 {
        mvar.as_ref()
            .and_then(|table| table.metric_delta(tag, coords).ok())
            .map(Fixed::to_i32)
            .unwrap_or(0)
    };

    let mut hhea: fontcull_write_fonts::tables::hhea::Hhea = font
        .hhea()
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .to_owned_table();
    hhea.number_of_h_metrics = num_glyphs;
    hhea.ascender = add_fword_delta(hhea.ascender, mvar_delta(mvar_tags::HASC));
    hhea.descender = add_fword_delta(hhea.descender, mvar_delta(mvar_tags::HDSC));
    hhea.line_gap = add_fword_delta(hhea.line_gap, mvar_delta(mvar_tags::HLGP));

    let maxp: fontcull_write_fonts::tables::maxp::Maxp = font
        .maxp()
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .to_owned_table();

    // Both optional in the schema (though not in practice for a real font);
    // `None` here only ever means the source didn't carry the table either,
    // in which case there is nothing to patch and the raw-copy loop below
    // already has nothing to skip.
    let os2 = font.os2().ok().map(|os2| {
        let mut os2: fontcull_write_fonts::tables::os2::Os2 = os2.to_owned_table();
        os2.us_win_ascent = add_u16_delta(os2.us_win_ascent, mvar_delta(mvar_tags::HCLA));
        os2.us_win_descent = add_u16_delta(os2.us_win_descent, mvar_delta(mvar_tags::HCLD));
        os2.y_strikeout_size = add_i16_delta(os2.y_strikeout_size, mvar_delta(mvar_tags::STRS));
        os2.y_strikeout_position =
            add_i16_delta(os2.y_strikeout_position, mvar_delta(mvar_tags::STRO));
        os2.sx_height = os2
            .sx_height
            .map(|v| add_i16_delta(v, mvar_delta(mvar_tags::XHGT)));
        os2.s_cap_height = os2
            .s_cap_height
            .map(|v| add_i16_delta(v, mvar_delta(mvar_tags::CPHT)));
        os2
    });
    let post = font.post().ok().map(|post| {
        let mut post: fontcull_write_fonts::tables::post::Post = post.to_owned_table();
        post.underline_position =
            add_fword_delta(post.underline_position, mvar_delta(mvar_tags::UNDO));
        post.underline_thickness =
            add_fword_delta(post.underline_thickness, mvar_delta(mvar_tags::UNDS));
        post
    });

    let mut builder = FontBuilder::new();
    builder
        .add_table(&glyf)
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .add_table(&loca)
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .add_table(&hmtx)
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .add_table(&head)
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .add_table(&hhea)
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?
        .add_table(&maxp)
        .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?;
    if let Some(os2) = &os2 {
        builder
            .add_table(os2)
            .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?;
    }
    if let Some(post) = &post {
        builder
            .add_table(post)
            .map_err(|e| InstanceBakeFailure::Failed(e.to_string()))?;
    }

    // Every other table carries over unchanged, except the variable-specific
    // ones — the whole point of baking is that the result no longer varies.
    const REBUILT: [[u8; 4]; 8] = [
        *b"glyf", *b"loca", *b"hmtx", *b"head", *b"hhea", *b"maxp", *b"OS/2", *b"post",
    ];
    const VARIABLE: [[u8; 4]; 8] = [
        *b"fvar", *b"gvar", *b"avar", *b"HVAR", *b"VVAR", *b"MVAR", *b"STAT", *b"cvar",
    ];
    for record in font.table_directory().table_records() {
        let tag = record.tag();
        let bytes = tag.into_bytes();
        if REBUILT.contains(&bytes) || VARIABLE.contains(&bytes) {
            continue;
        }
        if let Some(data) = font.data_for_tag(tag) {
            builder.add_raw(tag, data.as_bytes().to_vec());
        }
    }

    Ok(builder.build())
}

/// Adds an MVAR delta to an `FWord` (`hhea`/`post` line-metric) field,
/// saturating rather than overflowing — `bytes` may be a malformed font, and
/// a delta this far out of range is already meaningless, not worth a panic.
fn add_fword_delta(base: FWord, delta: i32) -> FWord {
    FWord::from(add_i16_delta(i16::from(base), delta))
}

fn add_i16_delta(base: i16, delta: i32) -> i16 {
    (i32::from(base) + delta).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn add_u16_delta(base: u16, delta: i32) -> u16 {
    (i32::from(base) + delta).clamp(0, i32::from(u16::MAX)) as u16
}

/// Accumulates one glyph's outline into a [`BezPath`], flagging a cubic
/// segment rather than approximating it — see [`InstanceBakeFailure::CubicOutlineUnsupported`].
#[derive(Default)]
struct PathPen {
    path: BezPath,
    cubic: bool,
}

impl OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to((x as f64, y as f64));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to((x as f64, y as f64));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.path
            .quad_to((cx0 as f64, cy0 as f64), (x as f64, y as f64));
    }

    fn curve_to(&mut self, _cx0: f32, _cy0: f32, _cx1: f32, _cy1: f32, _x: f32, _y: f32) {
        self.cubic = true;
    }

    fn close(&mut self) {
        self.path.close_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fonts::opentype::fvar::{AxisTag, VariationCoord};

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test-files/fonts")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| {
            panic!("fixture '{name}' is missing ({e}) — run scripts/make_font_fixtures.py")
        })
    }

    fn instance(subfamily: &str, wght: f32, wdth: f32) -> VariationInstance {
        VariationInstance {
            subfamily: subfamily.to_string(),
            post_script_name: None,
            coords: vec![
                VariationCoord {
                    axis: AxisTag(*b"wght"),
                    value: wght,
                },
                VariationCoord {
                    axis: AxisTag(*b"wdth"),
                    value: wdth,
                },
            ],
        }
    }

    /// The `glyf` table's raw bytes out of a baked SFNT — a cheap, honest proxy
    /// for "the outline changed": identical geometry compiles to identical
    /// bytes (`from_bezpath` is deterministic), and this needs no OpenType
    /// parsing of the *output* to check.
    fn glyf_bytes(sfnt: &[u8]) -> Vec<u8> {
        let font = FontRef::from_index(sfnt, 0).expect("baked bytes must be a valid SFNT");
        font.data_for_tag(Tag::new(b"glyf"))
            .expect("baked bytes must carry glyf")
            .as_bytes()
            .to_vec()
    }

    /// The literal acceptance criterion: baking a named instance yields
    /// outlines that differ from the default location's, and — since the
    /// fixture's four instances sit at four distinct points in the design
    /// space (see `scripts/make_font_fixtures.py::build_variable`) — from each
    /// other too.
    #[test]
    fn baked_instances_have_distinct_outlines() {
        let bytes = fixture("DxVariable.ttf");
        let default = glyf_bytes(&bytes);

        let regular =
            glyf_bytes(&bake_instance(&bytes, 0, &instance("Regular", 400.0, 100.0)).unwrap());
        let semibold =
            glyf_bytes(&bake_instance(&bytes, 0, &instance("SemiBold", 600.0, 100.0)).unwrap());
        let bold = glyf_bytes(&bake_instance(&bytes, 0, &instance("Bold", 700.0, 100.0)).unwrap());
        let condensed_semibold = glyf_bytes(
            &bake_instance(&bytes, 0, &instance("Condensed SemiBold", 600.0, 75.0)).unwrap(),
        );

        assert_eq!(
            regular, default,
            "the default-location instance must round-trip unchanged"
        );
        let all = [&semibold, &bold, &condensed_semibold];
        for (i, a) in all.iter().enumerate() {
            assert_ne!(**a, default, "instance {i} must differ from the default");
            for b in &all[i + 1..] {
                assert_ne!(a, b, "distinct instances must bake to distinct outlines");
            }
        }
    }

    /// The `MVAR` half of issue #113: baking also applies the font's own
    /// `MVAR` deltas to `hhea`/`post` fields, not just `glyf`. Expected
    /// values are hand-computed from the fixture's base values (`hhea`
    /// ascender/descender 800/-200, `post` underline position/thickness
    /// 0/0 — see `scripts/make_font_fixtures.py::build_variable`) and its
    /// single default→`wght`-max tent (the same shape as `gvar`'s, so
    /// `Regular` again reads as an exact match to the unbaked values); cross-
    /// checked independently against `fontTools.varLib.instancer` when the
    /// fixture was designed, not derived from this function's own output.
    #[test]
    fn baked_instances_apply_mvar_deltas() {
        let bytes = fixture("DxVariable.ttf");

        fn hhea_and_underline(sfnt: &[u8]) -> (i16, i16, i16, i16) {
            let font = FontRef::from_index(sfnt, 0).expect("baked bytes must be a valid SFNT");
            let hhea = font.hhea().expect("baked bytes must carry hhea");
            let post = font.post().expect("baked bytes must carry post");
            (
                hhea.ascender().to_i16(),
                hhea.descender().to_i16(),
                post.underline_position().to_i16(),
                post.underline_thickness().to_i16(),
            )
        }

        let regular = bake_instance(&bytes, 0, &instance("Regular", 400.0, 100.0)).unwrap();
        let semibold = bake_instance(&bytes, 0, &instance("SemiBold", 600.0, 100.0)).unwrap();
        let bold = bake_instance(&bytes, 0, &instance("Bold", 700.0, 100.0)).unwrap();

        assert_eq!(
            hhea_and_underline(&regular),
            (800, -200, 0, 0),
            "the default location must round-trip the font's own base values"
        );
        assert_eq!(
            hhea_and_underline(&semibold),
            (840, -216, -8, 16),
            "SemiBold sits 0.4 of the way through the default-to-max tent"
        );
        assert_eq!(
            hhea_and_underline(&bold),
            (860, -224, -12, 24),
            "Bold sits 0.6 of the way through the default-to-max tent"
        );
    }

    /// A minimal, hand-built SFNT whose table directory names `CFF2` — enough
    /// to exercise the check without a fully valid CFF2 font (which
    /// `bake_instance` never needs to parse: presence alone is disqualifying).
    fn sfnt_with_table(tag: &[u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // sfnt version
        out.extend_from_slice(&1u16.to_be_bytes()); // numTables
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // searchRange/entrySelector/rangeShift (unchecked by the reader)
        let table_start = 12 + 16;
        out.extend_from_slice(tag); // tag
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum (unchecked)
        out.extend_from_slice(&(table_start as u32).to_be_bytes()); // offset
        out.extend_from_slice(&4u32.to_be_bytes()); // length
        out.extend_from_slice(&[0, 0, 0, 0]); // the table's own (unparsed) bytes
        out
    }

    #[test]
    fn a_cff2_font_is_reported_not_baked() {
        let bytes = sfnt_with_table(b"CFF2");
        assert_eq!(
            bake_instance(&bytes, 0, &instance("SemiBold", 600.0, 100.0)),
            Err(InstanceBakeFailure::Cff2NotSupported)
        );
    }

    #[test]
    fn a_woff2_font_is_reported_not_baked() {
        let mut bytes = b"wOF2".to_vec();
        bytes.extend_from_slice(&[0u8; 40]);
        assert_eq!(
            bake_instance(&bytes, 0, &instance("SemiBold", 600.0, 100.0)),
            Err(InstanceBakeFailure::CompressedSourceUnsupported)
        );
    }

    #[test]
    fn a_truncated_font_fails_cleanly_not_a_panic() {
        let bytes = [0x00, 0x01, 0x00, 0x00, 0xff, 0xff];
        let result = bake_instance(&bytes, 0, &instance("SemiBold", 600.0, 100.0));
        assert!(matches!(result, Err(InstanceBakeFailure::Failed(_))));
    }
}

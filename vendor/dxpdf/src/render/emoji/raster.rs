//! Emoji cluster rasterization with per-render cache.
//!
//! Skia's PDF backend cannot emit color glyph tables (COLR/CPAL, CBDT/CBLC,
//! sbix, SVG-in-OT) — but its raster backend honors all four. We rasterize
//! emoji clusters onto an offscreen surface using the raster backend, snapshot
//! to an [`Image`], and let the painter embed it in the PDF at the run's
//! typographic position.
//!
//! The cache key includes the cluster text (NFC-normalized per UAX #15), the
//! typeface id, the requested point size, and the super-sample factor;
//! identical inputs yield a single rasterization shared across the document.

use std::collections::HashMap;

use skia_safe::{surfaces, Color, Font, Image, Paint, PaintStyle, Point};
use unicode_normalization::UnicodeNormalization;

use crate::render::dimension::Pt;
use crate::render::emoji::cluster::EmojiCluster;
use crate::render::fonts::{TypefaceEntry, TypefaceId};
use crate::render::geometry::PtSize;
use crate::render::shape::Shaper;

// ─── Public ADTs ─────────────────────────────────────────────────────────────

/// Pixel density at which clusters are rasterized.
///
/// PDF viewers re-rasterize images at the user's chosen zoom; super-sampling
/// here trades larger PDF size for crispness when zoomed in. For sbix /
/// CBDT bitmap-emoji fonts (Apple Color Emoji, Noto Color Emoji), higher
/// super-sampling also lets Skia pick a higher-resolution source bitmap
/// from the font's strike table, sharpening the result before any paint-
/// time downsampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuperSample {
    /// 1 pixel per Pt — minimum size, soft when zoomed.
    OnePerPt,
    /// 2 pixels per Pt — soft at print quality.
    TwoPerPt,
    /// 3 pixels per Pt — moderate.
    ThreePerPt,
    /// 4 pixels per Pt — default. Drives Skia to pick a 64–96px sbix
    /// strike at typical body-text sizes, which downsamples cleanly via
    /// Mitchell cubic at paint time.
    FourPerPt,
    /// 6 pixels per Pt — print-quality / display-zoom-friendly.
    SixPerPt,
}

impl SuperSample {
    pub const fn factor(self) -> f32 {
        match self {
            SuperSample::OnePerPt => 1.0,
            SuperSample::TwoPerPt => 2.0,
            SuperSample::ThreePerPt => 3.0,
            SuperSample::FourPerPt => 4.0,
            SuperSample::SixPerPt => 6.0,
        }
    }
}

/// Ceiling on the rasterized surface, in pixels.
///
/// The surface size is only a *resolution* choice: the painter draws the
/// snapshot into [`EmojiImage::draw_size`] regardless of how many pixels back
/// it, so reducing the pixel count of an absurd cluster changes nothing about
/// where the emoji lands or how large it appears — only how crisp it is.
///
/// Without a ceiling the surface is `target × factor` with nothing relating it
/// to the page it will be drawn on. `w:sz` is unbounded in the file format
/// (Word's UI caps it at 1638 half-points, but the schema does not), and one
/// run at `<w:sz w:val="20000"/>` — 10 000 pt — asks for a surface of roughly
/// 40 000 × 55 000 px, or 8.8 GB.
///
/// 8 Mi px is ≈ 33 MB at N32 premul, and exceeds a full A4 page rasterized at
/// the default 4 px/pt (2380 × 3368 = 8.0 M px). No emoji that fits on a page
/// is affected; anything larger is clipped by the page anyway, so the ceiling
/// costs no visible resolution.
const MAX_RASTER_PIXELS: f64 = (8 * 1024 * 1024) as f64;

/// The super-sample factor actually used for `target`: `requested`, reduced
/// uniformly if `target × requested` would exceed [`MAX_RASTER_PIXELS`].
///
/// Reducing the *factor* rather than the pixel dimensions keeps the surface
/// derivation single-sourced — the glyph size, the in-surface baseline, and
/// both dimensions all scale from this one number, so a clamped surface stays
/// internally consistent instead of drawing a full-size glyph onto a shrunken
/// canvas. It also preserves the image/target aspect equality that keeps
/// `draw_image_rect` isotropic (see `rasterize`).
///
/// A degenerate aspect (one axis near zero, the other enormous) can still ask
/// for a surface that fits the area budget but overflows a single dimension;
/// that is caught downstream by the allocation returning `None` rather than by
/// distorting the aspect here.
fn effective_super_sample(target: PtSize, requested: f32) -> f32 {
    let (w, h) = (target.width.raw() as f64, target.height.raw() as f64);
    let requested_f64 = requested as f64;
    if !w.is_finite() || !h.is_finite() || !requested_f64.is_finite() {
        return requested;
    }
    // `rasterize_uncached` rounds each axis *up*, so the budget has to be
    // stated for the rounded-up surface — `ceil(x) < x + 1` — or the ceiling
    // is one that only nearly holds.
    let pixels = (w * requested_f64 + 1.0) * (h * requested_f64 + 1.0);
    if pixels <= MAX_RASTER_PIXELS {
        return requested;
    }
    let (a, b) = (w * h, w + h);
    if a <= 0.0 {
        // A zero-area rect can exceed the budget on one axis alone, but has
        // no factor that fixes it. The allocation guard refuses the surface.
        return requested;
    }
    // Largest factor satisfying the same inequality: the positive root of
    // `a·f² + b·f + (1 − MAX_RASTER_PIXELS) = 0`.
    let reduced = ((b * b + 4.0 * a * (MAX_RASTER_PIXELS - 1.0)).sqrt() - b) / (2.0 * a);
    log::warn!(
        "[emoji] cluster rect {w:.0}×{h:.0}pt at {requested} px/pt would need {pixels:.0} px \
         (limit {MAX_RASTER_PIXELS:.0}); rasterizing at {reduced:.4} px/pt instead"
    );
    reduced as f32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RasterConfig {
    pub super_sample: SuperSample,
}

impl Default for RasterConfig {
    fn default() -> Self {
        Self {
            super_sample: SuperSample::FourPerPt,
        }
    }
}

/// Cache key for a rasterized cluster.
///
/// Cluster text is NFC-normalized (UAX #15) so canonically-equivalent inputs
/// share a slot. Size, scale, and target dimensions are stored as the bit
/// pattern of the f32 so the key is hashable and comparison is exact (no
/// rounding-induced misses).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EmojiKey {
    pub cluster: String,
    pub typeface_id: TypefaceId,
    pub size_bits: u32,
    pub scale_bits: u32,
    /// Target image width in Pt as f32 bits. The rasterizer guarantees
    /// image_aspect == rect_aspect to prevent anisotropic stretching at
    /// paint time, so the cache key includes the target dimensions.
    pub target_w_bits: u32,
    pub target_h_bits: u32,
}

impl EmojiKey {
    pub fn new(
        text: &str,
        typeface: &TypefaceEntry,
        size: Pt,
        scale: SuperSample,
        target: PtSize,
    ) -> Self {
        Self {
            cluster: text.nfc().collect(),
            typeface_id: TypefaceId::from(&typeface.typeface),
            size_bits: f32::from(size).to_bits(),
            scale_bits: scale.factor().to_bits(),
            target_w_bits: target.width.raw().to_bits(),
            target_h_bits: target.height.raw().to_bits(),
        }
    }
}

/// A rasterized emoji image plus the metadata needed to place it at paint
/// time at the run's baseline.
#[derive(Clone, Debug)]
pub struct EmojiImage {
    /// Skia image snapshot. Cheap to clone (reference-counted internally).
    pub image: Image,
    /// Pixel dimensions of the underlying surface (width, height).
    pub pixels: (i32, i32),
    /// The size at which to draw the image in the PDF, in original Pt units
    /// (i.e. de-scaled from the super-sampled raster).
    pub draw_size: PtSize,
    /// Distance from the run's baseline to the top of `draw_size`, in Pt.
    /// Positive values mean the top sits above the baseline (the typical
    /// case for an emoji whose bounds lie above the baseline).
    pub baseline_offset: Pt,
}

// ─── Rasterizer ──────────────────────────────────────────────────────────────

/// Per-render rasterizer that owns the image cache. Lifetime equals the
/// painter's.
///
/// It also owns the [`Shaper`], built once per render. Shaping goes
/// through the typeface rather than extracted font bytes, which is what keeps
/// a ~183 MB emoji font off the heap — see [`crate::render::shape`].
pub struct EmojiRasterizer {
    config: RasterConfig,
    cache: HashMap<EmojiKey, EmojiImage>,
    /// `None` if this Skia build exposes no HarfBuzz shaper; rasterization
    /// then takes the cmap-only `draw_str` fallback.
    shaper: Option<Shaper>,
}

impl Default for EmojiRasterizer {
    fn default() -> Self {
        Self::new(RasterConfig::default())
    }
}

impl EmojiRasterizer {
    pub fn new(config: RasterConfig) -> Self {
        Self {
            config,
            cache: HashMap::new(),
            shaper: Shaper::new().ok(),
        }
    }

    pub fn config(&self) -> RasterConfig {
        self.config
    }

    pub fn cached_count(&self) -> usize {
        self.cache.len()
    }

    /// Rasterize `cluster` at `size` using `typeface`, or return the cached
    /// image if previously seen.
    ///
    /// `target` is the layout's reserved rect (in Pt). The rasterizer
    /// allocates an image with **the same aspect ratio** as `target`,
    /// scaled by the super-sample factor — this is critical because
    /// `Canvas::draw_image_rect` does anisotropic scaling when image
    /// aspect ≠ rect aspect, distorting the emoji. By matching aspects
    /// here, the painter's image-to-rect scaling becomes uniform and
    /// the emoji's visual content is preserved.
    ///
    /// Internally shapes via Skia's HarfBuzz (GSUB-aware) so multi-codepoint
    /// emoji sequences (keycap, modifier, ZWJ, RIS) render as their
    /// ligated single glyph — `canvas.draw_str` would have rendered each
    /// codepoint separately. See `shape.rs` for the shaper.
    ///
    /// `typeface` is guaranteed by the type system to be a real
    /// [`TypefaceEntry`] — callers that hold an [`EmojiTypeface::Unavailable`]
    /// cannot reach this method. (See plan test X8.)
    ///
    /// Returns `None` when the offscreen surface cannot be allocated. Nothing
    /// is cached in that case, so a later call with a smaller `target` still
    /// gets its chance; the caller draws nothing for this cluster.
    ///
    /// [`EmojiTypeface::Unavailable`]: super::resolve::EmojiTypeface::Unavailable
    pub fn rasterize(
        &mut self,
        cluster: &EmojiCluster,
        typeface: &TypefaceEntry,
        size: Pt,
        target: PtSize,
    ) -> Option<&EmojiImage> {
        let scale = self.config.super_sample;
        let key = EmojiKey::new(cluster.text, typeface, size, scale, target);
        if !self.cache.contains_key(&key) {
            let image = rasterize_uncached(
                cluster.text,
                typeface,
                size,
                scale,
                target,
                self.shaper.as_ref(),
            )?;
            self.cache.insert(key.clone(), image);
        }
        self.cache.get(&key)
    }
}

fn rasterize_uncached(
    text: &str,
    typeface: &TypefaceEntry,
    size: Pt,
    scale: SuperSample,
    target: PtSize,
    shaper: Option<&Shaper>,
) -> Option<EmojiImage> {
    // Everything below scales from this one factor — glyph size, surface
    // dimensions, and the in-surface baseline — so a clamped surface stays
    // internally consistent. See `effective_super_sample`.
    let factor = effective_super_sample(target, scale.factor());
    let scaled_size = f32::from(size) * factor;
    let font = Font::from_typeface(typeface.typeface.clone(), scaled_size);

    // Image dimensions are derived from the target rect (× scale). This
    // guarantees image_aspect == target_aspect → uniform scaling at paint
    // time. Anisotropic scaling would distort the emoji (a square keycap
    // squished to a rectangle). Apple Color Emoji's font.metrics() are
    // non-linear across point sizes (ascent+descent ratio = 1.64 at 11pt
    // but 1.37 at 22pt), so deriving image height from the rasterizer's
    // own metrics() would mismatch the layout's rect.
    let width_px = (target.width.raw() * factor).ceil().max(1.0) as i32;
    let height_px = (target.height.raw() * factor).ceil().max(1.0) as i32;

    // Baseline within the surface: the layout reserves space using the
    // *original*-size ascent (via `TextMeasurer::measure_with_typeface`,
    // which calls `font.metrics()` at the run's font size). The painter
    // then places the rect with `top_y = baseline_y - metrics.ascent`.
    // We must therefore position the glyph baseline within the image at
    // `original_ascent × factor`, NOT the scaled-size font's own ascent —
    // for fonts with non-linear metrics (Apple Color Emoji's ascent+descent
    // ratio is 1.64 at 11pt but 1.37 at 22pt), the two differ and the
    // emoji ends up floating above or below the line's baseline.
    let original_font = Font::from_typeface(typeface.typeface.clone(), f32::from(size));
    let (_, original_metrics) = original_font.metrics();
    let baseline_y_px = -original_metrics.ascent * factor;

    // Try the GSUB-aware shaping path. On any shaping failure (no shaper in
    // this build, or no glyphs produced), fall through to the cmap-only
    // `draw_str` path so the rasterizer still produces output.
    let shaped = shaper.and_then(|s| {
        s.shape(
            &typeface.typeface,
            text,
            scaled_size,
            // An emoji cluster has no direction of its own — it is one
            // grapheme, and UTS #51 sequences are stored in painting order.
            crate::render::shape::RunDirection::LeftToRight,
        )
        .ok()
    });

    // `None` here means Skia refused the allocation — a degenerate aspect that
    // slipped past the area budget, or genuine memory pressure. Neither is a
    // programming error, so the cluster is dropped rather than panicking
    // through the public `convert()` API.
    let Some(mut surface) = surfaces::raster_n32_premul((width_px, height_px)) else {
        log::warn!(
            "[emoji] could not allocate a {width_px}×{height_px} px surface for cluster \
             {text:?} at {size:?}; the cluster will not be drawn"
        );
        return None;
    };
    let canvas = surface.canvas();

    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_style(PaintStyle::Fill);
    // Color emoji fonts (COLR / CBDT / sbix) ignore the paint color and use
    // their internal palette. Monochrome emoji (e.g. Noto Emoji) honour it.
    // Black is the safest default for monochrome fallbacks.
    paint.set_color(Color::BLACK);

    match shaped {
        Some(run) => {
            // Shaped positions are absolute offsets from the run origin, already
            // in Skia's y-down space, so they pass straight through; the origin
            // carries the baseline (= layout's ascent at the original size,
            // scaled by `factor`).
            let ids: Vec<_> = run.glyphs.iter().map(|g| g.id).collect();
            let positions: Vec<_> = run
                .glyphs
                .iter()
                .map(|g| Point::new(g.x.raw(), g.y.raw()))
                .collect();
            canvas.draw_glyphs_at(&ids, &*positions, (0.0, baseline_y_px), &font, &paint);
        }
        None => {
            // Fallback: cmap-level draw_str, no GSUB, so a multi-codepoint
            // sequence renders as separate glyphs rather than its ligature.
            // It must still honour `baseline_y_px` — `draw_str` takes the
            // baseline origin, so this is the single-run equivalent of the
            // shaped branch above. Landing the glyph by its own ink bounds
            // instead would put identical input at a different height
            // depending only on whether shaping succeeded, and would discard
            // the original-size-ascent correction that
            // non-linear emoji metrics need.
            canvas.draw_str(text, (0.0, baseline_y_px), &font, &paint);
        }
    }

    let image = surface.image_snapshot();

    // The image dimensions exactly match `target × factor` (modulo ceil),
    // so draw_size returns to `target`. The painter draws `image` into a
    // rect of exactly these dimensions for uniform scaling.
    Some(EmojiImage {
        image,
        pixels: (width_px, height_px),
        draw_size: target,
        baseline_offset: Pt::new(baseline_y_px / factor),
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────
//
// `xN_` names are the cache cases this module was built against: repeat
// rasterization of one cluster hitting a single entry, distinct clusters and
// distinct sizes taking distinct entries, non-degenerate image dimensions,
// non-empty pixel data, and canonically-equivalent (NFC) inputs sharing a slot.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::emoji::cluster::{EmojiPresentation, EmojiStructure};
    use crate::render::emoji::resolve::{resolve, EmojiTypeface, RegistryLookup};
    use crate::render::fonts::{FontRegistry, InstanceState, Synthesis, TypefaceOrigin};
    use skia_safe::{FontMgr, FontStyle};

    /// Construct a real `TypefaceEntry` from any host-default font. Used by
    /// the cache-shape tests that don't care about the actual glyphs.
    fn any_typeface() -> TypefaceEntry {
        let mgr = FontMgr::new();
        let tf = mgr
            .legacy_make_typeface(None::<&str>, FontStyle::normal())
            .expect("system has no default typeface — cannot run test");
        let id = TypefaceId::from(&tf);
        TypefaceEntry {
            instance: InstanceState::NotInstanced,
            typeface: tf,
            origin: TypefaceOrigin::System { typeface_id: id },
            synthesis: Synthesis::default(),
        }
    }

    fn single_emoji(text: &'static str) -> EmojiCluster<'static> {
        EmojiCluster {
            text,
            presentation: EmojiPresentation::Emoji,
            structure: EmojiStructure::Single,
        }
    }

    /// Default target rect for tests — non-degenerate, sized at the
    /// 12pt font size used throughout the test suite. Aspect 1:1.6 so
    /// distortion-style assertions can be checked.
    fn default_target() -> PtSize {
        PtSize::new(Pt::new(12.0), Pt::new(18.0))
    }

    /// X1 — same key twice → one cache entry.
    #[test]
    fn x1_same_input_dedupes_in_cache() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let c = single_emoji("\u{1F4DE}");
        let _ = r.rasterize(&c, &tf, Pt::new(12.0), default_target());
        let _ = r.rasterize(&c, &tf, Pt::new(12.0), default_target());
        assert_eq!(r.cached_count(), 1, "identical key must reuse cache slot");
    }

    /// X2 — different cluster text → distinct entries.
    #[test]
    fn x2_distinct_clusters_cache_independently() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let _ = r.rasterize(
            &single_emoji("\u{1F4DE}"),
            &tf,
            Pt::new(12.0),
            default_target(),
        );
        let _ = r.rasterize(
            &single_emoji("\u{1F4E7}"),
            &tf,
            Pt::new(12.0),
            default_target(),
        );
        assert_eq!(r.cached_count(), 2);
    }

    /// X3 — different size → distinct entries.
    #[test]
    fn x3_distinct_sizes_cache_independently() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let c = single_emoji("\u{1F4DE}");
        let _ = r.rasterize(&c, &tf, Pt::new(12.0), default_target());
        let _ = r.rasterize(&c, &tf, Pt::new(24.0), default_target());
        assert_eq!(r.cached_count(), 2);
    }

    /// X4 — pixel dimensions are always at least 1×1.
    #[test]
    fn x4_pixel_dimensions_non_degenerate() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let img = r
            .rasterize(
                &single_emoji("\u{1F4DE}"),
                &tf,
                Pt::new(12.0),
                default_target(),
            )
            .expect("rasterization must succeed for a non-degenerate target")
            .clone();
        assert!(
            img.pixels.0 >= 1,
            "width must be >= 1 px, got {}",
            img.pixels.0
        );
        assert!(
            img.pixels.1 >= 1,
            "height must be >= 1 px, got {}",
            img.pixels.1
        );
        assert!(img.draw_size.width.raw() > 0.0);
        assert!(img.draw_size.height.raw() > 0.0);
    }

    /// X4b — degenerate empty input must not crash. Image dimensions are
    /// governed by the target rect now (so the image aspect matches the
    /// painter's destination rect — see Y_aspect below), so we only
    /// assert non-degeneracy.
    #[test]
    fn x4b_zero_width_input_yields_non_degenerate_surface() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let img = r
            .rasterize(&single_emoji(""), &tf, Pt::new(12.0), default_target())
            .expect("rasterization must succeed for a non-degenerate target")
            .clone();
        assert!(img.pixels.0 >= 1);
        assert!(img.pixels.1 >= 1);
    }

    /// Y_aspect — image surface aspect == target rect aspect. This is
    /// the property that prevents `Canvas::draw_image_rect` from
    /// stretching the emoji at paint time. Without it, fonts whose
    /// `ascent + descent` doesn't scale linearly (Apple Color Emoji's
    /// ratio is 1.64 at 11pt vs 1.37 at 22pt) produce images of one
    /// aspect that get drawn into rects of a different aspect →
    /// distortion.
    #[test]
    fn y_aspect_image_matches_target() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        // Pick an asymmetric target so a regression — using ascent+descent
        // for height — would obviously change the aspect.
        let target = PtSize::new(Pt::new(11.0), Pt::new(18.0));
        let img = r
            .rasterize(&single_emoji("A"), &tf, Pt::new(11.0), target)
            .expect("rasterization must succeed for a non-degenerate target")
            .clone();
        let img_aspect = img.pixels.0 as f32 / img.pixels.1 as f32;
        let target_aspect = target.width.raw() / target.height.raw();
        // Within rounding (ceil + integer pixels) — within 5% of the
        // target aspect.
        let rel_err = (img_aspect - target_aspect).abs() / target_aspect;
        assert!(
            rel_err < 0.05,
            "image aspect {img_aspect:.4} must match target aspect {target_aspect:.4} \
             within rounding (rel err {rel_err:.4})"
        );
    }

    /// X5 — rasterization of a renderable glyph produces non-trivial pixel
    /// data. Skipped on hosts where no color emoji typeface resolves; we
    /// don't bundle fonts, so CI without one passes via a clean skip.
    #[test]
    fn x5_rasterized_image_has_visible_pixels() {
        let registry = FontRegistry::new(FontMgr::new());
        let lookup = RegistryLookup {
            registry: &registry,
        };
        let resolved = resolve(&lookup, None);
        let entry = match resolved {
            EmojiTypeface::Resolved { entry, .. } => entry,
            EmojiTypeface::Unavailable { .. } => {
                eprintln!("skipping X5: no color emoji typeface on this host");
                return;
            }
        };
        let mut r = EmojiRasterizer::default();
        let img = r
            .rasterize(
                &single_emoji("\u{1F4DE}"),
                &entry,
                Pt::new(24.0),
                PtSize::new(Pt::new(24.0), Pt::new(36.0)),
            )
            .expect("rasterization must succeed for a non-degenerate target")
            .clone();
        let peek = img.image.peek_pixels();
        // peek_pixels can return None if the image is GPU-backed; raster
        // images always succeed.
        let pixels = peek.expect("raster image must expose pixel data");
        let bytes = pixels.bytes().expect("RGBA pixel data must be readable");
        assert!(
            bytes.iter().any(|&b| b != 0),
            "rendered emoji must contain at least one non-zero pixel"
        );
    }

    /// X6 — NFC-different but canonically-equivalent inputs share a cache
    /// slot. "é" can be either U+00E9 (precomposed) or U+0065 + U+0301
    /// (combining acute). Both NFC-normalize to U+00E9.
    #[test]
    fn x6_canonically_equivalent_inputs_share_cache() {
        // Note: "é" alone is not classified as emoji by `cluster::classify`
        // (no Emoji property), but the rasterizer doesn't care — the cache
        // key is built from raw text. We exercise the NFC path with two
        // canonically-equivalent representations.
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let precomposed = EmojiCluster {
            text: "\u{00E9}",
            presentation: EmojiPresentation::Emoji,
            structure: EmojiStructure::Single,
        };
        let decomposed = EmojiCluster {
            text: "e\u{0301}",
            presentation: EmojiPresentation::Emoji,
            structure: EmojiStructure::Single,
        };
        let _ = r.rasterize(&precomposed, &tf, Pt::new(12.0), default_target());
        let _ = r.rasterize(&decomposed, &tf, Pt::new(12.0), default_target());
        assert_eq!(
            r.cached_count(),
            1,
            "NFC-equivalent inputs must share a cache slot"
        );
    }

    // ─── Shape invariants ─────────────────────────────────────────────────

    /// SuperSample.factor() is monotonically increasing. Sanity-check the
    /// enum values so a future refactor that swaps factors gets caught.
    #[test]
    fn super_sample_factors_monotonic() {
        assert!(SuperSample::OnePerPt.factor() < SuperSample::TwoPerPt.factor());
        assert!(SuperSample::TwoPerPt.factor() < SuperSample::ThreePerPt.factor());
    }

    /// Pixel surface scales with super-sample factor for outline glyphs.
    /// Use a plain ASCII letter so the test is independent of bitmap-only
    /// (sbix/CBDT) emoji glyph quantization, which selects different
    /// pre-rendered strike sizes at different requested point sizes.
    #[test]
    fn pixel_dimensions_scale_with_super_sample_for_outline_glyphs() {
        let tf = any_typeface();
        let c = single_emoji("A");
        let size = Pt::new(12.0);

        let mut r1 = EmojiRasterizer::new(RasterConfig {
            super_sample: SuperSample::OnePerPt,
        });
        let img1 = r1
            .rasterize(&c, &tf, size, default_target())
            .expect("rasterization must succeed for a non-degenerate target")
            .clone();

        let mut r3 = EmojiRasterizer::new(RasterConfig {
            super_sample: SuperSample::ThreePerPt,
        });
        let img3 = r3
            .rasterize(&c, &tf, size, default_target())
            .expect("rasterization must succeed for a non-degenerate target")
            .clone();

        // Outline glyphs scale linearly: at 3× super-sample the pixel
        // surface should be ~3× larger on each axis.
        assert!(
            img3.pixels.0 >= img1.pixels.0 * 2,
            "3× super-sample width must be at least 2× the 1× width"
        );
        assert!(
            img3.pixels.1 >= img1.pixels.1 * 2,
            "3× super-sample height must be at least 2× the 1× height"
        );

        // Draw size: should match within ceil-then-divide rounding (≤ 2pt).
        let dw1 = img1.draw_size.width.raw();
        let dw3 = img3.draw_size.width.raw();
        assert!(
            (dw1 - dw3).abs() <= 2.0,
            "outline glyph draw widths must match within rounding, got {dw1} vs {dw3}"
        );
    }

    // ─── draw_str fallback (G1#2) ─────────────────────────────────────────

    /// Topmost surface row carrying any ink, or `None` for a blank surface.
    fn first_ink_row(img: &EmojiImage) -> Option<i32> {
        let peek = img.image.peek_pixels()?;
        let row_bytes = peek.row_bytes();
        let bytes = peek.bytes()?;
        (0..img.pixels.1).find(|&y| {
            let start = y as usize * row_bytes;
            let end = (start + row_bytes).min(bytes.len());
            bytes[start..end].iter().any(|&b| b != 0)
        })
    }

    /// The cmap-only fallback (reached when shaping is unavailable) must place
    /// the glyph on the same baseline as the shaped path. It previously
    /// translated by the glyph's own ink bounds, so identical input landed at a
    /// different height depending only on whether shaping happened to succeed.
    #[test]
    fn draw_str_fallback_lands_on_the_same_baseline_as_shaping() {
        let tf = any_typeface();
        let Some(shaper) = Shaper::new().ok() else {
            eprintln!("skipping: no harfbuzz shaper in this build");
            return;
        };
        let size = Pt::new(48.0);
        let target = PtSize::new(Pt::new(48.0), Pt::new(64.0));
        let shaped = rasterize_uncached(
            "A",
            &tf,
            size,
            SuperSample::FourPerPt,
            target,
            Some(&shaper),
        )
        .expect("shaped path must rasterize");
        let fallback = rasterize_uncached("A", &tf, size, SuperSample::FourPerPt, target, None)
            .expect("fallback path must rasterize");

        let shaped_row = first_ink_row(&shaped).expect("shaped path must draw ink");
        let fallback_row = first_ink_row(&fallback).expect("fallback path must draw ink");
        assert!(
            (shaped_row - fallback_row).abs() <= 1,
            "fallback ink starts at row {fallback_row} but shaping puts it at \
             {shaped_row}; the two paths must agree on the baseline"
        );
        // Both must sit *below* the top edge: the baseline is `ascent ×
        // factor` down from it, so a cap-height glyph starts well inside the
        // surface. Ink flush against row 0 is the signature of positioning by
        // the glyph's own bounds instead.
        assert!(
            shaped_row > 1,
            "ink at row {shaped_row} means the baseline was ignored"
        );
    }

    /// The same agreement, on a font whose metrics are *non-linear* across
    /// point sizes — the case the baseline handling exists for. On a Latin
    /// system font `metrics().ascent` at the scaled size and
    /// `ascent(original) × factor` coincide, so the test above cannot tell
    /// them apart.
    ///
    /// Apple Color Emoji diverges, but only below ~24pt: measured
    /// `-ascent / size` is 1.25 at 8–12pt, 1.045 at 22pt and 1.0 from 24pt
    /// up. So this must run at a **small** size — at 12pt × 4 the correct
    /// baseline is `15.0 × 4 = 60` px while the scaled font's own ascent is
    /// 48 px, a 12-row gap. At 48pt both readings are 192 px and the bug is
    /// invisible. Host-conditional, like X5.
    #[test]
    fn draw_str_fallback_honours_the_original_size_ascent() {
        let registry = FontRegistry::new(FontMgr::new());
        let lookup = RegistryLookup {
            registry: &registry,
        };
        let entry = match resolve(&lookup, None) {
            EmojiTypeface::Resolved { entry, .. } => entry,
            EmojiTypeface::Unavailable { .. } => {
                eprintln!("skipping: no color emoji typeface on this host");
                return;
            }
        };
        let Some(shaper) = Shaper::new().ok() else {
            eprintln!("skipping: no harfbuzz shaper in this build");
            return;
        };
        // 12pt: inside the non-linear part of the metric curve.
        let size = Pt::new(12.0);
        let target = default_target();
        let shaped = rasterize_uncached(
            "\u{1F4DE}",
            &entry,
            size,
            SuperSample::FourPerPt,
            target,
            Some(&shaper),
        )
        .expect("shaped path must rasterize");
        let fallback = rasterize_uncached(
            "\u{1F4DE}",
            &entry,
            size,
            SuperSample::FourPerPt,
            target,
            None,
        )
        .expect("fallback path must rasterize");

        let shaped_row = first_ink_row(&shaped).expect("shaped path must draw ink");
        let fallback_row = first_ink_row(&fallback).expect("fallback path must draw ink");
        assert!(
            (shaped_row - fallback_row).abs() <= 1,
            "fallback ink starts at row {fallback_row} but shaping puts it at {shaped_row}; \
             the fallback must scale the *original*-size ascent, not read the scaled font's"
        );
    }

    // ─── Surface ceiling (G1#1) ───────────────────────────────────────────

    /// An oversized target rect is the only thing the ceiling may affect.
    /// Body text must go through untouched, or every emoji in the corpus
    /// re-renders at a different resolution.
    #[test]
    fn ordinary_target_uses_the_requested_super_sample() {
        assert_eq!(effective_super_sample(default_target(), 4.0), 4.0);
        assert_eq!(
            effective_super_sample(PtSize::new(Pt::new(72.0), Pt::new(96.0)), 6.0),
            6.0
        );
    }

    /// The documented boundary: a full A4 page at the default 4 px/pt is
    /// 2380 × 3368 = 8.0 M px, which must still fit under the ceiling.
    /// Nothing that fits on a page may be clamped.
    #[test]
    fn a4_page_at_default_super_sample_is_not_clamped() {
        let a4 = PtSize::new(Pt::new(595.0), Pt::new(842.0));
        assert_eq!(effective_super_sample(a4, 4.0), 4.0);
    }

    /// Past the ceiling the factor is reduced to land *at* the budget —
    /// not below it, which would throw away resolution for nothing.
    #[test]
    fn oversized_target_is_reduced_to_the_pixel_budget() {
        let huge = PtSize::new(Pt::new(5000.0), Pt::new(6000.0));
        let factor = effective_super_sample(huge, 4.0);
        assert!(factor < 4.0, "an oversized target must reduce the factor");
        // Stated for the rounded-up surface, exactly as the ceiling is.
        let area = (5000.0 * factor as f64).ceil() * (6000.0 * factor as f64).ceil();
        assert!(
            area <= MAX_RASTER_PIXELS,
            "clamped area {area} must fit the {MAX_RASTER_PIXELS} px budget"
        );
        assert!(
            area > MAX_RASTER_PIXELS * 0.99,
            "clamped area {area} must use the budget, not undershoot it"
        );
    }

    /// A non-finite rect must not propagate NaN into the surface
    /// dimensions. The factor is returned unchanged; the allocation guard
    /// downstream is what refuses the surface.
    #[test]
    fn non_finite_target_falls_back_to_the_requested_factor() {
        let nan = PtSize::new(Pt::new(f32::NAN), Pt::new(18.0));
        assert_eq!(effective_super_sample(nan, 4.0), 4.0);
        let inf = PtSize::new(Pt::new(12.0), Pt::new(f32::INFINITY));
        assert_eq!(effective_super_sample(inf, 4.0), 4.0);
    }

    /// G1#1 regression. `<w:sz w:val="20000"/>` (10 000 pt) used to ask for
    /// an ~8.8 GB surface and panic through `expect`. The clamp must keep
    /// the placement contract intact: `draw_size` is still the layout's
    /// rect, and the image aspect still matches it (the property that keeps
    /// `draw_image_rect` isotropic).
    #[test]
    fn oversized_cluster_rasterizes_within_the_pixel_budget() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let target = PtSize::new(Pt::new(5000.0), Pt::new(6000.0));
        let img = r
            .rasterize(&single_emoji("A"), &tf, Pt::new(5000.0), target)
            .expect("an oversized cluster must still rasterize, not panic")
            .clone();

        let area = img.pixels.0 as f64 * img.pixels.1 as f64;
        assert!(
            area <= MAX_RASTER_PIXELS,
            "surface {}×{} = {area} px exceeds the {MAX_RASTER_PIXELS} px budget",
            img.pixels.0,
            img.pixels.1
        );
        assert_eq!(
            img.draw_size, target,
            "clamping is a resolution choice — it must not move or resize the emoji"
        );
        let img_aspect = img.pixels.0 as f32 / img.pixels.1 as f32;
        let target_aspect = target.width.raw() / target.height.raw();
        let rel_err = (img_aspect - target_aspect).abs() / target_aspect;
        assert!(
            rel_err < 0.05,
            "clamped image aspect {img_aspect:.4} must still match target \
             {target_aspect:.4} (rel err {rel_err:.4})"
        );
    }

    /// The residual case the area budget cannot reach: an infinite axis
    /// bypasses the factor reduction entirely (there is no finite factor to
    /// solve for), so the surface request saturates and Skia refuses it.
    /// That must surface as `None`, not a panic — and must not poison the
    /// cache, since a later call with a sane rect deserves its own attempt.
    #[test]
    fn unallocatable_surface_yields_none_and_caches_nothing() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let degenerate = PtSize::new(Pt::new(f32::INFINITY), Pt::new(18.0));
        assert!(
            r.rasterize(&single_emoji("A"), &tf, Pt::new(12.0), degenerate)
                .is_none(),
            "a surface Skia refuses must be reported, not panicked on"
        );
        assert_eq!(r.cached_count(), 0, "a failed rasterization must not cache");
    }

    /// The clamp reduces the *factor*, so the glyph size and the in-surface
    /// baseline shrink with the surface. Clamping the pixel dimensions
    /// instead would leave the baseline at `ascent × 4` — far below a
    /// surface only a fraction that tall — and the glyph would be drawn
    /// entirely off-canvas. A blank surface is the sensor for that.
    #[test]
    fn clamped_surface_still_contains_the_glyph() {
        let mut r = EmojiRasterizer::default();
        let tf = any_typeface();
        let target = PtSize::new(Pt::new(5000.0), Pt::new(6000.0));
        let img = r
            .rasterize(&single_emoji("A"), &tf, Pt::new(5000.0), target)
            .expect("an oversized cluster must still rasterize")
            .clone();
        let peek = img
            .image
            .peek_pixels()
            .expect("raster image exposes pixels");
        let bytes = peek.bytes().expect("RGBA pixel data must be readable");
        assert!(
            bytes.iter().any(|&b| b != 0),
            "the glyph must land inside the clamped surface, not below it"
        );
    }
}

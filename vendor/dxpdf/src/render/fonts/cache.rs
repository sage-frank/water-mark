//! Per-component cache of fully-configured Skia [`Font`] objects.
//!
//! Sits one level above [`FontRegistry`]: the registry answers "which typeface
//! does this request resolve to?", this answers "which fully-configured `Font`
//! do I draw with?". Split out so the registry stays about identity and this
//! stays about the hot loop.

use std::collections::HashMap;

use skia_safe::Font;

use super::request::{FaceRequest, Toggle};
use super::FontRegistry;
use crate::render::dimension::Pt;

/// The shear `Font::set_skew_x` applies to synthesise italic (issue #115) —
/// **Word reference render**: this environment has no Word to compare
/// against, so the value is not verified against an actual Word render, only
/// against a cited convention: CSS Fonts Level 4's documented default oblique
/// angle, 14° (`tan(14°) ≈ 0.249`), used here as a non-arbitrary placeholder
/// rather than a guessed one. What would settle it: rendering a synthesised-
/// italic run and comparing the slant against Word's own output for the same
/// document. The sign (which direction the glyph leans) is verified — see
/// `an_oblique_font_leans_the_way_a_reader_expects` below.
const SYNTHETIC_OBLIQUE_SKEW_X: f32 = -0.249;

/// Cache key: the request plus the size, case-folded.
///
/// Keyed on the *toggles* rather than on a resolved weight and slant, so this
/// cache asks the registry exactly what the document asked for. Folding a
/// tri-state onto a `FontStyle` here would reintroduce, one layer down, the
/// collapse that [`super::request`] exists to undo.
#[derive(Hash, Eq, PartialEq)]
struct FontKey {
    family: String,
    /// Font size stored as bits for exact f32 hashing.
    size_bits: u32,
    bold: Toggle,
    italic: Toggle,
}

/// Raw (un-folded) inputs of the most recent [`FontCache::get`] call plus the
/// slot its result lives in. Words within a run share one `FontProps`, so
/// consecutive calls usually match this exactly and skip the `to_lowercase`
/// allocation and the hash lookup entirely.
struct LastCall {
    family: String,
    size_bits: u32,
    bold: Toggle,
    italic: Toggle,
    idx: usize,
}

/// Per-component cache of fully-configured `Font` objects, avoiding repeated
/// [`FontRegistry::resolve`] lookups and `Font::from_typeface` construction.
///
/// `fonts` owns the resolved `Font`s (append-only, so slot indices stay
/// stable); `index` maps a case-folded `FontKey` to a slot; `last` is a
/// one-entry fast path keyed on the raw inputs of the previous call, so the
/// common per-word case costs no allocation and no hashing.
///
/// Must be discarded if the underlying `FontRegistry` is mutated (e.g. by
/// [`FontRegistry::replace_typeface_by_id`]). The render pipeline already
/// creates a fresh `FontCache` for layout and another for paint, with the
/// subset pass in between; no stale Font objects can survive.
#[derive(Default)]
pub struct FontCache {
    fonts: Vec<Font>,
    index: HashMap<FontKey, usize>,
    last: Option<LastCall>,
}

impl FontCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create a `Font` for the given properties.
    pub fn get(
        &mut self,
        registry: &FontRegistry,
        font_family: &str,
        font_size: Pt,
        bold: Toggle,
        italic: Toggle,
    ) -> &Font {
        self.get_indexed(registry, font_family, font_size, bold, italic)
            .1
    }

    /// Like [`get`](Self::get), but also returns the resolved `Font`'s stable
    /// slot index. The index is a cheap integer identity for the (family, size,
    /// weight, slant) tuple — higher-level caches (e.g. the measurer's width
    /// memo) key on it to avoid re-hashing the family string per call.
    pub fn get_indexed(
        &mut self,
        registry: &FontRegistry,
        font_family: &str,
        font_size: Pt,
        bold: Toggle,
        italic: Toggle,
    ) -> (usize, &Font) {
        let size_bits = f32::from(font_size).to_bits();

        // Fast path: identical inputs to the previous call. Exact string
        // comparison keeps this behaviour-identical to the case-folded slow
        // path (a hit resolves to the same `Font`), while avoiding the
        // per-call `to_lowercase` allocation and hash probe. `idx` is copied
        // out so the `last` borrow ends before `fonts` is indexed.
        let hit = self.last.as_ref().and_then(|last| {
            (last.size_bits == size_bits
                && last.bold == bold
                && last.italic == italic
                && last.family == font_family)
                .then_some(last.idx)
        });
        if let Some(idx) = hit {
            return (idx, &self.fonts[idx]);
        }

        // Slow path: case-folded hash lookup. The owned `FontKey` (with its
        // lowercased `String`) is built only here — at most once per distinct
        // (family, size, style), not once per call.
        let key = FontKey {
            family: font_family.to_lowercase(),
            size_bits,
            bold,
            italic,
        };
        let idx = match self.index.get(&key) {
            Some(&i) => i,
            None => {
                let entry = registry.resolve(&FaceRequest::new(font_family, bold, italic));
                let mut font = Font::from_typeface(entry.typeface, f32::from(font_size));
                font.set_subpixel(true);
                font.set_linear_metrics(true);
                font.set_hinting(skia_safe::FontHinting::None);
                // §115: resolution already knows, for certain, whether this
                // face fell short of what was asked — applied here, the one
                // place both the measurer and the painter build a `Font`
                // from a resolved typeface, so measurement and paint can
                // never see a different embolden/skew than each other.
                if entry.synthesis.embolden {
                    font.set_embolden(true);
                }
                if entry.synthesis.oblique {
                    font.set_skew_x(SYNTHETIC_OBLIQUE_SKEW_X);
                }
                let i = self.fonts.len();
                self.fonts.push(font);
                self.index.insert(key, i);
                i
            }
        };
        self.last = Some(LastCall {
            family: font_family.to_owned(),
            size_bits,
            bold,
            italic,
            idx,
        });
        (idx, &self.fonts[idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EmbeddedFontVariant;
    use crate::render::fonts::tests::arbitrary_system_font_bytes;
    use crate::render::fonts::FaceRequest;
    use crate::render::fonts::Toggle;
    use crate::render::fonts::{TypefaceId, TypefaceOrigin};
    use skia_safe::{Data, FontMgr};

    fn fixture_bytes(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test-files/fonts")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| {
            panic!("fixture '{name}' is missing ({e}) — run scripts/make_font_fixtures.py")
        })
    }

    /// A registry carrying the single-face `Dx` fixture, which requesting
    /// bold or italic against always synthesises (nothing bolder or slanted
    /// exists to select instead).
    fn dx_registry() -> FontRegistry {
        let mut r = FontRegistry::new(FontMgr::new());
        r.register_embedded(
            "Dx",
            EmbeddedFontVariant::Regular,
            fixture_bytes("Dx-Regular.ttf"),
        )
        .expect("Dx-Regular.ttf must register");
        r
    }

    #[test]
    fn font_cache_uses_registry_after_replacement() {
        // The cross-cutting invariant: if the registry is mutated and a
        // *new* FontCache is created afterwards, that cache must produce
        // Fonts backed by the new typeface. (Stale FontCaches must be
        // discarded — that contract is satisfied by the pipeline creating
        // fresh caches around the subset pass.)
        let mut r = FontRegistry::new(FontMgr::new());
        let original = r.resolve(&FaceRequest::plain("CacheReplaceProbe"));
        let original_id = TypefaceId::from(&original.typeface);

        let bytes = arbitrary_system_font_bytes();
        let data = Data::new_copy(&bytes);
        let replacement = r
            .font_mgr()
            .new_from_data(&data, 0)
            .expect("replacement typeface should construct");
        let new_id = TypefaceId::from(&replacement);
        assert_ne!(
            new_id, original_id,
            "test precondition — replacement must be a different typeface"
        );
        let replacement_origin = TypefaceOrigin::System {
            typeface_id: new_id,
        };
        r.replace_typeface_by_id(original_id, replacement, replacement_origin);

        let mut fresh_cache = FontCache::new();
        let font = fresh_cache.get(
            &r,
            "CacheReplaceProbe",
            Pt::new(12.0),
            Toggle::Absent,
            Toggle::Absent,
        );
        assert_eq!(
            TypefaceId::from(&font.typeface()),
            new_id,
            "FontCache must observe the post-replacement typeface"
        );
    }

    // ── issue #115: synthesis applied at the shared Font-construction seam ──

    /// `measurer.rs` and `painter.rs` both call `get_indexed`, so pinning it
    /// once here covers both — there is no second call site to duplicate the
    /// test at.
    #[test]
    fn embolden_is_applied_to_the_cached_font() {
        let r = dx_registry();
        let mut cache = FontCache::new();
        let font = cache.get(&r, "Dx", Pt::new(12.0), Toggle::On, Toggle::Absent);
        assert!(
            font.is_embolden(),
            "the single-face fixture has nothing bolder to select"
        );
    }

    #[test]
    fn oblique_is_applied_to_the_cached_font() {
        let r = dx_registry();
        let mut cache = FontCache::new();
        let font = cache.get(&r, "Dx", Pt::new(12.0), Toggle::Absent, Toggle::On);
        assert_ne!(
            font.skew_x(),
            0.0,
            "the single-face fixture has nothing slanted to select"
        );
    }

    #[test]
    fn a_plain_request_synthesises_neither() {
        let r = dx_registry();
        let mut cache = FontCache::new();
        let font = cache.get(&r, "Dx", Pt::new(12.0), Toggle::Absent, Toggle::Absent);
        assert!(!font.is_embolden());
        assert_eq!(font.skew_x(), 0.0);
    }

    /// The `SYNTHETIC_OBLIQUE_SKEW_X` **Word reference render** gap names an
    /// unverified *magnitude*; this pins the *direction* regardless of it —
    /// the top of a straight vertical stroke must lean right of the bottom,
    /// the ordinary italic slant, not its mirror image.
    #[test]
    fn an_oblique_font_leans_the_way_a_reader_expects() {
        let r = dx_registry();
        let mut cache = FontCache::new();
        // A large size keeps the lean well above float noise.
        let font = cache.get(&r, "Dx", Pt::new(1000.0), Toggle::Absent, Toggle::On);
        // Dx-Regular.ttf's glyphs are a rectangle with a straight left edge
        // at font-design x=50 for y in [0,700] (see
        // scripts/make_font_fixtures.py::_outlines) — Skia's glyph path
        // space is Y-down, so the top of the glyph is the most-negative-y
        // point and the bottom is y=0.
        let glyphs = font.text_to_glyphs_vec("A");
        let path = font.get_path(glyphs[0]).expect("glyph must have a path");
        let points = path.points();
        let top_x = points
            .iter()
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .expect("glyph has points")
            .x;
        let bottom_x = points
            .iter()
            .filter(|p| p.y == 0.0)
            .map(|p| p.x)
            .fold(f32::INFINITY, f32::min);
        assert!(
            top_x > bottom_x,
            "the top of the glyph must lean right of the bottom: top={top_x} bottom={bottom_x}"
        );
    }
}

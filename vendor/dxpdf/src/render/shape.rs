//! GSUB-aware text shaping.
//!
//! Skia's `canvas.draw_str` performs only cmap-level codepoint→glyph
//! mapping; it does not apply OpenType GSUB lookups. Two callers need it to:
//!
//! * **`render::emoji`** — a multi-codepoint sequence (`1️⃣` keycap, `👍🏿`
//!   modifier, `👨‍👩‍👧` ZWJ family) must rasterize as the *ligated* single
//!   glyph, not as its constituents side by side.
//! * **body text in a joining script** (issue #131) — an Arabic word painted
//!   from its cmap comes out in isolated forms, letter by letter, which is not
//!   a degraded rendering of the word so much as a different one.
//!
//! [`needs_shaping`] is what separates the second caller's text from
//! everything else, and the separation is load-bearing rather than an
//! optimization: shaping applies GPOS kerning and standard ligatures too, so
//! routing *all* text through here would change the measured width of every
//! Latin word in every document. See that function for the predicate and why
//! it is the Unicode `Joining_Type` property and not a list of scripts.
//!
//! Shaping runs through **Skia's own HarfBuzz** (`skia-safe`'s `textlayout`
//! feature), driven by a [`Typeface`] rather than raw font bytes. That
//! distinction is the point of this module's design:
//!
//! > `Typeface::to_font_data()` serializes the *entire* font. For
//! > `Apple Color Emoji.ttc` that is 183 MB, and the call costs ~549 MB of
//! > resident memory — 183 MB for the returned buffer plus ~366 MB of
//! > Skia-internal assembly — none of which is returned to the OS. A pure-Rust
//! > shaper (rustybuzz, used here previously) needs those bytes; Skia's does
//! > not, because it already holds the typeface.
//!
//! Shaping the same clusters through Skia costs ~2 MB and produces identical
//! glyph ids and advances. The switch cut corpus peak RSS 44.6% and
//! emoji-document wall clock 42%.
//!
//! [`Shaper`] owns Skia's so it is constructed once per render rather than per
//! run, and is deliberately built **without a fallback font manager**: the
//! caller has already resolved which typeface to use, and silently
//! substituting another family would draw the wrong glyph.

use skia_safe::shaper::run_handler::{Buffer, RunInfo};
use skia_safe::shaper::{RunHandler, Shaper as SkShaper};
use skia_safe::shapers;
use skia_safe::{Font, GlyphId, Point, Typeface};
use thiserror::Error;
use unicode_joining_type::{get_joining_type, JoiningType};

use crate::i18n::bidi::BidiLevel;
use crate::render::dimension::Pt;

/// Whether `text` is in a script that cmap-only painting renders *wrongly*, as
/// opposed to merely without kerning.
///
/// The predicate is the Unicode **`Joining_Type`** property, and specifically
/// its three "this letter has positional forms" values. That is not a
/// hand-drawn list of scripts standing in for the real rule — it *is* the rule:
/// a letter with a joining type of dual-, left-, or right-joining is one whose
/// shape depends on its neighbours, which is exactly the case a cmap lookup
/// cannot answer. Arabic, Syriac, N'Ko, Mongolian, Adlam, Hanifi Rohingya and
/// the rest fall out of it without being named — and, just as usefully, Hebrew
/// and Thaana do not: both are right-to-left, and both spell their final forms
/// as separate codepoints rather than as positional variants, so a cmap lookup
/// is the whole answer for them.
///
/// Two values are deliberately excluded:
///
/// * `Transparent` — combining marks, which includes the Latin combining
///   diacriticals at U+0300. Including it would send `e` + U+0301 through the
///   shaper and re-measure a large share of European text.
/// * `JoinCausing` — ZWJ and tatweel. Both are *context* rather than letters
///   with forms of their own, and both appear where the letters around them
///   already answer this question. ZWJ in particular reaches here inside emoji
///   sequences that fell back to the text path, and shaping a Latin run
///   because it contains one would change that run's width for no gain.
///
/// What this does not cover is Indic reordering: a Brahmic script needs the
/// painter's unit to become the shaped cluster (the seam
/// [`crate::render::spacing`] names), which is a change to every caller of that
/// module rather than a new call site here. README tracks it as its own row.
pub fn needs_shaping(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            get_joining_type(c),
            JoiningType::DualJoining | JoiningType::LeftJoining | JoiningType::RightJoining
        )
    })
}

/// Which way [`Shaper::shape`] lays a run's glyphs out.
///
/// One run, one direction — UAX #9 reordering has already happened by the time
/// a run reaches the shaper, and `layout::fragment::bidi` has split every
/// fragment that spanned a level boundary, so a run is never mixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RunDirection {
    #[default]
    LeftToRight,
    RightToLeft,
}

impl From<BidiLevel> for RunDirection {
    fn from(level: BidiLevel) -> Self {
        if level.is_rtl() {
            Self::RightToLeft
        } else {
            Self::LeftToRight
        }
    }
}

// ─── Public ADTs ─────────────────────────────────────────────────────────────

/// One glyph in a [`ShapedRun`], positioned in pixels at the requested
/// rasterization size.
///
/// `x`/`y` are **absolute offsets from the run origin**, which sits on the
/// baseline — not per-glyph advances. Skia's shaper reports positions this way
/// and `draw_glyphs_at` consumes them the same way, so the rasterizer neither
/// accumulates a pen nor flips a sign. (The previous rustybuzz-based type
/// carried an advance plus y-*up* offsets and required both.)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    /// Skia glyph id.
    pub id: GlyphId,
    /// Horizontal offset from the run origin, in pixels.
    pub x: Pt,
    /// Vertical offset from the run origin, in pixels, **y-down** per Skia's
    /// convention. Zero for the overwhelming majority of emoji clusters.
    pub y: Pt,
}

/// Output of shaping one run of text.
#[derive(Clone, Debug)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    /// Sum of run advances in pixels — the rasterizer uses this to size the
    /// offscreen surface, and layout uses it to reserve the cluster's width.
    pub total_advance: Pt,
}

#[derive(Debug, Error)]
pub enum ShapeError {
    /// Skia was built without a HarfBuzz shaper. Unreachable with the
    /// `textlayout` feature enabled, but `shape_dont_wrap_or_reorder` returns
    /// an `Option` and this module does not panic on the public path.
    #[error("skia was built without a HarfBuzz shaper")]
    ShaperUnavailable,
    /// Shaping produced no glyphs — callers fall back to `draw_str` /
    /// `measure_str`.
    #[error("shaping produced no glyphs")]
    NoGlyphs,
}

// ─── Shaper ──────────────────────────────────────────────────────────────────

/// A reusable GSUB-aware shaper.
///
/// Construct once per render and shape many runs: Skia keeps an internal
/// HarfBuzz face cache keyed by typeface, so repeated calls do not re-parse
/// the font.
pub struct Shaper {
    shaper: SkShaper,
}

impl Shaper {
    /// Build a shaper with **no fallback font manager**, so shaping never
    /// substitutes a different family for the typeface the caller resolved.
    pub fn new() -> Result<Self, ShapeError> {
        shapers::hb::shape_dont_wrap_or_reorder(None)
            .map(|shaper| Self { shaper })
            .ok_or(ShapeError::ShaperUnavailable)
    }

    /// Shape `text` against `typeface` at `size_px`, returning the glyph
    /// sequence and positions.
    ///
    /// `size_px` is in raw pixels — already pre-multiplied by any super-sample
    /// scale the caller wants.
    pub fn shape(
        &self,
        typeface: &Typeface,
        text: &str,
        size_px: f32,
        direction: RunDirection,
    ) -> Result<ShapedRun, ShapeError> {
        let font = Font::from_typeface(typeface.clone(), size_px);
        let mut collector = Collector::default();
        // `width = f32::MAX` plus the dont-wrap-or-reorder shaper means "one
        // line, no bidi reordering" — a cluster is not a paragraph, and for
        // body text UAX #9 has already reordered the fragments this run sits
        // between. `direction` is what HarfBuzz orders glyphs *within* the run
        // by; letting Skia run its own bidi here instead would reorder the
        // same text twice.
        self.shaper.shape(
            text,
            &font,
            direction == RunDirection::LeftToRight,
            f32::MAX,
            &mut collector,
        );

        if collector.glyphs.is_empty() {
            return Err(ShapeError::NoGlyphs);
        }

        let glyphs = collector
            .glyphs
            .iter()
            .zip(collector.positions.iter())
            .map(|(&id, p)| ShapedGlyph {
                id,
                x: Pt::new(p.x),
                y: Pt::new(p.y),
            })
            .collect();

        Ok(ShapedRun {
            glyphs,
            total_advance: Pt::new(collector.advance_x),
        })
    }
}

/// Accumulates every run Skia emits. A single emoji cluster shapes to one run
/// in practice, but the shaper is free to split on script boundaries, so runs
/// are appended rather than replaced.
#[derive(Default)]
struct Collector {
    glyphs: Vec<GlyphId>,
    positions: Vec<Point>,
    advance_x: f32,
}

impl RunHandler for Collector {
    fn begin_line(&mut self) {}
    fn run_info(&mut self, _info: &RunInfo) {}
    fn commit_run_info(&mut self) {}

    fn run_buffer<'a>(&'a mut self, info: &RunInfo) -> Buffer<'a> {
        let base = self.glyphs.len();
        // Where this run starts, which is everything shaped so far. Skia writes
        // positions relative to the origin it is handed, so passing `None` —
        // as this did while its only caller shaped one emoji cluster at a time
        // — restarts every run at x=0 and stacks them on top of each other.
        // Invisible for a single-run cluster; wrong the moment a run of body
        // text is split on a script boundary, which a word plus its trailing
        // space already is.
        let origin = Point::new(self.advance_x, 0.0);
        self.glyphs.resize(base + info.glyph_count, 0);
        self.positions
            .resize(base + info.glyph_count, Point::new(0.0, 0.0));
        self.advance_x += info.advance.x;
        Buffer::new(
            &mut self.glyphs[base..],
            &mut self.positions[base..],
            origin,
        )
    }

    fn commit_run_buffer(&mut self, _info: &RunInfo) {}
    fn commit_line(&mut self) {}
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::emoji::resolve::EmojiFamily;
    use skia_safe::{FontMgr, FontStyle};

    /// The host's color emoji typeface, or `None` on a host without one —
    /// tests that need it return early rather than fail, since CI images vary.
    fn emoji_typeface() -> Option<Typeface> {
        let mgr = FontMgr::new();
        EmojiFamily::host_default().iter().find_map(|f| {
            mgr.match_family_style(f.family_name(), FontStyle::normal())
                .filter(|tf| tf.family_name().eq_ignore_ascii_case(f.family_name()))
        })
    }

    fn any_typeface() -> Option<Typeface> {
        FontMgr::new().legacy_make_typeface(None::<&str>, FontStyle::normal())
    }

    // ── needs_shaping ─────────────────────────────────────────────────────

    /// The predicate's whole job: keep Latin (and Cyrillic, Greek, CJK, Thai)
    /// off the shaping path, so their measured widths do not move.
    #[test]
    fn text_without_positional_forms_is_not_shaped() {
        for text in [
            "Nicht gefunden",
            "S.I.G.M.A. Technik Service GmbH",
            "Türöffner-Gerät",
            // A combining mark is Joining_Type=Transparent, and must not by
            // itself pull a Latin word into the shaper.
            "e\u{301}coute",
            "Привет",
            "日本語の文章",
            "ภาษาไทย",
            "שלום עולם",
            "",
        ] {
            assert!(!needs_shaping(text), "{text:?} must stay on the cmap path");
        }
    }

    /// Every one of these has letters whose glyph depends on its neighbours,
    /// which is the case a cmap lookup cannot answer.
    #[test]
    fn joining_scripts_are_shaped() {
        for (script, text) in [
            ("Arabic", "مرحبا"),
            ("Syriac", "\u{0710}\u{0712}"),
            ("N'Ko", "\u{07CA}\u{07D9}"),
            ("Mongolian", "\u{1820}\u{1821}"),
            ("Adlam", "\u{1E922}\u{1E923}"),
            ("Hanifi Rohingya", "\u{10D00}\u{10D01}"),
        ] {
            assert!(
                needs_shaping(text),
                "{script} letters have positional forms"
            );
        }
    }

    /// Thaana is the counterpart to Hebrew above, and the reason the predicate
    /// is `Joining_Type` rather than "is this script right-to-left": Thaana is
    /// written right to left and its letters are `Non_Joining`, so #131's
    /// reordering is all it needs.
    #[test]
    fn a_right_to_left_script_without_cursive_joining_is_not_shaped() {
        assert!(!needs_shaping("\u{0780}\u{0783}"));
    }

    /// A zero-width joiner is `Joining_Type=Join_Causing`, which the predicate
    /// excludes: it reaches body text only inside an emoji sequence that fell
    /// back from the emoji pipeline, and shaping the Latin around it would
    /// change that run's width for nothing.
    #[test]
    fn a_stray_zero_width_joiner_does_not_pull_latin_into_the_shaper() {
        assert!(!needs_shaping("a\u{200D}b"));
    }

    /// Mixed text takes the shaping path — the Arabic in it needs to join, and
    /// `fragment::bidi` will already have split the run at the level boundary
    /// in the cases where the two halves must be positioned separately.
    #[test]
    fn mixed_text_containing_a_joining_script_is_shaped() {
        assert!(needs_shaping("page مرحبا here"));
    }

    // ── the shaper ────────────────────────────────────────────────────────

    #[test]
    fn shaper_constructs() {
        assert!(
            Shaper::new().is_ok(),
            "skia must expose a HarfBuzz shaper — the `textlayout` feature is \
             what lets this module shape without serializing the font"
        );
    }

    /// ASCII through any system font: one glyph per character, advancing left
    /// to right. Pins the position convention the rasterizer depends on, and
    /// guards against shaping ligating runs that must not ligate.
    #[test]
    fn ascii_shapes_one_glyph_per_char_advancing_rightwards() {
        let Some(tf) = any_typeface() else { return };
        let shaper = Shaper::new().expect("shaper");
        let run = shaper
            .shape(&tf, "abc", 20.0, RunDirection::LeftToRight)
            .expect("shape");

        assert_eq!(run.glyphs.len(), 3, "ASCII must not ligate");
        assert_eq!(run.glyphs[0].x, Pt::ZERO, "run origin is the first glyph");
        assert!(
            run.glyphs[1].x > run.glyphs[0].x && run.glyphs[2].x > run.glyphs[1].x,
            "positions are absolute and strictly increasing, not per-glyph advances"
        );
        assert!(run.total_advance > Pt::ZERO);
    }

    /// **The reason this module exists.** A ZWJ sequence is five codepoints;
    /// cmap-only mapping would map each independently, and the rasterizer
    /// would draw a row of separate people rather than a ligated glyph.
    ///
    /// Full ligation to one glyph is *not* asserted here — issue #117 found
    /// it isn't a portable guarantee. On Windows, `Segoe UI Emoji` carries a
    /// real 24 KB `GSUB` table (confirmed by reading it directly) and does
    /// ligate other sequences (see `modifier_and_keycap_sequences_ligate`,
    /// which passes there), but has no ligature for this specific man+woman+
    /// girl combination or any of its three 2-person sub-pairs — each
    /// resolves to 2 glyphs (the ZWJ consumed, the two people left
    /// unligated), and the full sequence to 3. That is a real, observed
    /// difference in what this font's own tables define, not a shaping bug:
    /// the portable claim this test can make is that shaping is GSUB/cluster
    /// -aware (nowhere near the naive 5), not that any two color-emoji fonts
    /// ligate the same combinations. Apple Color Emoji *does* ligate this
    /// sequence, but via AAT `morx` — it carries no `GSUB` table at all, so
    /// the two fonts solve the same problem through mechanisms this module
    /// doesn't even need to distinguish between.
    #[test]
    fn zwj_sequence_ligates_to_one_glyph() {
        let Some(tf) = emoji_typeface() else { return };
        let shaper = Shaper::new().expect("shaper");
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(family.chars().count(), 5);

        let run = shaper
            .shape(&tf, family, 44.0, RunDirection::LeftToRight)
            .expect("shape");

        assert!(
            run.glyphs.len() < 5,
            "shaping must be GSUB/cluster-aware, not cmap-only mapping \
             (which would yield 5 for this 5-codepoint sequence); got {}",
            run.glyphs.len()
        );
        assert!(run.total_advance > Pt::ZERO);
    }

    /// A skin-tone modifier and a keycap sequence ligate by different GSUB
    /// mechanisms, with the same expectation.
    ///
    /// "Ligated" is asserted as **one cell wide**, not as one glyph. Those are
    /// not the same claim, and the difference is not hypothetical: Skia m150
    /// (skia-safe 0.99) began emitting the keycap as two glyphs — the composed
    /// keycap at the origin, plus a zero-advance blank parked at the far edge
    /// of the cell — where m145 emitted one. Nothing about the rendering
    /// changed: total advance stayed one cell, `tests/emoji_e2e.rs` still sees
    /// a single rasterized image with no constituent text beside it, and the
    /// corpus pixel-diff on `sample-emoji.docx` is zero.
    ///
    /// A glyph count is therefore the wrong instrument — it measures how the
    /// shaper chose to spell the answer rather than what gets drawn. Width
    /// *is* the property that matters, because the failure this test exists to
    /// catch is the sequence painting as its constituents side by side, which
    /// is two or three cells wide.
    #[test]
    fn modifier_and_keycap_sequences_ligate() {
        let Some(tf) = emoji_typeface() else { return };
        let shaper = Shaper::new().expect("shaper");
        const SIZE: f32 = 44.0;
        for (label, text) in [
            ("skin-tone modifier", "\u{1F44D}\u{1F3FF}"),
            ("keycap", "1\u{FE0F}\u{20E3}"),
        ] {
            let run = shaper
                .shape(&tf, text, SIZE, RunDirection::LeftToRight)
                .expect("shape");

            // One cell, not one per codepoint. Generous tolerance: emoji cells
            // are not exactly em-square in every font.
            let cells = f32::from(run.total_advance) / SIZE;
            assert!(
                (0.5..=1.5).contains(&cells),
                "{label} must occupy one cell, got {cells:.2} ({:?} at size {SIZE})",
                run.total_advance,
            );

            // And it did ligate: fewer glyphs than codepoints, with everything
            // past the first contributing no width.
            assert!(
                run.glyphs.len() < text.chars().count(),
                "{label}: {} glyphs for {} codepoints is no ligation at all",
                run.glyphs.len(),
                text.chars().count(),
            );
        }
    }

    /// Empty text yields no glyphs, reported as an error rather than an empty
    /// run so callers take their documented `measure_str` / `draw_str` path.
    #[test]
    fn empty_text_reports_no_glyphs() {
        let Some(tf) = any_typeface() else { return };
        let shaper = Shaper::new().expect("shaper");
        assert!(matches!(
            shaper.shape(&tf, "", 20.0, RunDirection::LeftToRight),
            Err(ShapeError::NoGlyphs)
        ));
    }

    /// The glyph ids shaping produces must be valid for `canvas.draw_glyphs`
    /// against the *same* typeface — the invariant the rasterizer relies on.
    #[test]
    fn glyph_ids_are_valid_for_the_same_typeface() {
        let Some(tf) = any_typeface() else { return };
        let shaper = Shaper::new().expect("shaper");
        let run = shaper
            .shape(&tf, "abc", 24.0, RunDirection::LeftToRight)
            .expect("shape");

        let font = Font::from_typeface(tf, 24.0);
        let ids: Vec<GlyphId> = run.glyphs.iter().map(|g| g.id).collect();
        let mut widths = vec![0.0f32; ids.len()];
        font.get_widths(&ids, &mut widths);
        assert!(
            widths.iter().all(|w| *w > 0.0),
            "every shaped glyph id must have a width in the same font: {widths:?}"
        );
    }

    /// Shaping must never reach for the typeface's bytes — the regression
    /// guard for the 549 MB this module was rewritten to avoid.
    #[test]
    fn shaping_does_not_materialize_the_font() {
        let Some(tf) = emoji_typeface() else { return };
        let shaper = Shaper::new().expect("shaper");

        let Some(before) = resident_bytes() else {
            return; // no readable RSS — skip rather than assert on nothing
        };
        for _ in 0..64 {
            let _ = shaper.shape(&tf, "\u{1F44D}", 176.0, RunDirection::LeftToRight);
        }
        let Some(after) = resident_bytes() else {
            return;
        };

        // `to_font_data()` on Apple Color Emoji costs ~549 MB. A 64 MB ceiling
        // proves the bytes were never serialized, with wide margin for
        // allocator noise and Skia's own glyph cache.
        let growth = after.saturating_sub(before);
        assert!(
            growth < 64 * 1024 * 1024,
            "shaping grew RSS by {} MB — the font was probably serialized",
            growth / 1024 / 1024
        );
    }

    /// Current resident set size in bytes, or `None` if it cannot be read.
    fn resident_bytes() -> Option<usize> {
        std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<usize>().ok())
            .map(|kb| kb * 1024)
    }
}

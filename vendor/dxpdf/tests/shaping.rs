//! Complex-script shaping, against a committed font (issue #131).
//!
//! The claim under test is that body text now goes through GSUB, and it cannot
//! be made against a host font: an assertion about Arabic joining that passes
//! on macOS (Geeza Pro) and quietly skips on a CI image with no Arabic face
//! proves nothing on the machine that matters. `test-files/fonts/DxJoining.ttf`
//! is built by `scripts/make_font_fixtures.py` for exactly this — one letter,
//! four positional forms, and the `init`/`medi`/`fina` lookups that pick
//! between them.
//!
//! The unit tests in `src/render/shape.rs` prove the *predicate* picks the
//! right scripts, and the ones in `src/render/layout/fragment/shape.rs` prove a
//! fragment gets marked and re-measured. Neither says the shaper actually
//! substitutes anything, which is what this file exists to say.

use dxpdf::render::shape::{needs_shaping, RunDirection, Shaper};
use skia_safe::{Font, FontMgr, GlyphId, Typeface};

/// Arabic letter beh, `Joining_Type=Dual_Joining` — the one letter the fixture
/// carries, in the three-letter word that puts it in all three contexts.
const BEH: char = '\u{0628}';

fn joining_face() -> Typeface {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test-files/fonts/DxJoining.ttf"
    );
    let bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("{path} is missing ({e}) — run scripts/make_font_fixtures.py"));
    FontMgr::new()
        .new_from_data(&bytes, 0)
        .expect("DxJoining.ttf is a valid SFNT")
}

/// What the painter did before #131, and still does for every script that does
/// not need shaping: map each codepoint through the cmap, independently.
fn cmap_glyphs(face: &Typeface, text: &str) -> Vec<GlyphId> {
    Font::from_typeface(face.clone(), 24.0).text_to_glyphs_vec(text)
}

/// **The defect, stated as a test.** Three behs are three occurrences of one
/// codepoint, so a cmap lookup gives one glyph three times — the isolated
/// form, three times over, which is not how the word is written.
#[test]
fn cmap_alone_paints_one_letter_three_times() {
    let face = joining_face();
    let word: String = std::iter::repeat_n(BEH, 3).collect();
    let ids = cmap_glyphs(&face, &word);
    assert_eq!(ids.len(), 3);
    assert_eq!(
        ids[0], ids[1],
        "a cmap lookup cannot see context, so every beh is the same glyph"
    );
    assert_eq!(ids[1], ids[2]);
}

/// The fix: shaping picks the initial, medial and final forms.
#[test]
fn shaping_substitutes_a_positional_form_for_each_letter() {
    let face = joining_face();
    let shaper = Shaper::new().expect("skia exposes a HarfBuzz shaper");
    let word: String = std::iter::repeat_n(BEH, 3).collect();

    let run = shaper
        .shape(&face, &word, 24.0, RunDirection::RightToLeft)
        .expect("three letters shape to three glyphs");

    let ids: Vec<GlyphId> = run.glyphs.iter().map(|g| g.id).collect();
    assert_eq!(ids.len(), 3, "no ligature is defined; one glyph per letter");
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "initial, medial and final are three different glyphs: {ids:?}",
    );
    assert!(
        !ids.contains(&cmap_glyphs(&face, "\u{0628}")[0]),
        "none of them is the isolated form the cmap would have given",
    );
}

/// The measured width has to move with the glyphs, or the run paints wider or
/// narrower than the box laid out for it and leaves its underline behind. The
/// fixture gives each positional form a different advance so this is checkable
/// rather than incidental.
#[test]
fn the_shaped_advance_differs_from_the_cmap_advance() {
    let face = joining_face();
    let shaper = Shaper::new().expect("shaper");
    let word: String = std::iter::repeat_n(BEH, 3).collect();

    let font = Font::from_typeface(face.clone(), 24.0);
    let cmap_width = font.measure_str(&word, None).0;
    let shaped = shaper
        .shape(&face, &word, 24.0, RunDirection::RightToLeft)
        .expect("shape")
        .total_advance;

    // The fixture gives beh a 500/1000-em advance and its three positional
    // forms 600, 700 and 800, so at 24pt the cmap sees 3 × 12 = 36pt and
    // shaping sees 14.4 + 16.8 + 19.2 = 50.4pt. Skia's default `Font` has
    // linear metrics off and rounds each glyph advance to a whole pixel, which
    // lands the shaped total on 50 — so the assertion is the *relation*, not
    // the unrounded arithmetic, with a tolerance of one rounding step per
    // glyph.
    assert!((cmap_width - 36.0).abs() < 0.01, "cmap: {cmap_width}");
    assert!(
        (f32::from(shaped) - 50.4).abs() <= 1.5,
        "shaped: {shaped:?} — expected ≈50.4pt from the positional forms",
    );
    assert!(
        f32::from(shaped) > cmap_width,
        "the shaped advance must be the one layout uses: {shaped:?} vs {cmap_width}",
    );
}

/// A single letter has no neighbours to join to, so it keeps its isolated
/// form. Guards against the shaper being wired up in a way that substitutes
/// unconditionally.
#[test]
fn one_letter_alone_keeps_its_isolated_form() {
    let face = joining_face();
    let shaper = Shaper::new().expect("shaper");
    let run = shaper
        .shape(&face, "\u{0628}", 24.0, RunDirection::RightToLeft)
        .expect("shape");
    assert_eq!(run.glyphs.len(), 1);
    assert_eq!(
        run.glyphs[0].id,
        cmap_glyphs(&face, "\u{0628}")[0],
        "an isolated letter shapes to what the cmap already said",
    );
}

/// And the predicate agrees that this text is the kind that gets here at all —
/// the link between the two halves of the change.
#[test]
fn the_fixture_text_is_what_the_predicate_selects() {
    assert!(needs_shaping(
        &std::iter::repeat_n(BEH, 3).collect::<String>()
    ));
    assert!(!needs_shaping("Nicht gefunden"));
}

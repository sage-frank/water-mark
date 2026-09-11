//! UAX #9 bidi reordering, on real laid-out pages (issue #131).
//!
//! The unit tests in `src/i18n/bidi.rs` prove `unicode-bidi` returns the right
//! levels, and the ones in `src/render/layout/paragraph/line_emit.rs` prove
//! rule L2 permutes a line of known levels. Neither says the two are wired to
//! each other through a real document, which is what this file exists to catch.
//!
//! Fixtures are `test-files/bidi-{hebrew,arabic}.docx`, built by
//! `scripts/make_bidi_fixtures.py`. Each carries its text three ways — one
//! `<w:r>`, several identically-formatted runs, and runs with `<w:rtl/>` — so
//! that a level resolved per run instead of per paragraph shows up as a
//! disagreement between paragraphs that read identically.
//!
//! Hebrew is the script this phase completes: its letters have no positional
//! forms, so ordering is the whole of what it needed. Arabic carries the same
//! assertions plus the ones about numbers, and its joining is proved separately
//! and hermetically in `tests/shaping.rs`.

use std::collections::BTreeMap;

use dxpdf::model::{Block, Inline, RunElement};
use dxpdf::render::layout::draw_command::{DrawCommand, LayoutedPage};

const HEBREW: &str = "test-files/bidi-hebrew.docx";
const ARABIC: &str = "test-files/bidi-arabic.docx";

/// One painted run: where it starts, and what it says.
type Piece = (f32, String);

/// Parse a fixture and lay it out, returning each paragraph's logical text
/// alongside the pieces painted for it.
///
/// The paragraph texts come from the parse rather than being restated here, so
/// the fixture generator stays the single source of truth for what the
/// documents say. Pieces are grouped by painted baseline, which is what makes
/// this a test of *layout*: it reads back the coordinates the PDF will carry.
fn fixture(path: &str) -> (Vec<String>, Vec<Vec<Piece>>) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let doc = dxpdf::docx::parse(&bytes).unwrap_or_else(|e| panic!("{path} parses: {e}"));
    let texts: Vec<String> = doc
        .body
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(p) => Some(
                p.content
                    .iter()
                    .filter_map(|inline| match inline {
                        Inline::TextRun(run) => {
                            Some(run.content.iter().filter_map(|el| match el {
                                RunElement::Text(t) => Some(t.as_str()),
                                _ => None,
                            }))
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect::<String>(),
            ),
            _ => None,
        })
        .filter(|t: &String| !t.is_empty())
        .collect();
    (texts, lines(&dxpdf::render::resolve_and_layout(doc).1))
}

/// Every painted line, as its pieces in left-to-right order.
fn lines(pages: &[LayoutedPage]) -> Vec<Vec<Piece>> {
    let mut out = Vec::new();
    for page in pages {
        let mut by_baseline: BTreeMap<i64, Vec<Piece>> = BTreeMap::new();
        for command in &page.commands {
            if let DrawCommand::Text { position, text, .. } = command {
                by_baseline
                    .entry(position.y.raw().round() as i64)
                    .or_default()
                    .push((position.x.raw(), text.to_string()));
            }
        }
        for (_, mut line) in by_baseline {
            line.sort_by(|a, b| a.0.total_cmp(&b.0));
            if !line.iter().all(|(_, t)| t.trim().is_empty()) {
                out.push(line);
            }
        }
    }
    out
}

/// The line's pieces read right to left, concatenated.
///
/// For a line every fragment of which resolved to an odd level — a paragraph of
/// nothing but Hebrew or Arabic — this **is** the definition of correct
/// reordering: take the pieces from the rightmost leftwards and you get the
/// text back in the order the document stored it.
///
/// Pieces, not glyphs. A command's `text` stays in logical order however its
/// glyphs are placed, so what these helpers see is the *fragment* order rule L2
/// produced. The reversal of the glyphs inside one is the shaper's half of the
/// job, asserted separately by
/// [`every_right_to_left_run_is_marked_for_shaping`] here and directly in
/// `tests/shaping.rs`.
fn right_to_left(line: &[Piece]) -> String {
    line.iter().rev().map(|(_, t)| t.as_str()).collect()
}

fn left_to_right(line: &[Piece]) -> String {
    line.iter().map(|(_, t)| t.as_str()).collect()
}

// ── Acceptance criterion 1: a w:bidi paragraph reorders visually ───────────

/// The whole claim, in one assertion per script: reading the painted pieces
/// from the right reproduces the paragraph. Before #131 it was reading them
/// from the *left* that did — which is to say Hebrew came out backwards.
#[test]
fn a_right_to_left_paragraph_paints_its_words_right_to_left() {
    for (path, expect_lines) in [(HEBREW, 7), (ARABIC, 6)] {
        let (texts, laid) = fixture(path);
        assert_eq!(
            laid.len(),
            expect_lines,
            "{path}: every fixture paragraph must fit one line",
        );
        assert_eq!(
            right_to_left(&laid[0]),
            texts[0],
            "{path}: the first paragraph must read back right to left",
        );
        assert_ne!(
            left_to_right(&laid[0]),
            texts[0],
            "{path}: and must *not* read back left to right — that is the bug",
        );
    }
}

// ── Formatting boundaries must not move linguistic ones ────────────────────

/// The same text laid out twice — once as one `<w:r>`, once split across four
/// identically-formatted runs — must paint at the same positions.
///
/// Word splits runs for reasons that have nothing to do with language, so a
/// level resolved per run gets a different answer for two documents that read
/// identically. This is the same defect #130 found for line breaking, one
/// algorithm along.
#[test]
fn run_boundaries_do_not_change_where_words_are_painted() {
    for path in [HEBREW, ARABIC] {
        let (texts, laid) = fixture(path);
        assert_eq!(texts[0], texts[1], "{path}: the two must carry one text");
        assert_eq!(
            right_to_left(&laid[1]),
            texts[1],
            "{path}: split across runs, it must still read right to left",
        );
        let one: Vec<String> = laid[0].iter().map(|(_, t)| t.clone()).collect();
        let many: Vec<String> = laid[1].iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(one, many, "{path}: same pieces, same order");
    }
}

/// §17.3.2.30: `<w:rtl/>` on the runs of a paragraph that is already `w:bidi`
/// adds nothing the characters did not already say, and must therefore change
/// nothing. A `w:rtl` implementation that fenced each fragment off in its own
/// isolate would show up here as a different set of pieces.
#[test]
fn an_explicit_rtl_run_agrees_with_what_the_characters_say() {
    for path in [HEBREW, ARABIC] {
        let (texts, laid) = fixture(path);
        assert_eq!(texts[0], texts[2]);
        assert_eq!(right_to_left(&laid[2]), texts[2], "{path}");
    }
}

// ── Rule L2 is a sort, not a reversal ──────────────────────────────────────

/// An embedded left-to-right phrase inside Hebrew keeps its own words in
/// reading order while the Hebrew around it reverses. A naive reversal of the
/// line would spell it "fox brown quick the".
#[test]
fn an_embedded_latin_phrase_keeps_its_own_order() {
    let (_, laid) = fixture(HEBREW);
    let painted = left_to_right(&laid[3]);
    assert!(
        painted.contains("the quick brown fox"),
        "the Latin phrase must read forwards: {painted:?}",
    );
}

/// And the mirror image, which is what most real documents contain: a Hebrew
/// phrase quoted inside a left-to-right paragraph. The Latin around it stays
/// put; only the quotation reverses.
#[test]
fn a_quoted_hebrew_phrase_reverses_inside_a_left_to_right_paragraph() {
    let (texts, laid) = fixture(HEBREW);
    let line = laid.last().expect("the fixture's last paragraph");
    let painted = left_to_right(line);
    assert!(
        painted.starts_with("Quoted:"),
        "the paragraph is left-to-right, so it starts at the left: {painted:?}",
    );
    assert!(
        painted.trim_end().ends_with("end."),
        "and ends at the right: {painted:?}",
    );
    assert_ne!(
        painted,
        *texts.last().unwrap(),
        "but the Hebrew inside it must have moved",
    );
}

/// Rule I1: Western digits inside Arabic resolve to an *even* level, so the
/// number keeps its own left-to-right order while the words around it reverse.
/// Painting "21" for "12" is the classic bidi bug.
#[test]
fn western_digits_inside_arabic_keep_their_order() {
    let (_, laid) = fixture(ARABIC);
    for line in &laid[3..5] {
        let painted = left_to_right(line);
        assert!(
            painted.contains("12") && painted.contains("345"),
            "each number must survive as itself: {painted:?}",
        );
        assert!(
            !painted.contains("21") && !painted.contains("543"),
            "and must not be reversed digit by digit: {painted:?}",
        );
    }
}

// ── Rule L4: mirroring ─────────────────────────────────────────────────────

/// A bracket at an odd level is painted as its mirror, so that a Hebrew
/// parenthetical opens on the right. The same document's Latin parenthetical
/// is at an even level and must keep its brackets as written — which is what
/// makes this a test of the *rule* and not of a global swap.
#[test]
fn brackets_mirror_around_right_to_left_text_only() {
    let (texts, laid) = fixture(HEBREW);
    let source = &texts[5];
    assert!(source.contains("(עולם)") && source.contains("(test)"));

    // Whitespace-insensitive: a space adjoining a bracket is a neutral that
    // rule L1 leaves at the paragraph level, so which painted piece carries it
    // is not what this test is about.
    let painted = left_to_right(&laid[5]);
    let tight: String = painted.chars().filter(|c| !c.is_whitespace()).collect();

    assert!(
        tight.contains(")עולם("),
        "the Hebrew parenthetical is at an odd level, so its brackets mirror \
         — read right to left it opens before עולם: {painted:?}",
    );
    assert!(
        tight.contains("(test)"),
        "the Latin one ends up surrounded the same way round it was written, \
         which is the point: its brackets are neutrals that took the \
         paragraph's level, mirrored, and were then reordered *past* the Latin \
         — two wrongs that must make a right: {painted:?}",
    );
}

// ── §17.3.1.13 / §17.3.1.12: which edge is the start ───────────────────────

/// Absent `w:jc`, alignment is `Alignment::Start`, and under `w:bidi` the start
/// is the right margin — so a right-to-left paragraph right-aligns without
/// saying so. This is the decision recorded at `line_emit::align_offset`, seen
/// from the outside.
#[test]
fn a_bidi_paragraph_with_no_jc_is_right_aligned() {
    let (_, laid) = fixture(HEBREW);
    let rtl_left = laid[0].first().expect("pieces").0;
    // Paragraph 5 is the same mixed text without `w:bidi`: left-to-right, so
    // it starts hard against the left margin.
    let ltr_left = laid[4].first().expect("pieces").0;
    assert!(
        rtl_left > ltr_left + 1.0,
        "the right-to-left paragraph must be pushed off the left margin \
         ({rtl_left} vs {ltr_left})",
    );
}

// ── The glyphs inside a run, not just the runs ─────────────────────────────

/// Rule L2 puts a line's *fragments* in visual order. Nothing but the shaper
/// puts the glyphs *inside* one there — `draw_str` walks a string left to
/// right, which paints `שלום` with its first letter at the left edge.
///
/// So every right-to-left command must be marked for shaping, and no
/// left-to-right one may be. This is the wiring the assertions above cannot
/// see: they read each command's `text`, which stays in logical order however
/// its glyphs are eventually placed.
#[test]
fn every_right_to_left_run_is_marked_for_shaping() {
    use dxpdf::render::shape::RunDirection;

    let bytes = std::fs::read(HEBREW).expect("fixture");
    let doc = dxpdf::docx::parse(&bytes).expect("parses");
    let pages = dxpdf::render::resolve_and_layout(doc).1;

    let mut rtl = 0;
    let mut ltr = 0;
    for page in &pages {
        for command in &page.commands {
            let DrawCommand::Text { text, shaped, .. } = command else {
                continue;
            };
            // Hebrew letters are U+0590..=U+05FF; a piece is all one level by
            // the time it reaches a draw command, so one character decides.
            let is_hebrew = text.chars().any(|c| ('\u{0590}'..='\u{05FF}').contains(&c));
            if is_hebrew {
                assert_eq!(
                    *shaped,
                    Some(RunDirection::RightToLeft),
                    "Hebrew run {text:?} must be shaped so its glyphs reverse",
                );
                rtl += 1;
            } else if text.trim().chars().any(|c| c.is_ascii_alphabetic()) {
                assert_eq!(
                    *shaped, None,
                    "Latin run {text:?} must keep the cmap path unchanged",
                );
                ltr += 1;
            }
        }
    }
    assert!(
        rtl > 10,
        "the fixture must produce many Hebrew runs, got {rtl}"
    );
    assert!(ltr > 5, "and some Latin ones, got {ltr}");
}

// ── Nothing is lost ────────────────────────────────────────────────────────

/// Reordering is a permutation, so every paragraph must still hold exactly its
/// own characters however they were arranged. Sorting both sides is what makes
/// this an assertion about *loss* rather than about order, which the tests
/// above already cover.
#[test]
fn reordering_neither_drops_nor_duplicates_text() {
    for path in [HEBREW, ARABIC] {
        let (texts, laid) = fixture(path);
        assert_eq!(texts.len(), laid.len());
        for (text, line) in texts.iter().zip(&laid) {
            let mut want: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
            let mut got: Vec<char> = left_to_right(line)
                .chars()
                // Rule L4 replaces a bracket with its mirror, which is the one
                // painted character that is legitimately not the stored one.
                .map(|c| dxpdf::i18n::bidi::mirror(c).unwrap_or(c))
                .filter(|c| !c.is_whitespace())
                .collect();
            want.sort_unstable();
            got.sort_unstable();
            // `want` is compared after mirroring both sides, so an unmirrored
            // bracket pair still matches a mirrored one.
            let mut want: Vec<char> = want
                .into_iter()
                .map(|c| dxpdf::i18n::bidi::mirror(c).unwrap_or(c))
                .collect();
            want.sort_unstable();
            assert_eq!(want, got, "{path}: {text:?}");
        }
    }
}

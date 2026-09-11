//! UAX #14 line breaking, on real laid-out pages (issue #130).
//!
//! The unit tests in `src/i18n/segment.rs` prove ICU4X returns the right break
//! offsets for a string, and the ones in `src/render/layout/line.rs` prove the
//! fitter obeys a fragment's [`BreakAfter`]. Neither says the two are wired to
//! each other, which is exactly the failure mode this file exists to catch:
//! before #130 the engine *did* wrap Thai and Japanese — a paragraph wider than
//! the line is cut into grapheme clusters by `split_oversized_fragments` as a
//! last resort — so "it wraps" was never the property worth asserting. Where
//! the breaks land is.
//!
//! Fixtures are `test-files/line-break-{thai,cjk}.docx`, built by
//! `scripts/make_line_break_fixtures.py`. Each carries the same text three
//! ways: once as a single `<w:r>`, once behind a varying prefix so the
//! punctuation sweeps every column position, and once split across several
//! identically-formatted runs.
//!
//! [`BreakAfter`]: dxpdf::render::layout::fragment::BreakAfter

use std::collections::BTreeMap;

use dxpdf::model::{Block, Inline, RunElement};
use dxpdf::render::layout::draw_command::{DrawCommand, LayoutedPage};

const THAI: &str = "test-files/line-break-thai.docx";
const CJK: &str = "test-files/line-break-cjk.docx";

/// UAX #14 classes CL/CP/EX/NS, in the characters this fixture uses. [LB13]
/// forbids a break *before* any of them, so none may open a line.
///
/// [LB13]: https://www.unicode.org/reports/tr14/#LB13
const MAY_NOT_OPEN_A_LINE: &str = "。、」』）〉？！ぁぃぅぇぉっゃゅょ・：；";

/// UAX #14 class OP. [LB14] forbids a break *after* any of them, so none may
/// close a line.
///
/// [LB14]: https://www.unicode.org/reports/tr14/#LB14
const MAY_NOT_CLOSE_A_LINE: &str = "「『（〈";

/// Parse a committed fixture and lay it out, returning the model's paragraph
/// texts alongside the pages. The paragraph texts come from the parse rather
/// than being restated here, so the fixture generator stays the single source
/// of truth for what the documents say.
fn fixture(path: &str) -> (Vec<String>, Vec<LayoutedPage>) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let doc = dxpdf::docx::parse(&bytes).unwrap_or_else(|e| panic!("{path} parses: {e}"));
    let paragraphs = doc
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
        .filter(|text: &String| !text.is_empty())
        .collect();
    (paragraphs, dxpdf::render::resolve_and_layout(doc).1)
}

/// Every laid-out line's text, in reading order.
///
/// A line is the set of text commands sharing a baseline. Grouping by the
/// painted `y` is what makes this a test of *layout* and not of fragment
/// building: it reads back the same positions the PDF will carry.
fn lines(pages: &[LayoutedPage]) -> Vec<String> {
    let mut out = Vec::new();
    for page in pages {
        // `BTreeMap` on the baseline's raw bits would order negatives wrongly;
        // every baseline here is positive, and quantising to whole points
        // tolerates the sub-point drift between fragments on one line.
        let mut by_baseline: BTreeMap<i64, Vec<(i64, &str)>> = BTreeMap::new();
        for command in &page.commands {
            if let DrawCommand::Text { position, text, .. } = command {
                by_baseline
                    .entry(position.y.raw().round() as i64)
                    .or_default()
                    .push((position.x.raw().round() as i64, text));
            }
        }
        for (_, mut run) in by_baseline {
            run.sort_by_key(|(x, _)| *x);
            let line: String = run.into_iter().map(|(_, text)| text).collect();
            if !line.trim().is_empty() {
                out.push(line);
            }
        }
    }
    out
}

/// Group `lines` by the paragraph each belongs to, by consuming lines until
/// their concatenation is the paragraph's own text.
///
/// This is also an assertion in its own right — if layout dropped, duplicated
/// or reordered any text, no grouping exists and this panics rather than
/// letting the tests below pass on a document that no longer says what the
/// fixture says.
fn by_paragraph(lines: &[String], paragraphs: &[String]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut next = 0;
    for paragraph in paragraphs {
        let mut taken: Vec<String> = Vec::new();
        while taken.concat().chars().count() < paragraph.chars().count() {
            let line = lines.get(next).unwrap_or_else(|| {
                panic!(
                    "ran out of laid-out lines while rebuilding paragraph {:?}; \
                     got {:?} so far",
                    paragraph, taken,
                )
            });
            taken.push(line.clone());
            next += 1;
        }
        assert_eq!(
            taken.concat(),
            *paragraph,
            "the lines of a paragraph must be exactly its text",
        );
        out.push(taken);
    }
    assert_eq!(
        next,
        lines.len(),
        "every laid-out line belongs to a paragraph"
    );
    out
}

/// The byte offsets at which `paragraph` was actually broken.
fn break_points(paragraph_lines: &[String]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut at = 0;
    for line in &paragraph_lines[..paragraph_lines.len() - 1] {
        at += line.len();
        offsets.push(at);
    }
    offsets
}

// ── Acceptance criterion 1: breaks land where UAX #14 puts them ────────────

/// Thai, the class-SA case. Every place layout chose to break must be a place
/// UAX #14 said it could — which for Thai means a word boundary the LSTM model
/// in `src/i18n/data/icu_data.blob` found. The old rule knew no break
/// characters in this script at all, so every one of these offsets used to
/// fall wherever the grapheme-cluster fallback happened to land: `แบบจำ|ลอง`
/// cut จำลอง in half.
#[test]
fn every_thai_break_is_a_uax14_break_opportunity() {
    let (paragraphs, pages) = fixture(THAI);
    let grouped = by_paragraph(&lines(&pages), &paragraphs);

    let mut checked = 0;
    for (paragraph, paragraph_lines) in paragraphs.iter().zip(&grouped) {
        let allowed = dxpdf::i18n::segment::break_offsets(paragraph);
        for offset in break_points(paragraph_lines) {
            assert!(
                allowed.contains(&offset),
                "broke at byte {offset} — {:?} | {:?} — which UAX #14 does not \
                 allow",
                paragraph[..offset]
                    .chars()
                    .rev()
                    .take(8)
                    .collect::<String>(),
                paragraph[offset..].chars().take(8).collect::<String>(),
            );
            checked += 1;
        }
    }
    assert!(
        checked > 20,
        "expected the fixture to force many breaks, checked only {checked}",
    );
}

/// Japanese. Class ID breaks between almost any two ideographs, so the
/// interesting rules are the two that *forbid* a break, and they are what a
/// reader notices: before #130 a sweep like this fixture's put `。` at the
/// start of 3 lines in 127, one of them a line containing nothing else.
#[test]
fn japanese_never_strands_punctuation_on_a_line_edge() {
    let (_, pages) = fixture(CJK);
    let lines = lines(&pages);
    assert!(lines.len() > 20, "fixture must produce many lines");

    for line in &lines {
        let first = line.chars().next().expect("no empty lines");
        assert!(
            !MAY_NOT_OPEN_A_LINE.contains(first),
            "LB13: line opens with {first:?} — {line:?}",
        );
        let last = line.chars().next_back().expect("no empty lines");
        assert!(
            !MAY_NOT_CLOSE_A_LINE.contains(last),
            "LB14: line closes with {last:?} — {line:?}",
        );
    }
}

// ── Formatting boundaries must not move linguistic ones ────────────────────

/// The same text laid out twice — once as one `<w:r>`, once split across three
/// identically-formatted runs — must break in the same places.
///
/// This is the defect that made a Japanese paragraph of twelve short runs lay
/// out two runs per line and leave the rest of each line empty: segmentation
/// ran per run, so a `<w:r>` boundary was the *only* thing the fitter could
/// break at, and a word spanning one could not be broken at all. Word splits
/// runs for reasons that have nothing to do with language — a spell-check
/// state, a revision id — so a line breaker that can see only one run at a
/// time gets a different answer for documents that read identically.
#[test]
fn run_boundaries_do_not_change_where_a_paragraph_breaks() {
    for path in [THAI, CJK] {
        let (paragraphs, pages) = fixture(path);
        let grouped = by_paragraph(&lines(&pages), &paragraphs);

        // The generator writes the same text first as one run and last as
        // several; find them by their (identical) text rather than by index.
        let single = grouped.first().expect("fixture has paragraphs");
        let split = grouped.last().expect("fixture has paragraphs");
        assert_eq!(
            paragraphs.first(),
            paragraphs.last(),
            "{path}: first and last paragraph must carry the same text",
        );
        assert!(single.len() > 1, "{path}: the text must wrap to be a test");
        assert_eq!(
            break_points(single),
            break_points(split),
            "{path}: one run and several runs broke the same text differently",
        );
    }
}

// ── Nothing is lost ────────────────────────────────────────────────────────

/// Finer fragments are the cost of UAX #14 — a Japanese paragraph becomes
/// roughly one fragment per character. Lines must still be filled, not left
/// one word wide, and the text must survive intact (`by_paragraph` asserts the
/// latter for every paragraph in both fixtures).
#[test]
fn lines_are_filled_rather_than_broken_at_the_first_opportunity() {
    for path in [THAI, CJK] {
        let (paragraphs, pages) = fixture(path);
        let grouped = by_paragraph(&lines(&pages), &paragraphs);
        for (paragraph, paragraph_lines) in paragraphs.iter().zip(&grouped) {
            // Every line but the last must be within one "word" of the widest
            // line in its own paragraph; a segmenter wired up wrongly (one
            // break taken per line, or a break at every cluster) shows up here
            // as a paragraph whose lines are wildly uneven.
            let widest = paragraph_lines
                .iter()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0);
            for line in &paragraph_lines[..paragraph_lines.len() - 1] {
                let len = line.chars().count();
                assert!(
                    len * 4 >= widest * 3,
                    "{path}: a line of {len} chars against a widest of {widest} \
                     in the same paragraph — lines are not being filled: \
                     {line:?} (paragraph {paragraph:?})",
                );
            }
        }
    }
}

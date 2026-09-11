//! An unbreakable token wider than its container must be cut, not overflowed.
//!
//! Fixture: `test-files/footer-path-wrap.docx`, built by
//! `scripts/make_footer_path_fixture.py` — a three-column table in the page
//! footer whose right-hand cell holds a Windows path in 6 pt Arial. Reduced
//! from a real report (`VE_Anlagenfreigabe_…docx`) to the geometry that makes
//! it hard.
//!
//! UAX #14 gives that path exactly one break opportunity, after the `:` in
//! `"Z:`, and none after: `\` is class PR, and [LB24] (`PR × AL`) and [LB25]
//! (`PR × NU`) both forbid breaking after it. So the fitter meets an early
//! opportunity followed by a run wider than the line with nothing legal behind
//! it — and on overflow it rewound to that opportunity but resumed measuring at
//! the fragment that overflowed. Everything in between was painted onto the new
//! line without being counted into its width, so the line could not overflow
//! again and swallowed the rest: one 295 pt line in a 167.80 pt cell, running
//! 91 pt past the right edge of the page.
//!
//! `split_oversized_fragments` had already done its part — the token arrives
//! here pre-cut into 97 single-cluster fragments — which is why the assertion
//! below is about *position*, not about whether anything was split.
//!
//! [LB24]: https://www.unicode.org/reports/tr14/#LB24
//! [LB25]: https://www.unicode.org/reports/tr14/#LB25

use std::collections::BTreeMap;

use dxpdf::render::layout::draw_command::{DrawCommand, LayoutedPage};

const FIXTURE: &str = "test-files/footer-path-wrap.docx";

/// From the generator's `w:pgSz`/`w:pgMar`: A4 (11907 twips) with a 1134-twip
/// right margin. Stated here rather than read back from the section so that a
/// layout bug in section geometry cannot move the goalposts this test measures
/// against.
const PAGE_WIDTH_PT: f32 = 11907.0 / 20.0;
const RIGHT_MARGIN_PT: f32 = 1134.0 / 20.0;
const TEXT_AREA_RIGHT_PT: f32 = PAGE_WIDTH_PT - RIGHT_MARGIN_PT;

fn pages() -> Vec<LayoutedPage> {
    let bytes = std::fs::read(FIXTURE).unwrap_or_else(|e| panic!("{FIXTURE}: {e}"));
    let doc = dxpdf::docx::parse(&bytes).unwrap_or_else(|e| panic!("{FIXTURE} parses: {e}"));
    dxpdf::render::resolve_and_layout(doc).1
}

/// Every text command's pen position, grouped by baseline.
fn baselines(pages: &[LayoutedPage]) -> BTreeMap<i64, Vec<(f32, String)>> {
    let mut out: BTreeMap<i64, Vec<(f32, String)>> = BTreeMap::new();
    for page in pages {
        for command in &page.commands {
            if let DrawCommand::Text {
                position,
                text,
                font_size,
                ..
            } = command
            {
                out.entry(position.y.raw().round() as i64)
                    .or_default()
                    // The pen is the *left* edge of the piece; a glyph's own
                    // advance can never exceed its em, so `x + size` bounds the
                    // right edge without re-measuring anything here.
                    .push((position.x.raw() + font_size.raw(), text.to_string()));
            }
        }
    }
    out
}

#[test]
fn an_unbreakable_token_stays_inside_the_text_area() {
    let pages = pages();
    let mut worst: Option<(f32, String)> = None;
    for (_, pieces) in baselines(&pages) {
        for (right_edge, text) in pieces {
            if worst.as_ref().is_none_or(|(w, _)| right_edge > *w) {
                worst = Some((right_edge, text));
            }
        }
    }
    let (right_edge, text) = worst.expect("the fixture draws text");
    assert!(
        right_edge <= TEXT_AREA_RIGHT_PT,
        "text reaches x={right_edge:.1}pt, past the text area's right edge at \
         {TEXT_AREA_RIGHT_PT:.1}pt (piece {text:?})",
    );
}

#[test]
fn the_token_is_wrapped_rather_than_left_on_one_line() {
    // The guard on the guard: staying inside the text area would also be true
    // of a document that dropped the token altogether, or of one where it
    // happened to fit. It must actually occupy several lines.
    let pages = pages();
    let path_baselines = baselines(&pages)
        .into_iter()
        .filter(|(_, pieces)| {
            let line: String = pieces.iter().map(|(_, t)| t.as_str()).collect();
            line.contains("Vorlagen") || line.contains("Formtastic") || line.contains("Z:")
        })
        .count();
    assert!(
        path_baselines >= 3,
        "the path should wrap across several lines of a 150pt cell, got \
         {path_baselines} line(s)",
    );
}

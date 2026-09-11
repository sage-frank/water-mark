//! §17.3.2 `w:vanish` end-to-end: text an author marked hidden must not reach
//! the page.
//!
//! `test-files/hidden-text.docx` (built by `scripts/make_hidden_text_fixture.py`)
//! puts every position `w:vanish` resolves differently in its own paragraph, and
//! pairs a marker that must survive with one that must not. `SECRET` is the only
//! string that may never be drawn, so its absence is asserted once over the whole
//! document rather than per paragraph.
//!
//! What this file adds over the unit tests in `fragment::collect` is that the
//! filter sits in the right *place*. Hidden runs are removed before anything
//! measures or joins them, so the visible text closes up around the gap instead
//! of leaving one — a draw-time skip would satisfy "SECRET is not painted" and
//! still be wrong. The fixture's first paragraph is the control that tells the
//! two apart without measuring any glyph: it carries the same two visible runs
//! with nothing hidden between them, so the second run has to land at the same x
//! in both.

use dxpdf::render::layout::draw_command::{DrawCommand, LayoutedPage};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-files/hidden-text.docx");

fn pages() -> Vec<LayoutedPage> {
    let bytes = std::fs::read(FIXTURE).expect("fixture is committed");
    let doc = dxpdf::docx::parse(&bytes).expect("fixture parses");
    dxpdf::render::resolve_and_layout(doc).1
}

/// Every drawn string with its origin, in page order.
fn drawn(pages: &[LayoutedPage]) -> Vec<(f32, f32, String)> {
    pages
        .iter()
        .flat_map(|p| &p.commands)
        .filter_map(|c| match c {
            DrawCommand::Text { position, text, .. } => {
                Some((position.x.raw(), position.y.raw(), text.to_string()))
            }
            _ => None,
        })
        .collect()
}

/// The defect, stated as the property that matters. Nothing else in this file
/// is worth much if this fails.
#[test]
fn hidden_text_is_never_drawn() {
    let drawn = drawn(&pages());
    assert!(
        !drawn.iter().any(|(_, _, t)| t.contains("SECRET")),
        "hidden runs reached the page: {drawn:?}"
    );
}

/// The other half: hiding is not a licence to drop the visible neighbours.
/// Two in the control, two around the hidden run, one un-hidden by `w:val="0"`,
/// two around the hidden tab-and-break run, one before the symbol.
#[test]
fn visible_text_around_hidden_runs_survives() {
    let drawn = drawn(&pages());
    let visible = drawn
        .iter()
        .filter(|(_, _, t)| t.contains("VISIBLE"))
        .count();
    assert_eq!(visible, 8, "expected eight VISIBLE markers, got {drawn:?}");
}

/// Removal, not a zero-width draw — the assertion the control exists for.
///
/// Paragraph 1 has two adjacent visible runs; paragraph 2 has the same two with
/// a hidden run between them. If hiding merely skipped the draw and still
/// reserved the run's advance, paragraph 2's second run would sit `SECRET`'s
/// width further right. No glyph metric is named here, so the test holds on any
/// host face.
#[test]
fn a_hidden_run_costs_no_width() {
    let drawn = drawn(&pages());
    let mut lines: Vec<f32> = drawn.iter().map(|(_, y, _)| *y).collect();
    lines.sort_by(|a, b| a.partial_cmp(b).expect("finite coordinates"));
    lines.dedup_by(|a, b| (*a - *b).abs() < 0.01);

    let xs_on = |line: f32| -> Vec<f32> {
        let mut xs: Vec<f32> = drawn
            .iter()
            .filter(|(_, y, _)| (y - line).abs() < 0.01)
            .map(|(x, _, _)| *x)
            .collect();
        xs.sort_by(|a, b| a.partial_cmp(b).expect("finite coordinates"));
        xs
    };

    let control = xs_on(lines[0]);
    let with_hidden = xs_on(lines[1]);
    assert_eq!(control.len(), 2, "the control draws both its runs");
    assert_eq!(with_hidden.len(), 2, "so does the paragraph that hides one");
    assert!(
        (control[1] - with_hidden[1]).abs() < 0.01,
        "the run after a hidden one must start where it would with no hidden \
         run at all: control {control:?} vs {with_hidden:?}"
    );
}

/// §17.3.2: hiding every run of a paragraph does not delete the paragraph. Its
/// mark is still visible, so it still occupies a line — paragraphs 3 and 5 draw
/// nothing yet keep their neighbours a line further apart.
///
/// The paragraph *mark*'s own `w:pPr/w:rPr/w:vanish` is a different feature — it
/// hides the mark, which merges the paragraph into the next — and is not
/// implemented. This pins the boundary from the near side: a paragraph whose
/// runs are all hidden must not be mistaken for one whose mark is.
#[test]
fn a_paragraph_of_only_hidden_runs_still_takes_a_line() {
    let drawn = drawn(&pages());
    let mut lines: Vec<f32> = drawn.iter().map(|(_, y, _)| *y).collect();
    lines.sort_by(|a, b| a.partial_cmp(b).expect("finite coordinates"));
    lines.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    assert_eq!(lines.len(), 5, "five paragraphs draw text, got {lines:?}");

    // 1 → 2 are adjacent; 2 → 4 and 4 → 6 each step over one hidden paragraph.
    let adjacent = lines[1] - lines[0];
    for (from, to) in [(1, 2), (2, 3)] {
        let across = lines[to] - lines[from];
        assert!(
            (across - adjacent * 2.0).abs() < 0.5,
            "a fully hidden paragraph still costs one line: {across} vs one \
             line of {adjacent} in {lines:?}"
        );
    }
}

/// **Known limit, characterized rather than fixed.** Word hides a `w:sym`,
/// `w:drawing` or `w:pict` in a hidden run along with its text. This engine
/// cannot: `docx::parse::body::extend_from_run` flushes those children into
/// sibling inlines of their own, and `Inline::Symbol` / `Inline::Image` /
/// `Inline::Pict` carry no run properties, so the `w:vanish` that governed them
/// is gone before layout sees it.
///
/// Closing it is a model change — carry the run's `w:rPr` onto those inlines —
/// not a change to the filter. This test exists so that change announces itself
/// instead of silently contradicting a passing suite. Asserted as "something
/// besides VISIBLE is drawn on that line", never as a glyph: which face the host
/// substitutes for Wingdings is its choice.
#[test]
fn a_hidden_symbol_still_draws_because_the_model_drops_its_run_properties() {
    let drawn = drawn(&pages());
    let last_line = drawn
        .iter()
        .map(|(_, y, _)| *y)
        .fold(f32::MIN, |a, b| a.max(b));
    let on_last: Vec<&String> = drawn
        .iter()
        .filter(|(_, y, _)| (y - last_line).abs() < 0.01)
        .map(|(_, _, t)| t)
        .collect();
    assert!(
        on_last.iter().any(|t| !t.contains("VISIBLE")),
        "the symbol survives its hidden run — see this test's doc comment: {on_last:?}"
    );
}

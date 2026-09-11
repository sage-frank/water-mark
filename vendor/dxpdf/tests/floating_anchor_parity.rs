//! §20.4.3.2 / §20.4.3.5 `inside`/`outside` on the **vertical** axis — issue
//! #165, question 3.
//!
//! `test-files/issue-165-floatv.docx` is a two-sided (`w:mirrorMargins`)
//! document of six pages, each carrying one anchored 36pt image, on a 612×792pt
//! sheet whose top margin (72pt) and bottom margin (144pt) differ on purpose:
//! with symmetric margins a top/bottom mirror is invisible.
//!
//! Word's placement of those six, which is what the expectations below encode:
//!
//! | page   | anchor                     | Word                    |
//! |--------|----------------------------|-------------------------|
//! | 1 odd  | `margin` + `align=inside`  | top                     |
//! | 2 even | `margin` + `align=inside`  | bottom                  |
//! | 3 odd  | `insideMargin` + offset 0  | glued to the page top   |
//! | 4 even | `insideMargin` + offset 0  | below the bottom margin |
//! | 5 odd  | `margin` + `align=outside` | bottom (like page 2)    |
//! | 6 even | `margin` + `align=outside` | top (like page 1)       |
//!
//! So vertically `inside` is the top on an odd (recto) page and the bottom on
//! an even one, and `outside` is the complement.
//!
//! The arithmetic is checked against the default page by unit tests in
//! `render::layout::build::floating`. What this file adds is the **wiring**: a
//! position that depends on the page cannot be settled where floats are
//! extracted, so it is carried as both readings and resolved during pagination.
//! Only an end-to-end render proves that deferral survives to the drawn page —
//! a unit test on the resolver would pass just as well if nothing downstream
//! ever asked for the parity.

use dxpdf::render::layout::draw_command::DrawCommand;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test-files/issue-165-floatv.docx"
);

/// Top edge of the single anchored image on each page, in page coordinates.
fn image_tops() -> Vec<f32> {
    let bytes = std::fs::read(FIXTURE).expect("fixture is committed");
    let doc = dxpdf::docx::parse(&bytes).expect("fixture parses");
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);
    pages
        .iter()
        .enumerate()
        .map(|(i, page)| {
            let tops: Vec<f32> = page
                .commands
                .iter()
                .filter_map(|c| match c {
                    DrawCommand::Image { rect, .. } => Some(rect.origin.y.raw()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                tops.len(),
                1,
                "page {} carries exactly one anchored image, found {tops:?}",
                i + 1
            );
            tops[0]
        })
        .collect()
}

/// Page geometry, in points: 612 × 792 with a 72pt top and a 144pt bottom
/// margin. The image is 36pt square.
const PAGE_H: f32 = 792.0;
const MARGIN_TOP: f32 = 72.0;
const MARGIN_BOTTOM: f32 = 144.0;
const IMAGE: f32 = 36.0;

#[test]
fn vertical_inside_and_outside_mirror_with_the_page() {
    let margin_box_bottom = PAGE_H - MARGIN_BOTTOM - IMAGE;
    let bottom_margin_strip = PAGE_H - MARGIN_BOTTOM;
    let expected = [
        ("1 odd  margin/inside", MARGIN_TOP),
        ("2 even margin/inside", margin_box_bottom),
        ("3 odd  insideMargin+0", 0.0),
        ("4 even insideMargin+0", bottom_margin_strip),
        ("5 odd  margin/outside", margin_box_bottom),
        ("6 even margin/outside", MARGIN_TOP),
    ];

    let tops = image_tops();
    assert_eq!(tops.len(), expected.len(), "six pages, one probe each");
    for (top, (what, want)) in tops.iter().zip(expected) {
        assert!(
            (top - want).abs() < 0.01,
            "page {what}: expected y={want}, got y={top}"
        );
    }
}

/// The discriminating property on its own, stated without the arithmetic: the
/// same anchor lands in a different place on an odd and an even page. If the
/// parity were resolved away at extraction time — or never asked for — these
/// pairs would be equal, and the test above could still pass by coincidence on
/// a symmetric page.
#[test]
fn the_same_anchor_lands_differently_on_facing_pages() {
    let tops = image_tops();
    for (odd, even, what) in [
        (0, 1, "margin/inside"),
        (2, 3, "insideMargin"),
        (4, 5, "margin/outside"),
    ] {
        assert!(
            (tops[odd] - tops[even]).abs() > 1.0,
            "{what}: odd page y={} and even page y={} must differ",
            tops[odd],
            tops[even]
        );
    }
    // …and `outside` is `inside` swapped, not some third placement.
    assert!(
        (tops[0] - tops[5]).abs() < 0.01 && (tops[1] - tops[4]).abs() < 0.01,
        "outside mirrors inside: got inside {:?}, outside {:?}",
        &tops[0..2],
        &tops[4..6]
    );
}

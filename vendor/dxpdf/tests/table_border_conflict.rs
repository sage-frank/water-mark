//! [MS-OI29500] §17.4.66 — resolving a shared table-cell edge when the two
//! facing cells disagree.
//!
//! The note's first conflict step reads:
//!
//! > If the conflicting table cell border is *none* (no border), then the
//! > opposing border shall be displayed. If the conflicting table cell border
//! > is *nil*, then no border shall be displayed.
//!
//! Read literally that makes `nil` beat everything on the far side of the edge,
//! which is what dxpdf did and what dropped real borders out of `IP 05
//! Trenches`. It is wrong. **`nil` empties its own cell's edge and nothing
//! else** — it is how a cell declines the inheritance the note describes one
//! step earlier (style → `tblPrEx` → `tblBorders`), which is the whole of its
//! difference from `none`. The facing cell's border is untouched.
//!
//! Three independent facts fix the reading, and nothing contradicts it:
//!
//! * `<w:bottom w:val="single"/>` above `<w:top w:val="nil"/>` — Word draws the
//!   line, and so does macOS's own DOCX renderer (checked via `qlmanage` on this
//!   very markup);
//! * the same document's `Date/Time:` cell *inherits* its bottom from `insideH`
//!   and is faced by a `gridSpan=2` spacer cell whose `nil` was aimed at the
//!   neighbouring column. Word draws that line too, and could not do otherwise:
//!   a cell paints one border across its whole width, so a wide cell's `nil`
//!   cannot punch a hole in the cell above it;
//! * down that document's spacer columns the generator writes `nil` on **both**
//!   sides of every shared edge. Writing both is only necessary because one
//!   alone does not suppress.
//!
//! It is also what makes Word's built-in `Medium List 2` render: its heavy
//! header rule sits on `firstRow`'s `bottom` with `nil` on the `band1Horz` `top`
//! directly below. Suppressing there erased the rule that defines the style.
//!
//! `nil` is still not a no-op — with nothing facing it, declining inheritance is
//! exactly what removes a border. The cases below pin both halves.

use std::io::Write;

use dxpdf::render::layout::draw_command::{DrawCommand, LayoutedPage};

fn make_docx(document_xml: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        zip.start_file("[Content_Types].xml", o).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#,
        )
        .unwrap();

        zip.start_file("_rels/.rels", o).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#,
        )
        .unwrap();

        zip.start_file("word/document.xml", o).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Every side plus `insideH`/`insideV`, as direct table formatting — the
/// `Table Grid` shape, so an omitted cell edge inherits a real border.
const TBL_BORDERS: &str = r#"<w:tblBorders>
  <w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/>
  <w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/>
  <w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/>
  <w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/>
  <w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/>
  <w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/>
</w:tblBorders>"#;

/// `None` omits the edge entirely (so it inherits); `Some(v)` writes
/// `<w:{edge} w:val="{v}"/>`, with `single` spelled out in full.
fn tc(text: &str, edge: &str, val: Option<&str>) -> String {
    let borders = match val {
        None => String::new(),
        Some("single") => format!(
            r#"<w:tcBorders><w:{edge} w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tcBorders>"#
        ),
        Some(v) => format!(r#"<w:tcBorders><w:{edge} w:val="{v}"/></w:tcBorders>"#),
    };
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/>{borders}</w:tcPr>
             <w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#
    )
}

fn document(grid_cols: usize, rows: &str) -> String {
    let grid: String = (0..grid_cols)
        .map(|_| r#"<w:gridCol w:w="3000"/>"#)
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tblPr><w:tblW w:w="0" w:type="auto"/>{TBL_BORDERS}<w:tblLayout w:type="fixed"/></w:tblPr>
      <w:tblGrid>{grid}</w:tblGrid>
      {rows}
    </w:tbl>
    <w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
  </w:body>
</w:document>"#
    )
}

fn layout(document_xml: &str) -> Vec<LayoutedPage> {
    let doc = dxpdf::docx::parse(&make_docx(document_xml)).expect("parse");
    dxpdf::render::resolve_and_layout(doc).1
}

/// Border rects are painted thin and long. Returns the distinct positions along
/// `across` (the axis the line spans *no* distance on), rounded to 0.01pt so
/// two segments of one line collapse to a single entry.
fn border_positions(pages: &[LayoutedPage], horizontal: bool) -> Vec<i64> {
    let mut v: Vec<i64> = pages
        .iter()
        .flat_map(|p| &p.commands)
        .filter_map(|c| match c {
            DrawCommand::Rect { rect, .. } => {
                let (w, h) = (rect.size.width.raw(), rect.size.height.raw());
                let thin = if horizontal {
                    h < 2.0 && w > 20.0
                } else {
                    w < 2.0 && h > 5.0
                };
                if !thin {
                    return None;
                }
                let pos = if horizontal {
                    rect.origin.y.raw()
                } else {
                    rect.origin.x.raw()
                };
                Some((pos * 100.0).round() as i64)
            }
            _ => None,
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Two stacked rows, each one cell; `upper_bottom`/`lower_top` are the
/// `w:val`s written on the shared edge (`None` = omitted).
fn stacked(upper_bottom: Option<&str>, lower_top: Option<&str>) -> Vec<i64> {
    let rows = format!(
        "<w:tr>{}</w:tr><w:tr>{}</w:tr>",
        tc("upper", "bottom", upper_bottom),
        tc("lower", "top", lower_top),
    );
    border_positions(&layout(&document(1, &rows)), true)
}

/// One row, two cells side by side; `left_right`/`right_left` are the `w:val`s
/// written on the shared edge.
fn abutting(left_right: Option<&str>, right_left: Option<&str>) -> Vec<i64> {
    let rows = format!(
        "<w:tr>{}{}</w:tr>",
        tc("L", "right", left_right),
        tc("R", "left", right_left),
    );
    border_positions(&layout(&document(2, &rows)), false)
}

// --- `nil` does not reach across the shared edge -------------------------------

/// The `IP 05 Trenches` regression, reduced: the row above declares a bottom
/// border, the row below declares `<w:top w:val="nil"/>`. Three horizontal
/// lines — table top, the shared edge, table bottom.
#[test]
fn a_declared_bottom_survives_a_nil_top_below() {
    assert_eq!(
        stacked(Some("single"), Some("nil")).len(),
        3,
        "the declared bottom must still be drawn under the nil top"
    );
}

/// …and symmetrically, with the `nil` above and the declaration below.
#[test]
fn a_declared_top_survives_a_nil_bottom_above() {
    assert_eq!(
        stacked(Some("nil"), Some("single")).len(),
        3,
        "resolution is symmetric — which side wrote nil is not which side wins"
    );
}

/// An **inherited** border survives a facing `nil` too. This is the same
/// document's `Date/Time:` cell: it leaves `bottom` omitted, so it takes the
/// table's `insideH`, and the thin spacer row below spans it with a
/// `gridSpan=2` cell whose `top` is `nil` — a `nil` aimed at the *spacer
/// column* beside it. A cell paints one border across its whole width, so a
/// wide cell's `nil` cannot punch a hole in the cell above it.
#[test]
fn an_inherited_border_survives_a_nil_on_the_facing_cell() {
    assert_eq!(
        stacked(None, Some("nil")).len(),
        3,
        "the upper cell still draws the insideH it inherited"
    );
}

/// The vertical axis takes the same rule. In `IP 05 Trenches` this is the
/// `Inspection Points | Inspection Type` divider, lost with the same markup.
#[test]
fn a_declared_right_survives_a_nil_left_beside() {
    assert_eq!(
        abutting(Some("single"), Some("nil")).len(),
        3,
        "the declared right must still be drawn against the nil left"
    );
}

#[test]
fn a_declared_left_survives_a_nil_right_beside() {
    assert_eq!(abutting(Some("nil"), Some("single")).len(), 3);
}

#[test]
fn an_inherited_border_survives_a_nil_beside_it() {
    assert_eq!(abutting(None, Some("nil")).len(), 3);
}

// --- …but `nil` still removes its own cell's border ----------------------------

/// `nil` on both sides — which is what Word writes when a border is removed
/// through its UI, and what this document's generator writes on **both** sides
/// of every shared edge down its spacer columns. Writing both is only necessary
/// because one alone does not suppress.
#[test]
fn nil_on_both_sides_suppresses() {
    assert_eq!(stacked(Some("nil"), Some("nil")).len(), 2);
}

#[test]
fn nil_on_both_sides_suppresses_vertically() {
    assert_eq!(abutting(Some("nil"), Some("nil")).len(), 2);
}

/// The proof that `nil` is not a no-op even with nothing facing it: at the
/// table's **outer** edge there is no opposing cell, so the only thing `nil`
/// can do is decline the table's own `top` border — and it does. This is the
/// half [MS-OI29500] §17.4.66 really carries, and the half that separates `nil`
/// from `none`: an omitted or `none` edge would inherit that border instead.
#[test]
fn nil_declines_the_table_border_at_an_outer_edge() {
    let one = |val: Option<&str>| {
        let rows = format!("<w:tr>{}</w:tr>", tc("only", "top", val));
        border_positions(&layout(&document(1, &rows)), true).len()
    };
    assert_eq!(one(None), 2, "baseline: the table's own top and bottom");
    assert_eq!(one(Some("none")), 2, "`none` inherits the table's top");
    assert_eq!(
        one(Some("nil")),
        1,
        "`nil` declines it — only the bottom left"
    );
}

// --- `none` is not `nil` ------------------------------------------------------

/// §17.4.66: *"If the conflicting table cell border is none (no border), then
/// the opposing border shall be displayed."*
#[test]
fn none_yields_to_the_declared_border() {
    assert_eq!(stacked(Some("single"), Some("none")).len(), 3);
}

/// …and `none` inherits exactly like an omitted edge, so the table's `insideH`
/// survives it.
#[test]
fn none_inherits_the_table_border() {
    assert_eq!(stacked(None, Some("none")).len(), 3);
}

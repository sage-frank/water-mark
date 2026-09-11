//! §17.7.6 / §17.4.16: conditional-formatting regions are addressed by **grid
//! column**, not by a cell's index within its row.
//!
//! The two readings only diverge once a row stops mapping one `<w:tc>` to one
//! `<w:gridCol>` — `w:gridSpan` (§17.4.17) makes one cell cover several, and
//! `w:gridBefore` (§17.4.15) makes the first cell start away from column 0.
//! Every case below is built so that the cell-index reading and the grid-column
//! reading predict *different* shading, which is the only thing that makes the
//! assertions worth writing.
//!
//! # The evidence
//!
//! Word records the regions it assigned in `w:cnfStyle` (§17.3.1.8), so a
//! Word-authored document containing `gridSpan` under a style with a `lastCol`
//! layer is Word's own answer, written down. In
//! `test-files/sample-docx-files-sample1.docx`, the `Calendar3` table declares
//! a 14-column `<w:tblGrid>` and `<w:tblLook w:val="05A0"/>` — which has
//! lastColumn **on**, so its absence on a cell is real signal — and its first
//! row is a single cell with `w:gridSpan="13"`, covering grid columns 0 to 12.
//! Word wrote `<w:cnfStyle w:val="001000000000"/>` on that cell: `firstCol`,
//! and nothing else. The cell-index reading has to say `firstCol` *and*
//! `lastCol`, the cell being both the first and the last in its row. Word says
//! it is not in the last column, because it never reaches column 13. Rows 1
//! onward corroborate from the other side: their cell 12 spans grid columns 12
//! and 13, reaching the last one, and Word marks it `lastCol`.
//!
//! That comparison is asserted directly against the fixture by
//! `word_cnf_style_agrees_with_the_grid_column_reading` in
//! `src/render/resolve/conditional.rs`. What is pinned *here* is the call site:
//! that the grid column a cell occupies is what layout actually hands the
//! resolver.

use std::io::Write;

use dxpdf::render::layout::draw_command::{DrawCommand, LayoutedPage};

/// firstCol layer shading.
const RED: &str = "#FF0000";
/// lastCol layer shading.
const BLUE: &str = "#0000FF";

fn make_docx(document_xml: &str, styles_xml: &str) -> Vec<u8> {
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
  <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
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

        zip.start_file("word/_rels/document.xml.rels", o).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#,
        )
        .unwrap();

        zip.start_file("word/styles.xml", o).unwrap();
        zip.write_all(styles_xml.as_bytes()).unwrap();

        zip.start_file("word/document.xml", o).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// A style whose only conditional layers are `firstCol` (red) and `lastCol`
/// (blue), so every shaded rect on the page names the region it came from.
fn styles() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="table" w:styleId="TestTbl">
    <w:name w:val="Test Table"/>
    <w:tblPr/>
    <w:tblStylePr w:type="firstCol">
      <w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="FF0000"/></w:tcPr>
    </w:tblStylePr>
    <w:tblStylePr w:type="lastCol">
      <w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="0000FF"/></w:tcPr>
    </w:tblStylePr>
  </w:style>
</w:styles>"#
}

/// A four-column table. `rows` supplies the `<w:tr>` elements verbatim, so each
/// test states its own grid geometry.
///
/// The `tblLook` switches firstColumn and lastColumn on and everything else
/// off: with no row regions and no banding, every rect the page carries is one
/// of the two column layers.
///
/// Geometry is stated rather than defaulted so the expected x positions read
/// off the fixture: 12240 − 2×1440 twips of content starts at x = 72 pt, and
/// `tblW` of 8000 twips over four 2000-twip columns makes each column 100 pt.
fn document(rows: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tblPr>
        <w:tblStyle w:val="TestTbl"/>
        <w:tblW w:w="8000" w:type="dxa"/>
        <w:tblLook w:firstRow="0" w:lastRow="0" w:firstColumn="1"
                   w:lastColumn="1" w:noHBand="1" w:noVBand="1"/>
      </w:tblPr>
      <w:tblGrid>
        <w:gridCol w:w="2000"/><w:gridCol w:w="2000"/>
        <w:gridCol w:w="2000"/><w:gridCol w:w="2000"/>
      </w:tblGrid>
      {rows}
    </w:tbl>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
               w:header="720" w:footer="720" w:gutter="0"/>
    </w:sectPr>
  </w:body>
</w:document>"#
    )
}

/// One `<w:tc>`, optionally spanning several grid columns.
fn cell(span: u32) -> String {
    let pr = if span > 1 {
        format!(r#"<w:tcPr><w:gridSpan w:val="{span}"/></w:tcPr>"#)
    } else {
        String::new()
    };
    format!("<w:tc>{pr}<w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>")
}

fn layout(document_xml: &str) -> Vec<LayoutedPage> {
    let doc = dxpdf::docx::parse(&make_docx(document_xml, styles())).expect("parse");
    dxpdf::render::resolve_and_layout(doc).1
}

/// Every shaded rect as `(color, left x, width)`, rounded to whole points.
/// Sorted, because what is being asserted is *which* cells carry a region, not
/// the order layout happened to emit them in.
fn shaded(pages: &[LayoutedPage]) -> Vec<(String, i32, i32)> {
    let mut v: Vec<(String, i32, i32)> = pages
        .iter()
        .flat_map(|p| &p.commands)
        .filter_map(|c| match c {
            DrawCommand::Rect { rect, color, .. } => {
                let hex = format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b);
                (hex == RED || hex == BLUE).then(|| {
                    (
                        hex,
                        rect.origin.x.raw().round() as i32,
                        rect.size.width.raw().round() as i32,
                    )
                })
            }
            _ => None,
        })
        .collect();
    v.sort();
    v
}

/// The discriminating case, and Word's own answer to it: a lone cell spanning
/// all but the last grid column is in the **first** column region and not the
/// last. Read as a cell index it is both — the only cell in its row.
#[test]
fn a_span_that_stops_short_of_the_last_grid_column_is_not_the_last_column() {
    let pages = layout(&document(&format!("<w:tr>{}</w:tr>", cell(3))));
    assert_eq!(
        shaded(&pages),
        vec![(RED.to_string(), 72, 300)],
        "grid columns 0..2 of 4: firstCol only, over the cell's full 3-column width"
    );
}

/// The other side of the same rule, and sample1's corroborating case: a span
/// that *reaches* the last grid column is in the last column region even though
/// it starts one column short of it.
#[test]
fn a_span_that_reaches_the_last_grid_column_is_the_last_column() {
    let row = format!("<w:tr>{}{}{}</w:tr>", cell(1), cell(1), cell(2));
    let pages = layout(&document(&row));
    assert_eq!(
        shaded(&pages),
        vec![(BLUE.to_string(), 272, 200), (RED.to_string(), 72, 100),],
        "grid column 0 is firstCol; the cell covering 2..3 reaches the last column"
    );
}

/// §17.4.15: `gridBefore` moves a row's first cell off grid column 0, so the
/// first `<w:tc>` is not in the first column region at all.
#[test]
fn grid_before_moves_the_first_cell_out_of_the_first_column_region() {
    let row = format!(
        r#"<w:tr><w:trPr><w:gridBefore w:val="1"/></w:trPr>{}{}{}</w:tr>"#,
        cell(1),
        cell(1),
        cell(1)
    );
    let pages = layout(&document(&row));
    assert_eq!(
        shaded(&pages),
        vec![(BLUE.to_string(), 372, 100)],
        "the row's first cell sits at grid column 1, so nothing is firstCol"
    );
}

/// The control: with one cell per grid column the two readings agree, so this
/// is what proves the fixture can produce both regions at all.
#[test]
fn one_cell_per_grid_column_marks_the_outer_two() {
    let row = format!("<w:tr>{}{}{}{}</w:tr>", cell(1), cell(1), cell(1), cell(1));
    let pages = layout(&document(&row));
    assert_eq!(
        shaded(&pages),
        vec![(BLUE.to_string(), 372, 100), (RED.to_string(), 72, 100)],
    );
}

/// One `<w:tc>` whose `gridSpan` is written verbatim, so a value the `u32`
/// range allows but the layout does not can be stated at all.
fn cell_with_raw_span(span: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:gridSpan w:val="{span}"/></w:tcPr>
             <w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>"#
    )
}

/// §17.4.17: `gridSpan` counts the grid columns a cell covers, and a `<w:tc>`
/// that exists covers at least one. Every pass that walks the grid says so with
/// `span.max(1)` — `measure_table_rows`, `emit_table_rows`,
/// `find_cell_at_grid_col`, the orphan-`vMerge` repair — but the build layer's
/// own walk spelled it `unwrap_or(1)`, which agrees on an *absent* `gridSpan`
/// and disagrees on `w:val="0"`.
///
/// The disagreement is not cosmetic now that the walk addresses §17.7.6
/// conditional formatting: a zero span gives the cell no columns, so every
/// later cell in the row is addressed one grid column short, the row's last
/// cell stops reaching the last grid column and loses its `lastCol` layer.
///
/// Asserted as parity against `gridSpan="1"` rather than against literal
/// coordinates, because what is being pinned is that the two spellings are one
/// walk — not any particular geometry, which the fixture's other tests own.
#[test]
fn a_zero_grid_span_covers_one_grid_column_like_an_absent_one() {
    let with_zero = layout(&document(&format!(
        "<w:tr>{}{}{}{}</w:tr>",
        cell_with_raw_span("0"),
        cell(1),
        cell(1),
        cell(1)
    )));
    let with_one = layout(&document(&format!(
        "<w:tr>{}{}{}{}</w:tr>",
        cell_with_raw_span("1"),
        cell(1),
        cell(1),
        cell(1)
    )));

    assert_eq!(
        shaded(&with_zero),
        shaded(&with_one),
        "a zero gridSpan must address the grid exactly as a span of one"
    );
    assert_eq!(
        shaded(&with_zero),
        vec![(BLUE.to_string(), 372, 100), (RED.to_string(), 72, 100)],
        "and that grid is the plain one-cell-per-column grid: firstCol at the \
         left margin, lastCol over the fourth column"
    );
}

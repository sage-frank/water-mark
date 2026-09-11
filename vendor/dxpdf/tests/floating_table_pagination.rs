//! §17.4.57 / §17.4.56 — a floating table (`<w:tblpPr>`) that is taller
//! than the page body must paginate, not loop.
//!
//! The combination that used to hang the layout pass is narrow: a
//! floating table, `<w:tblOverlap w:val="never"/>`, and a row count that
//! overflows one page. `resolve_floating_anchor` reported `Spillover`
//! for *any* overflow, and the caller answers a spillover by pushing a
//! fresh page and re-resolving. A fresh page has no floats, so the
//! anchor collapsed to the page top and overflowed again — an unbounded
//! loop that allocated one `LayoutedPage` per iteration until the OS
//! killed the process.
//!
//! These tests render the real document end-to-end, which is what makes
//! them meaningful: the unit tests in `floating_table.rs` pin the
//! resolver's contract, and these pin that the *caller* honors it.

use std::io::Write;

/// Create a minimal DOCX (ZIP) in memory wrapping `document_xml`.
fn make_docx(document_xml: &str) -> Vec<u8> {
    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml"
    ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#,
    )
    .unwrap();

    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
    Target="word/document.xml"/>
</Relationships>"#,
    )
    .unwrap();

    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();

    zip.finish().unwrap().into_inner()
}

/// A floating table of `rows` single-cell rows. `overlap` is spliced
/// into `<w:tblPr>` verbatim so a test can select the `never` variant.
fn floating_table_docx(rows: usize, overlap: &str) -> Vec<u8> {
    let body_rows: String = (0..rows)
        .map(|i| {
            format!(
                r#"<w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr>
                   <w:p><w:r><w:t>row {i}</w:t></w:r></w:p></w:tc></w:tr>"#
            )
        })
        .collect();

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>before</w:t></w:r></w:p>
    <w:tbl>
      <w:tblPr>
        <w:tblpPr w:vertAnchor="text" w:horzAnchor="margin" w:tblpX="0" w:tblpY="20"/>
        {overlap}
        <w:tblW w:w="4000" w:type="dxa"/>
      </w:tblPr>
      <w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>
      {body_rows}
    </w:tbl>
    <w:p><w:r><w:t>after</w:t></w:r></w:p>
  </w:body>
</w:document>"#
    );
    make_docx(&xml)
}

fn layout_page_count(bytes: &[u8]) -> usize {
    let doc = dxpdf::docx::parse(bytes).expect("parse");
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);
    pages.len()
}

/// The regression. Before the fix this never returned — it allocated
/// pages until the process was OOM-killed, so the assertion below is
/// secondary to the test completing at all.
#[test]
fn tall_floating_table_with_overlap_never_terminates() {
    let pages = layout_page_count(&floating_table_docx(80, r#"<w:tblOverlap w:val="never"/>"#));
    assert!(pages > 1, "80 rows must paginate across pages, got {pages}");
    assert!(
        pages < 20,
        "80 short rows should need a handful of pages, not {pages} — \
         a page count this high means the anchor is being re-pushed \
         rather than the table being sliced"
    );
}

/// The permitted-overlap case always worked; it is the control that
/// shows `never` now paginates the same way rather than differently.
#[test]
fn tall_floating_table_paginates_the_same_with_and_without_overlap_never() {
    let with_never =
        layout_page_count(&floating_table_docx(80, r#"<w:tblOverlap w:val="never"/>"#));
    let default_overlap = layout_page_count(&floating_table_docx(80, ""));
    assert_eq!(
        with_never, default_overlap,
        "tblOverlap only governs collision with prior floats; with no \
         prior float to collide with, both must paginate identically"
    );
}

/// A short floating table still fits on one page — the fix must not
/// have turned every floating table into a paginating one.
#[test]
fn short_floating_table_with_overlap_never_stays_on_one_page() {
    let pages = layout_page_count(&floating_table_docx(3, r#"<w:tblOverlap w:val="never"/>"#));
    assert_eq!(pages, 1, "3 rows fit on the anchor page");
}

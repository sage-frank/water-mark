//! §20.1.2.1.18 `a:normAutofit` — the shrink Word already computed, end to end.
//!
//! `normAutofit` is not a hint. When Word lays out a shape whose text does not
//! fit, it shrinks the text and writes the *result* into the file as
//! `@fontScale` and `@lnSpcReduction`. A renderer that parses the element but
//! drops its attributes therefore draws every shrunk body at full size — and
//! then overflows the box, because `vertOverflow` defaults to `overflow`.
//!
//! Neither `test-files/` nor `test-cases/` contains a single `a:normAutofit`
//! element — checked, not assumed — so the corpus renders identically whether
//! this works or not, and is no evidence either way. The fixture is built here
//! for the same reason `shape_text_anchor.rs` and `tab_alignment.rs` build
//! theirs: the XML *is* the point of the test.

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

/// A paragraph-anchored `wps:wsp` 200x120pt holding two 20pt paragraphs, with
/// `body_pr_children` spliced into its `wps:bodyPr`.
///
/// 2540000 x 1524000 EMU = 200 x 120pt. The run size is written explicitly
/// (`w:sz` is in half-points, so 40 = 20pt) so every assertion below is an
/// exact number rather than a function of the host's default font.
fn shape_document(body_pr_children: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
            xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
  <w:body>
    <w:p><w:r><w:drawing>
      <wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0"
                 relativeHeight="1" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">
        <wp:simplePos x="0" y="0"/>
        <wp:positionH relativeFrom="margin"><wp:posOffset>0</wp:posOffset></wp:positionH>
        <wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>
        <wp:extent cx="2540000" cy="1524000"/>
        <wp:wrapNone/>
        <wp:docPr id="1" name="Box"/>
        <a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
          <wps:wsp>
            <wps:cNvSpPr/>
            <wps:spPr>
              <a:xfrm><a:off x="0" y="0"/><a:ext cx="2540000" cy="1524000"/></a:xfrm>
              <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
            </wps:spPr>
            <wps:txbx><w:txbxContent>
              <w:p><w:r><w:rPr><w:sz w:val="40"/></w:rPr><w:t>One</w:t></w:r></w:p>
              <w:p><w:r><w:rPr><w:sz w:val="40"/></w:rPr><w:t>Two</w:t></w:r></w:p>
            </w:txbxContent></wps:txbx>
            <wps:bodyPr rot="0" vert="horz" wrap="square" vertOverflow="overflow"
                        lIns="0" tIns="0" rIns="0" bIns="0" anchor="t">{body_pr_children}</wps:bodyPr>
          </wps:wsp>
        </a:graphicData></a:graphic>
      </wp:anchor>
    </w:drawing></w:r></w:p>
  </w:body>
</w:document>"#
    )
}

fn layout(body_pr_children: &str) -> Vec<LayoutedPage> {
    let xml = shape_document(body_pr_children);
    let doc = dxpdf::docx::parse(&make_docx(&xml)).expect("parse");
    dxpdf::render::resolve_and_layout(doc).1
}

/// `(font_size, y)` of each text command in the shape's body, in draw order.
fn body_text(pages: &[LayoutedPage]) -> Vec<(f32, f32)> {
    pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Text {
                text,
                font_size,
                position,
                ..
            } if text.trim() == "One" || text.trim() == "Two" => {
                Some((font_size.raw(), position.y.raw()))
            }
            _ => None,
        })
        .collect()
}

fn font_sizes(body_pr_children: &str) -> Vec<f32> {
    body_text(&layout(body_pr_children))
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

/// Baseline-to-baseline distance between the body's two paragraphs.
fn line_advance(body_pr_children: &str) -> f32 {
    let t = body_text(&layout(body_pr_children));
    assert_eq!(t.len(), 2, "fixture must emit exactly two lines: {t:?}");
    t[1].1 - t[0].1
}

// --- `@fontScale` ------------------------------------------------------------

/// The regression pin, and it must be written first: a `normAutofit` with no
/// attributes means 100%, so the overwhelmingly common case cannot move.
#[test]
fn a_bare_norm_autofit_leaves_the_body_unscaled() {
    assert_eq!(font_sizes("<a:normAutofit/>"), vec![20.0, 20.0]);
}

#[test]
fn no_body_pr_autofit_at_all_leaves_the_body_unscaled() {
    assert_eq!(font_sizes(""), vec![20.0, 20.0]);
}

/// §20.1.2.1.18 `@fontScale`, in thousandths of a percent: 62500 = 62.5%.
/// 20pt × 0.625 = 12.5pt.
#[test]
fn font_scale_shrinks_every_run_in_the_body() {
    assert_eq!(
        font_sizes(r#"<a:normAutofit fontScale="62500"/>"#),
        vec![12.5, 12.5],
        "the scale is a property of the body, so it reaches every run in it"
    );
}

/// §20.1.2.1.16: `noAutofit` is the explicit "do not shrink". A `fontScale`
/// written alongside it is not this element's attribute and must not apply.
#[test]
fn no_autofit_does_not_shrink() {
    assert_eq!(font_sizes("<a:noAutofit/>"), vec![20.0, 20.0]);
}

/// §20.1.2.1.20: `spAutoFit` resizes the *shape* to its text. This sub-layout
/// cannot resize its host, so it degrades to no shrink rather than inventing
/// one.
#[test]
fn sp_auto_fit_renders_unscaled() {
    assert_eq!(font_sizes("<a:spAutoFit/>"), vec![20.0, 20.0]);
}

// --- `@lnSpcReduction` -------------------------------------------------------

/// §20.1.2.1.18 `@lnSpcReduction`, thousandths of a percent: 20000 = 20% *off*
/// the line spacing, so the advance is 80% of its unreduced value. Font size is
/// a separate attribute and must not move with it.
#[test]
fn line_spacing_reduction_tightens_the_body_without_resizing_it() {
    let plain = line_advance("<a:normAutofit/>");
    let tight = line_advance(r#"<a:normAutofit lnSpcReduction="20000"/>"#);

    assert!(plain > 0.0, "fixture must stack its two paragraphs");
    assert!(
        (tight / plain - 0.8).abs() < 0.001,
        "advance should fall to 80%: {plain} -> {tight}",
    );
    assert_eq!(
        font_sizes(r#"<a:normAutofit lnSpcReduction="20000"/>"#),
        vec![20.0, 20.0],
        "lnSpcReduction must not touch glyph size",
    );
}

/// The two attributes are independent and Word writes both together whenever
/// one shrink was not enough.
#[test]
fn font_scale_and_line_spacing_reduction_compose() {
    let both = r#"<a:normAutofit fontScale="62500" lnSpcReduction="20000"/>"#;
    assert_eq!(font_sizes(both), vec![12.5, 12.5]);

    // The advance follows the *scaled* text, then loses a further 20%.
    let scaled_only = line_advance(r#"<a:normAutofit fontScale="62500"/>"#);
    assert!(
        (line_advance(both) / scaled_only - 0.8).abs() < 0.001,
        "reduction applies on top of the font scale",
    );
}

/// A scale of 100000 is 100% — the identity, and worth pinning separately from
/// the absent case so the conversion cannot be off by a factor of 1000.
#[test]
fn an_explicit_full_scale_is_the_identity() {
    assert_eq!(
        font_sizes(r#"<a:normAutofit fontScale="100000" lnSpcReduction="0"/>"#),
        vec![20.0, 20.0]
    );
    let plain = line_advance("<a:normAutofit/>");
    let explicit = line_advance(r#"<a:normAutofit fontScale="100000" lnSpcReduction="0"/>"#);
    assert!((explicit - plain).abs() < 0.001);
}

// --- the shrink is scoped to the body that declares it -----------------------

/// Body text outside the shape shares the document's style cascade with the
/// text inside it. A scale that leaked out of the shape would resize the page.
#[test]
fn the_scale_does_not_escape_the_shape() {
    let xml = shape_document(r#"<a:normAutofit fontScale="62500"/>"#).replace(
        "</w:body>",
        r#"<w:p><w:r><w:rPr><w:sz w:val="40"/></w:rPr><w:t>Outside</w:t></w:r></w:p></w:body>"#,
    );
    let doc = dxpdf::docx::parse(&make_docx(&xml)).expect("parse");
    let pages = dxpdf::render::resolve_and_layout(doc).1;

    let outside: Vec<f32> = pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Text {
                text, font_size, ..
            } if text.trim() == "Outside" => Some(font_size.raw()),
            _ => None,
        })
        .collect();

    assert_eq!(outside, vec![20.0], "the page's own text keeps its size");
}

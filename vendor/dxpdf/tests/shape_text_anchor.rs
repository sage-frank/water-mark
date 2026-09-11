//! §20.1.10.60 `ST_TextAnchoringType` — vertical anchoring of a shape's text
//! body, end to end.
//!
//! No document in `test-files/` uses a non-top `anchor` on a `wps:wsp` — the
//! one `anchor="ctr"` in the corpus lives in `theme1.xml`, as a theme default
//! that no shape instantiates — so the whole corpus renders byte-identically
//! whether this works or not. The fixture is built here for the same reason
//! `tab_alignment.rs` builds its own: the XML *is* the point of the test.

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

/// A paragraph-anchored `wps:wsp` 200x120pt, with 4pt insets on every side and
/// the given `anchor`, holding one short line of text.
///
/// 2540000 x 1524000 EMU = 200 x 120pt; 50800 EMU = 4pt.
fn shape_document(anchor: &str) -> String {
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
              <w:p><w:r><w:t>Boxed</w:t></w:r></w:p>
            </w:txbxContent></wps:txbx>
            <wps:bodyPr rot="0" vert="horz" wrap="square" vertOverflow="overflow"
                        lIns="50800" tIns="50800" rIns="50800" bIns="50800"
                        anchor="{anchor}"/>
          </wps:wsp>
        </a:graphicData></a:graphic>
      </wp:anchor>
    </w:drawing></w:r></w:p>
  </w:body>
</w:document>"#
    )
}

/// The y of the text the shape's body holds.
fn boxed_text_y(pages: &[LayoutedPage]) -> f32 {
    pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .find_map(|c| match c {
            DrawCommand::Text { text, position, .. } if &**text == "Boxed" => {
                Some(position.y.raw())
            }
            _ => None,
        })
        .expect("the shape's text body is emitted")
}

fn y_for(anchor: &str) -> f32 {
    let bytes = make_docx(&shape_document(anchor));
    let doc = dxpdf::docx::parse(&bytes).expect("fixture parses");
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);
    boxed_text_y(&pages)
}

/// §20.1.10.60: `t` pins the body under the top inset, `ctr` centres it in the
/// box `bIns` closes off, `b` sits it on the bottom inset.
///
/// The assertions are differences between the three runs — the emitted y is a
/// baseline carrying the host font's ascent, and differencing cancels it.
#[test]
fn body_anchor_moves_shape_text_within_the_box() {
    let (top, centre, bottom) = (y_for("t"), y_for("ctr"), y_for("b"));

    assert!(
        top < centre && centre < bottom,
        "t < ctr < b, got {top} / {centre} / {bottom}"
    );

    // The box is 120 − 4 − 4 = 112pt tall. Bottom-anchoring spends all the
    // slack, centring exactly half of it, whatever the line height is.
    let (half, full) = (centre - top, bottom - top);
    assert!(
        (full - 2.0 * half).abs() < 1e-3,
        "the centre offset is half the bottom offset, got {half} / {full}"
    );
    // And the slack the anchoring spent implies a plausible single line.
    let line_height = 112.0 - full;
    assert!(
        line_height > 0.0 && line_height < 112.0,
        "one line fits the 112pt box, implied height {line_height}"
    );
}

/// An omitted `anchor` is `t` (§20.1.2.1.1), so a document that never mentions
/// the attribute renders exactly as it did before anchoring existed.
#[test]
fn an_omitted_anchor_is_top() {
    let without = {
        let xml = shape_document("t").replace("\n                        anchor=\"t\"", "");
        assert!(!xml.contains("anchor="), "the attribute is gone: {xml}");
        let bytes = make_docx(&xml);
        let doc = dxpdf::docx::parse(&bytes).expect("fixture parses");
        let (_, pages) = dxpdf::render::resolve_and_layout(doc);
        boxed_text_y(&pages)
    };
    assert!(
        (without - y_for("t")).abs() < 1e-3,
        "no anchor attribute matches anchor=\"t\", got {without}"
    );
}

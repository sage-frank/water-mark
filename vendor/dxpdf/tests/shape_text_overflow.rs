//! `a:bodyPr/@vertOverflow` — what becomes of shape text taller than its box.
//!
//! The default is `overflow`, and it is the value the whole corpus asks for: 4
//! explicit `vertOverflow="overflow"` and 10 `bodyPr` elements with no
//! `vertOverflow` at all, and not one `clip` or `ellipsis` (checked, not
//! assumed). An earlier attempt on this file read the symptom — "text overflows
//! the shape" — and prescribed clipping unconditionally, which would have
//! deleted content that renders correctly today. So the first two tests here
//! are the ones that pin the default, and they were written to pass.

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

/// Words the fixture puts one per paragraph, so a test can name the lines it
/// expects to survive.
const LINES: [&str; 4] = ["Aaa", "Bbb", "Ccc", "Ddd"];

/// A paragraph-anchored `wps:wsp` `200 x box_h_pt` pt with zero insets, holding
/// four 20pt paragraphs — far more than a short box can hold.
///
/// `overflow_attr` is spliced into `wps:bodyPr` verbatim (pass `""` to omit the
/// attribute entirely); `b_ins_pt` sets `bIns`. 12700 EMU = 1pt.
fn shape_document(overflow_attr: &str, box_h_pt: u32, b_ins_pt: u32) -> String {
    let cy = box_h_pt * 12700;
    let b_ins = b_ins_pt * 12700;
    let paragraphs: String = LINES
        .iter()
        .map(|w| {
            format!(r#"<w:p><w:r><w:rPr><w:sz w:val="40"/></w:rPr><w:t>{w}</w:t></w:r></w:p>"#)
        })
        .collect();
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
        <wp:extent cx="2540000" cy="{cy}"/>
        <wp:wrapNone/>
        <wp:docPr id="1" name="Box"/>
        <a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
          <wps:wsp>
            <wps:cNvSpPr/>
            <wps:spPr>
              <a:xfrm><a:off x="0" y="0"/><a:ext cx="2540000" cy="{cy}"/></a:xfrm>
              <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
              <a:solidFill><a:srgbClr val="DDDDDD"/></a:solidFill>
            </wps:spPr>
            <wps:txbx><w:txbxContent>{paragraphs}</w:txbxContent></wps:txbx>
            <wps:bodyPr rot="0" vert="horz" wrap="square" {overflow_attr}
                        lIns="0" tIns="0" rIns="0" bIns="{b_ins}" anchor="t"/>
          </wps:wsp>
        </a:graphicData></a:graphic>
      </wp:anchor>
    </w:drawing></w:r></w:p>
  </w:body>
</w:document>"#
    )
}

fn layout(overflow_attr: &str, box_h_pt: u32) -> Vec<LayoutedPage> {
    layout_inset(overflow_attr, box_h_pt, 0)
}

fn layout_inset(overflow_attr: &str, box_h_pt: u32, b_ins_pt: u32) -> Vec<LayoutedPage> {
    let xml = shape_document(overflow_attr, box_h_pt, b_ins_pt);
    let doc = dxpdf::docx::parse(&make_docx(&xml)).expect("parse");
    dxpdf::render::resolve_and_layout(doc).1
}

/// The body words actually drawn, in draw order.
fn drawn(overflow_attr: &str, box_h_pt: u32) -> Vec<String> {
    drawn_inset(overflow_attr, box_h_pt, 0)
}

fn drawn_inset(overflow_attr: &str, box_h_pt: u32, b_ins_pt: u32) -> Vec<String> {
    layout_inset(overflow_attr, box_h_pt, b_ins_pt)
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Text { text, .. } if LINES.contains(&text.trim()) => {
                Some(text.trim().to_string())
            }
            _ => None,
        })
        .collect()
}

/// Does the shape's own fill still get drawn? Clipping the *text* must not
/// touch the shape.
fn shape_is_drawn(overflow_attr: &str, box_h_pt: u32) -> bool {
    layout(overflow_attr, box_h_pt)
        .iter()
        .flat_map(|p| p.commands.iter())
        .any(|c| matches!(c, DrawCommand::Path { .. }))
}

// --- `overflow` is the default, and the default must not move ----------------

/// The regression pin. A 40pt box cannot hold four 20pt lines; every one of
/// them is still drawn, because `vertOverflow` defaults to `overflow` and Word
/// draws overflowing shape text.
#[test]
fn an_absent_vert_overflow_draws_every_line() {
    assert_eq!(drawn("", 40), LINES, "the default is overflow, not clip");
}

#[test]
fn an_explicit_overflow_matches_an_absent_one() {
    assert_eq!(drawn(r#"vertOverflow="overflow""#, 40), drawn("", 40));
}

// --- `clip` ------------------------------------------------------------------

/// A box tall enough for everything clips nothing — `clip` is not licence to
/// drop a line that fits.
#[test]
fn clip_keeps_a_body_that_fits() {
    assert_eq!(drawn(r#"vertOverflow="clip""#, 200), LINES);
}

/// …and a box too short keeps only the lines that stay inside it. The exact
/// count depends on the host font's line height, so this asserts the property
/// that matters — `clip` is a strict prefix of `overflow`, and a shorter one.
#[test]
fn clip_drops_the_lines_that_leave_the_box() {
    let overflowed = drawn("", 40);
    let clipped = drawn(r#"vertOverflow="clip""#, 40);

    assert_eq!(overflowed, LINES, "precondition: the body overflows");
    assert!(
        clipped.len() < overflowed.len(),
        "clip must drop something: {clipped:?}",
    );
    assert!(
        overflowed.starts_with(&clipped[..]),
        "clip keeps a prefix — it removes from the bottom, not the middle: {clipped:?}",
    );
    assert!(
        !clipped.is_empty(),
        "a line that does fit must survive: {clipped:?}",
    );
}

/// Nothing the body draws may sit below the box. This is the actual contract
/// `clip` asks for, stated directly rather than through a line count.
#[test]
fn clip_leaves_nothing_below_the_box() {
    let pages = layout(r#"vertOverflow="clip""#, 40);
    let shape_top = pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .find_map(|c| match c {
            DrawCommand::Path { origin, .. } => Some(origin.y.raw()),
            _ => None,
        })
        .expect("the shape itself is drawn");
    let box_bottom = shape_top + 40.0;

    for cmd in pages.iter().flat_map(|p| p.commands.iter()) {
        if let DrawCommand::Text {
            text,
            position,
            font_size,
            ..
        } = cmd
        {
            if LINES.contains(&text.trim()) {
                assert!(
                    position.y.raw() + font_size.raw() <= box_bottom + 0.001,
                    "'{}' is drawn below the box bottom ({} > {box_bottom})",
                    text.trim(),
                    position.y.raw() + font_size.raw(),
                );
            }
        }
    }
}

/// Clipping the text must not clip the shape.
#[test]
fn clip_does_not_remove_the_shape_itself() {
    assert!(shape_is_drawn(r#"vertOverflow="clip""#, 40));
    assert!(shape_is_drawn(r#"vertOverflow="ellipsis""#, 40));
}

/// §20.1.2.1.1: `bIns` closes off the bottom of the box the body sits in, so
/// that — not the shape's extent — is what `clip` clips to. Same 60pt shape
/// twice: with no bottom inset it holds one more line than with a 25pt one.
#[test]
fn clip_clips_to_the_inset_box_not_the_shape_extent() {
    let flush = drawn_inset(r#"vertOverflow="clip""#, 60, 0);
    let inset = drawn_inset(r#"vertOverflow="clip""#, 60, 25);

    assert!(
        !flush.is_empty(),
        "precondition: a 60pt box holds at least one line",
    );
    assert!(
        inset.len() < flush.len(),
        "bIns must shrink what survives the clip: {flush:?} vs {inset:?}",
    );
    assert!(
        flush.starts_with(&inset[..]),
        "still a prefix — the inset only moves the bottom edge up",
    );
}

// --- `ellipsis` --------------------------------------------------------------

/// `ellipsis` is `clip` plus an ellipsis on the last visible line. Deciding
/// which line that is — and refitting it to leave room for the glyph — is a
/// decision this sub-layout does not make, so it degrades to `clip`: the same
/// text is kept, only the indicator is missing. Degrading to `overflow`
/// instead would be further from Word, since `ellipsis` does clip.
#[test]
fn ellipsis_degrades_to_clip() {
    assert_eq!(
        drawn(r#"vertOverflow="ellipsis""#, 40),
        drawn(r#"vertOverflow="clip""#, 40),
    );
}

#[test]
fn ellipsis_keeps_a_body_that_fits() {
    assert_eq!(drawn(r#"vertOverflow="ellipsis""#, 200), LINES);
}

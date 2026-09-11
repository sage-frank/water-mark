//! §17.3.1.19 `w:outlineLvl` → the PDF document outline, end to end.
//!
//! Issue #90: a document whose headings LibreOffice turns into a navigable
//! outline produced a PDF with no `/Outlines` at all, so every viewer's
//! bookmarks sidebar was empty. `w:outlineLvl` was parsed, typed and cascaded,
//! and then dropped between resolve and paint.
//!
//! These tests read the emitted PDF rather than the layout, because the outline
//! is not a layout artefact — it is a document-level structure Skia builds from
//! a structure-element tree, and nothing below the PDF proves it arrived.
//!
//! `test-files/document_outline.docx` is the reporter's own `Minimal.docx`. The
//! titles asserted against it are **measured from the reference PDF attached to
//! the issue**, not chosen: LibreOffice writes the paragraph's text without the
//! §17.9.22 numbering label, even though the fixture's headings are numbered.

use std::io::Write;

/// One outline entry, flattened with its nesting depth.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    depth: usize,
    title: String,
    /// 1-based page number the entry's destination lands on.
    page: u32,
}

// ── Reading the PDF ─────────────────────────────────────────────────────────

/// PDF text strings are either UTF-16BE with a BOM or PDFDocEncoding, and Skia
/// picks per string. Both appear in practice, so decode both rather than
/// asserting against whichever one today's Skia happens to emit.
fn decode_pdf_text(raw: &[u8]) -> String {
    if raw.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        raw.iter().map(|&b| b as char).collect()
    }
}

/// Every outline entry, depth-first in document order.
///
/// Returns `None` when the catalog has no `/Outlines` key at all — which is a
/// different thing from an outline with no entries, and the two are asserted
/// separately below.
fn outline(bytes: &[u8]) -> Option<Vec<Entry>> {
    let doc = lopdf::Document::load_mem(bytes).expect("output parses as PDF");

    // Page object id → 1-based page number, for resolving destinations.
    let pages: std::collections::HashMap<lopdf::ObjectId, u32> =
        doc.get_pages().into_iter().map(|(n, id)| (id, n)).collect();

    let root = doc
        .catalog()
        .expect("catalog")
        .get(b"Outlines")
        .ok()?
        .clone();
    let root = doc.dereference(&root).expect("outline root").1;
    let root = root.as_dict().expect("outline root is a dictionary");

    let mut out = Vec::new();
    if let Ok(first) = root.get(b"First") {
        walk(&doc, &pages, first, 0, &mut out);
    }
    Some(out)
}

fn walk(
    doc: &lopdf::Document,
    pages: &std::collections::HashMap<lopdf::ObjectId, u32>,
    first: &lopdf::Object,
    depth: usize,
    out: &mut Vec<Entry>,
) {
    let mut cur = Some(first.clone());
    while let Some(r) = cur {
        let Ok((_, obj)) = doc.dereference(&r) else {
            return;
        };
        let Ok(d) = obj.as_dict() else { return };

        let title = d
            .get(b"Title")
            .ok()
            .and_then(|t| t.as_str().ok())
            .map(decode_pdf_text)
            .unwrap_or_default();

        // `/Dest` is `[page /XYZ left top zoom]`.
        let page = d
            .get(b"Dest")
            .ok()
            .and_then(|dest| dest.as_array().ok())
            .and_then(|a| a.first())
            .and_then(|p| match p {
                lopdf::Object::Reference(id) => pages.get(id).copied(),
                _ => None,
            })
            .unwrap_or(0);

        out.push(Entry { depth, title, page });

        if let Ok(child) = d.get(b"First") {
            walk(doc, pages, child, depth + 1, out);
        }
        cur = d.get(b"Next").ok().cloned();
    }
}

fn render(bytes: &[u8]) -> Vec<u8> {
    dxpdf::convert(bytes).expect("conversion succeeds")
}

fn render_fixture() -> Vec<u8> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test-files/document_outline.docx"
    );
    render(&std::fs::read(path).expect("fixture is committed"))
}

// ── Building documents ──────────────────────────────────────────────────────

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

fn make_docx(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let overrides: String = parts
            .iter()
            .map(|(name, _)| {
                let ct = match *name {
                    "word/document.xml" => "wordprocessingml.document.main",
                    "word/styles.xml" => "wordprocessingml.styles",
                    n if n.starts_with("word/header") => "wordprocessingml.header",
                    "word/footnotes.xml" => "wordprocessingml.footnotes",
                    other => panic!("no content type registered for {other}"),
                };
                format!(
                    r#"<Override PartName="/{name}" ContentType="application/vnd.openxmlformats-officedocument.{ct}+xml"/>"#
                )
            })
            .collect();

        zip.start_file("[Content_Types].xml", o).unwrap();
        zip.write_all(
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  {overrides}
</Types>"#
            )
            .as_bytes(),
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
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
  <Relationship Id="rId8" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>
</Relationships>"#,
        )
        .unwrap();

        for (name, body) in parts {
            zip.start_file(*name, o).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// `styles.xml` defining `H1`/`H2`/`H3` with `w:outlineLvl` 0/1/2, plus a
/// `Deep` style at whatever level the caller names — used for the levels above
/// PDF's H6.
fn styles(deep_level: u8) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles {W}>
  <w:style w:type="paragraph" w:styleId="H1"><w:name w:val="heading 1"/>
    <w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="H2"><w:name w:val="heading 2"/>
    <w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="H3"><w:name w:val="heading 3"/>
    <w:pPr><w:outlineLvl w:val="2"/></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="Deep"><w:name w:val="deep"/>
    <w:pPr><w:outlineLvl w:val="{deep_level}"/></w:pPr></w:style>
</w:styles>"#
    )
}

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document {W}><w:body>{body}</w:body></w:document>"#
    )
}

/// A paragraph in `style` holding `text`.
fn para(style: &str, text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr>
             <w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}

fn render_body(body: &str) -> Vec<u8> {
    render(&make_docx(&[
        ("word/document.xml", &document(body)),
        ("word/styles.xml", &styles(6)),
    ]))
}

// ── The reporter's document ─────────────────────────────────────────────────

/// The regression pin, and the issue verbatim: this document produced no
/// `/Outlines` key at all.
#[test]
fn the_reporters_document_produces_an_outline() {
    let entries = outline(&render_fixture()).expect("the PDF has an /Outlines catalog entry");
    assert!(!entries.is_empty(), "and it has entries");
}

/// Titles measured from the reference PDF attached to the issue — six entries,
/// in document order, **without** the `w:numPr` labels the headings render
/// with on the page. The title is the paragraph's text, not its emitted line.
#[test]
fn the_titles_match_the_reference_pdf() {
    let titles: Vec<String> = outline(&render_fixture())
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(
        titles,
        [
            "Some heading",
            "Some heading",
            "Some sub-heading",
            "Some heading",
            "Some heading",
            "Some sub-heading",
        ],
    );
}

/// …and the two `Heading2` paragraphs nest under the `Heading1` above each,
/// while the `Heading1`s stay at the top level.
#[test]
fn heading_levels_become_outline_nesting() {
    let depths: Vec<usize> = outline(&render_fixture())
        .unwrap()
        .into_iter()
        .map(|e| e.depth)
        .collect();

    assert_eq!(depths, [0, 0, 1, 0, 0, 1]);
}

// ── Destinations ────────────────────────────────────────────────────────────

/// Every entry points at a real page. A `/Dest` that resolves to page 0 means
/// the destination array was malformed or the page reference did not match any
/// page object — either way the entry does not navigate.
#[test]
fn every_entry_names_a_page() {
    for entry in outline(&render_fixture()).unwrap() {
        assert!(entry.page >= 1, "{entry:?} has no resolvable destination");
    }
}

/// A heading on the second page points at the second page. Without this, an
/// outline whose entries all point at page 1 would pass every test above.
#[test]
fn a_heading_on_a_later_page_points_at_that_page() {
    let filler: String = (0..60)
        .map(|i| para("Normal", &format!("line {i}")))
        .collect();
    let body = format!("{}{filler}{}", para("H1", "First"), para("H1", "Second"));
    let entries = outline(&render_body(&body)).unwrap();

    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0].page, 1);
    assert!(
        entries[1].page > 1,
        "the second heading is pushed onto a later page: {entries:?}",
    );
}

// ── What is and is not a heading ────────────────────────────────────────────

/// §17.3.1.19: a document with no outline levels gets **no** `/Outlines` key —
/// not an empty outline dictionary, which some viewers render as an empty
/// sidebar rather than hiding it.
#[test]
fn a_document_without_headings_has_no_outline_at_all() {
    let body = format!("{}{}", para("Normal", "one"), para("Normal", "two"));
    assert_eq!(outline(&render_body(&body)), None);
}

/// §17.3.1.19 value 9 is "body text" — an explicit *absence* of outline level,
/// not a ninth heading level. `OutlineLevel::from_ooxml` already rejects it;
/// this pins that the rejection reaches the outline.
#[test]
fn outline_level_nine_is_body_text_and_not_a_heading() {
    let body = format!(
        r#"<w:p><w:pPr><w:outlineLvl w:val="9"/></w:pPr>
             <w:r><w:t>not a heading</w:t></w:r></w:p>{}"#,
        para("H1", "a heading")
    );
    let titles: Vec<String> = outline(&render_body(&body))
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["a heading"]);
}

/// The level is a *paragraph* property (§17.3.1.19), not a style name. A direct
/// `w:outlineLvl` with no heading style is a heading; a style called "Heading"
/// with no level is not.
#[test]
fn a_direct_outline_level_is_a_heading_without_any_heading_style() {
    let body = r#"<w:p><w:pPr><w:outlineLvl w:val="0"/></w:pPr>
                    <w:r><w:t>direct</w:t></w:r></w:p>"#;
    let titles: Vec<String> = outline(&render_body(body))
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["direct"]);
}

/// A heading paragraph with no text has no usable title, and an untitled
/// outline row navigates nowhere a user can name. Skipped — decided, and
/// recorded rather than inherited.
#[test]
fn an_empty_heading_produces_no_entry() {
    let body = format!(
        r#"{}<w:p><w:pPr><w:pStyle w:val="H1"/></w:pPr></w:p>{}"#,
        para("H1", "before"),
        para("H1", "after")
    );
    let titles: Vec<String> = outline(&render_body(&body))
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["before", "after"]);
}

/// A heading's text is its runs joined — a title must not be truncated at the
/// first run boundary, which is where formatting changes fall.
#[test]
fn a_title_joins_every_run_of_its_paragraph() {
    let body = r#"<w:p><w:pPr><w:pStyle w:val="H1"/></w:pPr>
                    <w:r><w:t xml:space="preserve">Split </w:t></w:r>
                    <w:r><w:rPr><w:b/></w:rPr><w:t>across</w:t></w:r>
                    <w:r><w:t xml:space="preserve"> runs</w:t></w:r></w:p>"#;
    let titles: Vec<String> = outline(&render_body(body))
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["Split across runs"]);
}

// ── Levels beyond what PDF can express ──────────────────────────────────────

/// PDF 1.7's heading structure types stop at `H6`, and Skia enforces it, but
/// §17.3.1.19 allows nine levels. Levels 7–9 clamp to `H6`: the entries survive
/// — losing a heading entirely would be worse — and the hierarchy flattens
/// there. The degrade is asserted, not left to chance.
#[test]
fn outline_levels_past_six_clamp_rather_than_vanish() {
    let body = format!(
        "{}{}",
        para("H1", "top"),
        para("Deep", "deep") // styles(deep_level) below puts this at w:val="8" → level 9
    );
    let rendered = render(&make_docx(&[
        ("word/document.xml", &document(&body)),
        ("word/styles.xml", &styles(8)),
    ]));
    let titles: Vec<String> = outline(&rendered)
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert!(
        titles.contains(&"deep".to_string()),
        "a level-9 heading still reaches the outline: {titles:?}",
    );
}

// ── Where headings must not be collected ────────────────────────────────────

/// A heading-styled paragraph inside a header is page furniture repeated on
/// every page, not a position in the document. It must not enter the outline —
/// and on a multi-page document it would otherwise enter it once per page.
#[test]
fn a_heading_in_a_header_is_not_part_of_the_outline() {
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr {W}>{}</w:hdr>"#,
        para("H1", "header heading")
    );
    let body = format!(
        r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId9"
             xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/>
           </w:sectPr></w:pPr></w:p>{}"#,
        para("H1", "body heading")
    );
    let rendered = render(&make_docx(&[
        ("word/document.xml", &document(&body)),
        ("word/styles.xml", &styles(6)),
        ("word/header1.xml", &header),
    ]));

    let titles: Vec<String> = outline(&rendered)
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["body heading"]);
}

// ── A heading that splits across a page boundary ────────────────────────────

/// Lay out `body` and report, per page, how many commands draw `needle`.
fn pages_drawing(body: &str, needle: &str) -> Vec<usize> {
    let doc = dxpdf::docx::parse(&make_docx(&[
        ("word/document.xml", &document(body)),
        ("word/styles.xml", &styles(6)),
    ]))
    .expect("fixture parses");
    dxpdf::render::resolve_and_layout(doc)
        .1
        .iter()
        .map(|page| {
            page.commands
                .iter()
                .filter(|c| {
                    matches!(c, dxpdf::render::layout::draw_command::DrawCommand::Text { text, .. }
                        if text.contains(needle))
                })
                .count()
        })
        .collect()
}

/// Filler plus a heading long enough to wrap past the page break.
fn split_heading_body() -> String {
    let filler: String = (0..40)
        .map(|i| para("Normal", &format!("filler {i}")))
        .collect();
    let long = std::iter::repeat_n("wrapping", 400)
        .collect::<Vec<_>>()
        .join(" ");
    format!("{filler}{}", para("H1", &long))
}

/// §17.3.1.14: `emit_line_commands` runs once per page-slice of a paragraph, so
/// a heading spanning a page break passes through it twice. It must still
/// produce **one** outline entry, on the page it starts.
#[test]
fn a_heading_split_across_pages_produces_one_entry() {
    let body = split_heading_body();

    let drawn = pages_drawing(&body, "wrapping");
    assert!(
        drawn.iter().filter(|&&n| n > 0).count() >= 2,
        "precondition: the heading must actually split across pages: {drawn:?}",
    );

    let entries = outline(&render(&make_docx(&[
        ("word/document.xml", &document(&body)),
        ("word/styles.xml", &styles(6)),
    ])))
    .expect("the heading produces an outline");

    assert_eq!(entries.len(), 1, "one heading, one entry: {entries:?}");
    let first_page_with_text = drawn.iter().position(|&n| n > 0).unwrap() + 1;
    assert_eq!(
        entries[0].page, first_page_with_text as u32,
        "the entry lands on the page the heading starts: {entries:?}",
    );
}

/// The marker pair is a marked-content bracket, and an unbalanced one leaves a
/// node ID set across whatever follows — dragging that heading's destination
/// onto unrelated content. Nothing in the type system enforces the pairing, so
/// it is asserted directly, across a document with every shape that emits one.
#[test]
fn every_outline_marker_is_balanced() {
    use dxpdf::render::layout::draw_command::{DrawCommand, OutlineMark};

    let body = format!(
        "{}{}{}{}",
        para("H1", "one"),
        para("Normal", "body"),
        para("H2", "two"),
        split_heading_body(),
    );
    let doc = dxpdf::docx::parse(&make_docx(&[
        ("word/document.xml", &document(&body)),
        ("word/styles.xml", &styles(6)),
    ]))
    .expect("fixture parses");
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);

    let mut open = 0i32;
    let mut begins = 0usize;
    for (page_idx, page) in pages.iter().enumerate() {
        for cmd in &page.commands {
            match cmd {
                DrawCommand::Outline(OutlineMark::Begin(_)) => {
                    open += 1;
                    begins += 1;
                    assert_eq!(open, 1, "page {page_idx}: a heading opened inside another");
                }
                DrawCommand::Outline(OutlineMark::End) => {
                    open -= 1;
                    assert_eq!(open, 0, "page {page_idx}: an End with no open heading");
                }
                _ => {}
            }
        }
    }
    assert_eq!(open, 0, "every Begin is closed");
    assert_eq!(begins, 3, "three headings, three markers");
}

/// Node IDs are what bind an outline entry to its destination, so a repeat
/// silently retargets one heading's entry at another's content.
#[test]
fn every_heading_gets_a_distinct_node_id() {
    use dxpdf::render::layout::draw_command::{DrawCommand, OutlineMark};

    let doc = dxpdf::docx::parse(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test-files/document_outline.docx"
        ))
        .unwrap(),
    )
    .unwrap();
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);

    let ids: Vec<i32> = pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Outline(OutlineMark::Begin(h)) => Some(h.node_id),
            _ => None,
        })
        .collect();

    assert_eq!(ids.len(), 6, "the fixture's six headings: {ids:?}");
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "IDs repeat: {ids:?}");
    assert!(ids.iter().all(|&id| id > 0), "0 means 'no node': {ids:?}");
}

/// A heading inside a shape text box is not a position in the document's main
/// story, and Word's navigation pane leaves it out too.
///
/// The stakes are higher than one missing row: a shape text body is laid out by
/// a **fresh** `BuildState`, so a collector left enabled there would restart the
/// node-ID counter at 1 and hand out IDs already owned by body headings —
/// silently retargeting their outline entries at the shape. See
/// `build::OutlineCollector`.
#[test]
fn a_heading_in_a_shape_text_box_is_not_part_of_the_outline() {
    let body = format!(
        r#"<w:p><w:r><w:drawing
             xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
             xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
             xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
      <wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="1"
                 behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">
        <wp:simplePos x="0" y="0"/>
        <wp:positionH relativeFrom="margin"><wp:posOffset>0</wp:posOffset></wp:positionH>
        <wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>
        <wp:extent cx="2540000" cy="1270000"/><wp:wrapNone/>
        <wp:docPr id="1" name="Box"/>
        <a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
          <wps:wsp><wps:cNvSpPr/>
            <wps:spPr>
              <a:xfrm><a:off x="0" y="0"/><a:ext cx="2540000" cy="1270000"/></a:xfrm>
              <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
            </wps:spPr>
            <wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx>
            <wps:bodyPr/>
          </wps:wsp>
        </a:graphicData></a:graphic>
      </wp:anchor></w:drawing></w:r></w:p>{}"#,
        para("H1", "shape heading"),
        para("H1", "body heading")
    );

    let titles: Vec<String> = outline(&render_body(&body))
        .expect("the body heading still produces an outline")
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["body heading"]);
}

/// The same rule, reached by a different route. `build_header_footer_content`
/// handles a `Block::Table` by calling the **body** table builder, so a heading
/// inside a table in a header reaches the body paragraph builder — and, before
/// the collector was suspended for the whole header, entered the outline once
/// per page the header was drawn on.
#[test]
fn a_heading_in_a_header_table_is_not_part_of_the_outline() {
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr {W}><w:tbl>
  <w:tblGrid><w:gridCol w:w="9000"/></w:tblGrid>
  <w:tr><w:tc><w:tcPr><w:tcW w:w="9000" w:type="dxa"/></w:tcPr>
    {}
  </w:tc></w:tr>
</w:tbl></w:hdr>"#,
        para("H1", "header table heading")
    );
    // Two pages, so a per-page leak shows up as two entries rather than one.
    let filler: String = (0..70)
        .map(|i| para("Normal", &format!("filler {i}")))
        .collect();
    let body = format!(
        r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId9"
             xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/>
           </w:sectPr></w:pPr></w:p>{filler}{}"#,
        para("H1", "body heading")
    );
    let rendered = render(&make_docx(&[
        ("word/document.xml", &document(&body)),
        ("word/styles.xml", &styles(6)),
        ("word/header1.xml", &header),
    ]));

    let titles: Vec<String> = outline(&rendered)
        .expect("the body heading still produces an outline")
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["body heading"]);
}

/// A title is the paragraph's rendered text, so a heading whose words sit
/// inside a hyperlink or a field result keeps them.
#[test]
fn a_title_includes_hyperlink_and_field_text() {
    let body = r#"<w:p><w:pPr><w:pStyle w:val="H1"/></w:pPr>
      <w:r><w:t xml:space="preserve">Plain </w:t></w:r>
      <w:hyperlink w:anchor="bm"><w:r><w:t xml:space="preserve">linked </w:t></w:r></w:hyperlink>
      <w:fldSimple w:instr=" AUTHOR "><w:r><w:t>field</w:t></w:r></w:fldSimple>
    </w:p>"#;
    let titles: Vec<String> = outline(&render_body(body))
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["Plain linked field"]);
}

/// A document with no headings gets no PDF structure additions at all — not
/// just no `/Outlines`. Emitting an empty structure tree would add
/// `/StructTreeRoot` and `/MarkInfo` to the catalog of every document in the
/// corpus, changing their bytes to describe a structure they do not have.
#[test]
fn a_document_without_headings_gains_no_structure_tree() {
    let body = format!("{}{}", para("Normal", "one"), para("Normal", "two"));
    let bytes = render_body(&body);
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let keys: Vec<String> = doc
        .catalog()
        .unwrap()
        .iter()
        .map(|(k, _)| String::from_utf8_lossy(k).to_string())
        .collect();

    assert!(!keys.contains(&"Outlines".to_string()), "{keys:?}");
    assert!(!keys.contains(&"StructTreeRoot".to_string()), "{keys:?}");
    assert!(!keys.contains(&"MarkInfo".to_string()), "{keys:?}");
}

/// An empty heading emits no marker at all — asserted at the layout level
/// because Skia *also* drops a node with an empty title, so a PDF-level
/// assertion passes whether or not this renderer made the decision.
#[test]
fn an_empty_heading_emits_no_marker() {
    use dxpdf::render::layout::draw_command::{DrawCommand, OutlineMark};

    let body = format!(
        r#"{}<w:p><w:pPr><w:pStyle w:val="H1"/></w:pPr></w:p>"#,
        para("H1", "real")
    );
    let doc = dxpdf::docx::parse(&make_docx(&[
        ("word/document.xml", &document(&body)),
        ("word/styles.xml", &styles(6)),
    ]))
    .unwrap();
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);

    let titles: Vec<String> = pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Outline(OutlineMark::Begin(h)) => Some(h.title.to_string()),
            _ => None,
        })
        .collect();

    assert_eq!(titles, ["real"]);
}

/// A footnote body is not the document's main story either, and it reaches the
/// ordinary paragraph builder through `build_note_content`. Same rule, third
/// route — pinned because the suspension there is a separate call from the one
/// covering headers, footers and shape text.
#[test]
fn a_heading_in_a_footnote_is_not_part_of_the_outline() {
    let footnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:footnotes {W}>
  <w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>
  <w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>
  <w:footnote w:id="1">{}</w:footnote>
</w:footnotes>"#,
        para("H1", "footnote heading")
    );
    let body = format!(
        r#"<w:p><w:r><w:t>body text</w:t></w:r>
             <w:r><w:footnoteReference w:id="1"/></w:r></w:p>{}"#,
        para("H1", "body heading")
    );
    let rendered = render(&make_docx(&[
        ("word/document.xml", &document(&body)),
        ("word/styles.xml", &styles(6)),
        ("word/footnotes.xml", &footnotes),
    ]));

    let titles: Vec<String> = outline(&rendered)
        .expect("the body heading still produces an outline")
        .into_iter()
        .map(|e| e.title)
        .collect();

    assert_eq!(titles, ["body heading"]);
}

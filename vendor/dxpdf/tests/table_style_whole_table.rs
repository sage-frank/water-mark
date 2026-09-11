//! §17.7.6 `<w:tblStylePr w:type="wholeTable">` — the table style's base
//! conditional layer.
//!
//! No document in `test-files/` uses `wholeTable`, so the fixture is built here
//! rather than committed as a binary: the XML is the point of the test, and a
//! `.docx` would hide it.

use std::io::Write;

use dxpdf::render::layout::draw_command::DrawCommand;

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

/// A 2×2 table using style `TestTbl`, one word per cell.
fn table_document() -> String {
    let cell = |t: &str| {
        format!(
            r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr>
                 <w:p><w:r><w:t>{t}</w:t></w:r></w:p></w:tc>"#
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tblPr><w:tblStyle w:val="TestTbl"/><w:tblW w:w="4000" w:type="dxa"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>
      <w:tr>{}{}</w:tr>
      <w:tr>{}{}</w:tr>
    </w:tbl>
    <w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
  </w:body>
</w:document>"#,
        cell("A"),
        cell("B"),
        cell("C"),
        cell("D"),
    )
}

fn styles_with(table_style_body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="table" w:styleId="TestTbl">
    <w:name w:val="Test Table"/>
    {table_style_body}
  </w:style>
</w:styles>"#
    )
}

fn layout(
    document_xml: &str,
    styles_xml: &str,
) -> Vec<dxpdf::render::layout::draw_command::LayoutedPage> {
    let doc = dxpdf::docx::parse(&make_docx(document_xml, styles_xml)).expect("parse");
    dxpdf::render::resolve_and_layout(doc).1
}

/// How many of the table's cells carry a `color` background.
///
/// Counted by asking, for each cell's own text, whether its baseline lies
/// inside a rect of that colour — not by counting the rects. §17.4.33 shading
/// is a cell property and reaches the page as one rect per cell, but only until
/// `coalesce_abutting_rects` fuses abutting same-colour neighbours into one
/// (`tests/table_shading_seams.rs` says why it must), after which a rect count
/// answers how many *runs* were painted rather than how many cells. Every cell
/// of the fixture holds a distinct label, so this counts cells however the
/// fills were emitted — and asserts what a rect count never did: that the
/// shading actually covers the cell whose text it is behind.
fn cells_shaded(
    pages: &[dxpdf::render::layout::draw_command::LayoutedPage],
    color: (u8, u8, u8),
) -> usize {
    let rects: Vec<_> = pages
        .iter()
        .flat_map(|p| &p.commands)
        .filter_map(|c| match c {
            DrawCommand::Rect { rect, color: c } if (c.r, c.g, c.b) == color => Some((
                rect.origin.x.raw(),
                rect.origin.y.raw(),
                rect.origin.x.raw() + rect.size.width.raw(),
                rect.origin.y.raw() + rect.size.height.raw(),
            )),
            _ => None,
        })
        .collect();
    pages
        .iter()
        .flat_map(|p| &p.commands)
        .filter(|c| {
            let DrawCommand::Text { text, position, .. } = c else {
                return false;
            };
            if text.trim().is_empty() {
                return false;
            }
            let (x, y) = (position.x.raw(), position.y.raw());
            rects
                .iter()
                .any(|&(x0, y0, x1, y1)| x >= x0 && x <= x1 && y >= y0 && y <= y1)
        })
        .count()
}

/// The base layer reaches every cell. Before this, `wholeTable` was parsed and
/// modelled but had zero read sites under `src/render/`, so a table style whose
/// only formatting was `wholeTable` rendered completely unstyled.
#[test]
fn whole_table_shading_reaches_every_cell() {
    let pages = layout(
        &table_document(),
        &styles_with(
            r#"<w:tblStylePr w:type="wholeTable">
                 <w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="FF0000"/></w:tcPr>
               </w:tblStylePr>"#,
        ),
    );
    assert_eq!(
        cells_shaded(&pages, (0xFF, 0x00, 0x00)),
        4,
        "all four cells must carry the wholeTable shading"
    );
}

/// …and a positional override still wins where it applies, leaving the base
/// showing everywhere else.
#[test]
fn first_row_overrides_the_base_layer_but_only_in_the_first_row() {
    let pages = layout(
        &table_document(),
        &styles_with(
            r#"<w:tblStylePr w:type="wholeTable">
                 <w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="FF0000"/></w:tcPr>
               </w:tblStylePr>
               <w:tblStylePr w:type="firstRow">
                 <w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="00FF00"/></w:tcPr>
               </w:tblStylePr>"#,
        ),
    );
    assert_eq!(
        cells_shaded(&pages, (0x00, 0xFF, 0x00)),
        2,
        "first row takes the positional override"
    );
    assert_eq!(
        cells_shaded(&pages, (0xFF, 0x00, 0x00)),
        2,
        "the second row still shows the base layer"
    );
}

/// The table-level half: a `wholeTable` override's `tblPr` has to reach
/// `build_table`, which reads borders from `ResolvedStyle::table` rather than
/// from the conditional cascade.
#[test]
fn whole_table_borders_reach_the_table_level_cascade() {
    let with_borders = layout(
        &table_document(),
        &styles_with(
            r#"<w:tblStylePr w:type="wholeTable">
                 <w:tblPr><w:tblBorders>
                   <w:top w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:bottom w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:left w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:right w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:insideH w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:insideV w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                 </w:tblBorders></w:tblPr>
               </w:tblStylePr>"#,
        ),
    );
    // Borders are emitted as thin `Rect`s, not `Line`s. This table sets no
    // shading, so every blue rect is a border segment.
    let blue = |pages: &[dxpdf::render::layout::draw_command::LayoutedPage]| {
        pages
            .iter()
            .flat_map(|p| &p.commands)
            .filter(|c| {
                matches!(c, DrawCommand::Rect { color, .. }
                    if (color.r, color.g, color.b) == (0x00, 0x00, 0xFF))
            })
            .count()
    };
    assert!(
        blue(&with_borders) > 0,
        "wholeTable tblBorders must reach the table-level border cascade"
    );

    // Control: the same table with no wholeTable override draws no blue.
    let without = layout(&table_document(), &styles_with(""));
    assert_eq!(
        blue(&without),
        0,
        "the blue borders must come from wholeTable"
    );
}

/// The fold is an *overlay*, not a replacement: a table style may set its own
/// `tblPr` and a `wholeTable` `tblPr`, and what `wholeTable` does not mention
/// has to fall through rather than vanish.
///
/// Here the style's own `tblPr` carries the borders and `wholeTable` carries
/// only cell margins. Building the fold in the inheritance direction instead
/// would keep the margins and drop the borders entirely.
#[test]
fn whole_table_overlays_the_styles_own_table_properties() {
    let pages = layout(
        &table_document(),
        &styles_with(
            r#"<w:tblPr><w:tblBorders>
                 <w:top w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                 <w:bottom w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                 <w:left w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                 <w:right w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                 <w:insideH w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                 <w:insideV w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
               </w:tblBorders></w:tblPr>
               <w:tblStylePr w:type="wholeTable">
                 <w:tblPr><w:tblCellMar>
                   <w:top w:w="120" w:type="dxa"/>
                   <w:bottom w:w="120" w:type="dxa"/>
                 </w:tblCellMar></w:tblPr>
               </w:tblStylePr>"#,
        ),
    );
    let blue = pages
        .iter()
        .flat_map(|p| &p.commands)
        .filter(|c| {
            matches!(c, DrawCommand::Rect { color, .. }
                if (color.r, color.g, color.b) == (0x00, 0x00, 0xFF))
        })
        .count();
    assert!(
        blue > 0,
        "the style's own borders must survive a wholeTable override that does \
         not mention them"
    );
}

/// And where both specify the same property, `wholeTable` wins — it is
/// conditional formatting layered on top of the style definition (§17.7.6).
#[test]
fn whole_table_beats_the_styles_own_value_for_the_same_property() {
    let pages = layout(
        &table_document(),
        &styles_with(
            r#"<w:tblPr><w:tblBorders>
                 <w:top w:val="single" w:sz="24" w:space="0" w:color="FF0000"/>
                 <w:bottom w:val="single" w:sz="24" w:space="0" w:color="FF0000"/>
                 <w:left w:val="single" w:sz="24" w:space="0" w:color="FF0000"/>
                 <w:right w:val="single" w:sz="24" w:space="0" w:color="FF0000"/>
                 <w:insideH w:val="single" w:sz="24" w:space="0" w:color="FF0000"/>
                 <w:insideV w:val="single" w:sz="24" w:space="0" w:color="FF0000"/>
               </w:tblBorders></w:tblPr>
               <w:tblStylePr w:type="wholeTable">
                 <w:tblPr><w:tblBorders>
                   <w:top w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:bottom w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:left w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:right w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:insideH w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                   <w:insideV w:val="single" w:sz="24" w:space="0" w:color="0000FF"/>
                 </w:tblBorders></w:tblPr>
               </w:tblStylePr>"#,
        ),
    );
    let count = |rgb: (u8, u8, u8)| {
        pages
            .iter()
            .flat_map(|p| &p.commands)
            .filter(|c| {
                matches!(c, DrawCommand::Rect { color, .. }
                if (color.r, color.g, color.b) == rgb)
            })
            .count()
    };
    assert!(
        count((0x00, 0x00, 0xFF)) > 0,
        "wholeTable's borders must win"
    );
    assert_eq!(
        count((0xFF, 0x00, 0x00)),
        0,
        "the style's own borders must be fully replaced, not blended"
    );
}

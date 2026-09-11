//! §17.18.85 `ST_TabJc` — `decimal` and `bar` tab stops, end to end.
//!
//! No document in `test-files/` uses `<w:tab w:val="decimal"/>` or
//! `w:val="bar"`, so the whole corpus renders byte-identically whether either
//! works or not. The fixtures are built here rather than committed as binaries
//! for the same reason the `wholeTable` fixture is: the XML *is* the point of
//! the test, and a `.docx` would hide it.

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

/// One paragraph per entry, each `<tab>` + the entry, against a single
/// decimal stop at 4320 twips (216pt = 3in).
fn decimal_tab_document(entries: &[&str]) -> String {
    let paragraphs: String = entries
        .iter()
        .map(|entry| {
            format!(
                r#"<w:p>
  <w:pPr>
    <w:tabs><w:tab w:val="decimal" w:pos="4320"/></w:tabs>
  </w:pPr>
  <w:r><w:tab/><w:t>{entry}</w:t></w:r>
</w:p>"#
            )
        })
        .collect();

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>{paragraphs}</w:body>
</w:document>"#
    )
}

/// The x of every emitted text command carrying `needle`, in order.
fn xs_of_texts(pages: &[LayoutedPage]) -> Vec<(String, f32)> {
    pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Text { text, position, .. } => Some((text.to_string(), position.x.raw())),
            _ => None,
        })
        .collect()
}

#[test]
fn decimal_stop_aligns_separators_across_paragraphs() {
    // §17.18.85: whatever the integer part's width, the separator lands on
    // the stop — so a column of figures aligns on its decimal points.
    let bytes = make_docx(&decimal_tab_document(&["1.5", "22.75", "333.125"]));
    let doc = dxpdf::docx::parse(&bytes).expect("fixture parses");
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);

    let texts = xs_of_texts(&pages);
    assert_eq!(texts.len(), 3, "one text command per entry: {texts:?}");

    // Each entry's separator x = start x + width of the part before it. The
    // three must coincide; the starts must not.
    let starts: Vec<f32> = texts.iter().map(|(_, x)| *x).collect();
    assert!(
        starts[0] > starts[1] && starts[1] > starts[2],
        "wider integer parts start further left: {starts:?}"
    );

    // A left-aligned stop would put all three at the same x — the exact bug
    // this guards. Assert they differ by the integer-part widths.
    assert!(
        (starts[0] - starts[2]).abs() > 1.0,
        "1.5 and 333.125 must not start at the same x: {starts:?}"
    );
}

#[test]
fn decimal_stop_right_aligns_an_entry_with_no_separator() {
    // Word right-aligns a separator-less decimal zone rather than
    // left-aligning it, so a whole number stays flush with the column.
    let bytes = make_docx(&decimal_tab_document(&["1234", "1.5"]));
    let doc = dxpdf::docx::parse(&bytes).expect("fixture parses");
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);

    let texts = xs_of_texts(&pages);
    assert_eq!(texts.len(), 2, "{texts:?}");

    // Coordinate-free: "1234" *ends* at the stop, "1.5" puts its separator
    // there, so the whole number starts further left by the width of its extra
    // digits. Under a left-aligned fallback both would start at the same x —
    // which is exactly the defect this guards.
    let (whole, whole_x) = &texts[0];
    let (fraction, fraction_x) = &texts[1];
    assert_eq!((whole.as_str(), fraction.as_str()), ("1234", "1.5"));
    assert!(
        whole_x < fraction_x,
        "a separator-less zone must right-align, so {whole:?} at {whole_x} \
         starts left of {fraction:?} at {fraction_x}; equal x means it was \
         left-aligned instead"
    );
}

// ── §17.3.1.30 position tabs ─────────────────────────────────────────────────

/// `lead`, a centre margin ptab, `mid`, a right margin ptab, then `trail`.
///
/// Two tabs are required to reproduce the defect: a single right ptab can only
/// have its anchor behind the pen when the content already exceeds the line, in
/// which case the fitter breaks for width first. It is the *centre* tab jumping
/// the pen forward — past where the right tab wants to start — that creates the
/// case the clamp mishandled.
fn two_ptab_document(lead: &str, mid: &str, trail: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t xml:space="preserve">{lead}</w:t></w:r>
      <w:r><w:ptab w:relativeTo="margin" w:alignment="center" w:leader="none"/></w:r>
      <w:r><w:t xml:space="preserve">{mid}</w:t></w:r>
      <w:r><w:ptab w:relativeTo="margin" w:alignment="right" w:leader="none"/></w:r>
      <w:r><w:t xml:space="preserve">{trail}</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#
    )
}

#[test]
fn a_ptab_anchor_behind_the_pen_advances_to_the_next_line() {
    // §17.3.1.30: the centre tab leaves the pen right of where the right tab
    // anchors its zone, so the right tab must advance to the next line rather
    // than clamping — which drew the trailing run past the right margin.
    let bytes = make_docx(&two_ptab_document("L", &"m".repeat(34), &"m".repeat(19)));
    let doc = dxpdf::docx::parse(&bytes).expect("fixture parses");
    let (_, pages) = dxpdf::render::resolve_and_layout(doc);

    let placed: Vec<(String, f32, f32)> = pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Text { text, position, .. } => {
                Some((text.to_string(), position.x.raw(), position.y.raw()))
            }
            _ => None,
        })
        .collect();

    let mid = placed
        .iter()
        .find(|(t, _, _)| t.starts_with('m'))
        .expect("middle run emitted");
    let trail = placed
        .iter()
        .rev()
        .find(|(t, _, _)| t.starts_with('m'))
        .expect("trailing run emitted — not dropped");

    assert!(
        trail.2 > mid.2,
        "the trailing run advances to the next line: {placed:?}"
    );
    // Letter, 1in margins → the text area ends at 540pt. The clamp put the
    // trailing run at the pen, running past that edge.
    assert!(
        trail.1 < 540.0,
        "the trailing run starts inside the text area, at {}",
        trail.1
    );
}

// ── §17.18.85 `bar` ──────────────────────────────────────────────────────────
//
// A `bar` entry in `w:tabs` is **not a tab stop**. It names a place where a
// vertical rule is drawn on every line of the paragraph, and a tab character
// passes straight over it to the next real stop. That is two behaviours, and
// this renderer had neither: `bar` shared an arm with `left`, so a tab landed
// on it and no rule was ever drawn.
//
// Both halves are pinned below, because fixing only the drawing half leaves a
// paragraph whose text is positioned by a stop Word does not have.

/// A paragraph with `tabs` (raw `<w:tab .../>` XML) holding `body`.
fn tabbed_paragraph(tabs: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:tabs>{tabs}</w:tabs></w:pPr>
      {body}
    </w:p>
  </w:body>
</w:document>"#
    )
}

fn layout(document_xml: &str) -> Vec<LayoutedPage> {
    let doc = dxpdf::docx::parse(&make_docx(document_xml)).expect("fixture parses");
    dxpdf::render::resolve_and_layout(doc).1
}

/// Every vertical line drawn, as `(x, top, bottom, colour)` in draw order.
///
/// Filtered to *vertical* segments so an underline or a run border edge can
/// never be mistaken for a bar rule.
fn vertical_rules(
    pages: &[LayoutedPage],
) -> Vec<(f32, f32, f32, dxpdf::render::resolve::color::RgbColor)> {
    pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Line { line, color, .. }
                if (line.start.x.raw() - line.end.x.raw()).abs() < 0.001 =>
            {
                Some((
                    line.start.x.raw(),
                    line.start.y.raw().min(line.end.y.raw()),
                    line.start.y.raw().max(line.end.y.raw()),
                    *color,
                ))
            }
            _ => None,
        })
        .collect()
}

/// 4320 twips = 216pt = 3in, the position both fixtures below use.
const THREE_INCHES: &str = "4320";

#[test]
fn a_bar_stop_draws_a_vertical_rule_at_its_position() {
    let pages = layout(&tabbed_paragraph(
        &format!(r#"<w:tab w:val="bar" w:pos="{THREE_INCHES}"/>"#),
        r#"<w:r><w:t>text</w:t></w:r>"#,
    ));

    let rules = vertical_rules(&pages);
    assert_eq!(rules.len(), 1, "one line, one rule: {rules:?}");
    assert!(rules[0].2 > rules[0].1, "the rule has height: {rules:?}");
}

/// Coordinate-free position check: the rule lands exactly where a `left` stop
/// at the same `w:pos` puts the text that follows a tab. Nothing here depends
/// on the page margin or on twips-to-points arithmetic done twice.
#[test]
fn the_rule_sits_where_a_stop_at_the_same_position_would_put_content() {
    let rule_x = vertical_rules(&layout(&tabbed_paragraph(
        &format!(r#"<w:tab w:val="bar" w:pos="{THREE_INCHES}"/>"#),
        r#"<w:r><w:t>text</w:t></w:r>"#,
    )))[0]
        .0;

    let left_stop_x = xs_of_texts(&layout(&tabbed_paragraph(
        &format!(r#"<w:tab w:val="left" w:pos="{THREE_INCHES}"/>"#),
        r#"<w:r><w:tab/><w:t>text</w:t></w:r>"#,
    )))[0]
        .1;

    assert!(
        (rule_x - left_stop_x).abs() < 0.01,
        "rule at {rule_x}, a left stop at the same w:pos puts text at {left_stop_x}",
    );
}

/// The defining property, and the one that separates a bar from every other
/// alignment: the rule is a paragraph decoration, not something a tab draws.
/// This paragraph has no `<w:tab/>` at all.
#[test]
fn a_bar_stop_draws_its_rule_with_no_tab_character_in_the_paragraph() {
    let pages = layout(&tabbed_paragraph(
        &format!(r#"<w:tab w:val="bar" w:pos="{THREE_INCHES}"/>"#),
        r#"<w:r><w:t>no tab here</w:t></w:r>"#,
    ));

    assert_eq!(vertical_rules(&pages).len(), 1);
}

/// …and it draws on an empty paragraph, which still has a line.
#[test]
fn an_empty_paragraph_with_a_bar_stop_still_draws_its_rule() {
    let pages = layout(&tabbed_paragraph(
        &format!(r#"<w:tab w:val="bar" w:pos="{THREE_INCHES}"/>"#),
        "",
    ));

    assert_eq!(vertical_rules(&pages).len(), 1);
}

/// One rule per line, all at the same x, each spanning its own line's band and
/// abutting the next — which is what makes a multi-line paragraph read as one
/// continuous vertical rule rather than a dashed one.
#[test]
fn a_bar_stop_draws_on_every_line_and_the_rules_abut() {
    let pages = layout(&tabbed_paragraph(
        &format!(r#"<w:tab w:val="bar" w:pos="{THREE_INCHES}"/>"#),
        &format!(r#"<w:r><w:t>{}</w:t></w:r>"#, "wrapping ".repeat(60)),
    ));

    let rules = vertical_rules(&pages);
    assert!(
        rules.len() >= 3,
        "the fixture must wrap to several lines: {rules:?}",
    );
    for w in rules.windows(2) {
        assert!(
            (w[0].0 - w[1].0).abs() < 0.001,
            "every line's rule is at the same x: {rules:?}",
        );
        assert!(
            (w[0].2 - w[1].1).abs() < 0.01,
            "each rule ends where the next begins: {rules:?}",
        );
    }
}

/// §17.18.85: a bar is not a stop, so a tab character passes over it. With a
/// bar at 3in and a left stop at 5in, the text lands at 5in — under the old
/// shared `left` arm it stopped at 3in.
#[test]
fn a_tab_character_passes_over_a_bar_stop_to_the_next_real_one() {
    let with_bar = xs_of_texts(&layout(&tabbed_paragraph(
        r#"<w:tab w:val="bar" w:pos="4320"/><w:tab w:val="left" w:pos="7200"/>"#,
        r#"<w:r><w:tab/><w:t>text</w:t></w:r>"#,
    )))[0]
        .1;

    let without_bar = xs_of_texts(&layout(&tabbed_paragraph(
        r#"<w:tab w:val="left" w:pos="7200"/>"#,
        r#"<w:r><w:tab/><w:t>text</w:t></w:r>"#,
    )))[0]
        .1;

    assert!(
        (with_bar - without_bar).abs() < 0.01,
        "the bar must be invisible to the tab: with it {with_bar}, without it {without_bar}",
    );
}

/// …and with no other stop defined, the tab falls through to the document's
/// default interval (§17.15.1.25) rather than landing on the bar. 3in is an
/// exact multiple of the 0.5in default, so a bar-as-stop and a default stop
/// would coincide — hence a bar at 2.6in, which no interval can produce.
#[test]
fn a_lone_bar_stop_leaves_a_tab_to_the_default_interval() {
    let bar_only = xs_of_texts(&layout(&tabbed_paragraph(
        r#"<w:tab w:val="bar" w:pos="3744"/>"#,
        r#"<w:r><w:tab/><w:t>text</w:t></w:r>"#,
    )))[0]
        .1;

    let no_tabs_at_all = xs_of_texts(&layout(&tabbed_paragraph(
        "",
        r#"<w:r><w:tab/><w:t>text</w:t></w:r>"#,
    )))[0]
        .1;

    assert!(
        (bar_only - no_tabs_at_all).abs() < 0.01,
        "a lone bar leaves tabbing exactly as it was: {bar_only} vs {no_tabs_at_all}",
    );
}

/// Adding a bar stop must not move a single glyph — it consumes no zone and
/// takes no part in line fitting.
///
/// The short prefix matters: it leaves the pen well left of 3in, so the bar is
/// the first entry past it and the one a naive `find_next_tab_stop` would
/// return. A fixture whose pen is already past the bar would pass this whether
/// or not the bar is skipped. The trailing run then wraps, so the pin covers
/// fitting as well as placement.
#[test]
fn adding_a_bar_stop_moves_no_content() {
    let body = format!(
        r#"<w:r><w:t>ab</w:t></w:r><w:r><w:tab/><w:t>{}</w:t></w:r>"#,
        "wrapping ".repeat(40)
    );

    let without = xs_of_texts(&layout(&tabbed_paragraph(
        r#"<w:tab w:val="left" w:pos="7200"/>"#,
        &body,
    )));
    let with = xs_of_texts(&layout(&tabbed_paragraph(
        r#"<w:tab w:val="bar" w:pos="4320"/><w:tab w:val="left" w:pos="7200"/>"#,
        &body,
    )));

    assert!(without.len() > 2, "the fixture must wrap: {without:?}");
    assert_eq!(without, with, "the bar rule must not disturb line fitting");
}

#[test]
fn two_bar_stops_draw_two_rules_per_line() {
    let pages = layout(&tabbed_paragraph(
        r#"<w:tab w:val="bar" w:pos="2880"/><w:tab w:val="bar" w:pos="4320"/>"#,
        r#"<w:r><w:t>text</w:t></w:r>"#,
    ));

    let rules = vertical_rules(&pages);
    assert_eq!(rules.len(), 2, "{rules:?}");
    assert!(rules[0].0 < rules[1].0, "distinct positions: {rules:?}");
}

/// A stop past everything on the line still draws — the rule's x owes nothing
/// to where the content happens to end.
#[test]
fn a_bar_stop_beyond_the_lines_content_still_draws() {
    let pages = layout(&tabbed_paragraph(
        r#"<w:tab w:val="bar" w:pos="6480"/>"#,
        r#"<w:r><w:t>hi</w:t></w:r>"#,
    ));

    let rules = vertical_rules(&pages);
    assert_eq!(rules.len(), 1, "{rules:?}");

    let text_end = xs_of_texts(&pages)[0].1;
    assert!(
        rules[0].0 > text_end + 100.0,
        "the rule is far right of the content: rule {:?}, text at {text_end}",
        rules[0],
    );
}

/// §17.3.1.38's rule for tab leaders — a decoration has no formatting of its
/// own, it takes the formatting in effect — applied here. A red paragraph gets
/// a red rule, not a black one.
#[test]
fn the_rule_takes_the_paragraphs_text_colour() {
    let pages = layout(&tabbed_paragraph(
        &format!(r#"<w:tab w:val="bar" w:pos="{THREE_INCHES}"/>"#),
        r#"<w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>red</w:t></w:r>"#,
    ));

    let rules = vertical_rules(&pages);
    assert_eq!(rules.len(), 1, "{rules:?}");
    assert_eq!(
        (rules[0].3.r, rules[0].3.g, rules[0].3.b),
        (0xFF, 0x00, 0x00),
        "the rule follows the run's colour: {rules:?}",
    );
}

/// A paragraph mixing both kinds draws a rule for the bar and nothing for the
/// rest — at the bar's own position, not at whichever entry came first.
///
/// "Does this paragraph have a bar?" and "which of its entries are bars?" are
/// two different questions, and a fixture holding only bars cannot tell them
/// apart: the first answer alone would draw a rule at every stop.
#[test]
fn only_the_bar_entry_draws_when_a_paragraph_mixes_stop_kinds() {
    let mixed = vertical_rules(&layout(&tabbed_paragraph(
        r#"<w:tab w:val="left" w:pos="2880"/><w:tab w:val="bar" w:pos="4320"/><w:tab w:val="right" w:pos="7200"/>"#,
        r#"<w:r><w:tab/><w:t>text</w:t></w:r>"#,
    )));
    assert_eq!(mixed.len(), 1, "one bar entry, one rule: {mixed:?}");

    let bar_only = vertical_rules(&layout(&tabbed_paragraph(
        &format!(r#"<w:tab w:val="bar" w:pos="{THREE_INCHES}"/>"#),
        r#"<w:r><w:t>text</w:t></w:r>"#,
    )));
    assert!(
        (mixed[0].0 - bar_only[0].0).abs() < 0.01,
        "the rule is at the bar's position: {mixed:?} vs {bar_only:?}",
    );
}

/// The control. No `bar` entry, no rule — so the tests above cannot be passing
/// on some line this renderer draws anyway.
#[test]
fn stops_that_are_not_bars_draw_no_rule() {
    for val in ["left", "center", "right", "decimal"] {
        let pages = layout(&tabbed_paragraph(
            &format!(r#"<w:tab w:val="{val}" w:pos="{THREE_INCHES}"/>"#),
            r#"<w:r><w:tab/><w:t>text</w:t></w:r>"#,
        ));
        assert!(
            vertical_rules(&pages).is_empty(),
            "{val} draws no vertical rule: {:?}",
            vertical_rules(&pages),
        );
    }
}

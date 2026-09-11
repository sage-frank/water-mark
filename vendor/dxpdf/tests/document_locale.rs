//! §17.3.2.20 `w:lang` — the document's language, and the two things layout
//! does with it.
//!
//! The tag is parsed and cascades like any other run property, but nothing
//! downstream read it, so two decisions were made in English regardless of what
//! the document said:
//!
//! * §17.18.85 — a `decimal` tab aligned on `.`, so a German column of figures
//!   found no separator and fell through to the right-aligning degrade;
//! * §17.9.27 — `ordinal` spelled English suffixes onto a German list, and
//!   `cardinalText` / `ordinalText` were not rendered as words at all.
//!
//! Neither symptom appears in `test-files/` or `test-cases/`: the corpus's one
//! decimal tab is in an `en-US` document and no numbering definition in it uses
//! a text format (counted, not assumed). The fixtures are therefore built here.
//!
//! Both halves have since grown a second round. Issue #128 made the separator
//! region-aware, so `de-CH` and `de-DE` now disagree correctly; issue #132
//! spelled German, French and Spanish number words, so the assertions that a
//! German list gets `1, 2, 3` became assertions that it gets `Eins, Zwei,
//! Drei`. That replacement, visible as a diff, is what those issues delivered.

use std::io::Write;

use dxpdf::render::layout::draw_command::{DrawCommand, LayoutedPage};

fn make_docx(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let overrides: String = parts
            .iter()
            .map(|(name, _)| {
                let content_type = match *name {
                    "word/document.xml" => "wordprocessingml.document.main",
                    "word/styles.xml" => "wordprocessingml.styles",
                    "word/numbering.xml" => "wordprocessingml.numbering",
                    other => panic!("no content type registered for {other}"),
                };
                format!(
                    r#"<Override PartName="/{name}" ContentType="application/vnd.openxmlformats-officedocument.{content_type}+xml"/>"#
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
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>
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

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

/// `styles.xml` whose `docDefaults` declare `lang` — the document-wide default
/// every other layer falls back to.
fn styles(default_lang: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles {W}>
  <w:docDefaults><w:rPrDefault><w:rPr>
    <w:lang w:val="{default_lang}"/>
  </w:rPr></w:rPrDefault></w:docDefaults>
</w:styles>"#
    )
}

fn layout(parts: &[(&str, &str)]) -> Vec<LayoutedPage> {
    let doc = dxpdf::docx::parse(&make_docx(parts)).expect("fixture parses");
    dxpdf::render::resolve_and_layout(doc).1
}

/// Every drawn string with its x, in draw order.
fn texts(pages: &[LayoutedPage]) -> Vec<(String, f32)> {
    pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Text { text, position, .. } => Some((text.to_string(), position.x.raw())),
            _ => None,
        })
        .collect()
}

// ── §17.18.85 `decimal` tabs ────────────────────────────────────────────────

/// Two entries against one decimal stop. `mark_lang`, if non-empty, is spliced
/// into `<w:pPr><w:rPr>` — the paragraph mark, which is what governs the stop.
fn decimal_tab_document(entries: &[&str], mark_lang: &str) -> String {
    let mark_rpr = if mark_lang.is_empty() {
        String::new()
    } else {
        format!(r#"<w:rPr><w:lang w:val="{mark_lang}"/></w:rPr>"#)
    };
    let paragraphs: String = entries
        .iter()
        .map(|entry| {
            format!(
                r#"<w:p>
  <w:pPr><w:tabs><w:tab w:val="decimal" w:pos="4320"/></w:tabs>{mark_rpr}</w:pPr>
  <w:r><w:tab/><w:t>{entry}</w:t></w:r>
</w:p>"#
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document {W}><w:body>{paragraphs}</w:body></w:document>"#
    )
}

/// Whether the separators of two entries were aligned on each other.
///
/// Coordinate-free, and the only property that matters: two numbers whose
/// integer parts differ in width start at *different* x exactly when the stop
/// found a separator in each. A stop that found none right-aligns both, which
/// puts the wider one further left by its own extra digits — so the sign of the
/// difference cannot distinguish them, but this can: with separators found,
/// `"1,5"` and `"333,125"` start 2 digits apart; right-aligned, 3.
fn separator_offsets(pages: &[LayoutedPage]) -> Vec<f32> {
    texts(pages).into_iter().map(|(_, x)| x).collect()
}

/// The baseline: an English document aligns on the point it always has.
#[test]
fn an_english_document_aligns_a_decimal_tab_on_the_point() {
    let xs = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "en-US"),
        ),
        ("word/styles.xml", &styles("en-US")),
    ]));

    // Both put their `.` on the stop, so the wider integer part starts further
    // left by exactly its two extra digits.
    assert!(xs[0] > xs[1], "aligned on the separator: {xs:?}");
}

/// §17.18.85 in a comma-decimal document. `1,5` and `333,125` must align on
/// their commas — which means starting at *different* x, by the width of the
/// wider integer part alone.
#[test]
fn a_german_document_aligns_a_decimal_tab_on_the_comma() {
    let german = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], "de-DE"),
        ),
        ("word/styles.xml", &styles("de-DE")),
    ]));
    let english_dots = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "en-US"),
        ),
        ("word/styles.xml", &styles("en-US")),
    ]));

    // The two documents are the same shape with the separator swapped, so the
    // German commas must land the same way the English points do.
    let german_gap = german[0] - german[1];
    let english_gap = english_dots[0] - english_dots[1];
    assert!(
        (german_gap - english_gap).abs() < 1.5,
        "a comma must anchor a German zone the way a point anchors an English \
         one: German gap {german_gap}, English gap {english_gap}",
    );
}

/// …and the same German document written with points has no separator the
/// locale recognises, so it takes §17.18.85's settled degrade: right-aligned,
/// keeping the column flush rather than anchoring on a thousands separator.
#[test]
fn a_german_document_right_aligns_a_zone_written_with_points() {
    let xs = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "de-DE"),
        ),
        ("word/styles.xml", &styles("de-DE")),
    ]));

    // Right-aligned: every zone *ends* at the stop, so the wider one starts
    // further left by its full extra width — three characters, not two.
    let aligned = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], "de-DE"),
        ),
        ("word/styles.xml", &styles("de-DE")),
    ]));
    assert!(
        (xs[0] - xs[1]) > (aligned[0] - aligned[1]) + 1.0,
        "a point in a German document anchors nothing: degraded {:?} vs \
         anchored {:?}",
        xs[0] - xs[1],
        aligned[0] - aligned[1],
    );
}

// ── issue #128: regional decimal-separator divergence ──────────────────────
//
// `Locale::from_tag` buckets on the primary subtag alone, so on its own it
// cannot tell `de-DE` from `de-CH` — both are just "German". These tabs are
// now resolved through `crate::i18n::decimal_separator_for_tag` first, which
// answers from real CLDR data instead, and this is the acceptance test #128
// exists for: two regions of the same language, disagreeing correctly.

/// Swiss German writes a **point**, unlike every other German-language
/// document in this file — the opposite of `a_german_document_...`'s comma.
#[test]
fn a_swiss_german_document_aligns_a_decimal_tab_on_the_point() {
    let swiss = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "de-CH"),
        ),
        ("word/styles.xml", &styles("de-CH")),
    ]));
    let english = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "en-US"),
        ),
        ("word/styles.xml", &styles("en-US")),
    ]));

    // Same shape, same literal separator character in the source XML, so a
    // de-CH document must anchor exactly as an en-US one does.
    assert_eq!(
        swiss, english,
        "de-CH anchors on the point exactly as English does: {swiss:?} vs {english:?}",
    );
}

/// …and the converse: a de-CH document written with a comma finds no
/// separator it recognises and right-aligns — the same degrade a de-DE
/// document written with a point takes above.
#[test]
fn a_swiss_german_document_right_aligns_a_zone_written_with_commas() {
    let xs = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], "de-CH"),
        ),
        ("word/styles.xml", &styles("de-CH")),
    ]));
    let anchored = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "de-CH"),
        ),
        ("word/styles.xml", &styles("de-CH")),
    ]));
    assert!(
        (xs[0] - xs[1]) > (anchored[0] - anchored[1]) + 1.0,
        "a comma in a Swiss German document anchors nothing: degraded {:?} vs \
         anchored {:?}",
        xs[0] - xs[1],
        anchored[0] - anchored[1],
    );
}

/// The direct claim: the *same* primary subtag, two regions, two different
/// separators — each anchoring correctly on its own region's character. A
/// primary-subtag-only bucket could not produce this; both `de-DE` and
/// `de-CH` here are written with the character that only *their own* region
/// recognises, and both anchor (same gap magnitude either way).
#[test]
fn de_de_and_de_ch_disagree_on_the_decimal_separator() {
    let de_de = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], "de-DE"),
        ),
        ("word/styles.xml", &styles("de-DE")),
    ]));
    let de_ch = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "de-CH"),
        ),
        ("word/styles.xml", &styles("de-CH")),
    ]));
    let de_de_gap = de_de[0] - de_de[1];
    let de_ch_gap = de_ch[0] - de_ch[1];
    assert!(
        (de_de_gap - de_ch_gap).abs() < 1.5,
        "de-DE's comma and de-CH's point must anchor the same way despite \
         being written with different characters: de-DE gap {de_de_gap}, \
         de-CH gap {de_ch_gap}",
    );
}

/// #128 names `en-ZA` explicitly as needing verification, not a guess: South
/// African English writes a **comma**, unlike `en-US`'s point.
#[test]
fn a_south_african_document_aligns_a_decimal_tab_on_the_comma() {
    let za = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], "en-ZA"),
        ),
        ("word/styles.xml", &styles("en-ZA")),
    ]));
    let german = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], "de-DE"),
        ),
        ("word/styles.xml", &styles("de-DE")),
    ]));

    assert_eq!(
        za, german,
        "en-ZA anchors on the comma exactly as de-DE does: {za:?} vs {german:?}",
    );
}

/// #128 names `es-MX` explicitly too: Mexican Spanish writes a **point**,
/// unlike the comma general/European Spanish (`Locale::from_tag`'s "es"
/// bucket) writes.
#[test]
fn a_mexican_spanish_document_aligns_a_decimal_tab_on_the_point() {
    let mx = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "es-MX"),
        ),
        ("word/styles.xml", &styles("es-MX")),
    ]));
    let english = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "en-US"),
        ),
        ("word/styles.xml", &styles("en-US")),
    ]));

    assert_eq!(
        mx, english,
        "es-MX anchors on the point exactly as English does: {mx:?} vs {english:?}",
    );
}

/// The document default reaches a paragraph that declares nothing of its own.
#[test]
fn the_document_default_language_governs_a_paragraph_that_declares_none() {
    let xs = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], ""),
        ),
        ("word/styles.xml", &styles("de-DE")),
    ]));
    let english = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], ""),
        ),
        ("word/styles.xml", &styles("en-US")),
    ]));

    assert!(
        (xs[0] - xs[1]) < (english[0] - english[1]) - 1.0,
        "docDefaults de-DE anchors the comma; en-US does not: {xs:?} vs {english:?}",
    );
}

/// …and the paragraph mark **overrides** it. §17.7.2 resolves `w:lang` like any
/// other run property, so the nearer layer wins — which is also the only test
/// here that can tell the two layers apart, every other fixture setting both to
/// the same tag.
#[test]
fn the_paragraph_mark_overrides_the_document_default_language() {
    let german_mark = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], "de-DE"),
        ),
        ("word/styles.xml", &styles("en-US")),
    ]));
    let default_only = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], ""),
        ),
        ("word/styles.xml", &styles("en-US")),
    ]));

    assert!(
        (german_mark[0] - german_mark[1]) < (default_only[0] - default_only[1]) - 1.0,
        "a de-DE paragraph mark beats an en-US docDefault: {german_mark:?} vs \
         {default_only:?}",
    );
}

/// The converse, so the test above cannot pass by the mark being ignored in
/// both directions: an English mark in a German document keeps the point.
#[test]
fn an_english_paragraph_mark_overrides_a_german_document_default() {
    let english_mark = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], "en-US"),
        ),
        ("word/styles.xml", &styles("de-DE")),
    ]));
    let default_only = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1,5", "333,125"], ""),
        ),
        ("word/styles.xml", &styles("de-DE")),
    ]));

    assert!(
        (english_mark[0] - english_mark[1]) > (default_only[0] - default_only[1]) + 1.0,
        "an en-US paragraph mark beats a de-DE docDefault: {english_mark:?} vs \
         {default_only:?}",
    );
}

/// A tag this engine does not know changes nothing — the point stays the
/// separator, so an unfamiliar document renders exactly as it did before
/// `w:lang` was ever read.
#[test]
fn an_unrecognised_language_tag_leaves_the_separator_alone() {
    let unknown = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "zz-ZZ"),
        ),
        ("word/styles.xml", &styles("zz-ZZ")),
    ]));
    let english = separator_offsets(&layout(&[
        (
            "word/document.xml",
            &decimal_tab_document(&["1.5", "333.125"], "en-US"),
        ),
        ("word/styles.xml", &styles("en-US")),
    ]));

    assert_eq!(unknown, english, "an unknown tag is a no-op, not a degrade");
}

// ── §17.9.27 numbering text formats ─────────────────────────────────────────

fn numbering(format: &str) -> String {
    numbering_with_level_lang(format, "")
}

/// `level_lang`, if non-empty, is spliced into `<w:lvl><w:rPr>` — the top of
/// the §17.9.23 label cascade.
fn numbering_with_level_lang(format: &str, level_lang: &str) -> String {
    let level_rpr = if level_lang.is_empty() {
        String::new()
    } else {
        format!(r#"<w:rPr><w:lang w:val="{level_lang}"/></w:rPr>"#)
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering {W}>
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="{format}"/>
      <w:lvlText w:val="%1"/>
      <w:suff w:val="space"/>
      {level_rpr}
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#
    )
}

/// Three numbered paragraphs, so the labels are 1, 2, 3.
fn numbered_document(mark_lang: &str) -> String {
    let mark_rpr = if mark_lang.is_empty() {
        String::new()
    } else {
        format!(r#"<w:rPr><w:lang w:val="{mark_lang}"/></w:rPr>"#)
    };
    let paragraphs: String = (1..=3)
        .map(|i| {
            format!(
                r#"<w:p>
  <w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>{mark_rpr}</w:pPr>
  <w:r><w:t>item{i}</w:t></w:r>
</w:p>"#
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document {W}><w:body>{paragraphs}</w:body></w:document>"#
    )
}

/// The three list labels, in order.
fn labels(format: &str, lang: &str) -> Vec<String> {
    let pages = layout(&[
        ("word/document.xml", &numbered_document(lang)),
        ("word/styles.xml", &styles(lang)),
        ("word/numbering.xml", &numbering(format)),
    ]);
    texts(&pages)
        .into_iter()
        .map(|(t, _)| t.trim().to_string())
        .filter(|t| !t.starts_with("item"))
        .filter(|t| !t.is_empty())
        .collect()
}

#[test]
fn english_ordinals_are_spelled_with_english_suffixes() {
    assert_eq!(labels("ordinal", "en-US"), ["1st", "2nd", "3rd"]);
}

#[test]
fn english_cardinal_text_is_spelled_out() {
    assert_eq!(labels("cardinalText", "en-US"), ["One", "Two", "Three"]);
}

#[test]
fn english_ordinal_text_is_spelled_out() {
    assert_eq!(labels("ordinalText", "en-US"), ["First", "Second", "Third"]);
}

// ── issue #132: three more languages spelled ────────────────────────────────
//
// Issue #91's request, and #132's first acceptance criterion: a German
// document's `cardinalText` must read *Eins, Zwei, Drei* and not `1, 2, 3`.
// The tests these replace asserted the digits — that replacement, visible as a
// diff, *is* the criterion.

#[test]
fn a_german_document_gets_german_number_words() {
    assert_eq!(labels("cardinalText", "de-DE"), ["Eins", "Zwei", "Drei"]);
    assert_eq!(
        labels("ordinalText", "de-DE"),
        ["Erste", "Zweite", "Dritte"]
    );
    assert_eq!(labels("ordinal", "de-DE"), ["1.", "2.", "3."]);
}

#[test]
fn a_french_document_gets_french_number_words() {
    assert_eq!(labels("cardinalText", "fr-FR"), ["Un", "Deux", "Trois"]);
    assert_eq!(
        labels("ordinalText", "fr-FR"),
        ["Premier", "Deuxième", "Troisième"],
    );
    assert_eq!(labels("ordinal", "fr-FR"), ["1er", "2e", "3e"]);
}

#[test]
fn a_spanish_document_gets_spanish_number_words() {
    assert_eq!(labels("cardinalText", "es-ES"), ["Uno", "Dos", "Tres"]);
    assert_eq!(
        labels("ordinalText", "es-ES"),
        ["Primero", "Segundo", "Tercero"],
    );
    assert_eq!(labels("ordinal", "es-ES"), ["1.º", "2.º", "3.º"]);
}

/// Every region of a spelled language spells it — the tag is classified by its
/// primary subtag, which is the level at which number words are a language's
/// and not a region's.
#[test]
fn a_regional_variant_of_a_spelled_language_still_spells_it() {
    assert_eq!(labels("cardinalText", "de-AT"), ["Eins", "Zwei", "Drei"]);
    assert_eq!(labels("cardinalText", "fr-CA"), ["Un", "Deux", "Trois"]);
    assert_eq!(labels("cardinalText", "es-MX"), ["Uno", "Dos", "Tres"]);
}

/// §17.9.27 is language-dependent, and this engine now knows four languages.
/// A fifth still gets the digits: emitting `1st` — or *Eins* — onto a Polish
/// list is not a degrade, it is a different language's text, and the digits
/// Word itself falls back to are closer than another language's words.
#[test]
fn a_language_without_number_words_still_gets_digits() {
    for lang in ["pl-PL", "ja-JP", "it-IT"] {
        assert_eq!(labels("ordinal", lang), ["1", "2", "3"], "ordinal/{lang}");
        assert_eq!(
            labels("cardinalText", lang),
            ["1", "2", "3"],
            "cardinalText/{lang}",
        );
        assert_eq!(
            labels("ordinalText", lang),
            ["1", "2", "3"],
            "ordinalText/{lang}",
        );
    }
}

/// An unrecognised tag keeps today's behaviour here too: English words, because
/// that is what every document got before `w:lang` was read. Degrading it to
/// digits would make an unfamiliar tag *lose* labels it renders today.
#[test]
fn an_unrecognised_language_tag_still_gets_english_number_words() {
    assert_eq!(labels("ordinal", "zz-ZZ"), ["1st", "2nd", "3rd"]);
    assert_eq!(labels("cardinalText", "zz-ZZ"), ["One", "Two", "Three"]);
}

/// §17.9.23: a label reads its language from its *own* cascade, whose top layer
/// is the level's `<w:rPr>` — the same layer its font and colour come from. A
/// German document whose numbering level declares English gets English words.
#[test]
fn a_numbering_levels_own_language_beats_the_paragraphs() {
    let pages = layout(&[
        ("word/document.xml", &numbered_document("de-DE")),
        ("word/styles.xml", &styles("de-DE")),
        (
            "word/numbering.xml",
            &numbering_with_level_lang("cardinalText", "en-US"),
        ),
    ]);
    let got: Vec<String> = texts(&pages)
        .into_iter()
        .map(|(t, _)| t.trim().to_string())
        .filter(|t| !t.starts_with("item") && !t.is_empty())
        .collect();

    assert_eq!(got, ["One", "Two", "Three"]);
}

/// Formats that are not language-dependent are untouched by any of this —
/// including the §17.18.59 sequences issue #132 added, which is also the
/// end-to-end proof that those new `<w:numFmt>` values survive the parse seam
/// instead of degrading to decimal on the way through.
#[test]
fn language_independent_number_formats_are_unchanged_by_locale() {
    for lang in ["en-US", "de-DE", "ja-JP", "zz-ZZ"] {
        for (format, want) in [
            ("decimal", ["1", "2", "3"]),
            ("lowerRoman", ["i", "ii", "iii"]),
            ("upperLetter", ["A", "B", "C"]),
            ("decimalZero", ["01", "02", "03"]),
            ("decimalFullWidth", ["１", "２", "３"]),
            ("decimalEnclosedCircle", ["①", "②", "③"]),
            ("aiueoFullWidth", ["ア", "イ", "ウ"]),
            ("chicago", ["*", "†", "‡"]),
            ("hebrew1", ["א", "ב", "ג"]),
            ("ideographZodiacTraditional", ["甲子", "乙丑", "丙寅"]),
        ] {
            assert_eq!(labels(format, lang), want, "{format}/{lang}");
        }
    }
}

/// …and the §17.18.59 values that are *still* unsupported keep degrading to
/// decimal rather than failing the parse, which is the boundary the
/// classification draws.
#[test]
fn an_unsupported_number_format_still_degrades_to_decimal() {
    for format in ["japaneseCounting", "bahtText", "custom"] {
        assert_eq!(labels(format, "en-US"), ["1", "2", "3"], "{format}");
    }
}

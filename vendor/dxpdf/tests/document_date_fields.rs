//! §17.16.5.13 `DATE` and §17.16.5.76 `TIME` — evaluated during layout, with
//! §17.16.4.2 picture names in the document's own language.
//!
//! Two claims that only an end-to-end test can make, because the unit tests
//! in `src/field/format.rs` exercise `format_date` directly and would pass
//! just as well if nothing ever called it:
//!
//! * a `DATE` field is *evaluated at all* during rendering. Until issue #129
//!   only PAGE and NUMPAGES were; every other field kept whatever text Word
//!   had cached in the run, so a document last saved in 2019 rendered "2019"
//!   forever.
//! * the §17.3.2.20 `w:lang` in effect reaches the picture's name tokens, all
//!   the way from `docDefaults` through fragment collection.
//!
//! These assert set membership rather than an exact date. The render reads
//! the wall clock (`crate::field::now`), so the day is whatever the day is;
//! what is *not* left to chance is the language, which is the property under
//! test. Checking the result is one of the twelve German month names — and
//! not one of the twelve English ones — pins that precisely without pinning
//! the calendar. The arithmetic itself is covered deterministically by
//! `field::now`'s own tests.

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

/// One `w:fldSimple` carrying `instr`, with the cached content Word would
/// have left behind. That cached text is deliberately a *recognisable*
/// string: if evaluation silently stops happening, these tests fail on
/// seeing it rather than passing on a coincidence.
fn field_document(instr: &str) -> String {
    let escaped = instr.replace('"', "&quot;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document {W}><w:body>
  <w:p><w:fldSimple w:instr="{escaped}">
    <w:r><w:t>CACHED</w:t></w:r>
  </w:fldSimple></w:p>
</w:body></w:document>"#
    )
}

fn layout(parts: &[(&str, &str)]) -> Vec<LayoutedPage> {
    let doc = dxpdf::docx::parse(&make_docx(parts)).expect("fixture parses");
    dxpdf::render::resolve_and_layout(doc).1
}

/// The text of the one field in a document written in `lang`.
fn field_text(lang: &str, instr: &str) -> String {
    let pages = layout(&[
        ("word/document.xml", &field_document(instr)),
        ("word/styles.xml", &styles(lang)),
    ]);
    pages
        .iter()
        .flat_map(|p| p.commands.iter())
        .filter_map(|c| match c {
            DrawCommand::Text { text, .. } => Some(text.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

const ENGLISH_MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const GERMAN_MONTHS: [&str; 12] = [
    "Januar",
    "Februar",
    "März",
    "April",
    "Mai",
    "Juni",
    "Juli",
    "August",
    "September",
    "Oktober",
    "November",
    "Dezember",
];
const ENGLISH_WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const GERMAN_WEEKDAYS: [&str; 7] = [
    "Montag",
    "Dienstag",
    "Mittwoch",
    "Donnerstag",
    "Freitag",
    "Samstag",
    "Sonntag",
];

/// The whole point of wiring evaluation in: the field renders *something the
/// engine computed*, not the text Word cached in the run.
#[test]
fn a_date_field_is_evaluated_rather_than_left_as_cached_text() {
    let text = field_text("en-US", r#" DATE \@ "yyyy" "#);
    assert_ne!(text, "CACHED", "the field kept its cached content");
    assert!(
        text.len() == 4 && text.chars().all(|c| c.is_ascii_digit()),
        "expected a four-digit year, got {text:?}",
    );
}

/// #129's first acceptance criterion, end to end: a non-English document's
/// `MMMM` is a German month name — and, so this can't pass on a month whose
/// two spellings coincide (April, September, November), *not* an English one
/// except where German spells it identically.
#[test]
fn a_date_field_renders_a_localized_month_name() {
    let german = field_text("de-DE", r#" DATE \@ "MMMM" "#);
    assert!(
        GERMAN_MONTHS.contains(&german.as_str()),
        "not a German month name: {german:?}",
    );

    let english = field_text("en-US", r#" DATE \@ "MMMM" "#);
    assert!(
        ENGLISH_MONTHS.contains(&english.as_str()),
        "not an English month name: {english:?}",
    );

    // Both documents render the same instant, so the two names must be the
    // same month — the pair differs only in language.
    let same_index = GERMAN_MONTHS.iter().position(|m| *m == german)
        == ENGLISH_MONTHS.iter().position(|m| *m == english);
    assert!(
        same_index,
        "the same date named two different months: {german:?} vs {english:?}",
    );
}

/// #129's second acceptance criterion: `dddd` in English and in one other
/// language. Also the regression guard for the token itself — before #129,
/// `dddd` fell through to the zero-padded day-of-month branch and rendered a
/// *number*, so a bare digit here means the token split was lost.
#[test]
fn a_date_field_renders_a_localized_weekday_name() {
    let german = field_text("de-DE", r#" DATE \@ "dddd" "#);
    assert!(
        GERMAN_WEEKDAYS.contains(&german.as_str()),
        "not a German weekday name: {german:?}",
    );

    let english = field_text("en-US", r#" DATE \@ "dddd" "#);
    assert!(
        ENGLISH_WEEKDAYS.contains(&english.as_str()),
        "not an English weekday name: {english:?}",
    );

    let same_index = GERMAN_WEEKDAYS.iter().position(|d| *d == german)
        == ENGLISH_WEEKDAYS.iter().position(|d| *d == english);
    assert!(
        same_index,
        "the same date named two different weekdays: {german:?} vs {english:?}",
    );
}

/// A whole picture through the real pipeline, not a single token: literal
/// text survives, and the numeric tokens still read the date.
#[test]
fn a_full_date_picture_renders_through_the_pipeline() {
    let text = field_text("de-DE", r#" DATE \@ "dddd, d. MMMM yyyy" "#);
    let (weekday, rest) = text.split_once(", ").unwrap_or_else(|| {
        panic!("expected 'weekday, d. month yyyy', got {text:?}");
    });
    assert!(
        GERMAN_WEEKDAYS.contains(&weekday),
        "leading token is not a German weekday: {text:?}",
    );
    assert!(
        GERMAN_MONTHS.iter().any(|m| rest.contains(m)),
        "no German month name in {text:?}",
    );
    assert!(rest.ends_with("2026") || rest.ends_with("2027"), "{text:?}");
}

/// §17.16.5.76 `TIME` is evaluated on the same path. Nothing in its
/// vocabulary is localized, so this only proves it stops showing cached text
/// and produces a well-formed clock reading.
#[test]
fn a_time_field_is_evaluated() {
    let text = field_text("en-US", r#" TIME \@ "HH:mm" "#);
    assert_ne!(text, "CACHED", "the field kept its cached content");
    let (hh, mm) = text
        .split_once(':')
        .unwrap_or_else(|| panic!("expected HH:mm, got {text:?}"));
    let (hh, mm): (u32, u32) = (
        hh.parse().unwrap_or_else(|_| panic!("{text:?}")),
        mm.parse().unwrap_or_else(|_| panic!("{text:?}")),
    );
    assert!(hh < 24 && mm < 60, "not a real time: {text:?}");
}

/// A tag with no CLDR data behind it falls back to English rather than
/// rendering nothing — the same discipline the decimal separator follows.
#[test]
fn an_unrecognised_language_falls_back_to_english_names() {
    let text = field_text("zz-ZZ", r#" DATE \@ "MMMM" "#);
    assert!(
        ENGLISH_MONTHS.contains(&text.as_str()),
        "expected an English fallback, got {text:?}",
    );
}

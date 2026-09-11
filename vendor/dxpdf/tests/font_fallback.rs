//! Issue #139 end-to-end: a codepoint the resolved face cannot draw must be
//! drawn by a face that can, instead of being dropped.
//!
//! `test-files/issue-139-minimal.docx` is the issue's own reproduction, built
//! by `scripts/make_font_fallback_fixture.py`. It names **no font anywhere**,
//! so the run resolves to the §17.7.2 spec fallback and the engine has nothing
//! but a family name that cannot draw most of the text.
//!
//! # Asserted by structure, never by face name
//!
//! Which face covers `ア` is a property of the machine the test runs on — macOS
//! answers Songti SC, a Linux container answers whatever it has, and a
//! container with no CJK fonts answers nothing at all. So these tests assert
//! that the family *changed* and that the face it changed to *covers the
//! codepoint*, and never that it is any particular font. A test naming Songti
//! SC would pass on the author's machine and fail in CI, which is worse than
//! no test.
//!
//! Hosts that genuinely cannot cover a script skip that case, the way
//! `emoji_e2e.rs` skips when the host has no color emoji typeface — this
//! converter bundles no fonts and uses the host's or none.

use dxpdf::render::fonts::{FaceRequest, FontRegistry, Toggle};
use dxpdf::render::layout::draw_command::{DrawCommand, LayoutedPage};
use dxpdf::render::resolve_and_layout;
use skia_safe::FontMgr;

/// The three codepoints the issue reports as dropped, plus the control.
const CIRCLED: char = '\u{2460}'; // ①
const KATAKANA: char = '\u{30A2}'; // ア
const THAI: char = '\u{0E51}'; // ๑
/// Times New Roman covers Hebrew, so this rendered correctly *before* the fix.
/// It is here to catch a fix that moves text which was never broken.
const HEBREW: char = '\u{05D0}'; // א

fn fixture() -> dxpdf::model::Document {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test-files/issue-139-minimal.docx"
    ))
    .expect(
        "test-files/issue-139-minimal.docx — build it with scripts/make_font_fallback_fixture.py",
    );
    dxpdf::docx::parse(&bytes).expect("the fixture must parse")
}

/// Every `(text, font_family)` pair the layout produced.
fn text_commands(pages: &[LayoutedPage]) -> Vec<(String, String)> {
    pages
        .iter()
        .flat_map(|p| &p.commands)
        .filter_map(|c| match c {
            DrawCommand::Text {
                text, font_family, ..
            } => Some((text.to_string(), font_family.to_string())),
            _ => None,
        })
        .collect()
}

/// The family that drew `ch`, or `None` if nothing drew it at all.
fn family_drawing(commands: &[(String, String)], ch: char) -> Option<String> {
    commands
        .iter()
        .find(|(text, _)| text.contains(ch))
        .map(|(_, family)| family.clone())
}

/// The family that drew the plain ASCII — the document's own resolved face,
/// and the baseline every fallback is compared against.
fn base_family(commands: &[(String, String)]) -> String {
    family_drawing(commands, 'A').expect("the ASCII text must be drawn by something")
}

/// Whether some face on this host can draw `ch` at all. A host that cannot
/// has nothing to fall back to, and the case is skipped rather than failed.
fn host_covers(ch: char) -> bool {
    let mgr = FontMgr::new();
    mgr.match_family_style_character(
        "Times New Roman",
        skia_safe::FontStyle::normal(),
        &[],
        ch as i32,
    )
    .is_some_and(|t| t.unichar_to_glyph(ch as i32) != 0)
}

/// Whether the family named in a draw command really covers `ch` once
/// re-resolved — which is what the painter and `subset::collect` both do.
fn resolved_family_covers(family: &str, ch: char) -> bool {
    let registry = FontRegistry::new(FontMgr::new());
    let entry = registry.resolve(&FaceRequest::new(family, Toggle::Absent, Toggle::Absent));
    entry.typeface.unichar_to_glyph(ch as i32) != 0
}

/// The defect. Before the fix all four codepoints were drawn by the single
/// resolved face, and the three it could not cover produced no glyph at all —
/// not a `.notdef` box, nothing.
#[test]
fn an_uncovered_codepoint_is_drawn_by_a_face_that_covers_it() {
    let (_, pages) = resolve_and_layout(fixture());
    let commands = text_commands(&pages);
    let base = base_family(&commands);

    for (label, ch) in [("circled", CIRCLED), ("katakana", KATAKANA), ("thai", THAI)] {
        if !host_covers(ch) {
            eprintln!(
                "skipping {label}: no face on this host covers U+{:04X}",
                ch as u32
            );
            continue;
        }
        let family = family_drawing(&commands, ch)
            .unwrap_or_else(|| panic!("{label}: U+{:04X} reached no draw command", ch as u32));
        assert_ne!(
            family, base,
            "{label}: U+{:04X} is still drawn by the base face, which cannot draw it",
            ch as u32
        );
        assert!(
            resolved_family_covers(&family, ch),
            "{label}: the family reported for U+{:04X} ({family:?}) does not resolve to a face \
             covering it — the painter and the subsetter would both draw nothing",
            ch as u32
        );
    }
}

/// The control, and acceptance criterion 3 in miniature: coverage is luck, and
/// a codepoint the document's own face already covers must not be moved.
#[test]
fn a_codepoint_the_base_face_covers_is_left_where_it_was() {
    let (_, pages) = resolve_and_layout(fixture());
    let commands = text_commands(&pages);
    let base = base_family(&commands);

    if !resolved_family_covers(&base, HEBREW) {
        eprintln!("skipping: this host's default face does not cover Hebrew either");
        return;
    }
    assert_eq!(
        family_drawing(&commands, HEBREW).as_deref(),
        Some(base.as_str()),
        "Hebrew rendered correctly before per-glyph fallback existed; moving it to another \
         face means fallback fired on a codepoint that was never missing"
    );
}

/// Acceptance criterion 2: the fallback face has to survive the subsetting
/// pass, which keys codepoint usage by `TypefaceId` re-resolved from the
/// command's family name. A face the collector cannot see is subsetted out of
/// existence and the glyph is lost again — this renders the whole pipeline and
/// checks the bytes that come out.
#[test]
#[cfg(feature = "subset-fonts")]
fn the_fallback_face_survives_subsetting() {
    if !host_covers(KATAKANA) {
        eprintln!("skipping: no face on this host covers katakana");
        return;
    }
    let pdf = dxpdf::render::render_with_font_mgr(
        fixture(),
        &FontMgr::new(),
        &dxpdf::RenderOptions::default(),
    )
    .expect("the fixture must render");

    let parsed = lopdf::Document::load_mem(&pdf).expect("the PDF must parse");
    // `/FontDescriptor`, not `/Type /Font`: a CID-keyed face emits *two* font
    // dictionaries — the Type 0 and its descendant CIDFont — so counting those
    // would report two for a single embedded face and pass whether or not any
    // fallback happened. One descriptor is one embedded face.
    let faces: Vec<String> = parsed
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter(|d| {
            d.get(b"Type")
                .ok()
                .and_then(|t| t.as_name().ok())
                .is_some_and(|n| n == b"FontDescriptor")
        })
        .filter_map(|d| d.get(b"FontName").ok()?.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect();

    assert!(
        faces.len() > 1,
        "expected the fallback face to be embedded alongside the base face, found {faces:?} — \
         the subsetter keys usage by re-resolved family name, so a face it cannot see is culled"
    );
}

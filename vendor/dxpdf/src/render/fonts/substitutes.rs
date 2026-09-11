//! §17.8: metric-compatible substitution, and the family-name parsing that
//! feeds it.
//!
//! Two concerns that have to live together because the second exists to serve
//! the first: [`FONT_SUBSTITUTIONS`] is keyed by *family*, so a document naming
//! a *face* only reaches the table after [`strip_weight_suffix`] has cut the
//! face name back to its family.
//!
//! This is the engine's last resort before the system default, and it is
//! deliberately the *last* place a name is parsed rather than read. Everything
//! that can be settled by reading the font's own `name` and `OS/2` tables is
//! settled before control arrives here — see the resolution chain in
//! [`super::FontRegistry::resolve`].
//!
//! "Metric-compatible" means *same advance widths*, so line breaks and page
//! counts match Word even though glyph shapes differ — why Carlito/Caladea
//! are used here rather than a generic sans/serif. Substitution is also a
//! *host-dependent* step: the same DOCX can paginate differently on macOS and
//! Linux depending on what is installed. When cross-platform output differs,
//! check this table first.

/// Open-source metric-compatible substitutes for proprietary fonts. Tried
/// in order when `match_family_style` for the requested family fails.
pub(crate) const FONT_SUBSTITUTIONS: &[(&str, &[&str])] = &[
    ("Calibri", &["Carlito", "Liberation Sans", "Noto Sans"]),
    ("Cambria", &["Caladea", "Liberation Serif", "Noto Serif"]),
    ("Arial", &["Liberation Sans", "Noto Sans", "Helvetica"]),
    (
        "Times New Roman",
        &["Liberation Serif", "Noto Serif", "Times"],
    ),
    (
        "Courier New",
        &["Liberation Mono", "Noto Sans Mono", "Courier"],
    ),
    ("Verdana", &["DejaVu Sans", "Noto Sans"]),
    ("Georgia", &["DejaVu Serif", "Noto Serif"]),
    ("Trebuchet MS", &["Ubuntu", "Noto Sans"]),
    (
        "Consolas",
        &["Inconsolata", "Liberation Mono", "Noto Sans Mono"],
    ),
    ("Segoe UI", &["Noto Sans", "Liberation Sans"]),
];

/// Strip a trailing weight word from a face-qualified family name, yielding the
/// base family: `"Segoe UI Light"` → `"Segoe UI"`. `None` when the name does not
/// end in a recognised weight word.
///
/// [`FONT_SUBSTITUTIONS`] is keyed by family, so a document naming a *face*
/// otherwise walks straight past the substitution step to the system default —
/// `"Segoe UI"` is in the table but `"Segoe UI Light"` was not reachable from
/// it. When the metadata steps also decline the name there was no path at all
/// from a face name to its family's metric-compatible substitutes.
///
/// The longest matching suffix wins, so `"Foo Extra Light"` yields `"Foo"`
/// rather than `"Foo Extra"`. A weight word must be a separate trailing word:
/// `"Highlight"` ends in `"light"` but is not face-qualified.
pub(crate) fn strip_weight_suffix(name: &str) -> Option<&str> {
    let trimmed = name.trim_end();
    let mut best: Option<&str> = None;
    for weight in (100..=900).step_by(100) {
        for suffix in canonical_weight_names(weight) {
            let Some(cut) = trimmed.len().checked_sub(suffix.len()) else {
                continue;
            };
            if cut == 0 || !trimmed.is_char_boundary(cut) {
                continue;
            }
            if !trimmed[cut..].eq_ignore_ascii_case(suffix) {
                continue;
            }
            let base = trimmed[..cut].trim_end();
            // The suffix has to be its own word, and something must precede it.
            if base.len() == cut || base.is_empty() {
                continue;
            }
            if best.is_none_or(|b| base.len() < b.len()) {
                best = Some(base);
            }
        }
    }
    best
}

/// Metric-compatible substitutes for `family`, falling back to its base family
/// when the name is face-qualified. Returns the matched key alongside the list
/// so the caller can log which one fired.
pub(crate) fn substitutes_for(family: &str) -> Option<(&'static str, &'static [&'static str])> {
    let lookup = |name: &str| {
        FONT_SUBSTITUTIONS
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(key, subs)| (*key, *subs))
    };
    lookup(family).or_else(|| lookup(strip_weight_suffix(family)?))
}

/// The English spellings a face of `weight` is conventionally called.
///
/// This is a *guess table*, and its existence is the reason the metadata steps
/// run first: a font's `OS/2` `usWeightClass` says 600 outright, whereas
/// matching the word "Semibold" only infers it. Kept for the step that runs
/// when a font has no readable metadata at all.
pub(crate) fn canonical_weight_names(weight: i32) -> &'static [&'static str] {
    match weight {
        100 => &["Thin", "Hairline"],
        200 => &["ExtraLight", "Extra Light", "UltraLight", "Ultra Light"],
        300 => &["Light"],
        400 => &["Regular", "Normal"],
        500 => &["Medium"],
        600 => &["Semibold", "SemiBold", "Semi Bold", "DemiBold", "Demi Bold"],
        700 => &["Bold"],
        800 => &["ExtraBold", "Extra Bold", "UltraBold", "Ultra Bold"],
        900 => &["Black", "Heavy"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_trailing_weight_word_to_the_base_family() {
        assert_eq!(strip_weight_suffix("Segoe UI Light"), Some("Segoe UI"));
        assert_eq!(strip_weight_suffix("Calibri Light"), Some("Calibri"));
        assert_eq!(strip_weight_suffix("Segoe UI Semibold"), Some("Segoe UI"));
        assert_eq!(strip_weight_suffix("Arial Black"), Some("Arial"));
        // Case-insensitive, and tolerant of the spacing variants the alias
        // index already accepts.
        assert_eq!(strip_weight_suffix("Calibri LIGHT"), Some("Calibri"));
        assert_eq!(strip_weight_suffix("Foo Extra Bold"), Some("Foo"));
    }

    /// The longest suffix wins, so a two-word weight name is not left half
    /// attached.
    #[test]
    fn longest_weight_suffix_wins() {
        assert_eq!(strip_weight_suffix("Foo Extra Light"), Some("Foo"));
        assert_eq!(strip_weight_suffix("Foo Ultra Bold"), Some("Foo"));
    }

    /// A weight word must be a separate trailing word — otherwise every family
    /// ending in those letters would be silently truncated.
    #[test]
    fn weight_word_must_be_its_own_word() {
        assert_eq!(strip_weight_suffix("Highlight"), None);
        assert_eq!(strip_weight_suffix("Blackadder"), None);
        assert_eq!(strip_weight_suffix("Light"), None, "nothing precedes it");
        assert_eq!(strip_weight_suffix("Times New Roman"), None);
    }

    /// H2#4 regression: `"Segoe UI"` is in the table, so a face-qualified name
    /// built on it must reach the same substitutes. Before this, the lookup
    /// was an exact whole-name match and face names fell straight through to
    /// the system default.
    #[test]
    fn substitutes_reach_face_qualified_names_through_the_base_family() {
        let (matched, subs) = substitutes_for("Segoe UI").expect("base family is in the table");
        assert_eq!(matched, "Segoe UI");

        let (via, face_subs) =
            substitutes_for("Segoe UI Light").expect("face-qualified name must reach the family");
        assert_eq!(via, "Segoe UI", "resolved through the base family");
        assert_eq!(face_subs, subs, "and gets the same substitutes");

        assert_eq!(
            substitutes_for("Calibri Light").map(|(k, _)| k),
            Some("Calibri")
        );
        // A family that is not in the table stays absent, stripped or not.
        assert!(substitutes_for("Wingdings").is_none());
        assert!(substitutes_for("Wingdings Light").is_none());
    }
}

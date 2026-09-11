//! §17.3.2.20 `w:lang` reduced to the distinctions layout actually makes.

use crate::model::RunProperties;

/// The language a piece of the document is written in, as far as this engine's
/// layout is concerned.
///
/// Deliberately **not** a BCP-47 parser. The tag space is open and unbounded,
/// while the engine asks a language exactly two questions:
///
/// * §17.18.85 — which character a `decimal` tab aligns its zone on;
/// * §17.9.27 — whose number words `ordinal` / `cardinalText` / `ordinalText`
///   are written in.
///
/// So this names the groups those two questions have distinct answers for, and
/// stops. When a third question arrives, or when a language's number words are
/// implemented, the enum gains a variant and the compiler finds every site that
/// has to answer for it — which is the whole reason this is an enum and not a
/// `&str` compared afresh at each call site. [`German`](Self::German),
/// [`French`](Self::French) and [`Spanish`](Self::Spanish) are exactly that
/// having happened: each was a `CommaDecimal` until issue #132 spelled its
/// numbers.
///
/// Only `w:lang/@w:val` is read. `@w:eastAsia` and `@w:bidi` name the languages
/// of *other script runs* in the same document, and neither of the two
/// questions above is asked of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    /// English, in any region. Also the answer for a document that declares no
    /// language at all, which is what every document was assumed to be before
    /// `w:lang` was read.
    #[default]
    English,
    /// German, in any region. Writes a decimal comma, and its number words are
    /// spelled — see [`crate::render::resolve::spellout`].
    German,
    /// French, in any region. Writes a decimal comma; number words spelled.
    French,
    /// Spanish, in any region. Writes a decimal comma; number words spelled.
    Spanish,
    /// A recognised language that writes a decimal **comma** and whose number
    /// words this engine does not spell: Italian, Portuguese, Russian, Polish,
    /// Dutch, the Nordics, and most of Central and Eastern Europe.
    CommaDecimal,
    /// A recognised language that is not English and writes a decimal **point**:
    /// Japanese, Chinese, Korean, Hebrew, Thai, and most of South and
    /// South-East Asia.
    PointDecimal,
    /// A tag whose primary subtag is not in the table below.
    ///
    /// It answers every question exactly as [`Locale::English`] does, on
    /// purpose: before `w:lang` was read, *every* document got a decimal point
    /// and English number words, so an unfamiliar tag has to keep rendering as
    /// it does today rather than silently losing content to a degrade. It is a
    /// separate variant because it is a different *fact* — it logs, so an
    /// unhandled language is visible rather than silently assumed English.
    Unrecognised,
}

impl Locale {
    /// §17.18.85: the character a `decimal` tab stop aligns its zone on.
    pub fn decimal_separator(self) -> char {
        match self {
            Locale::German | Locale::French | Locale::Spanish | Locale::CommaDecimal => ',',
            Locale::English | Locale::PointDecimal | Locale::Unrecognised => '.',
        }
    }

    /// §17.7.2: the first non-empty `w:lang/@w:val` a cascade sets, before
    /// classification — the raw tag [`from_cascade`](Self::from_cascade)
    /// reduces to a [`Locale`] bucket, and what a caller needing more than
    /// the bucket (§128's region-aware decimal separator, which the bucket
    /// alone can't represent) resolves further itself.
    ///
    /// Layers come highest-priority first, exactly as §17.7.2 resolves any
    /// other run property. The emptiness test belongs *inside* the search: an
    /// empty `@w:val` sets nothing, so it must not stop the walk and shadow a
    /// lower layer that does.
    pub fn first_tag<'a>(layers: impl IntoIterator<Item = &'a RunProperties>) -> Option<&'a str> {
        layers.into_iter().find_map(|rp| {
            rp.lang
                .get()
                .and_then(|l| l.val.as_deref())
                .filter(|tag| !tag.is_empty())
        })
    }

    /// §17.7.2: classify the first `w:lang/@w:val` a cascade sets.
    ///
    /// A cascade that sets none anywhere is [`Locale::English`] and logs
    /// nothing — an absent tag is the common case for a minimal document, not
    /// an unhandled language.
    pub fn from_cascade<'a>(layers: impl IntoIterator<Item = &'a RunProperties>) -> Self {
        Self::first_tag(layers).map_or(Locale::English, Locale::from_tag)
    }

    /// Classify one §17.3.2.20 tag by its **primary subtag**.
    ///
    /// BCP-47's primary subtag is everything before the first `-`, and it is
    /// all either question depends on for the languages below: `de-DE` and
    /// `de-AT` write the same decimal comma, `en-US` and `en-GB` the same
    /// point. Matched case-insensitively, because the attribute is a tag and
    /// tags are case-insensitive even though Word writes them `ll-CC`.
    ///
    /// **Known simplification**, still true of *this function* — it stays
    /// primary-subtag-only on purpose, since it also picks which language's
    /// number words a label is spelled in, a question region doesn't change
    /// (`de-AT` and `de-DE` both write *Eins*). But issue #128 closed the
    /// simplification for the one question
    /// region *does* change: real §17.18.85 decimal-tab resolution no longer
    /// goes through this bucket alone.
    /// [`crate::i18n::decimal_separator_for_tag`] answers from real CLDR data
    /// first, using this function's bucket only when ICU4X has nothing for
    /// the tag — so `de-CH`/`it-CH` (write a point where `de`/`it` write a
    /// comma) and `en-ZA` (a comma where `en` writes a point) all render
    /// correctly today, `de-CH` and `en-ZA` explicitly tested, `it-CH`
    /// working via ICU4X's own fallback without being explicitly baked
    /// (`scripts/make_icu_data.sh`). Latin-American Spanish "splits both
    /// ways" is verified only for `es-MX` so far — other regions in that
    /// split are still open. Likewise still open: Arabic and Persian are
    /// bucketed point-decimal, right for their Latin-digit documents and
    /// wrong for the Arabic-Indic `٫` some regions use. That one is a
    /// *numbering-system* question — which digits the document is written in
    /// — which `decimal_separator_for_tag` doesn't answer and which issue
    /// #132 left where it found it: §17.18.59 has explicit values for the
    /// digit sets (`hindiNumbers`, `thaiNumbers`, `decimalFullWidth`), all
    /// rendered, but nothing says a `fa-IR` document's plain `decimal` should
    /// switch script, and this engine does not infer it.
    pub fn from_tag(tag: &str) -> Self {
        let primary = tag.split('-').next().unwrap_or("").to_ascii_lowercase();
        match primary.as_str() {
            "en" => Locale::English,

            // Writes a decimal comma, *and* has number words here. Matched
            // ahead of the bucket below, which is otherwise where they'd land.
            "de" => Locale::German,
            "fr" => Locale::French,
            "es" => Locale::Spanish,

            // Writes a decimal comma.
            "af" | "sq" | "hy" | "az" | "be" | "bs" | "bg" | "ca" | "hr" | "cs" | "da" | "nl"
            | "et" | "eu" | "fi" | "fo" | "gl" | "ka" | "el" | "hu" | "is" | "id" | "it" | "kk"
            | "lb" | "lv" | "lt" | "mk" | "mn" | "nb" | "nn" | "no" | "pl" | "pt" | "ro" | "ru"
            | "sr" | "sk" | "sl" | "sv" | "tr" | "uk" | "vi" => Locale::CommaDecimal,

            // Writes a decimal point, but is not English.
            "am" | "ar" | "bn" | "cy" | "fa" | "fil" | "ga" | "gu" | "he" | "hi" | "iw" | "ja"
            | "km" | "kn" | "ko" | "lo" | "ml" | "mr" | "ms" | "mt" | "my" | "ne" | "pa" | "si"
            | "sw" | "ta" | "te" | "th" | "tl" | "ur" | "zh" => Locale::PointDecimal,

            _ => {
                log::warn!(
                    "w:lang: unhandled language tag {tag:?} (§17.3.2.20) — \
                     assuming a decimal point and English number words, which \
                     is what every document got before locale was read"
                );
                Locale::Unrecognised
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Dup;
    use crate::model::Lang;

    fn rp(tag: Option<&str>) -> RunProperties {
        RunProperties {
            lang: Dup::from(tag.map(|t| Lang {
                val: Some(t.to_string()),
                east_asia: None,
                bidi: None,
            })),
            ..Default::default()
        }
    }

    #[test]
    fn english_regions_are_all_english() {
        for tag in ["en", "en-US", "en-GB", "en-AU", "EN-us"] {
            assert_eq!(Locale::from_tag(tag), Locale::English, "{tag}");
        }
    }

    #[test]
    fn the_corpus_languages_classify_as_they_should() {
        // Every tag `test-files/` and `test-cases/` actually declares.
        for tag in ["pl-PL", "it-IT", "ca-ES"] {
            assert_eq!(Locale::from_tag(tag), Locale::CommaDecimal, "{tag}");
        }
        for tag in ["de-AT", "de-DE"] {
            assert_eq!(Locale::from_tag(tag), Locale::German, "{tag}");
        }
        assert_eq!(Locale::from_tag("fr-FR"), Locale::French);
        for tag in ["en-US", "en-GB"] {
            assert_eq!(Locale::from_tag(tag), Locale::English, "{tag}");
        }
    }

    /// The three languages issue #132 spelled: they leave the comma bucket for
    /// a variant of their own, and every region of each follows.
    #[test]
    fn the_spelled_languages_have_their_own_variants() {
        for (tag, want) in [
            ("de", Locale::German),
            ("de-CH", Locale::German),
            ("DE-at", Locale::German),
            ("fr", Locale::French),
            ("fr-CA", Locale::French),
            ("es", Locale::Spanish),
            ("es-MX", Locale::Spanish),
        ] {
            assert_eq!(Locale::from_tag(tag), want, "{tag}");
        }
    }

    #[test]
    fn point_decimal_languages_are_not_english() {
        for tag in ["ja-JP", "zh-CN", "ko-KR", "he-IL", "th-TH", "hi-IN"] {
            assert_eq!(Locale::from_tag(tag), Locale::PointDecimal, "{tag}");
        }
    }

    /// The primary subtag alone decides, so a region this table has never seen
    /// still classifies rather than falling through to `Unrecognised`.
    #[test]
    fn an_unknown_region_still_resolves_by_its_primary_subtag() {
        assert_eq!(Locale::from_tag("de-LI"), Locale::German);
        assert_eq!(Locale::from_tag("en-ZZ"), Locale::English);
    }

    #[test]
    fn an_unknown_primary_subtag_is_unrecognised() {
        for tag in ["zz-ZZ", "x-klingon", "qqq"] {
            assert_eq!(Locale::from_tag(tag), Locale::Unrecognised, "{tag}");
        }
    }

    /// The reason `Unrecognised` is its own variant rather than `English`: it
    /// must answer identically, so an unfamiliar document renders unchanged.
    #[test]
    fn an_unrecognised_tag_answers_exactly_as_english_does() {
        use crate::render::resolve::spellout;
        assert_eq!(
            Locale::Unrecognised.decimal_separator(),
            Locale::English.decimal_separator(),
        );
        assert_eq!(
            spellout::cardinal(21, Locale::Unrecognised),
            spellout::cardinal(21, Locale::English),
        );
    }

    #[test]
    fn only_comma_languages_write_a_comma() {
        for locale in [
            Locale::CommaDecimal,
            Locale::German,
            Locale::French,
            Locale::Spanish,
        ] {
            assert_eq!(locale.decimal_separator(), ',', "{locale:?}");
        }
        assert_eq!(Locale::English.decimal_separator(), '.');
        assert_eq!(Locale::PointDecimal.decimal_separator(), '.');
    }

    /// §17.9.27: which languages have number words at all. The `Option` in
    /// `spellout` is the answer `spells_numbers()` used to give as a bool.
    #[test]
    fn only_the_four_spelled_languages_have_number_words() {
        use crate::render::resolve::spellout;
        for locale in [
            Locale::English,
            Locale::German,
            Locale::French,
            Locale::Spanish,
            Locale::Unrecognised,
        ] {
            assert!(spellout::cardinal(1, locale).is_some(), "{locale:?}");
        }
        for locale in [Locale::CommaDecimal, Locale::PointDecimal] {
            assert!(spellout::cardinal(1, locale).is_none(), "{locale:?}");
        }
    }

    #[test]
    fn the_cascade_takes_the_first_layer_that_sets_a_tag() {
        let layers = [rp(None), rp(Some("de-DE")), rp(Some("en-US"))];
        assert_eq!(Locale::from_cascade(layers.iter()), Locale::German);
    }

    /// An empty `@w:val` sets nothing — it must not shadow a lower layer that
    /// does, and it must not be classified as an unknown language either.
    #[test]
    fn an_empty_tag_falls_through_to_the_next_layer() {
        let layers = [rp(Some("")), rp(Some("de-DE"))];
        assert_eq!(Locale::from_cascade(layers.iter()), Locale::German);
    }

    /// A document that declares no language anywhere is English and silent —
    /// the overwhelmingly common minimal document, not an unhandled language.
    #[test]
    fn a_cascade_with_no_tag_at_all_is_english() {
        assert_eq!(Locale::from_cascade([rp(None)].iter()), Locale::English);
        assert_eq!(Locale::from_cascade(std::iter::empty()), Locale::English);
    }
}

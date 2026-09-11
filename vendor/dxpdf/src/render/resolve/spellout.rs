//! §17.9.27 number words — how a language writes a number out in full.
//!
//! `cardinalText`, `ordinalText` and `ordinal` are the three §17.18.59 formats
//! whose output is a *language*, not a sequence. Everything else in
//! `super::numbering` is the same in Tokyo and Toledo; these are not, and this
//! module is where that difference lives — one question, one place, the way
//! `crate::i18n::bidi` owns direction and `crate::i18n::segment` owns line
//! breaking.
//!
//! Each function answers `None` for a language it cannot spell. That is
//! deliberately a value rather than a boolean the caller has to remember to
//! consult first: it puts the digit fallback at exactly one site per format in
//! `super::numbering`'s `format_number`, and it makes adding a language a
//! matter of returning `Some` instead of touching every call site.
//!
//! # Why these words are written out here (issue #132)
//!
//! Spelling numbers is [CLDR RBNF]'s job, and the ICU4X this crate already
//! depends on for §17.18.85 separators, §17.16.4.2 date names and UAX #14
//! breaking does not implement it. `icu_rbnf` exists on crates.io — version
//! 0.0.1, published by the ICU4X project, whose entire `src/lib.rs` is
//! `assert_eq!(2 + 2, 4)`. A name reservation, not an implementation.
//!
//! The one crate that does do it is `num2words2-core`: 172 languages, with
//! `to_cardinal`/`to_ordinal`/`to_ordinal_num` mapping one-to-one onto OOXML's
//! three formats, and correct output for all three of the languages below.
//! It was measured rather than argued about, and rejected on the number: it
//! has **no per-language Cargo features** and registers all 172 languages
//! through a single lookup, so the linker keeps every one — a control binary
//! went 431,232 → 8,109,360 bytes, **+7.32 MB**, which is +62.8% of the entire
//! Python wheel to add three languages. The measurement is recorded so the
//! next person weighing it starts from a number instead of repeating it.
//!
//! [CLDR RBNF]: https://unicode.org/reports/tr35/tr35-numbers.html#Rule-Based_Number_Formatting
//!
//! # What is verified and what is not
//!
//! The words themselves are each language's standard orthography, checked
//! against its own rules — French's `soixante-dix`/`quatre-vingts`, German's
//! units-before-tens compounding, Spanish's `cien`/`ciento` split — and
//! covered by decade and boundary tables across the whole `u32` range rather
//! than by spot checks.
//!
//! Two things are choices no rule settles, and **Word reference render**: this
//! environment has no Word to compare against.
//!
//! * **Capitalisation.** English capitalises every word ("One Thousand"),
//!   which is what this engine has always emitted. German, French and Spanish
//!   capitalise only the first — capitalising every word would be wrong in all
//!   three, and German's scale nouns (`Million`, `Milliarde`) are capitalised
//!   because they are nouns, not because they start a label.
//! * **Declension.** German ordinals inflect for case and gender (`Erste`,
//!   `Erster`, `Erstes`); a list label has no sentence to agree with, so the
//!   weak nominative form is used throughout. Spanish `primero`/`primer` and
//!   French `premier`/`première` are the same question, answered the same way.
//!
//! What would overturn either: a Word render of a list whose `w:numFmt` is
//! `cardinalText` in each language.

use super::locale::Locale;

/// §17.9.27 `cardinalText` — the number in words. `None` if this engine does
/// not spell this language.
pub fn cardinal(n: u32, locale: Locale) -> Option<String> {
    match locale {
        Locale::English | Locale::Unrecognised => Some(english::cardinal(n)),
        Locale::German => Some(german::cardinal(n)),
        Locale::French => Some(french::cardinal(n)),
        Locale::Spanish => Some(spanish::cardinal(n)),
        Locale::CommaDecimal | Locale::PointDecimal => None,
    }
}

/// §17.9.27 `ordinalText` — the ordinal in words.
pub fn ordinal_words(n: u32, locale: Locale) -> Option<String> {
    match locale {
        Locale::English | Locale::Unrecognised => Some(english::ordinal_words(n)),
        Locale::German => Some(german::ordinal_words(n)),
        Locale::French => Some(french::ordinal_words(n)),
        Locale::Spanish => Some(spanish::ordinal_words(n)),
        Locale::CommaDecimal | Locale::PointDecimal => None,
    }
}

/// §17.9.27 `ordinal` — the ordinal in digits with the language's own
/// indicator: `1st`, `1.`, `1er`, `1.º`.
pub fn ordinal_numeric(n: u32, locale: Locale) -> Option<String> {
    match locale {
        Locale::English | Locale::Unrecognised => Some(english::ordinal_numeric(n)),
        // German writes an ordinal as the digits followed by a full stop, at
        // every magnitude — no irregular forms, which is why there is no
        // table here.
        Locale::German => Some(format!("{n}.")),
        Locale::French => Some(french::ordinal_numeric(n)),
        Locale::Spanish => Some(format!("{n}.º")),
        Locale::CommaDecimal | Locale::PointDecimal => None,
    }
}

/// Upper-case the first character, leaving the rest alone — the shape German,
/// French and Spanish labels take. (English capitalises every word, which its
/// own speller does as it builds them.)
fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

// ── English ────────────────────────────────────────────────────────────────

mod english {
    /// §17.9.27 `cardinalText`: e.g. `1234` → "One Thousand Two Hundred
    /// Thirty-Four".
    ///
    /// US English convention, which is what Word writes: tens and units joined
    /// by a hyphen, scale groups by a space, and **no** "and" before the final
    /// group. Each word capitalised.
    pub fn cardinal(n: u32) -> String {
        const UNITS: [&str; 20] = [
            "Zero",
            "One",
            "Two",
            "Three",
            "Four",
            "Five",
            "Six",
            "Seven",
            "Eight",
            "Nine",
            "Ten",
            "Eleven",
            "Twelve",
            "Thirteen",
            "Fourteen",
            "Fifteen",
            "Sixteen",
            "Seventeen",
            "Eighteen",
            "Nineteen",
        ];
        const TENS: [&str; 10] = [
            "", "", "Twenty", "Thirty", "Forty", "Fifty", "Sixty", "Seventy", "Eighty", "Ninety",
        ];
        /// Groups of a thousand, smallest first. `u32::MAX` needs three.
        const SCALES: [&str; 4] = ["", "Thousand", "Million", "Billion"];

        /// 1..=999 — never called with 0, so it never emits a stray "Zero".
        fn under_thousand(n: u32) -> String {
            match n {
                0 => String::new(),
                1..=19 => UNITS[n as usize].to_string(),
                20..=99 => {
                    let (tens, unit) = (TENS[(n / 10) as usize], n % 10);
                    if unit == 0 {
                        tens.to_string()
                    } else {
                        format!("{tens}-{}", UNITS[unit as usize])
                    }
                }
                _ => {
                    let (hundreds, rest) = (UNITS[(n / 100) as usize], n % 100);
                    if rest == 0 {
                        format!("{hundreds} Hundred")
                    } else {
                        format!("{hundreds} Hundred {}", under_thousand(rest))
                    }
                }
            }
        }

        if n == 0 {
            return UNITS[0].to_string();
        }

        // Split into thousand-groups, then emit largest-first.
        let mut groups = Vec::new();
        let mut rest = n;
        while rest > 0 {
            groups.push(rest % 1000);
            rest /= 1000;
        }
        let mut words = Vec::new();
        for (i, group) in groups.iter().enumerate().rev() {
            if *group == 0 {
                continue;
            }
            let scale = SCALES[i];
            words.push(if scale.is_empty() {
                under_thousand(*group)
            } else {
                format!("{} {scale}", under_thousand(*group))
            });
        }
        words.join(" ")
    }

    /// §17.9.27 `ordinalText`: `21` → "Twenty-First".
    ///
    /// Only the **final word** takes the ordinal form — "One Thousand Two
    /// Hundred Thirty-Four" becomes "…Thirty-Fourth", not "First Thousandth …"
    /// — so this spells the cardinal and rewrites its last word. The separator
    /// before that word (space or hyphen) is preserved exactly.
    pub fn ordinal_words(n: u32) -> String {
        let cardinal = cardinal(n);
        // ASCII throughout, so a byte index from `rfind` is a char boundary.
        match cardinal.rfind([' ', '-']) {
            Some(i) => format!("{}{}", &cardinal[..=i], ordinal_word(&cardinal[i + 1..])),
            None => ordinal_word(&cardinal),
        }
    }

    /// The ordinal form of one English number word.
    ///
    /// The irregulars are listed; everything else — "Four", "Six", "Seven",
    /// "Ten", the teens, and the scale words
    /// "Hundred"/"Thousand"/"Million"/"Billion" — takes a plain `th`.
    fn ordinal_word(word: &str) -> String {
        match word {
            "One" => "First",
            "Two" => "Second",
            "Three" => "Third",
            "Five" => "Fifth",
            "Eight" => "Eighth",
            "Nine" => "Ninth",
            "Twelve" => "Twelfth",
            "Twenty" => "Twentieth",
            "Thirty" => "Thirtieth",
            "Forty" => "Fortieth",
            "Fifty" => "Fiftieth",
            "Sixty" => "Sixtieth",
            "Seventy" => "Seventieth",
            "Eighty" => "Eightieth",
            "Ninety" => "Ninetieth",
            other => return format!("{other}th"),
        }
        .to_string()
    }

    pub fn ordinal_numeric(n: u32) -> String {
        let suffix = match n % 100 {
            11..=13 => "th",
            _ => match n % 10 {
                1 => "st",
                2 => "nd",
                3 => "rd",
                _ => "th",
            },
        };
        format!("{n}{suffix}")
    }
}

// ── German ─────────────────────────────────────────────────────────────────

mod german {
    use super::capitalise;

    #[rustfmt::skip]
    const UNITS: [&str; 20] = [
        "null", "eins", "zwei", "drei", "vier", "fünf", "sechs", "sieben",
        "acht", "neun", "zehn", "elf", "zwölf", "dreizehn", "vierzehn",
        "fünfzehn", "sechzehn", "siebzehn", "achtzehn", "neunzehn",
    ];
    /// Indexed by the tens digit, so 0 and 1 are unreachable placeholders.
    #[rustfmt::skip]
    const TENS: [&str; 10] = [
        "", "", "zwanzig", "dreißig", "vierzig", "fünfzig",
        "sechzig", "siebzig", "achtzig", "neunzig",
    ];

    /// 1..=99. German says the unit **before** the ten, joined by "und" and
    /// written solid — 21 is *ein*undzwanzig, and the unit is "ein", not
    /// "eins": the free-standing form appears only at the very end of a
    /// number.
    fn under_hundred(n: u32) -> String {
        match n {
            0..=19 => UNITS[n as usize].to_string(),
            _ => {
                let (tens, unit) = (TENS[(n / 10) as usize], n % 10);
                if unit == 0 {
                    tens.to_string()
                } else {
                    let unit = if unit == 1 {
                        "ein"
                    } else {
                        UNITS[unit as usize]
                    };
                    format!("{unit}und{tens}")
                }
            }
        }
    }

    /// 1..=999, one word. The multiplier of "hundert" is "ein" for 1 and the
    /// plain unit otherwise — *sechs*hundert and *sieben*hundert keep their
    /// full forms, unlike sechzehn and siebzig.
    fn under_thousand(n: u32) -> String {
        let (hundreds, rest) = (n / 100, n % 100);
        if hundreds == 0 {
            return under_hundred(rest);
        }
        let multiplier = if hundreds == 1 {
            "ein"
        } else {
            UNITS[hundreds as usize]
        };
        if rest == 0 {
            format!("{multiplier}hundert")
        } else {
            format!("{multiplier}hundert{}", under_hundred(rest))
        }
    }

    /// The form a number takes when it multiplies "tausend" or a scale noun:
    /// a trailing "eins" becomes "ein". 101 000 is einhundert**ein**tausend,
    /// never einhunderteinstausend — and "eins" can only ever be the last
    /// four characters, because it is only produced by a free-standing unit.
    fn multiplier(n: u32, one: &str) -> String {
        let s = under_thousand(n);
        match s.strip_suffix("eins") {
            Some(rest) => format!("{rest}{one}"),
            None => s,
        }
    }

    /// Thousand-groups, largest first: milliards, millions, then the solid
    /// word that thousands and units make together.
    fn groups(n: u32) -> (u32, u32, u32, u32) {
        (
            n / 1_000_000_000,
            (n / 1_000_000) % 1000,
            (n / 1000) % 1000,
            n % 1000,
        )
    }

    /// §17.9.27 `cardinalText` in German.
    ///
    /// Everything below a million is **one word** — "einhundertdreiundzwanzig"
    /// — while `Million` and `Milliarde` are feminine nouns that take their
    /// own number word, a space, and a capital: "eine Million", "zwei
    /// Millionen".
    pub fn cardinal(n: u32) -> String {
        if n == 0 {
            return capitalise(UNITS[0]);
        }
        let (milliards, millions, thousands, units) = groups(n);
        let mut parts = Vec::new();
        if milliards > 0 {
            let noun = if milliards == 1 {
                "Milliarde"
            } else {
                "Milliarden"
            };
            parts.push(format!("{} {noun}", multiplier(milliards, "eine")));
        }
        if millions > 0 {
            let noun = if millions == 1 {
                "Million"
            } else {
                "Millionen"
            };
            parts.push(format!("{} {noun}", multiplier(millions, "eine")));
        }
        let mut word = String::new();
        if thousands > 0 {
            word.push_str(&multiplier(thousands, "ein"));
            word.push_str("tausend");
        }
        if units > 0 {
            word.push_str(&under_thousand(units));
        }
        if !word.is_empty() {
            parts.push(word);
        }
        capitalise(&parts.join(" "))
    }

    /// The number as one lowercase word with **singular** scale nouns and no
    /// spaces — the form a German ordinal is built from, since an ordinal is
    /// always written solid: 1 000 000. is *einmillionste*, not "eine
    /// Millionste".
    fn compound(n: u32) -> String {
        if n == 0 {
            return UNITS[0].to_string();
        }
        let (milliards, millions, thousands, units) = groups(n);
        let mut s = String::new();
        if milliards > 0 {
            s.push_str(&multiplier(milliards, "ein"));
            s.push_str("milliarde");
        }
        if millions > 0 {
            s.push_str(&multiplier(millions, "ein"));
            s.push_str("million");
        }
        if thousands > 0 {
            s.push_str(&multiplier(thousands, "ein"));
            s.push_str("tausend");
        }
        if units > 0 {
            s.push_str(&under_thousand(units));
        }
        s
    }

    /// The ordinal endings, **longest match first**: the teens must be tried
    /// before "zehn", which they all end in, or `dreizehn` would come out as
    /// "dreizehnte" by way of the wrong rule and `neunzehn` as "neunzehnte" by
    /// luck. Anything that matches none of these — every ten from twenty up,
    /// and every number ending in hundert, tausend, million or milliarde —
    /// takes "-ste".
    #[rustfmt::skip]
    const ORDINAL_ENDINGS: [(&str, &str); 21] = [
        ("dreizehn", "dreizehnte"), ("vierzehn", "vierzehnte"),
        ("fünfzehn", "fünfzehnte"), ("sechzehn", "sechzehnte"),
        ("siebzehn", "siebzehnte"), ("achtzehn", "achtzehnte"),
        ("neunzehn", "neunzehnte"),
        ("zwölf", "zwölfte"), ("elf", "elfte"), ("zehn", "zehnte"),
        ("neun", "neunte"), ("acht", "achte"), ("sieben", "siebte"),
        ("sechs", "sechste"), ("fünf", "fünfte"), ("vier", "vierte"),
        ("drei", "dritte"), ("zwei", "zweite"), ("eins", "erste"),
        ("null", "nullte"),
        // Never matched by the loop — the "-ste" default covers it — but
        // listed so the table reads as the whole rule rather than most of it.
        ("hundert", "hundertste"),
    ];

    /// §17.9.27 `ordinalText` in German.
    ///
    /// Only the number's **final element** inflects, and it is the element
    /// rather than the last word: 101. is einhundert*erste*. Numbers under 20
    /// take "-te" with a handful of irregulars (erste, dritte, siebte, achte);
    /// everything from twenty up takes "-ste".
    pub fn ordinal_words(n: u32) -> String {
        // Zero has no ordinal in any of these languages; German at least has
        // a regular form for it, and it is in the table above.
        let base = compound(n);
        for (ending, ordinal) in ORDINAL_ENDINGS {
            if let Some(rest) = base.strip_suffix(ending) {
                return capitalise(&format!("{rest}{ordinal}"));
            }
        }
        capitalise(&format!("{base}ste"))
    }
}

// ── French ─────────────────────────────────────────────────────────────────

mod french {
    use super::capitalise;

    #[rustfmt::skip]
    const UNITS: [&str; 17] = [
        "zéro", "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit",
        "neuf", "dix", "onze", "douze", "treize", "quatorze", "quinze", "seize",
    ];
    /// Indexed by the tens digit and only up to 6 — French has no word for
    /// seventy, eighty or ninety; it counts them in twenties.
    #[rustfmt::skip]
    const TENS: [&str; 7] = [
        "", "", "vingt", "trente", "quarante", "cinquante", "soixante",
    ];

    /// 0..=99, where the four irregular decades are the whole difficulty.
    ///
    /// * 17–19 are *dix-sept*, built on ten rather than named.
    /// * 70–79 continue sixty: *soixante-dix*, *soixante et onze*.
    /// * 80–89 are four twenties: *quatre-vingts*, and the final **s** drops
    ///   the moment anything follows — *quatre-vingt-un*.
    /// * 90–99 continue eighty the way seventy continues sixty.
    /// * The unit 1 is joined with "et" at 21…61 and 71, but **not** at 81 or
    ///   91, where the vingt is a multiplicand rather than a decade name.
    fn under_hundred(n: u32) -> String {
        match n {
            0..=16 => UNITS[n as usize].to_string(),
            17..=19 => format!("dix-{}", UNITS[(n - 10) as usize]),
            20..=69 => {
                let (tens, unit) = (TENS[(n / 10) as usize], n % 10);
                match unit {
                    0 => tens.to_string(),
                    1 => format!("{tens} et un"),
                    _ => format!("{tens}-{}", UNITS[unit as usize]),
                }
            }
            70..=79 => {
                if n == 71 {
                    "soixante et onze".to_string()
                } else {
                    format!("soixante-{}", under_hundred(n - 60))
                }
            }
            80..=89 => {
                if n == 80 {
                    "quatre-vingts".to_string()
                } else {
                    format!("quatre-vingt-{}", UNITS[(n - 80) as usize])
                }
            }
            _ => format!("quatre-vingt-{}", under_hundred(n - 80)),
        }
    }

    /// 0..=999. *cent* takes an **s** when it is multiplied and nothing
    /// follows it — "deux cents", but "deux cent un".
    fn under_thousand(n: u32) -> String {
        let (hundreds, rest) = (n / 100, n % 100);
        match (hundreds, rest) {
            (0, _) => under_hundred(rest),
            (1, 0) => "cent".to_string(),
            (1, _) => format!("cent {}", under_hundred(rest)),
            (_, 0) => format!("{} cents", UNITS[hundreds as usize]),
            _ => format!("{} cent {}", UNITS[hundreds as usize], under_hundred(rest)),
        }
    }

    /// The form used before *mille*, which is a number word rather than a
    /// noun: a trailing *cents* or *vingts* loses its **s**. Before *million*
    /// and *milliard*, which are nouns, it keeps it — "deux cents millions".
    fn before_mille(n: u32) -> String {
        let s = under_thousand(n);
        for plural in ["cents", "vingts"] {
            if let Some(rest) = s.strip_suffix(plural) {
                return format!("{rest}{}", &plural[..plural.len() - 1]);
            }
        }
        s
    }

    fn parts(n: u32) -> Vec<String> {
        let (milliards, millions, thousands, units) = (
            n / 1_000_000_000,
            (n / 1_000_000) % 1000,
            (n / 1000) % 1000,
            n % 1000,
        );
        let mut parts = Vec::new();
        if milliards > 0 {
            let noun = if milliards == 1 {
                "milliard"
            } else {
                "milliards"
            };
            parts.push(format!("{} {noun}", under_thousand(milliards)));
        }
        if millions > 0 {
            let noun = if millions == 1 { "million" } else { "millions" };
            parts.push(format!("{} {noun}", under_thousand(millions)));
        }
        if thousands == 1 {
            // *mille* is invariable and takes no "un" before it.
            parts.push("mille".to_string());
        } else if thousands > 0 {
            parts.push(format!("{} mille", before_mille(thousands)));
        }
        if units > 0 {
            parts.push(under_thousand(units));
        }
        parts
    }

    /// §17.9.27 `cardinalText` in French.
    pub fn cardinal(n: u32) -> String {
        if n == 0 {
            return capitalise(UNITS[0]);
        }
        capitalise(&parts(n).join(" "))
    }

    /// §17.9.27 `ordinalText` in French: the cardinal plus **-ième**, on the
    /// number as a whole rather than on its last element.
    ///
    /// `un` alone is the exception — *premier*, not *unième* — but `vingt et
    /// un` is *vingt et unième*, so the exception is the number 1 and not the
    /// word. The rest is spelling: a final mute **e** is dropped
    /// (quatre → quatrième), `cinq` gains a **u**, `neuf` turns its **f** to
    /// **v**, and a plural **s** on a multiple is dropped along with it
    /// (quatre-vingts → quatre-vingtième).
    pub fn ordinal_words(n: u32) -> String {
        // No language here has an ordinal of zero; the cardinal is returned
        // rather than a coined word.
        if n == 0 {
            return cardinal(0);
        }
        if n == 1 {
            return "Premier".to_string();
        }
        let mut base = parts(n).join(" ");
        for plural in ["cents", "vingts", "millions", "milliards"] {
            if let Some(rest) = base.strip_suffix(plural) {
                base = format!("{rest}{}", &plural[..plural.len() - 1]);
                break;
            }
        }
        let stem = if let Some(rest) = base.strip_suffix('e') {
            rest.to_string()
        } else if let Some(rest) = base.strip_suffix('q') {
            format!("{rest}qu")
        } else if let Some(rest) = base.strip_suffix('f') {
            format!("{rest}v")
        } else {
            base
        };
        capitalise(&format!("{stem}ième"))
    }

    /// §17.9.27 `ordinal` in French: `1er` for the first, `2e` thereafter.
    /// The feminine `1re` needs a noun to agree with, which a list label has
    /// none of.
    pub fn ordinal_numeric(n: u32) -> String {
        if n == 1 {
            "1er".to_string()
        } else {
            format!("{n}e")
        }
    }
}

// ── Spanish ────────────────────────────────────────────────────────────────

mod spanish {
    use super::capitalise;

    /// 0..=29 named outright: Spanish writes the twenties solid and
    /// contracted — *veintiuno*, *veintidós* — and only from thirty on does it
    /// separate the ten from the unit with *y*.
    #[rustfmt::skip]
    const UNITS: [&str; 30] = [
        "cero", "uno", "dos", "tres", "cuatro", "cinco", "seis", "siete",
        "ocho", "nueve", "diez", "once", "doce", "trece", "catorce", "quince",
        "dieciséis", "diecisiete", "dieciocho", "diecinueve", "veinte",
        "veintiuno", "veintidós", "veintitrés", "veinticuatro", "veinticinco",
        "veintiséis", "veintisiete", "veintiocho", "veintinueve",
    ];
    #[rustfmt::skip]
    const TENS: [&str; 10] = [
        "", "", "", "treinta", "cuarenta", "cincuenta",
        "sesenta", "setenta", "ochenta", "noventa",
    ];
    /// The hundreds are named, not multiplied — *quinientos*, not
    /// "cincocientos". Index 1 is unused: 100 alone is *cien*.
    #[rustfmt::skip]
    const HUNDREDS: [&str; 10] = [
        "", "", "doscientos", "trescientos", "cuatrocientos", "quinientos",
        "seiscientos", "setecientos", "ochocientos", "novecientos",
    ];

    fn under_hundred(n: u32) -> String {
        match n {
            0..=29 => UNITS[n as usize].to_string(),
            _ => {
                let (tens, unit) = (TENS[(n / 10) as usize], n % 10);
                if unit == 0 {
                    tens.to_string()
                } else {
                    format!("{tens} y {}", UNITS[unit as usize])
                }
            }
        }
    }

    /// 0..=999. *cien* becomes *ciento* the moment anything follows it, which
    /// is the split this function exists to get right.
    fn under_thousand(n: u32) -> String {
        let (hundreds, rest) = (n / 100, n % 100);
        match (hundreds, rest) {
            (0, _) => under_hundred(rest),
            (1, 0) => "cien".to_string(),
            (1, _) => format!("ciento {}", under_hundred(rest)),
            (_, 0) => HUNDREDS[hundreds as usize].to_string(),
            _ => format!("{} {}", HUNDREDS[hundreds as usize], under_hundred(rest)),
        }
    }

    /// The apocopated form used before a noun — *un*, *veintiún* — which is
    /// what a multiplier of *mil* or *millones* is.
    fn apocopated(s: String) -> String {
        if let Some(rest) = s.strip_suffix("veintiuno") {
            format!("{rest}veintiún")
        } else if let Some(rest) = s.strip_suffix("uno") {
            format!("{rest}un")
        } else {
            s
        }
    }

    /// 1..=999_999: thousands and units. *mil* is invariable and takes no
    /// *uno* before it.
    fn under_million(n: u32) -> String {
        let (thousands, units) = (n / 1000, n % 1000);
        let mut parts = Vec::new();
        if thousands == 1 {
            parts.push("mil".to_string());
        } else if thousands > 0 {
            parts.push(format!("{} mil", apocopated(under_thousand(thousands))));
        }
        if units > 0 {
            parts.push(under_thousand(units));
        }
        parts.join(" ")
    }

    /// §17.9.27 `cardinalText` in Spanish.
    ///
    /// Spanish is **long scale**: 10⁹ has no word of its own and is written
    /// *mil millones*, so the millions group runs to four digits rather than
    /// three. `u32::MAX` is "cuatro mil doscientos noventa y cuatro
    /// millones …".
    pub fn cardinal(n: u32) -> String {
        if n == 0 {
            return capitalise(UNITS[0]);
        }
        let (millions, rest) = (n / 1_000_000, n % 1_000_000);
        let mut parts = Vec::new();
        if millions == 1 {
            parts.push("un millón".to_string());
        } else if millions > 0 {
            parts.push(format!("{} millones", apocopated(under_million(millions))));
        }
        if rest > 0 {
            parts.push(under_million(rest));
        }
        capitalise(&parts.join(" "))
    }

    // Spanish ordinals are Latin-derived and share almost nothing with the
    // cardinals, so they are their own tables rather than a suffix rule.
    #[rustfmt::skip]
    const ORD_UNITS: [&str; 10] = [
        "", "primero", "segundo", "tercero", "cuarto", "quinto",
        "sexto", "séptimo", "octavo", "noveno",
    ];
    #[rustfmt::skip]
    const ORD_TEENS: [&str; 10] = [
        "décimo", "undécimo", "duodécimo", "decimotercero", "decimocuarto",
        "decimoquinto", "decimosexto", "decimoséptimo", "decimoctavo",
        "decimonoveno",
    ];
    #[rustfmt::skip]
    const ORD_TENS: [&str; 10] = [
        "", "", "vigésimo", "trigésimo", "cuadragésimo", "quincuagésimo",
        "sexagésimo", "septuagésimo", "octogésimo", "nonagésimo",
    ];
    #[rustfmt::skip]
    const ORD_HUNDREDS: [&str; 10] = [
        "", "centésimo", "ducentésimo", "tricentésimo", "cuadringentésimo",
        "quingentésimo", "sexcentésimo", "septingentésimo", "octingentésimo",
        "noningentésimo",
    ];

    fn under_hundred_ordinal(n: u32) -> String {
        match n {
            1..=9 => ORD_UNITS[n as usize].to_string(),
            10..=19 => ORD_TEENS[(n - 10) as usize].to_string(),
            _ => {
                let (tens, unit) = (ORD_TENS[(n / 10) as usize], n % 10);
                if unit == 0 {
                    tens.to_string()
                } else {
                    format!("{tens} {}", ORD_UNITS[unit as usize])
                }
            }
        }
    }

    /// §17.9.27 `ordinalText` in Spanish.
    ///
    /// Each magnitude names itself and they are simply juxtaposed —
    /// *centésimo vigésimo primero* for 121. Above a thousand the multiplier
    /// is written separately (*dos milésimo*); the RAE writes those solid
    /// (*dosmilésimo*), and the spaced form is used here because it stays
    /// legible at the sizes `u32` admits and no rule this engine can check
    /// settles which Word emits.
    pub fn ordinal_words(n: u32) -> String {
        // Zero has no ordinal; the cardinal stands in.
        if n == 0 {
            return cardinal(0);
        }
        let mut parts = Vec::new();
        let (millions, mut rest) = (n / 1_000_000, n % 1_000_000);
        if millions == 1 {
            parts.push("millonésimo".to_string());
        } else if millions > 0 {
            parts.push(format!(
                "{} millonésimo",
                apocopated(under_million(millions))
            ));
        }
        let thousands = rest / 1000;
        rest %= 1000;
        if thousands == 1 {
            parts.push("milésimo".to_string());
        } else if thousands > 0 {
            parts.push(format!(
                "{} milésimo",
                apocopated(under_thousand(thousands))
            ));
        }
        let hundreds = rest / 100;
        rest %= 100;
        if hundreds > 0 {
            parts.push(ORD_HUNDREDS[hundreds as usize].to_string());
        }
        if rest > 0 {
            parts.push(under_hundred_ordinal(rest));
        }
        capitalise(&parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn de(n: u32) -> String {
        cardinal(n, Locale::German).unwrap()
    }
    fn fr(n: u32) -> String {
        cardinal(n, Locale::French).unwrap()
    }
    fn es(n: u32) -> String {
        cardinal(n, Locale::Spanish).unwrap()
    }

    // ── the dispatch itself ──────────────────────────────────────────────

    /// The point of returning `Option`: "we do not spell this language" is a
    /// value, so the digit fallback lives at one site rather than at each
    /// format.
    #[test]
    fn a_language_without_number_words_answers_none_for_all_three_formats() {
        for locale in [Locale::CommaDecimal, Locale::PointDecimal] {
            assert_eq!(cardinal(3, locale), None, "{locale:?}");
            assert_eq!(ordinal_words(3, locale), None, "{locale:?}");
            assert_eq!(ordinal_numeric(3, locale), None, "{locale:?}");
        }
    }

    /// An unrecognised tag keeps answering exactly as English does — the
    /// reason it is a separate variant at all.
    #[test]
    fn an_unrecognised_tag_spells_english() {
        assert_eq!(
            cardinal(21, Locale::Unrecognised),
            cardinal(21, Locale::English),
        );
        assert_eq!(
            ordinal_words(21, Locale::Unrecognised),
            ordinal_words(21, Locale::English),
        );
    }

    // ── English (moved from numbering.rs, behaviour unchanged) ───────────

    #[test]
    fn cardinal_text_spells_each_decade_boundary() {
        for (n, want) in [
            (0, "Zero"),
            (1, "One"),
            (12, "Twelve"),
            (19, "Nineteen"),
            (20, "Twenty"),
            (21, "Twenty-One"),
            (99, "Ninety-Nine"),
            (100, "One Hundred"),
            (101, "One Hundred One"),
            (115, "One Hundred Fifteen"),
            (999, "Nine Hundred Ninety-Nine"),
        ] {
            assert_eq!(english::cardinal(n), want, "{n}");
        }
    }

    /// A zero group is skipped rather than spelled: 1,000,007 has no
    /// "Thousand" in it at all. Getting this wrong yields "One Million Zero
    /// Thousand …".
    #[test]
    fn cardinal_text_skips_empty_scale_groups() {
        assert_eq!(english::cardinal(1_000), "One Thousand");
        assert_eq!(english::cardinal(1_000_007), "One Million Seven");
        assert_eq!(
            english::cardinal(1_234_567),
            "One Million Two Hundred Thirty-Four Thousand Five Hundred Sixty-Seven",
        );
    }

    #[test]
    fn cardinal_text_spells_the_whole_u32_range() {
        assert_eq!(
            english::cardinal(u32::MAX),
            "Four Billion Two Hundred Ninety-Four Million Nine Hundred Sixty-Seven \
             Thousand Two Hundred Ninety-Five",
        );
    }

    #[test]
    fn ordinal_text_rewrites_only_the_final_word() {
        for (n, want) in [
            (1, "First"),
            (2, "Second"),
            (3, "Third"),
            (4, "Fourth"),
            (5, "Fifth"),
            (8, "Eighth"),
            (9, "Ninth"),
            (12, "Twelfth"),
            (13, "Thirteenth"),
            (20, "Twentieth"),
            (21, "Twenty-First"),
            (40, "Fortieth"),
            (100, "One Hundredth"),
            (101, "One Hundred First"),
            (1_000, "One Thousandth"),
            (1_021, "One Thousand Twenty-First"),
        ] {
            assert_eq!(english::ordinal_words(n), want, "{n}");
        }
    }

    // ── German ───────────────────────────────────────────────────────────

    #[test]
    fn german_spells_each_decade_boundary() {
        for (n, want) in [
            (0, "Null"),
            (1, "Eins"),
            (6, "Sechs"),
            (7, "Sieben"),
            (12, "Zwölf"),
            (16, "Sechzehn"),
            (17, "Siebzehn"),
            (19, "Neunzehn"),
            (20, "Zwanzig"),
            (30, "Dreißig"),
            (60, "Sechzig"),
            (70, "Siebzig"),
            (99, "Neunundneunzig"),
        ] {
            assert_eq!(de(n), want, "{n}");
        }
    }

    /// The rule that makes German different from every other language here:
    /// the unit is said **first**, joined with "und", and the whole thing is
    /// one word. And 1 is "ein" in that position, never "eins".
    #[test]
    fn german_says_the_unit_before_the_ten() {
        assert_eq!(de(21), "Einundzwanzig");
        assert_eq!(de(22), "Zweiundzwanzig");
        assert_eq!(de(45), "Fünfundvierzig");
        assert_eq!(de(97), "Siebenundneunzig");
    }

    #[test]
    fn german_hundreds_and_thousands_stay_one_word() {
        for (n, want) in [
            (100, "Einhundert"),
            (101, "Einhunderteins"),
            (121, "Einhunderteinundzwanzig"),
            (600, "Sechshundert"),
            (700, "Siebenhundert"),
            (1_000, "Eintausend"),
            (2_000, "Zweitausend"),
            (21_000, "Einundzwanzigtausend"),
            (
                999_999,
                "Neunhundertneunundneunzigtausendneunhundertneunundneunzig",
            ),
        ] {
            assert_eq!(de(n), want, "{n}");
        }
    }

    /// 101 000 is einhundert**ein**tausend — the multiplier of a scale word
    /// drops the "s" of "eins". Getting this wrong gives
    /// "einhunderteinstausend".
    #[test]
    fn a_german_scale_multiplier_ending_in_one_loses_its_s() {
        assert_eq!(de(101_000), "Einhunderteintausend");
        assert_eq!(de(1_000_000), "Eine Million");
        assert_eq!(de(101_000_000), "Einhunderteine Millionen");
    }

    /// `Million` and `Milliarde` are nouns: their own word, a space, a
    /// capital, and a feminine "eine" rather than "ein".
    #[test]
    fn german_scale_nouns_are_separate_capitalised_words() {
        assert_eq!(de(1_000_000), "Eine Million");
        assert_eq!(de(2_000_000), "Zwei Millionen");
        assert_eq!(de(1_000_007), "Eine Million sieben");
        assert_eq!(de(1_000_000_000), "Eine Milliarde");
        assert_eq!(de(2_000_000_000), "Zwei Milliarden");
    }

    #[test]
    fn german_spells_the_whole_u32_range() {
        assert_eq!(
            de(u32::MAX),
            "Vier Milliarden zweihundertvierundneunzig Millionen \
             neunhundertsiebenundsechzigtausendzweihundertfünfundneunzig",
        );
    }

    /// Under twenty the ending is "-te" with four irregulars; from twenty up
    /// it is "-ste"; and it is the number's final *element* that inflects, so
    /// 101. is einhundert**erste**.
    #[test]
    fn german_ordinals_inflect_their_final_element() {
        for (n, want) in [
            (1, "Erste"),
            (2, "Zweite"),
            (3, "Dritte"),
            (4, "Vierte"),
            (7, "Siebte"),
            (8, "Achte"),
            (12, "Zwölfte"),
            (13, "Dreizehnte"),
            (19, "Neunzehnte"),
            (20, "Zwanzigste"),
            (21, "Einundzwanzigste"),
            (100, "Einhundertste"),
            (101, "Einhunderterste"),
            (103, "Einhundertdritte"),
            (1_000, "Eintausendste"),
            (1_000_000, "Einmillionste"),
        ] {
            assert_eq!(ordinal_words(n, Locale::German).unwrap(), want, "{n}");
        }
    }

    #[test]
    fn german_numeric_ordinals_are_a_digit_and_a_full_stop() {
        assert_eq!(ordinal_numeric(1, Locale::German).unwrap(), "1.");
        assert_eq!(ordinal_numeric(23, Locale::German).unwrap(), "23.");
    }

    // ── French ───────────────────────────────────────────────────────────

    #[test]
    fn french_spells_each_decade_boundary() {
        for (n, want) in [
            (0, "Zéro"),
            (1, "Un"),
            (16, "Seize"),
            (17, "Dix-sept"),
            (19, "Dix-neuf"),
            (20, "Vingt"),
            (30, "Trente"),
            (60, "Soixante"),
        ] {
            assert_eq!(fr(n), want, "{n}");
        }
    }

    /// The vigesimal decades, which is where French spelling goes wrong if it
    /// is going to: seventy counts on from sixty, ninety from eighty, and
    /// eighty carries an **s** only when nothing follows it.
    #[test]
    fn french_counts_seventy_to_ninety_nine_in_twenties() {
        for (n, want) in [
            (70, "Soixante-dix"),
            (71, "Soixante et onze"),
            (72, "Soixante-douze"),
            (77, "Soixante-dix-sept"),
            (79, "Soixante-dix-neuf"),
            (80, "Quatre-vingts"),
            (81, "Quatre-vingt-un"),
            (89, "Quatre-vingt-neuf"),
            (90, "Quatre-vingt-dix"),
            (91, "Quatre-vingt-onze"),
            (97, "Quatre-vingt-dix-sept"),
            (99, "Quatre-vingt-dix-neuf"),
        ] {
            assert_eq!(fr(n), want, "{n}");
        }
    }

    /// "et" joins the unit 1 to a *named* decade, and only to a named one —
    /// eighty and ninety are multiples of twenty, not decades, so they take a
    /// hyphen.
    #[test]
    fn french_joins_a_trailing_one_with_et_except_after_a_score() {
        for (n, want) in [
            (21, "Vingt et un"),
            (31, "Trente et un"),
            (41, "Quarante et un"),
            (51, "Cinquante et un"),
            (61, "Soixante et un"),
            (71, "Soixante et onze"),
            (81, "Quatre-vingt-un"),
            (91, "Quatre-vingt-onze"),
        ] {
            assert_eq!(fr(n), want, "{n}");
        }
    }

    /// *cent* pluralises when multiplied and final; *mille* never pluralises
    /// and takes no "un"; and a *cents* before *mille* loses its s because
    /// *mille* is a number word rather than a noun.
    #[test]
    fn french_pluralises_cent_but_never_mille() {
        for (n, want) in [
            (100, "Cent"),
            (101, "Cent un"),
            (200, "Deux cents"),
            (201, "Deux cent un"),
            (1_000, "Mille"),
            (1_001, "Mille un"),
            (2_000, "Deux mille"),
            (200_000, "Deux cent mille"),
            (80_000, "Quatre-vingt mille"),
            (1_000_000, "Un million"),
            (2_000_000, "Deux millions"),
            (200_000_000, "Deux cents millions"),
        ] {
            assert_eq!(fr(n), want, "{n}");
        }
    }

    #[test]
    fn french_spells_the_whole_u32_range() {
        assert_eq!(
            fr(u32::MAX),
            "Quatre milliards deux cent quatre-vingt-quatorze millions \
             neuf cent soixante-sept mille deux cent quatre-vingt-quinze",
        );
    }

    #[test]
    fn french_ordinals_add_ieme_to_the_whole_number() {
        for (n, want) in [
            (1, "Premier"),
            (2, "Deuxième"),
            (4, "Quatrième"),
            (5, "Cinquième"),
            (9, "Neuvième"),
            (11, "Onzième"),
            (20, "Vingtième"),
            (21, "Vingt et unième"),
            (80, "Quatre-vingtième"),
            (100, "Centième"),
            (200, "Deux centième"),
            (1_000, "Millième"),
            (1_000_000, "Un millionième"),
        ] {
            assert_eq!(ordinal_words(n, Locale::French).unwrap(), want, "{n}");
        }
    }

    #[test]
    fn french_numeric_ordinals_mark_only_the_first() {
        assert_eq!(ordinal_numeric(1, Locale::French).unwrap(), "1er");
        assert_eq!(ordinal_numeric(2, Locale::French).unwrap(), "2e");
        assert_eq!(ordinal_numeric(21, Locale::French).unwrap(), "21e");
    }

    // ── Spanish ──────────────────────────────────────────────────────────

    #[test]
    fn spanish_spells_each_decade_boundary() {
        for (n, want) in [
            (0, "Cero"),
            (1, "Uno"),
            (15, "Quince"),
            (16, "Dieciséis"),
            (19, "Diecinueve"),
            (20, "Veinte"),
            (30, "Treinta"),
            (90, "Noventa"),
        ] {
            assert_eq!(es(n), want, "{n}");
        }
    }

    /// The twenties contract into one word and take an accent; from thirty on
    /// the ten and the unit are separate, joined by *y*.
    #[test]
    fn spanish_contracts_the_twenties_and_separates_the_rest() {
        for (n, want) in [
            (21, "Veintiuno"),
            (22, "Veintidós"),
            (26, "Veintiséis"),
            (29, "Veintinueve"),
            (31, "Treinta y uno"),
            (45, "Cuarenta y cinco"),
            (99, "Noventa y nueve"),
        ] {
            assert_eq!(es(n), want, "{n}");
        }
    }

    /// *cien* becomes *ciento* the moment anything follows, and the hundreds
    /// are named rather than multiplied — *quinientos*, not "cincocientos".
    #[test]
    fn spanish_splits_cien_from_ciento() {
        for (n, want) in [
            (100, "Cien"),
            (101, "Ciento uno"),
            (150, "Ciento cincuenta"),
            (200, "Doscientos"),
            (500, "Quinientos"),
            (700, "Setecientos"),
            (900, "Novecientos"),
        ] {
            assert_eq!(es(n), want, "{n}");
        }
    }

    /// *mil* takes no *uno*, and a multiplier that ends in *uno* apocopates
    /// before it: *veintiún mil*, not "veintiuno mil".
    #[test]
    fn spanish_apocopates_a_multiplier_before_mil() {
        for (n, want) in [
            (1_000, "Mil"),
            (1_001, "Mil uno"),
            (2_000, "Dos mil"),
            (21_000, "Veintiún mil"),
            (101_000, "Ciento un mil"),
            (1_000_000, "Un millón"),
            (2_000_000, "Dos millones"),
        ] {
            assert_eq!(es(n), want, "{n}");
        }
    }

    /// Spanish is long-scale: there is no word for 10⁹, so it is *mil
    /// millones* and the millions group runs to four digits.
    #[test]
    fn spanish_spells_the_whole_u32_range() {
        assert_eq!(es(1_000_000_000), "Mil millones");
        assert_eq!(
            es(u32::MAX),
            "Cuatro mil doscientos noventa y cuatro millones \
             novecientos sesenta y siete mil doscientos noventa y cinco",
        );
    }

    #[test]
    fn spanish_ordinals_use_their_own_latin_words() {
        for (n, want) in [
            (1, "Primero"),
            (2, "Segundo"),
            (3, "Tercero"),
            (7, "Séptimo"),
            (10, "Décimo"),
            (11, "Undécimo"),
            (13, "Decimotercero"),
            (18, "Decimoctavo"),
            (20, "Vigésimo"),
            (21, "Vigésimo primero"),
            (40, "Cuadragésimo"),
            (100, "Centésimo"),
            (101, "Centésimo primero"),
            (500, "Quingentésimo"),
            (1_000, "Milésimo"),
            (2_000, "Dos milésimo"),
            (1_000_000, "Millonésimo"),
        ] {
            assert_eq!(ordinal_words(n, Locale::Spanish).unwrap(), want, "{n}");
        }
    }

    #[test]
    fn spanish_numeric_ordinals_use_the_masculine_indicator() {
        assert_eq!(ordinal_numeric(1, Locale::Spanish).unwrap(), "1.º");
        assert_eq!(ordinal_numeric(23, Locale::Spanish).unwrap(), "23.º");
    }

    // ── every language, every value ──────────────────────────────────────

    /// No input in the whole `u32` range may panic, index out of bounds, or
    /// come back empty — an empty label is a list item with no number on it.
    /// Sampled rather than exhaustive, but across every carry boundary each
    /// speller has.
    #[test]
    fn no_language_leaves_any_counter_unspelled() {
        let mut cases: Vec<u32> = (0..1_100).collect();
        for scale in [1_000u64, 1_000_000, 1_000_000_000] {
            for k in [1u64, 2, 9, 10, 21, 100, 101, 999] {
                // The high scales overflow `u32` for the larger multipliers;
                // those combinations simply do not exist as counters.
                cases.extend(
                    [scale * k - 1, scale * k, scale * k + 1]
                        .into_iter()
                        .filter_map(|v| u32::try_from(v).ok()),
                );
            }
        }
        cases.extend([u32::MAX, u32::MAX - 1, 4_000_000_000]);

        for locale in [
            Locale::English,
            Locale::German,
            Locale::French,
            Locale::Spanish,
        ] {
            for n in &cases {
                let c = cardinal(*n, locale).unwrap();
                assert!(!c.trim().is_empty(), "{locale:?} cardinal {n}");
                let o = ordinal_words(*n, locale).unwrap();
                assert!(!o.trim().is_empty(), "{locale:?} ordinal {n}");
                assert!(
                    !c.contains("  ") && !o.contains("  "),
                    "{locale:?} {n}: doubled space in {c:?} / {o:?}",
                );
            }
        }
    }
}

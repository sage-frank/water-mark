//! §17.16.4.2 date-time picture strings — the argument of a field's `\@` switch.
//!
//! One grammar, one parser. `DATE` and `TIME` do not have separate picture
//! languages: §17.16.4.2 defines a single date-and-time picture, and a `DATE`
//! field may legitimately ask for `MMM d, yyyy h:mm am/pm`. This module turns a
//! picture into [`PictureToken`]s once, and [`crate::field::format`] renders
//! them against whichever of the date and time the caller has.
//!
//! # Why a token list rather than a scanner
//!
//! Recognising tokens while emitting output — the shape this replaced — makes
//! every question about a token a question about a character index, and the
//! answers drift apart. Three of the four defects behind this module were that:
//!
//! * a backslash escape was never recognised at all, so `MMM\ d, yyyy` rendered
//!   `Aug\ 11, 2026` (issue #159);
//! * `ddd` once fell into the zero-padded day-of-month branch and rendered a
//!   day *number* where §17.16.4.2 asks for a day *name* (issue #129);
//! * 12-hour vs 24-hour was decided by `pattern.contains("AM/PM")` on the raw
//!   string, which a quoted `'AM/PM'` fooled.
//!
//! Parsing first makes each of these a property of the token list. The widths
//! below are enums rather than repeat counts for the same reason: "three `d` is
//! a name, not a padded number" becomes a fact the type carries instead of a
//! `count >= 3` a reader has to find.

/// One element of a parsed picture, in document order.
///
/// A token never carries a formatting *character* — by the time one of these
/// exists the decision it encodes has been made. [`PictureToken::Literal`] is
/// the only variant holding text, and it holds text that must be reproduced
/// exactly: quoted runs, escaped characters, and ordinary separators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PictureToken {
    /// `y` `yy` | `yyyy`
    Year(YearWidth),
    /// `M` `MM` | `MMM` | `MMMM`
    Month(NameWidth),
    /// `d` `dd` | `ddd` | `dddd` — note that three or more is the day of the
    /// *week*, which is why this shares [`NameWidth`] with the month.
    Day(NameWidth),
    /// `H` `HH` — always the 24-hour clock.
    Hour24(Pad),
    /// `h` `hh` — the 12-hour clock *only* when the picture also carries an
    /// [`PictureToken::AmPm`]; otherwise it reads as 24-hour, which is what
    /// Word does and what this engine did before the rewrite.
    Hour12(Pad),
    /// `m` `mm` — minutes. Lowercase; uppercase `M` is the month.
    Minute(Pad),
    /// `s` `ss`
    Second(Pad),
    /// `AM/PM` | `am/pm`
    AmPm(AmPmCase),
    /// Text reproduced verbatim.
    Literal(String),
}

/// Whether a numeric token pads to two digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pad {
    /// One character: `d` → `5`.
    None,
    /// Two or more: `dd` → `05`.
    Zero,
}

/// How a month or weekday renders — as a number, or as a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameWidth {
    /// One or two characters: the ordinal, optionally zero-padded.
    Numeric(Pad),
    /// Three characters: `Mar`, `Mon`.
    Abbreviated,
    /// Four or more: `March`, `Monday`.
    Full,
}

/// §17.16.4.2 gives the year two widths, not four.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum YearWidth {
    /// `y` or `yy` → `26`.
    TwoDigit,
    /// `yyyy` (or more) → `2026`.
    FourDigit,
}

/// The case the picture asked for, preserved rather than normalised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AmPmCase {
    /// `AM/PM`
    Upper,
    /// `am/pm`
    Lower,
}

/// Parse a picture string into tokens.
///
/// Total: every input produces a token list, because a picture has no invalid
/// form — anything the grammar does not recognise is literal text, which is
/// what Word does with it. Adjacent literals are merged, so the token list is
/// the shortest one that reproduces the input.
pub(crate) fn parse(pattern: &str) -> Vec<PictureToken> {
    let chars: Vec<char> = pattern.chars().collect();
    let len = chars.len();
    let mut tokens: Vec<PictureToken> = Vec::new();
    let mut i = 0;

    while i < len {
        // Quoted literal. An unterminated quote runs to the end of the picture
        // rather than failing: a picture has no invalid form (see above), and
        // this is what the scanner it replaced did.
        //
        // A backslash *inside* quotes stays a backslash. Quoting already makes
        // the content literal, so there is nothing for an escape to do, and
        // consuming one here would make `'a\b'` unable to express `a\b`.
        if chars[i] == '\'' {
            i += 1;
            let start = i;
            while i < len && chars[i] != '\'' {
                i += 1;
            }
            push_literal(&mut tokens, chars[start..i].iter().collect::<String>());
            if i < len {
                i += 1; // closing quote
            }
            continue;
        }

        // §17.16.4.2: a backslash escapes the character that follows, so that
        // it is taken literally rather than as a formatting code. The escape
        // itself is consumed. This has to run *before* token recognition —
        // `\d` is the letter, and must never reach the `d` arm below.
        //
        // Word reference render needed: a *trailing* backslash, with nothing
        // to escape. §17.16.4.2 defines the escape only in terms of "the
        // character that follows" and does not say what a picture ending in
        // `\` means. Consumed here, on the reading that the backslash is
        // punctuation rather than content. A render of `DATE \@ "yyyy\"`
        // would settle it: `2026` confirms this, `2026\` overturns it.
        if chars[i] == '\\' {
            i += 1;
            if i < len {
                push_literal(&mut tokens, chars[i].to_string());
                i += 1;
            }
            continue;
        }

        // AM/PM before the single-character arms: `A` is not otherwise a token,
        // but `M` is, so `AM/PM` would tokenize as literal `A`, month `M`, … if
        // this ran later. Compared on the char slice, not on byte offsets:
        // `i` is a char index, so `pattern[i..i + 5]` would misalign — and can
        // panic — after any multi-byte character earlier in the picture.
        if chars[i..].starts_with(&['A', 'M', '/', 'P', 'M']) {
            tokens.push(PictureToken::AmPm(AmPmCase::Upper));
            i += 5;
            continue;
        }
        if chars[i..].starts_with(&['a', 'm', '/', 'p', 'm']) {
            tokens.push(PictureToken::AmPm(AmPmCase::Lower));
            i += 5;
            continue;
        }

        let run = count_run(&chars, i);
        let token = match chars[i] {
            'y' => Some(PictureToken::Year(if run >= 4 {
                YearWidth::FourDigit
            } else {
                YearWidth::TwoDigit
            })),
            'M' => Some(PictureToken::Month(name_width(run))),
            'd' => Some(PictureToken::Day(name_width(run))),
            'H' => Some(PictureToken::Hour24(pad(run))),
            'h' => Some(PictureToken::Hour12(pad(run))),
            'm' => Some(PictureToken::Minute(pad(run))),
            's' => Some(PictureToken::Second(pad(run))),
            _ => None,
        };

        match token {
            Some(t) => {
                tokens.push(t);
                i += run;
            }
            // Separators — `/`, `-`, `,`, spaces, and any character the grammar
            // does not claim.
            None => {
                push_literal(&mut tokens, chars[i].to_string());
                i += 1;
            }
        }
    }

    tokens
}

/// Append text, merging into the preceding literal when there is one.
fn push_literal(tokens: &mut Vec<PictureToken>, text: String) {
    match tokens.last_mut() {
        Some(PictureToken::Literal(prev)) => prev.push_str(&text),
        _ => tokens.push(PictureToken::Literal(text)),
    }
}

/// How many times the character at `start` repeats.
fn count_run(chars: &[char], start: usize) -> usize {
    let ch = chars[start];
    chars[start..].iter().take_while(|&&c| c == ch).count()
}

fn pad(run: usize) -> Pad {
    if run >= 2 {
        Pad::Zero
    } else {
        Pad::None
    }
}

fn name_width(run: usize) -> NameWidth {
    match run {
        1 => NameWidth::Numeric(Pad::None),
        2 => NameWidth::Numeric(Pad::Zero),
        3 => NameWidth::Abbreviated,
        _ => NameWidth::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::PictureToken::*;
    use super::*;

    fn lit(s: &str) -> PictureToken {
        Literal(s.to_string())
    }

    #[test]
    fn a_plain_picture_tokenizes_by_run_length() {
        assert_eq!(
            parse("MMM d, yyyy"),
            vec![
                Month(NameWidth::Abbreviated),
                lit(" "),
                Day(NameWidth::Numeric(Pad::None)),
                lit(", "),
                Year(YearWidth::FourDigit),
            ]
        );
    }

    /// Three `d` is the day of the *week*, one or two the day of the month —
    /// the distinction issue #129 fixed by hand and this type now carries.
    #[test]
    fn run_length_picks_the_name_width() {
        assert_eq!(parse("d"), vec![Day(NameWidth::Numeric(Pad::None))]);
        assert_eq!(parse("dd"), vec![Day(NameWidth::Numeric(Pad::Zero))]);
        assert_eq!(parse("ddd"), vec![Day(NameWidth::Abbreviated)]);
        assert_eq!(parse("dddd"), vec![Day(NameWidth::Full)]);
        assert_eq!(parse("ddddd"), vec![Day(NameWidth::Full)]);
    }

    // ── §17.16.4.2 escapes (issue #159) ─────────────────────────────────────

    /// The escape is consumed and its character becomes literal text.
    #[test]
    fn an_escape_is_consumed_and_its_character_is_literal() {
        assert_eq!(
            parse(r"MMM\ d"),
            vec![
                Month(NameWidth::Abbreviated),
                lit(" "),
                Day(NameWidth::Numeric(Pad::None)),
            ],
            "issue #159 case A"
        );
        assert_eq!(
            parse(r"d \a yyyy"),
            vec![
                Day(NameWidth::Numeric(Pad::None)),
                lit(" a "),
                Year(YearWidth::FourDigit),
            ],
            "issue #159 case C — and the literals around it merged"
        );
    }

    /// The whole point of escaping before recognising: an escaped format
    /// character must not survive as a format character.
    #[test]
    fn an_escaped_format_character_is_not_a_format_character() {
        assert_eq!(parse(r"\d\d\d"), vec![lit("ddd")]);
        assert_eq!(parse(r"\y\M\d\h"), vec![lit("yMdh")]);
    }

    #[test]
    fn a_backslash_inside_quotes_stays_literal() {
        assert_eq!(parse(r"'a\b'"), vec![lit(r"a\b")]);
    }

    /// See the comment at the escape arm: consumed, and the Word render that
    /// would overturn that is named there.
    #[test]
    fn a_trailing_backslash_is_consumed() {
        assert_eq!(parse(r"yyyy\"), vec![Year(YearWidth::FourDigit)]);
        assert_eq!(parse(r"\"), vec![]);
    }

    // ── quoting ─────────────────────────────────────────────────────────────

    #[test]
    fn quoted_text_is_literal_and_not_tokenized() {
        assert_eq!(
            parse("'on' d"),
            vec![lit("on "), Day(NameWidth::Numeric(Pad::None))]
        );
    }

    #[test]
    fn an_unterminated_quote_runs_to_the_end() {
        assert_eq!(
            parse("d 'tail"),
            vec![Day(NameWidth::Numeric(Pad::None)), lit(" tail")]
        );
    }

    /// `AM/PM` inside quotes is text. The scanner this replaced decided
    /// 12-hour with `pattern.contains("AM/PM")`, which this input fooled.
    #[test]
    fn quoted_am_pm_is_literal_text() {
        assert_eq!(parse("'AM/PM'"), vec![lit("AM/PM")]);
        assert_eq!(parse("AM/PM"), vec![AmPm(AmPmCase::Upper)]);
        assert_eq!(parse("am/pm"), vec![AmPm(AmPmCase::Lower)]);
    }

    // ── the shared grammar ──────────────────────────────────────────────────

    /// `M` is the month and `m` the minute, and one picture holds both — the
    /// case the two-scanner split could not express.
    #[test]
    fn one_picture_carries_both_date_and_time_tokens() {
        assert_eq!(
            parse("MMM d h:mm"),
            vec![
                Month(NameWidth::Abbreviated),
                lit(" "),
                Day(NameWidth::Numeric(Pad::None)),
                lit(" "),
                Hour12(Pad::None),
                lit(":"),
                Minute(Pad::Zero),
            ]
        );
    }

    #[test]
    fn an_empty_picture_is_no_tokens() {
        assert_eq!(parse(""), vec![]);
    }

    /// A multi-byte literal ahead of `AM/PM` used to be a panic risk when the
    /// lookahead was done on byte offsets.
    #[test]
    fn a_multibyte_literal_before_am_pm_does_not_panic() {
        assert_eq!(parse("«h» AM/PM").last(), Some(&AmPm(AmPmCase::Upper)));
    }
}

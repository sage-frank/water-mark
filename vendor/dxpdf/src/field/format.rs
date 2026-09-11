use crate::field::context::{Date, Time};
use crate::field::picture::{self, AmPmCase, NameWidth, Pad, PictureToken, YearWidth};

/// Render a §17.16.4.2 date-time picture.
///
/// One function for both `DATE` and `TIME`, because §17.16.4.2 is one picture
/// grammar: a `DATE` field may ask for `MMM d, yyyy h:mm am/pm`, and splitting
/// this in two made each half treat the other's tokens as literal letters.
/// `crate::field::picture` does the parsing and says more about why.
///
/// Tokens:
/// - `d`, `dd` — day of month (9 vs 09); `ddd`, `dddd` — day of *week*
/// - `M`, `MM`, `MMM`, `MMMM` — month (3, 03, Mar, March)
/// - `yy`, `yyyy` — year (26, 2026)
/// - `H`, `HH` — hour, 24-clock; `h`, `hh` — hour, 12-clock when the picture
///   also carries `AM/PM`
/// - `m`, `mm` — minute; `s`, `ss` — second; `AM/PM`, `am/pm`
///
/// Literal text goes in single quotes (`'on' d MMMM`), and a backslash escapes
/// the single character after it (`MMM\ d`).
///
/// **A token whose source is absent renders as nothing**: `format_datetime`
/// takes the date and the time separately because the evaluator has them
/// separately, and a `TIME` field with no date in context still has to render
/// the rest of its picture rather than fail.
///
/// `locale_tag` is the §17.3.2.20 `w:lang` in effect where the field sits;
/// the two name-bearing tokens (`MMM`/`MMMM` and `ddd`/`dddd`) render in that
/// language. `None`, or a tag this engine has no CLDR data for, falls back to
/// the English tables below — the same discipline `Locale::decimal_separator`
/// backstops [`crate::i18n::decimal_separator_for_tag`] with.
pub fn format_datetime(
    date: Option<&Date>,
    time: Option<&Time>,
    pattern: &str,
    locale_tag: Option<&str>,
) -> String {
    let tokens = picture::parse(pattern);

    // §17.16.4.2: `h` is the 12-hour clock only when the picture says which
    // half of the day it means. A property of the token list, so a quoted
    // `'AM/PM'` — text, not a token — no longer flips it.
    let twelve_hour = tokens.iter().any(|t| matches!(t, PictureToken::AmPm(_)));

    let mut result = String::new();
    for token in &tokens {
        match token {
            PictureToken::Literal(text) => result.push_str(text),

            PictureToken::Year(width) => {
                if let Some(d) = date {
                    match width {
                        YearWidth::FourDigit => result.push_str(&format!("{:04}", d.year)),
                        YearWidth::TwoDigit => {
                            result.push_str(&format!("{:02}", d.year.rem_euclid(100)))
                        }
                    }
                }
            }
            PictureToken::Month(width) => {
                if let Some(d) = date {
                    match width {
                        NameWidth::Numeric(pad) => result.push_str(&pad_num(d.month, *pad)),
                        NameWidth::Abbreviated => {
                            result.push_str(&month_name(d, false, locale_tag))
                        }
                        NameWidth::Full => result.push_str(&month_name(d, true, locale_tag)),
                    }
                }
            }
            PictureToken::Day(width) => {
                if let Some(d) = date {
                    match width {
                        NameWidth::Numeric(pad) => result.push_str(&pad_num(d.day, *pad)),
                        NameWidth::Abbreviated => {
                            result.push_str(&weekday_name(d, false, locale_tag))
                        }
                        NameWidth::Full => result.push_str(&weekday_name(d, true, locale_tag)),
                    }
                }
            }

            PictureToken::Hour24(pad) => {
                if let Some(t) = time {
                    result.push_str(&pad_num(t.hour, *pad));
                }
            }
            PictureToken::Hour12(pad) => {
                if let Some(t) = time {
                    let hour = if twelve_hour {
                        to_12hour(t.hour).0
                    } else {
                        t.hour
                    };
                    result.push_str(&pad_num(hour, *pad));
                }
            }
            PictureToken::Minute(pad) => {
                if let Some(t) = time {
                    result.push_str(&pad_num(t.minute, *pad));
                }
            }
            PictureToken::Second(pad) => {
                if let Some(t) = time {
                    result.push_str(&pad_num(t.second, *pad));
                }
            }
            PictureToken::AmPm(case) => {
                if let Some(t) = time {
                    result.push_str(&day_period(t, *case, locale_tag));
                }
            }
        }
    }

    result
}

/// Render a numeric token at the width its run length asked for.
fn pad_num(value: u32, pad: Pad) -> String {
    match pad {
        Pad::None => value.to_string(),
        Pad::Zero => format!("{value:02}"),
    }
}

/// The `AM/PM` token, in the document's language where there is one.
///
/// **The picture chooses the case, CLDR chooses the word.** §17.16.4.2 gives
/// two spellings of this token, `AM/PM` and `am/pm`, and the difference
/// between them is case — an English distinction the token inherits from
/// Word. Applying it to CLDR's string keeps every English document rendering
/// exactly as before (`en-US` is already `AM`, so upper-casing is a no-op)
/// and gives a Spanish one `a. m.`/`A. M.` rather than a bare `AM`.
///
/// Word reference render needed: whether Word case-folds a localized day
/// period at all, or prints CLDR's own casing for both spellings. A Spanish
/// document with `\@ "h:mm AM/PM"` would settle it — `A. M.` confirms this
/// reading, `a. m.` overturns it.
fn day_period(time: &Time, case: AmPmCase, locale_tag: Option<&str>) -> String {
    let english = to_12hour(time.hour).1;
    let localized = locale_tag.and_then(|tag| {
        let icu_time = to_icu_time(time)?;
        crate::i18n::day_period_for_tag(&icu_time, tag)
    });
    let name = localized.unwrap_or_else(|| english.to_string());
    match case {
        AmPmCase::Upper => name.to_uppercase(),
        AmPmCase::Lower => name.to_lowercase(),
    }
}

/// §17.16.5.13: the locale's short date, for a `DATE` field with no `\@`
/// picture.
///
/// Word resolves this against the *system* locale; this engine reads the
/// document's `w:lang`, and falls back to `M/d/yyyy` when the document states
/// no language or states one the baked data cannot serve. The argument for
/// reading the document rather than the host is `crate::field::now`'s: a
/// converter that reads the host's regional settings renders the same document
/// differently on two machines.
///
/// That fallback is the pre-existing behaviour, kept deliberately. A
/// document that never said which language it is in has not asked for a
/// localized date, and picking one from the host would be exactly the
/// machine-dependence the paragraph above rejects.
pub fn default_date(date: &Date, locale_tag: Option<&str>) -> String {
    locale_tag
        .and_then(|tag| {
            let icu_date = to_icu_date(date)?;
            crate::i18n::short_date_for_tag(&icu_date, tag)
        })
        .unwrap_or_else(|| format_datetime(Some(date), None, "M/d/yyyy", None))
}

/// §17.16.5.76: the locale's short time, for a `TIME` field with no `\@`
/// picture. The twin of [`default_date`]; the same locale argument applies,
/// and the fallback is Word's `h:mm AM/PM`.
pub fn default_time(time: &Time, locale_tag: Option<&str>) -> String {
    locale_tag
        .and_then(|tag| {
            let icu_time = to_icu_time(time)?;
            crate::i18n::short_time_for_tag(&icu_time, tag)
        })
        .unwrap_or_else(|| format_datetime(None, Some(time), "h:mm AM/PM", None))
}

/// This engine's `Date` as ICU4X's. `None` for a date outside the Gregorian
/// range ICU accepts, which the caller answers by falling back — the same
/// shape `month_name`/`weekday_name` already use.
fn to_icu_date(date: &Date) -> Option<icu_calendar::Date<icu_calendar::Gregorian>> {
    let month = u8::try_from(date.month).ok()?;
    let day = u8::try_from(date.day).ok()?;
    icu_calendar::Date::try_new_gregorian(date.year, month, day).ok()
}

/// This engine's `Time` as ICU4X's.
fn to_icu_time(time: &Time) -> Option<icu_datetime::input::Time> {
    let hour = u8::try_from(time.hour).ok()?.try_into().ok()?;
    let minute = u8::try_from(time.minute).ok()?.try_into().ok()?;
    let second = u8::try_from(time.second).ok()?.try_into().ok()?;
    Some(icu_datetime::input::Time {
        hour,
        minute,
        second,
        subsecond: icu_datetime::input::Time::start_of_day().subsecond,
    })
}

/// Format a number using an OOXML numeric format string (§17.16.4.1).
///
/// Basic support: `0` = digit (pad with zero), `#` = digit (no pad).
pub fn format_number(value: f64, pattern: &str) -> String {
    // Find decimal point in pattern
    let parts: Vec<&str> = pattern.split('.').collect();

    if parts.len() == 2 {
        let decimal_places = parts[1].len();
        format!("{:.prec$}", value, prec = decimal_places)
    } else if pattern.contains('0') || pattern.contains('#') {
        // Integer format: round (like the decimal branch's `{:.prec$}`) rather
        // than truncate toward zero. `.round()` first also avoids a "-0" result
        // from `-0.4 as i64`.
        format!("{}", value.round() as i64)
    } else {
        value.to_string()
    }
}

/// Apply a general format switch (`\* FORMAT`) to a string value.
///
/// For `ROMAN`/`ALPHABETIC` the *case of the switch keyword* selects the output
/// case (`\* roman` → `iv`, `\* ROMAN` → `IV`), per §17.16.4.1.
pub fn apply_general_format(value: &str, format: &str) -> String {
    // Lowercase output when the keyword itself is written lowercase.
    let lowercase = format
        .chars()
        .find(|c| c.is_ascii_alphabetic())
        .is_some_and(|c| c.is_ascii_lowercase());
    match format.to_ascii_uppercase().as_str() {
        "UPPER" => value.to_ascii_uppercase(),
        "LOWER" => value.to_ascii_lowercase(),
        // §17.16.4.1: FirstCap capitalizes the first letter of the *first* word;
        // Caps capitalizes the first letter of *every* word (title case).
        "FIRSTCAP" => first_cap(value),
        "CAPS" => title_case(value),
        "MERGEFORMAT" => value.to_string(), // preserve existing formatting, no-op for text
        "ALPHABETIC" => match value.parse::<u32>() {
            Ok(n) => to_alphabetic(n, lowercase),
            Err(_) => value.to_string(),
        },
        "ROMAN" => match value.parse::<u32>() {
            Ok(n) => to_roman(n, lowercase),
            Err(_) => value.to_string(),
        },
        _ => value.to_string(),
    }
}

/// Capitalize the first letter of the first word only (`\* FirstCap`).
fn first_cap(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let mut s = c.to_uppercase().to_string();
            s.extend(chars);
            s
        }
    }
}

/// Capitalize the first letter of every word (`\* Caps` — title case). A word is
/// a maximal run of alphabetic characters; the rest of each word is left as-is.
fn title_case(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut at_word_start = true;
    for c in value.chars() {
        if c.is_alphabetic() {
            if at_word_start {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            at_word_start = false;
        } else {
            out.push(c);
            at_word_start = true;
        }
    }
    out
}

fn to_12hour(hour: u32) -> (u32, &'static str) {
    match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    }
}

/// §17.16.4.2 `MMM`/`MMMM` — the month named in `locale_tag`'s language.
///
/// Falls back to the English tables below when no tag is in effect, when the
/// tag has no baked CLDR data, or when `date` isn't a real Gregorian date
/// (a caller-supplied month of 13 has no name in any language, but must not
/// panic mid-render).
fn month_name(date: &Date, long: bool, locale_tag: Option<&str>) -> String {
    let localized = locale_tag.and_then(|tag| {
        // Day 1 is valid in every month of every year, so a month-only
        // lookup never has to care whether the caller's day is in range.
        let month = u8::try_from(date.month).ok()?;
        let icu_date = icu_calendar::Date::try_new_gregorian(date.year, month, 1).ok()?;
        crate::i18n::month_name_for_tag(&icu_date, long, tag)
    });
    localized.unwrap_or_else(|| {
        if long {
            long_month_name(date.month).to_string()
        } else {
            short_month_name(date.month).to_string()
        }
    })
}

/// §17.16.4.2 `ddd`/`dddd` — the weekday named in `locale_tag`'s language.
///
/// Unlike the month, this needs the *whole* date to exist: which weekday a
/// day falls on is calendar arithmetic, not a table lookup. An out-of-range
/// date has no weekday at all, so it renders as nothing rather than guessing
/// one — the same "no answer" the empty string already means elsewhere in
/// this module.
fn weekday_name(date: &Date, long: bool, locale_tag: Option<&str>) -> String {
    let Some(weekday) = u8::try_from(date.month)
        .ok()
        .zip(u8::try_from(date.day).ok())
        .and_then(|(month, day)| icu_calendar::Date::try_new_gregorian(date.year, month, day).ok())
        .map(|d| d.weekday())
    else {
        return String::new();
    };
    let localized =
        locale_tag.and_then(|tag| crate::i18n::weekday_name_for_tag(weekday, long, tag));
    localized.unwrap_or_else(|| {
        if long {
            long_weekday_name(weekday).to_string()
        } else {
            short_weekday_name(weekday).to_string()
        }
    })
}

/// The English `ddd` fallback, for a document that declares no language or
/// one this engine has no CLDR data for — the weekday counterpart of
/// [`short_month_name`].
fn short_weekday_name(weekday: icu_calendar::types::Weekday) -> &'static str {
    use icu_calendar::types::Weekday;
    match weekday {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

/// The English `dddd` fallback — see [`short_weekday_name`].
fn long_weekday_name(weekday: icu_calendar::types::Weekday) -> &'static str {
    use icu_calendar::types::Weekday;
    match weekday {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

/// §17.16.4.2 date-picture month name, in English — the fallback
/// [`month_name`] uses when no CLDR data applies. Not the primary path since
/// issue #129: a German document's `DATE \@ "MMMM"` renders "August" because
/// German spells it that way, not because this table is hardcoded.
fn short_month_name(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "???",
    }
}

/// §17.16.4.2 date-picture month name, in English — see `short_month_name`.
fn long_month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "???",
    }
}

fn to_roman(mut n: u32, lowercase: bool) -> String {
    const TABLE: &[(u32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut result = String::new();
    for &(value, numeral) in TABLE {
        while n >= value {
            result.push_str(numeral);
            n -= value;
        }
    }
    if lowercase {
        result.to_ascii_lowercase()
    } else {
        result
    }
}

fn to_alphabetic(n: u32, lowercase: bool) -> String {
    if n == 0 {
        return String::new();
    }
    let base = if lowercase { b'a' } else { b'A' };
    let mut result = Vec::new();
    let mut val = n - 1;
    loop {
        result.push(base + (val % 26) as u8);
        if val < 26 {
            break;
        }
        val = val / 26 - 1;
    }
    result.reverse();
    String::from_utf8(result).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A date-only picture. Deliberately *not* named `format_date`: the two
    /// separate renderers are what this module stopped having, and a helper
    /// wearing the old name would invite them back.
    fn date_picture(date: &Date, pattern: &str, locale_tag: Option<&str>) -> String {
        format_datetime(Some(date), None, pattern, locale_tag)
    }

    /// A time-only picture. See [`date_picture`].
    fn time_picture(time: &Time, pattern: &str) -> String {
        format_datetime(None, Some(time), pattern, None)
    }

    #[test]
    fn format_date_basic() {
        let date = Date {
            year: 2026,
            month: 3,
            day: 5,
        };
        assert_eq!(date_picture(&date, "dd/MM/yyyy", None), "05/03/2026");
        assert_eq!(date_picture(&date, "d/M/yy", None), "5/3/26");
        assert_eq!(date_picture(&date, "MMMM d, yyyy", None), "March 5, 2026");
    }

    // ── §17.16.4.2 escapes (issue #159) ─────────────────────────────────────

    /// 2026-08-11, the day the reporter ran their comparison, so the expected
    /// strings below are literally the ones in the issue's table.
    fn reported_date() -> Date {
        Date {
            year: 2026,
            month: 8,
            day: 11,
        }
    }

    /// §17.16.4.2: a backslash escapes the character that follows it, so that
    /// the character is taken literally rather than as a formatting code. The
    /// escape is consumed and is *not* part of the output.
    ///
    /// Cases A and C from issue #159, where 0.5.0 emitted the backslash.
    #[test]
    fn an_escape_is_consumed_and_its_character_is_literal() {
        let d = reported_date();
        assert_eq!(
            date_picture(&d, r"MMM\ d, yyyy", None),
            "Aug 11, 2026",
            "issue #159 case A: an escaped space"
        );
        assert_eq!(
            date_picture(&d, r"MMM d \a yyyy", None),
            "Aug 11 a 2026",
            "issue #159 case C: an escaped letter"
        );
    }

    /// Case B — the same picture without an escape. Correct before the fix, and
    /// what proves the fault is the escape rather than the picture parser.
    #[test]
    fn an_unescaped_picture_is_unaffected() {
        assert_eq!(
            date_picture(&reported_date(), "MMM d, yyyy", None),
            "Aug 11, 2026"
        );
    }

    /// The escape has to happen *before* token recognition, not after: an
    /// escaped `d` is the letter `d`, never a day. Three of them in a row would
    /// otherwise be a weekday name.
    #[test]
    fn an_escaped_format_character_is_not_a_format_character() {
        assert_eq!(date_picture(&reported_date(), r"\d\d\d", None), "ddd");
        assert_eq!(date_picture(&reported_date(), r"\y\M\d", None), "yMd");
    }

    // ── one picture grammar, not two ────────────────────────────────────────

    fn half_past_nine() -> Time {
        Time {
            hour: 21,
            minute: 30,
            second: 5,
        }
    }

    /// §17.16.4.2 is a single date-*and*-time picture. A `DATE` field may ask
    /// for the time, and did not get it while date and time had separate
    /// renderers: `h:mm` came out as the literal letters `h:mm`.
    #[test]
    fn one_picture_renders_both_date_and_time_tokens() {
        assert_eq!(
            format_datetime(
                Some(&reported_date()),
                Some(&half_past_nine()),
                "MMM d, yyyy h:mm am/pm",
                None,
            ),
            "Aug 11, 2026 9:30 pm",
        );
        // …and the mirror: `M` is the month even in a picture reached from TIME.
        assert_eq!(
            format_datetime(
                Some(&reported_date()),
                Some(&half_past_nine()),
                "HH:mm:ss 'on' yyyy-MM-dd",
                None,
            ),
            "21:30:05 on 2026-08-11",
        );
    }

    /// A token whose source the caller does not have contributes nothing,
    /// rather than rendering a placeholder or dropping the whole picture.
    #[test]
    fn a_token_without_its_source_renders_as_nothing() {
        assert_eq!(date_picture(&reported_date(), "yyyy h:mm", None), "2026 :");
        assert_eq!(time_picture(&half_past_nine(), "yyyy HH:mm"), " 21:30");
    }

    /// 12-hour is now a property of the token list, not of the raw string.
    /// `contains("AM/PM")` — what this replaced — read the quoted literal and
    /// switched the clock on the strength of text it was about to print.
    #[test]
    fn am_pm_inside_a_quoted_literal_does_not_switch_to_12_hour() {
        assert_eq!(time_picture(&half_past_nine(), "h 'AM/PM'"), "21 AM/PM");
        // An actual AM/PM token *does* switch it.
        assert_eq!(time_picture(&half_past_nine(), "h AM/PM"), "9 PM");
    }

    // ── localized day period ────────────────────────────────────────────────

    /// The day period is a name like the month and the weekday, and has to
    /// come from CLDR for the same reason: `AM`/`PM` are English, and a
    /// document that states `w:lang` has said which language it wants.
    #[test]
    fn the_day_period_follows_the_locale() {
        let t = half_past_nine();
        assert_eq!(
            format_datetime(None, Some(&t), "am/pm", Some("es-MX")),
            "p.m."
        );
        assert_eq!(
            format_datetime(None, Some(&t), "AM/PM", Some("es-MX")),
            "P.M."
        );
    }

    /// English is unchanged, and an unusable tag falls back to it — the same
    /// discipline `month_name`/`weekday_name` already follow.
    #[test]
    fn the_day_period_falls_back_to_english() {
        let t = half_past_nine();
        assert_eq!(
            format_datetime(None, Some(&t), "AM/PM", Some("en-US")),
            "PM"
        );
        assert_eq!(format_datetime(None, Some(&t), "AM/PM", None), "PM");
        assert_eq!(
            format_datetime(None, Some(&t), "am/pm", Some("zz-ZZ")),
            "pm"
        );
    }

    // ── the picture-less default (issue #159 case D) ────────────────────────

    /// §17.16.5.13: a `DATE` with no `\@` picture renders the locale's short
    /// date. Word reads the *system* locale for this; dxpdf reads the
    /// document's `w:lang`, for the reason `field::now` gives about the host's
    /// regional settings — see `default_date`'s own doc.
    #[test]
    fn a_picture_less_date_follows_the_document_locale() {
        let d = reported_date();
        assert_eq!(default_date(&d, Some("de-DE")), "11.08.26");
        assert_eq!(default_date(&d, Some("en-GB")), "11/08/2026");
    }

    /// No `w:lang`, or one with no data, keeps the pre-existing default rather
    /// than rendering nothing.
    #[test]
    fn a_picture_less_date_without_a_locale_keeps_the_engine_default() {
        let d = reported_date();
        assert_eq!(default_date(&d, None), "8/11/2026");
        assert_eq!(default_date(&d, Some("zz-ZZ")), "8/11/2026");
    }

    #[test]
    fn a_picture_less_time_follows_the_document_locale() {
        let t = half_past_nine();
        assert_eq!(default_time(&t, Some("de-DE")), "21:30");
        assert_eq!(default_time(&t, None), "9:30 PM");
    }

    // ── §17.16.4.2 name tokens (issue #129) ─────────────────────────────────

    /// 2026-08-10, a Monday — `now.rs`'s own tests pin the same date from the
    /// other direction (a Unix timestamp), so the two can't drift.
    fn monday() -> Date {
        Date {
            year: 2026,
            month: 8,
            day: 10,
        }
    }

    /// The regression this token split exists for: `ddd`/`dddd` used to fall
    /// into the zero-padded day-of-month branch and render "10", a day
    /// *number*, where §17.16.4.2 asks for a day *name*.
    #[test]
    fn ddd_is_a_weekday_name_not_a_zero_padded_day() {
        assert_eq!(date_picture(&monday(), "ddd", None), "Mon");
        assert_eq!(date_picture(&monday(), "dddd", None), "Monday");
        // …while one and two `d` still mean the day of the month.
        assert_eq!(date_picture(&monday(), "d", None), "10");
        assert_eq!(date_picture(&monday(), "dd", None), "10");
    }

    #[test]
    fn month_and_weekday_names_follow_the_locale() {
        let d = monday();
        assert_eq!(date_picture(&d, "MMMM", Some("fr-FR")), "août");
        assert_eq!(date_picture(&d, "MMMM", Some("ru-RU")), "август");
        assert_eq!(date_picture(&d, "dddd", Some("de-DE")), "Montag");
        assert_eq!(date_picture(&d, "ddd", Some("de-DE")), "Mo");
        assert_eq!(date_picture(&d, "dddd", Some("fr-FR")), "lundi");
    }

    /// A whole picture, not one token: the literal text around the names is
    /// untouched and the numeric tokens still read the *date*, not the
    /// weekday.
    #[test]
    fn a_full_picture_localizes_only_its_name_tokens() {
        assert_eq!(
            date_picture(&monday(), "dddd, d MMMM yyyy", Some("de-DE")),
            "Montag, 10 August 2026",
        );
        assert_eq!(
            date_picture(&monday(), "dddd, d MMMM yyyy", None),
            "Monday, 10 August 2026",
        );
    }

    /// Both fallbacks: a tag with no baked data, and no tag at all, render
    /// the English tables rather than erroring or emitting nothing.
    #[test]
    fn an_unusable_locale_falls_back_to_english_names() {
        assert_eq!(date_picture(&monday(), "MMMM", Some("zz-ZZ")), "August");
        assert_eq!(date_picture(&monday(), "dddd", Some("zz-ZZ")), "Monday");
        assert_eq!(date_picture(&monday(), "MMM", None), "Aug");
    }

    /// A date that isn't a real calendar date has no weekday to name. It must
    /// degrade, not panic — `Date` is a plain struct with no range invariant,
    /// so nothing upstream guarantees the caller's fields are sane.
    #[test]
    fn an_impossible_date_does_not_panic() {
        let nonsense = Date {
            year: 2026,
            month: 13,
            day: 40,
        };
        assert_eq!(date_picture(&nonsense, "dddd", Some("de-DE")), "");
        // The month table still answers for an out-of-range month, exactly as
        // it did before this change.
        assert_eq!(date_picture(&nonsense, "MMMM", Some("de-DE")), "???");
    }

    #[test]
    fn format_time_24h() {
        let time = Time {
            hour: 14,
            minute: 5,
            second: 9,
        };
        assert_eq!(time_picture(&time, "HH:mm:ss"), "14:05:09");
        assert_eq!(time_picture(&time, "H:m"), "14:5");
    }

    #[test]
    fn format_time_12h() {
        let time = Time {
            hour: 14,
            minute: 30,
            second: 0,
        };
        assert_eq!(time_picture(&time, "h:mm AM/PM"), "2:30 PM");
    }

    #[test]
    fn format_number_decimal() {
        assert_eq!(format_number(1.2345, "0.00"), "1.23");
        assert_eq!(format_number(42.0, "0.000"), "42.000");
    }

    #[test]
    fn general_format_upper() {
        assert_eq!(apply_general_format("hello", "Upper"), "HELLO");
        assert_eq!(apply_general_format("hello", "Lower"), "hello");
        assert_eq!(
            apply_general_format("hello world", "FirstCap"),
            "Hello world"
        );
    }

    #[test]
    fn roman_numerals() {
        assert_eq!(to_roman(1, false), "I");
        assert_eq!(to_roman(4, false), "IV");
        assert_eq!(to_roman(14, false), "XIV");
        assert_eq!(to_roman(2026, false), "MMXXVI");
    }

    #[test]
    fn alphabetic_numbering() {
        assert_eq!(to_alphabetic(1, false), "A");
        assert_eq!(to_alphabetic(26, false), "Z");
        assert_eq!(to_alphabetic(27, false), "AA");
    }

    #[test]
    fn general_format_roman_case_follows_switch() {
        // §17.16.4.1: `\* roman` → lowercase, `\* ROMAN` → uppercase.
        assert_eq!(apply_general_format("4", "roman"), "iv");
        assert_eq!(apply_general_format("4", "ROMAN"), "IV");
        assert_eq!(apply_general_format("4", "Roman"), "IV");
        // Non-numeric input is passed through unchanged.
        assert_eq!(apply_general_format("abc", "roman"), "abc");
    }

    #[test]
    fn general_format_alphabetic_case_follows_switch() {
        assert_eq!(apply_general_format("1", "alphabetic"), "a");
        assert_eq!(apply_general_format("1", "ALPHABETIC"), "A");
        assert_eq!(apply_general_format("28", "alphabetic"), "ab");
    }

    #[test]
    fn general_format_caps_is_title_case_firstcap_is_not() {
        // Caps capitalizes every word; FirstCap only the first.
        assert_eq!(apply_general_format("hello world", "Caps"), "Hello World");
        assert_eq!(
            apply_general_format("hello world", "FirstCap"),
            "Hello world"
        );
        // Existing capitals inside a word are left alone.
        assert_eq!(
            apply_general_format("the fbi report", "Caps"),
            "The Fbi Report"
        );
    }

    #[test]
    fn number_integer_format_rounds_not_truncates() {
        assert_eq!(format_number(2.7, "0"), "3");
        assert_eq!(format_number(2.4, "0"), "2");
        assert_eq!(format_number(-2.7, "0"), "-3");
        // `.round()` first avoids a "-0" result.
        assert_eq!(format_number(-0.4, "0"), "0");
    }

    #[test]
    fn time_ampm_after_multibyte_literal_does_not_panic() {
        let time = Time {
            hour: 14,
            minute: 30,
            second: 0,
        };
        // A multi-byte char in a literal shifts byte vs char offsets; the AM/PM
        // match must still align (previously this byte-sliced and could panic).
        assert_eq!(time_picture(&time, "'é' h AM/PM"), "é 2 PM");
    }
}

//! The wall-clock moment §17.16.5.13 `DATE` and §17.16.5.76 `TIME` fields
//! render against.
//!
//! # UTC, not the host's local time
//!
//! Deliberate, and the same argument `build::convert::paragraph_locale` makes
//! for the §17.18.85 decimal separator: a converter that reads the host's
//! regional settings renders the same document differently on two machines.
//! A local-time reading would put a document converted at 23:30 in Berlin on
//! a different *day* than the same document converted at the same instant in
//! Los Angeles. UTC keys the answer to the moment of conversion alone, which
//! is the most a `DATE` field can be keyed to and still mean "now".
//!
//! # Why the split
//!
//! [`from_unix_seconds`] is the whole calendar conversion and is pure — every
//! test drives it directly with a fixed timestamp. [`now`] is the one line
//! that reads the clock, and is deliberately not unit-tested: there is
//! nothing in it to assert that wouldn't just restate `SystemTime::now`.
//! Layout calls `now` once per render (`render::layout_document`) and carries
//! the result on the field context, so every `DATE` field in one document
//! agrees even if the render spans midnight.

use icu_calendar::{Date as IcuDate, Gregorian};

use crate::field::context::{Date, Time};

const SECONDS_PER_DAY: i64 = 86_400;
const SECONDS_PER_HOUR: i64 = 3_600;
const SECONDS_PER_MINUTE: i64 = 60;

/// Split a Unix timestamp into the Gregorian date and the time of day, both
/// in UTC.
///
/// The calendar arithmetic is `icu_calendar`'s rather than hand-rolled: a
/// day count offset from the epoch's own [`RataDie`](icu_calendar::types::RataDie)
/// is exactly what that type is for, and leap years — including the
/// century rules that make 1900 and 2100 ordinary — are then someone else's
/// tested problem. `div_euclid`/`rem_euclid` rather than `/` and `%` so a
/// pre-1970 timestamp floors into the previous day instead of truncating
/// toward zero and landing on a negative hour.
pub fn from_unix_seconds(unix_seconds: i64) -> (Date, Time) {
    let days = unix_seconds.div_euclid(SECONDS_PER_DAY);
    let seconds_of_day = unix_seconds.rem_euclid(SECONDS_PER_DAY);

    // Derived rather than hardcoded: `icu_calendar`'s own epoch constant is
    // private, and a literal RataDie here would be a magic number no reader
    // could check.
    let epoch = IcuDate::try_new_gregorian(1970, 1, 1)
        .expect("1970-01-01 is a valid Gregorian date")
        .to_rata_die();
    let date = IcuDate::from_rata_die(epoch + days, Gregorian);

    (
        Date {
            year: date.year().extended_year(),
            month: u32::from(date.month().ordinal),
            day: u32::from(date.day_of_month().0),
        },
        Time {
            hour: (seconds_of_day / SECONDS_PER_HOUR) as u32,
            minute: (seconds_of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE) as u32,
            second: (seconds_of_day % SECONDS_PER_MINUTE) as u32,
        },
    )
}

/// The current UTC date and time.
///
/// A clock set before 1970 reads as the epoch rather than panicking: a
/// misconfigured host should render a wrong date, not fail the conversion.
pub fn now() -> (Date, Time) {
    let unix_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    from_unix_seconds(unix_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-08-09T12:34:56Z — independently confirmed with `date -u -r`, not
    /// produced by the code under test.
    #[test]
    fn decodes_a_known_instant() {
        let (date, time) = from_unix_seconds(1_786_278_896);
        assert_eq!((date.year, date.month, date.day), (2026, 8, 9));
        assert_eq!((time.hour, time.minute, time.second), (12, 34, 56));
    }

    #[test]
    fn decodes_the_epoch_itself() {
        let (date, time) = from_unix_seconds(0);
        assert_eq!((date.year, date.month, date.day), (1970, 1, 1));
        assert_eq!((time.hour, time.minute, time.second), (0, 0, 0));
    }

    /// The last second of a day and the first of the next must not collapse
    /// into the same date — the off-by-one a `/`-instead-of-`div_euclid`
    /// mistake shows up as.
    #[test]
    fn a_day_boundary_falls_on_the_right_side() {
        let (before, t_before) = from_unix_seconds(1_786_233_599); // 2026-08-08T23:59:59Z
        assert_eq!((before.year, before.month, before.day), (2026, 8, 8));
        assert_eq!(
            (t_before.hour, t_before.minute, t_before.second),
            (23, 59, 59)
        );

        let (after, t_after) = from_unix_seconds(1_786_233_600); // 2026-08-09T00:00:00Z
        assert_eq!((after.year, after.month, after.day), (2026, 8, 9));
        assert_eq!((t_after.hour, t_after.minute, t_after.second), (0, 0, 0));
    }

    /// A pre-epoch timestamp floors into the previous day rather than
    /// truncating toward zero, which is what `div_euclid` buys over `/`.
    #[test]
    fn a_pre_epoch_timestamp_floors_instead_of_truncating() {
        let (date, time) = from_unix_seconds(-1); // 1969-12-31T23:59:59Z
        assert_eq!((date.year, date.month, date.day), (1969, 12, 31));
        assert_eq!((time.hour, time.minute, time.second), (23, 59, 59));
    }

    /// A leap day, and the century rule that makes 2000 a leap year — the
    /// two cases a hand-rolled conversion would most likely get wrong, kept
    /// as a check that the delegation to `icu_calendar` is really happening.
    #[test]
    fn leap_days_land_correctly() {
        let (date, _) = from_unix_seconds(951_782_400); // 2000-02-29T00:00:00Z
        assert_eq!((date.year, date.month, date.day), (2000, 2, 29));

        let (date, _) = from_unix_seconds(1_709_164_800); // 2024-02-29T00:00:00Z
        assert_eq!((date.year, date.month, date.day), (2024, 2, 29));
    }

    /// `now` must agree with the pure half — a smoke test that the wrapper
    /// isn't wired up backwards, without asserting on the clock's value.
    #[test]
    fn now_returns_a_plausible_present_date() {
        let (date, time) = now();
        assert!(date.year >= 2026, "year {} looks wrong", date.year);
        assert!((1..=12).contains(&date.month));
        assert!((1..=31).contains(&date.day));
        assert!(time.hour < 24 && time.minute < 60 && time.second < 60);
    }
}

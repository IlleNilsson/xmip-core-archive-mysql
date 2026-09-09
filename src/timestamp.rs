//! When an item was archived, as the text a `DATETIME` column reads:
//! `2026-09-09 12:00:00`, in UTC, because a `DATETIME` keeps no zone and
//! a literal with one is not read by every server version.
//!
//! Written by hand from the seconds since the epoch, because a date is the
//! one thing this crate needs from a calendar and a dependency for it would
//! be the largest thing in the crate.

use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_A_DAY: i64 = 86_400;

/// `at` as a `DATETIME` literal in UTC to the second. A time before the
/// epoch is the epoch.
#[must_use]
pub fn datetime_utc(at: SystemTime) -> String {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(seconds.div_euclid(SECONDS_A_DAY));
    let of_day = seconds.rem_euclid(SECONDS_A_DAY);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01, by the era
/// arithmetic every calendar library uses: four-hundred-year eras of
/// 146 097 days, the year within the era, the day within a March-first year.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn the_epoch_a_leap_day_and_a_year_end_are_dated_right() {
        assert_eq!(datetime_utc(at(0)), "1970-01-01 00:00:00");
        assert_eq!(datetime_utc(at(951_782_400)), "2000-02-29 00:00:00");
        assert_eq!(datetime_utc(at(1_230_767_999)), "2008-12-31 23:59:59");
        assert_eq!(datetime_utc(at(1_788_912_000)), "2026-09-09 00:00:00");
        assert_eq!(datetime_utc(at(1_788_955_445)), "2026-09-09 12:04:05");
    }

    #[test]
    fn before_the_epoch_is_the_epoch() {
        let before = UNIX_EPOCH - Duration::from_secs(5);
        assert_eq!(datetime_utc(before), "1970-01-01 00:00:00");
    }
}

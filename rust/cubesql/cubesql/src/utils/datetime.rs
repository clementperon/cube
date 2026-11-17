//! Conversions between Arrow's epoch-based date/time values and jiff civil types, plus the
//! text forms the rest of the crate prints civil values in.
//!
//! Timestamps here carry no time zone and are read as UTC wall clock, so everything goes through
//! civil arithmetic rather than `jiff::Timestamp`: `Timestamp` stops at
//! `9999-12-30T22:00:00.999999999Z`, short of `9999-12-31`, a common "end of time" sentinel.

use std::{convert::TryFrom, fmt};

use jiff::{
    civil::{Date, DateTime},
    SignedDuration,
};

pub const UNIX_EPOCH: DateTime = DateTime::constant(1970, 1, 1, 0, 0, 0, 0);

const NANOS_PER_MICRO: i128 = 1_000;
const NANOS_PER_MILLI: i128 = 1_000_000;
const NANOS_PER_SECOND: i128 = 1_000_000_000;
const HOURS_PER_DAY: i64 = 24;

fn datetime_from_unix(since_epoch: SignedDuration) -> Option<DateTime> {
    UNIX_EPOCH.checked_add(since_epoch).ok()
}

/// `None` when the value is outside years `-9999..=9999`.
pub fn datetime_from_unix_seconds(seconds: i64) -> Option<DateTime> {
    datetime_from_unix(SignedDuration::from_secs(seconds))
}

/// `None` when the value is outside years `-9999..=9999`.
pub fn datetime_from_unix_millis(millis: i64) -> Option<DateTime> {
    datetime_from_unix(SignedDuration::from_millis(millis))
}

/// `None` when the value is outside years `-9999..=9999`.
pub fn datetime_from_unix_micros(micros: i64) -> Option<DateTime> {
    datetime_from_unix(SignedDuration::from_micros(micros))
}

pub fn datetime_from_unix_nanos(nanos: i64) -> DateTime {
    datetime_from_unix(SignedDuration::from_nanos(nanos))
        .expect("every i64 nanosecond count (years 1677..=2262) is a valid DateTime")
}

/// `None` when the value is outside years `-9999..=9999`.
pub fn date_from_unix_days(days: i32) -> Option<Date> {
    UNIX_EPOCH
        .date()
        .checked_add(SignedDuration::from_hours(i64::from(days) * HOURS_PER_DAY))
        .ok()
}

/// `None` when the value does not fit `i64` nanoseconds (outside years 1677..=2262).
pub fn unix_nanos(dt: DateTime) -> Option<i64> {
    i64::try_from(dt.duration_since(UNIX_EPOCH).as_nanos()).ok()
}

/// Microseconds since the Unix epoch, rounded towards negative infinity.
pub fn unix_micros(dt: DateTime) -> i64 {
    // Every DateTime is within ±9999 years of the epoch, far inside i64 microseconds.
    dt.duration_since(UNIX_EPOCH)
        .as_nanos()
        .div_euclid(NANOS_PER_MICRO) as i64
}

/// Milliseconds since the Unix epoch, rounded towards negative infinity.
pub fn unix_millis(dt: DateTime) -> i64 {
    // Every DateTime is within ±9999 years of the epoch, far inside i64 milliseconds.
    dt.duration_since(UNIX_EPOCH)
        .as_nanos()
        .div_euclid(NANOS_PER_MILLI) as i64
}

/// Seconds since the Unix epoch, rounded towards negative infinity.
pub fn unix_seconds(dt: DateTime) -> i64 {
    // Every DateTime is within ±9999 years of the epoch, far inside i64 seconds.
    dt.duration_since(UNIX_EPOCH)
        .as_nanos()
        .div_euclid(NANOS_PER_SECOND) as i64
}

pub fn unix_days(date: Date) -> i32 {
    // Both dates are civil, so the difference is a whole number of 24-hour days, and every Date
    // is within ±9999 years of the epoch, far inside i32 days.
    (date.duration_since(UNIX_EPOCH.date()).as_hours() / HOURS_PER_DAY) as i32
}

/// The fraction of a second with its leading dot, as 3, 6 or 9 digits, whichever is the shortest
/// that loses nothing. Empty when there is no fraction.
pub struct SubsecFraction(pub DateTime);

impl fmt::Display for SubsecFraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanos = self.0.subsec_nanosecond();
        if nanos == 0 {
            Ok(())
        } else if nanos % 1_000_000 == 0 {
            write!(f, ".{:03}", nanos / 1_000_000)
        } else if nanos % 1_000 == 0 {
            write!(f, ".{:06}", nanos / 1_000)
        } else {
            write!(f, ".{:09}", nanos)
        }
    }
}

/// `YYYY-MM-DD HH:MM:SS` followed by [`SubsecFraction`].
pub struct SqlDateTime(pub DateTime);

impl fmt::Display for SqlDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dt = self.0;
        write!(
            f,
            "{} {:02}:{:02}:{:02}{}",
            dt.date(),
            dt.hour(),
            dt.minute(),
            dt.second(),
            SubsecFraction(dt)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn unix_values_round_trip() {
        let dt = date(2024, 2, 29).at(13, 45, 7, 123_456_789);
        let nanos = 1_709_214_307_123_456_789;
        assert_eq!(unix_nanos(dt), Some(nanos));
        assert_eq!(datetime_from_unix_nanos(nanos), dt);
        assert_eq!(unix_micros(dt), nanos / 1_000);
        assert_eq!(unix_millis(dt), nanos / 1_000_000);
        assert_eq!(unix_seconds(dt), nanos / 1_000_000_000);
        assert_eq!(
            datetime_from_unix_millis(nanos / 1_000_000),
            Some(date(2024, 2, 29).at(13, 45, 7, 123_000_000))
        );
        assert_eq!(
            datetime_from_unix_micros(nanos / 1_000),
            Some(date(2024, 2, 29).at(13, 45, 7, 123_456_000))
        );
        assert_eq!(
            datetime_from_unix_seconds(nanos / 1_000_000_000),
            Some(date(2024, 2, 29).at(13, 45, 7, 0))
        );

        assert_eq!(unix_days(date(2024, 2, 29)), 19_782);
        assert_eq!(date_from_unix_days(19_782), Some(date(2024, 2, 29)));
        assert_eq!(unix_days(date(1969, 12, 31)), -1);
        assert_eq!(date_from_unix_days(-1), Some(date(1969, 12, 31)));
    }

    #[test]
    fn pre_epoch_fractions_round_down() {
        let dt = date(1969, 12, 31).at(23, 59, 59, 999_500_000);
        assert_eq!(unix_nanos(dt), Some(-500_000));
        assert_eq!(unix_micros(dt), -500);
        assert_eq!(unix_millis(dt), -1);
        assert_eq!(unix_seconds(dt), -1);
        assert_eq!(datetime_from_unix_nanos(-500_000), dt);
        assert_eq!(datetime_from_unix_nanos(i64::MIN).year(), 1677);
        assert_eq!(datetime_from_unix_nanos(i64::MAX).year(), 2262);
    }

    #[test]
    fn range_limits() {
        // The last day jiff can represent is past what `jiff::Timestamp` holds.
        let end_of_time = date(9999, 12, 31).at(0, 0, 0, 0);
        let millis = 253_402_214_400_000;
        assert_eq!(unix_millis(end_of_time), millis);
        assert_eq!(datetime_from_unix_millis(millis), Some(end_of_time));
        assert_eq!(unix_nanos(end_of_time), None);

        assert_eq!(datetime_from_unix_millis(millis + 86_400_000), None);
        assert_eq!(datetime_from_unix_seconds(i64::MAX), None);
        assert_eq!(datetime_from_unix_micros(i64::MIN), None);
        assert_eq!(date_from_unix_days(i32::MAX), None);
        assert_eq!(date_from_unix_days(i32::MIN), None);
    }

    #[test]
    fn sql_datetime_fraction_width() {
        let cases = [
            (0, "2024-02-29 13:45:07"),
            (120_000_000, "2024-02-29 13:45:07.120"),
            (123_400_000, "2024-02-29 13:45:07.123400"),
            (123_456_780, "2024-02-29 13:45:07.123456780"),
        ];
        for (nanos, expected) in cases {
            let dt = date(2024, 2, 29).at(13, 45, 7, nanos);
            assert_eq!(SqlDateTime(dt).to_string(), expected);
        }
    }
}

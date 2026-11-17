//! Timestamp value representation for PostgreSQL protocol

use crate::{
    protocol::{ErrorCode, ErrorResponse},
    FromProtocolValue, ProtocolError, ToProtocolValue,
};
use byteorder::{BigEndian, ByteOrder};
use bytes::{BufMut, BytesMut};
use jiff::{
    civil::{Date, DateTime, Time},
    tz::{Offset, TimeZone},
    Timestamp, ZonedDateTime,
};
use std::backtrace::Backtrace;
use std::convert::TryFrom;
use std::io::Error;
use std::{
    fmt::{self, Debug, Display, Formatter},
    io,
};

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TimestampValue {
    unix_nano: i64,
    tz: Option<String>,
}

impl TimestampValue {
    pub fn new(mut unix_nano: i64, tz: Option<String>) -> TimestampValue {
        // This is a hack to workaround a mismatch between on-disk and in-memory representations.
        // We use microsecond precision on-disk.
        unix_nano -= unix_nano % 1000;
        TimestampValue { unix_nano, tz }
    }

    fn to_timestamp(&self) -> Timestamp {
        Timestamp::from_nanosecond(i128::from(self.unix_nano))
            .expect("every i64 nanosecond count (years 1677..=2262) is a valid Timestamp")
    }

    /// The civil (wall clock) date and time of this timestamp in UTC.
    pub fn to_naive_datetime(&self) -> DateTime {
        Offset::UTC.to_datetime(self.to_timestamp())
    }

    pub fn to_fixed_datetime(&self) -> io::Result<ZonedDateTime> {
        assert!(self.tz.is_some());
        let name = self.tz.as_ref().unwrap();
        // UTC is what nearly every value carries, and needs no time zone database lookup.
        let tz = if name == "UTC" {
            TimeZone::UTC
        } else {
            TimeZone::get(name).map_err(|err| io::Error::other(err.to_string()))?
        };
        Ok(self.to_timestamp().to_zoned_datetime(tz))
    }

    pub fn tz_ref(&self) -> &Option<String> {
        &self.tz
    }

    pub fn get_time_stamp(&self) -> i64 {
        self.unix_nano
    }
}

impl Debug for TimestampValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimestampValue")
            .field("unix_nano", &self.unix_nano)
            .field("tz", &self.tz)
            .field("str", &self.to_string())
            .finish()
    }
}

/// `YYYY-MM-DD HH:MM:SS.ffffff+00`, the longest text form of a timestamp.
const MAX_TEXT_LEN: usize = 29;

/// Writes `value` as exactly `width` zero-padded digits.
fn put_padded(buf: &mut impl BufMut, mut value: u32, width: usize) {
    let mut digits = [b'0'; 9];
    for digit in digits[..width].iter_mut().rev() {
        *digit = b'0' + (value % 10) as u8;
        value /= 10;
    }
    buf.put_slice(&digits[..width]);
}

/// Writes `YYYY-MM-DD<sep>HH:MM:SS.<fraction>`, truncating the fraction to `frac_digits` like
/// strftime's `%.6f`. `i64` nanoseconds only span years 1677..=2262, so the year is always
/// 4 digits.
fn put_timestamp(buf: &mut impl BufMut, ndt: &DateTime, sep: u8, frac_digits: usize) {
    put_padded(buf, ndt.year() as u32, 4);
    buf.put_u8(b'-');
    put_padded(buf, ndt.month() as u32, 2);
    buf.put_u8(b'-');
    put_padded(buf, ndt.day() as u32, 2);
    buf.put_u8(sep);
    put_padded(buf, ndt.hour() as u32, 2);
    buf.put_u8(b':');
    put_padded(buf, ndt.minute() as u32, 2);
    buf.put_u8(b':');
    put_padded(buf, ndt.second() as u32, 2);
    buf.put_u8(b'.');
    put_padded(
        buf,
        ndt.subsec_nanosecond() as u32 / 10u32.pow(9 - frac_digits as u32),
        frac_digits,
    );
}

impl Display for TimestampValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let ndt = self.to_naive_datetime();

        let mut out = [0u8; MAX_TEXT_LEN];
        let mut rest = &mut out[..];
        put_timestamp(&mut rest, &ndt, b'T', 3);
        let len = MAX_TEXT_LEN - rest.len();

        f.write_str(std::str::from_utf8(&out[..len]).expect("timestamp is ASCII"))
    }
}

// POSTGRES_EPOCH_JDATE
// https://github.com/postgres/postgres/blob/REL_14_4/src/include/datatype/timestamp.h#L163
pub(crate) fn pg_base_date_epoch() -> DateTime {
    DateTime::constant(2000, 1, 1, 0, 0, 0, 0)
}

/// [`pg_base_date_epoch`] as nanoseconds since the Unix epoch.
const PG_BASE_DATE_EPOCH_UNIX_NANOS: i64 = 946_684_800_000_000_000;

impl ToProtocolValue for TimestampValue {
    fn to_text(&self, buf: &mut BytesMut) -> Result<(), ProtocolError> {
        let ndt = match self.tz_ref() {
            None => self.to_naive_datetime(),
            Some(_) => Offset::UTC.to_datetime(self.to_fixed_datetime()?.timestamp()),
        };

        // 2022-04-25 15:36:49.397050+00
        let mut out = [0u8; MAX_TEXT_LEN];
        let mut rest = &mut out[..];
        put_timestamp(&mut rest, &ndt, b' ', 6);
        if self.tz_ref().is_some() {
            rest.put_slice(b"+00");
        }
        let len = MAX_TEXT_LEN - rest.len();

        buf.put_i32(len as i32);
        buf.put_slice(&out[..len]);

        Ok(())
    }

    // https://github.com/postgres/postgres/blob/REL_14_4/src/backend/utils/adt/timestamp.c#L267
    fn to_binary(&self, buf: &mut BytesMut) -> Result<(), ProtocolError> {
        let ndt = match self.tz_ref() {
            None => self.to_naive_datetime(),
            Some(_) => Offset::UTC.to_datetime(self.to_fixed_datetime()?.timestamp()),
        };

        let n = i64::try_from(ndt.duration_since(pg_base_date_epoch()).as_micros())
            .map_err(|_| Error::other("Unable to extract number of seconds from timestamp"))?;

        buf.put_i32(8);
        buf.put_i64(n);

        Ok(())
    }
}

impl FromProtocolValue for TimestampValue {
    fn from_text(raw: &[u8]) -> Result<Self, ProtocolError> {
        let as_str = std::str::from_utf8(raw).map_err(|err| ProtocolError::ErrorResponse {
            source: ErrorResponse::error(ErrorCode::ProtocolViolation, err.to_string()),
            backtrace: Backtrace::capture(),
        })?;

        // Parse timestamp string in format "YYYY-MM-DD HH:MM:SS[.fff]", but PostgreSQL supports
        // more formats, so let's align this with parse_date_str function from cubesql crate.
        let parsed_datetime = DateTime::strptime("%Y-%m-%d %H:%M:%S", as_str)
            .or_else(|_| DateTime::strptime("%Y-%m-%d %H:%M:%S%.f", as_str))
            .or_else(|_| DateTime::strptime("%Y-%m-%d %H:%M:%S%.f UTC", as_str))
            .or_else(|_| DateTime::strptime("%Y-%m-%dT%H:%M:%S", as_str))
            .or_else(|_| DateTime::strptime("%Y-%m-%dT%H:%M:%S%.f", as_str))
            .or_else(|_| DateTime::strptime("%Y-%m-%dT%H:%M:%S%.fZ", as_str))
            .or_else(|_| {
                Date::strptime("%Y-%m-%d", as_str).map(|date| date.to_datetime(Time::midnight()))
            })
            .map_err(|err| ProtocolError::ErrorResponse {
                source: ErrorResponse::error(
                    ErrorCode::ProtocolViolation,
                    format!(
                        "Unable to parse timestamp from text: '{}', error: {}",
                        as_str, err
                    ),
                ),
                backtrace: Backtrace::capture(),
            })?;

        // Convert to Unix nanoseconds
        let unix_nano = Offset::UTC
            .to_timestamp(parsed_datetime)
            .ok()
            .and_then(|timestamp| i64::try_from(timestamp.as_nanosecond()).ok())
            .ok_or_else(|| ProtocolError::ErrorResponse {
                source: ErrorResponse::error(
                    ErrorCode::ProtocolViolation,
                    format!("Timestamp out of range: '{}'", as_str),
                ),
                backtrace: Backtrace::capture(),
            })?;

        Ok(TimestampValue::new(unix_nano, None))
    }

    // https://github.com/postgres/postgres/blob/REL_14_4/src/backend/utils/adt/timestamp.c#L234
    fn from_binary(raw: &[u8]) -> Result<Self, ProtocolError> {
        if raw.len() != 8 {
            return Err(ProtocolError::ErrorResponse {
                source: ErrorResponse::error(
                    ErrorCode::ProtocolViolation,
                    format!(
                        "Invalid binary timestamp length: expected 8 bytes, got {}",
                        raw.len()
                    ),
                ),
                backtrace: Backtrace::capture(),
            });
        }

        let pg_microseconds = BigEndian::read_i64(raw);

        // Convert PostgreSQL microseconds to Unix nanoseconds
        let unix_nano = pg_microseconds
            .checked_mul(1_000)
            .and_then(|nanos| PG_BASE_DATE_EPOCH_UNIX_NANOS.checked_add(nanos))
            .ok_or_else(|| ProtocolError::ErrorResponse {
                source: ErrorResponse::error(
                    ErrorCode::ProtocolViolation,
                    format!("Timestamp out of range: {} microseconds", pg_microseconds),
                ),
                backtrace: Backtrace::capture(),
            })?;

        Ok(TimestampValue::new(unix_nano, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProtocolError;

    #[test]
    fn test_timestamp_creation() -> Result<(), ProtocolError> {
        let ts = TimestampValue::new(1650890322000000000, None);
        assert_eq!(ts.get_time_stamp(), 1650890322000000000);
        assert_eq!(ts.tz_ref(), &None);

        let ts_with_tz = TimestampValue::new(1650890322000000000, Some("UTC".to_string()));
        assert_eq!(ts_with_tz.get_time_stamp(), 1650890322000000000);
        assert_eq!(ts_with_tz.tz_ref(), &Some("UTC".to_string()));

        Ok(())
    }

    #[test]
    fn test_timestamp_to_string() {
        let ts = TimestampValue::new(1650890322000000000, None);
        // The string representation should match the expected format
        assert!(!ts.to_string().is_empty());
    }

    #[test]
    fn test_timestamp_format() {
        let cases = [
            (0, "1970-01-01 00:00:00.000000", "1970-01-01T00:00:00.000"),
            (
                1_000,
                "1970-01-01 00:00:00.000001",
                "1970-01-01T00:00:00.000",
            ),
            (
                1650890322123456789,
                "2022-04-25 12:38:42.123456",
                "2022-04-25T12:38:42.123",
            ),
            (
                1709164800000000000,
                "2024-02-29 00:00:00.000000",
                "2024-02-29T00:00:00.000",
            ),
            (
                1704067199999999000,
                "2023-12-31 23:59:59.999999",
                "2023-12-31T23:59:59.999",
            ),
            (
                -86_400_000_000_000,
                "1969-12-31 00:00:00.000000",
                "1969-12-31T00:00:00.000",
            ),
            (
                -1_500_000,
                "1969-12-31 23:59:59.998500",
                "1969-12-31T23:59:59.998",
            ),
            (
                -9_223_372_036_000_000_000,
                "1677-09-21 00:12:44.000000",
                "1677-09-21T00:12:44.000",
            ),
            (
                i64::MAX,
                "2262-04-11 23:47:16.854775",
                "2262-04-11T23:47:16.854",
            ),
        ];

        for (unix_nano, text, display) in cases {
            for (tz, suffix) in [(None, ""), (Some("UTC".to_string()), "+00")] {
                let ts = TimestampValue::new(unix_nano, tz);
                assert_eq!(ts.to_string(), display);

                let expected = format!("{}{}", text, suffix);
                let mut buf = BytesMut::new();
                ts.to_text(&mut buf).unwrap();
                assert_eq!(&buf[..4], &(expected.len() as i32).to_be_bytes());
                assert_eq!(&buf[4..], expected.as_bytes());
            }
        }
    }

    #[test]
    fn test_timestamp_to_binary_pre_epoch_fraction() {
        let ts = TimestampValue::new(-1_500_000, None);
        let mut buf = BytesMut::new();
        ts.to_binary(&mut buf).unwrap();
        assert_eq!(&buf[..4], &8_i32.to_be_bytes());
        assert_eq!(&buf[4..], &(-946_684_800_001_500_i64).to_be_bytes());
        assert_eq!(TimestampValue::from_binary(&buf[4..]).unwrap(), ts);
    }

    #[test]
    fn test_timestamp_precision_hack() {
        // Test that nanoseconds are truncated to milliseconds
        let ts = TimestampValue::new(1650890322123456789, None);
        assert_eq!(ts.get_time_stamp(), 1650890322123456000);
    }

    #[test]
    fn test_invalid_timestamp_text() {
        // Test that invalid text formats return errors
        assert!(TimestampValue::from_text(b"invalid-date").is_err());
        assert!(TimestampValue::from_text(b"2025-13-45 25:70:99").is_err());
        assert!(TimestampValue::from_text(b"").is_err());
    }

    #[test]
    fn test_timestamp_from_text_various_formats() {
        // Test basic format without fractional seconds
        let ts1 = TimestampValue::from_text(b"2025-08-04 20:15:47").unwrap();
        assert_eq!(ts1.to_naive_datetime().to_string(), "2025-08-04T20:15:47");

        // Test PostgreSQL format with 6-digit fractional seconds
        let ts2 = TimestampValue::from_text(b"2025-08-04 20:16:54.853660").unwrap();
        assert_eq!(
            ts2.to_naive_datetime()
                .strftime("%Y-%m-%d %H:%M:%S%.6f")
                .to_string(),
            "2025-08-04 20:16:54.853660"
        );

        // Test format with 3 fractional seconds
        let ts3 = TimestampValue::from_text(b"2025-08-04 20:15:47.953").unwrap();
        assert_eq!(
            ts3.to_naive_datetime()
                .strftime("%Y-%m-%d %H:%M:%S%.3f")
                .to_string(),
            "2025-08-04 20:15:47.953"
        );

        // Test ISO format with T separator
        let ts4 = TimestampValue::from_text(b"2025-08-04T20:15:47").unwrap();
        assert_eq!(ts4.to_naive_datetime().to_string(), "2025-08-04T20:15:47");

        // Test ISO format with T separator and fractional seconds
        let ts5 = TimestampValue::from_text(b"2025-08-04T20:15:47.953116").unwrap();
        assert_eq!(
            ts5.to_naive_datetime()
                .strftime("%Y-%m-%d %H:%M:%S%.6f")
                .to_string(),
            "2025-08-04 20:15:47.953116"
        );
    }

    #[test]
    fn test_invalid_timestamp_binary() {
        // Test that invalid binary data returns errors
        assert!(TimestampValue::from_binary(&[1, 2, 3]).is_err()); // Wrong length
        assert!(TimestampValue::from_binary(&[]).is_err()); // Empty
    }

    #[test]
    fn test_timestamp_binary_out_of_range() {
        // Overflows the microseconds -> nanoseconds multiplication
        assert!(TimestampValue::from_binary(&i64::MAX.to_be_bytes()).is_err());
        assert!(TimestampValue::from_binary(&i64::MIN.to_be_bytes()).is_err());
        // Largest value that still fits; one more overflows adding the 2000-01-01 epoch offset
        let max_ok = (i64::MAX - 946_684_800_000_000_000) / 1_000;
        assert!(TimestampValue::from_binary(&max_ok.to_be_bytes()).is_ok());
        assert!(TimestampValue::from_binary(&(max_ok + 1).to_be_bytes()).is_err());
    }
}

use crate::compile::engine::df::scan::DataFusionError;
use jiff::civil::{Date, DateTime, Time};

pub fn parse_date_str(s: &str) -> Result<DateTime, DataFusionError> {
    if let Some(parsed) = parse_fast(s) {
        return Ok(parsed);
    }

    parse_with_strptime(s).map_err(|e| {
        DataFusionError::Internal(format!(
            "Can't parse date/time string literal {:?}: {}",
            s, e
        ))
    })
}

#[inline]
fn digit(b: u8) -> Option<u32> {
    let d = b.wrapping_sub(b'0');
    if d <= 9 {
        Some(d as u32)
    } else {
        None
    }
}

#[inline]
fn ascii_u32_2(b: &[u8], at: usize) -> Option<u32> {
    Some(digit(b[at])? * 10 + digit(b[at + 1])?)
}

#[inline]
fn ascii_u32_4(b: &[u8], at: usize) -> Option<u32> {
    Some(
        digit(b[at])? * 1000 + digit(b[at + 1])? * 100 + digit(b[at + 2])? * 10 + digit(b[at + 3])?,
    )
}

/// Recognizes only `YYYY-MM-DDTHH:MM:SS.fff` (23 bytes, `T` separator, 3-digit fraction).
fn parse_fast(s: &str) -> Option<DateTime> {
    let b = s.as_bytes();
    if b.len() != 23
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
    {
        return None;
    }

    // Every value is at most four (year) or two (the rest) decimal digits, so the casts are
    // lossless; the constructor rejects anything that is not a real date or time.
    let year = ascii_u32_4(b, 0)? as i16;
    let month = ascii_u32_2(b, 5)? as i8;
    let day = ascii_u32_2(b, 8)? as i8;
    let hour = ascii_u32_2(b, 11)? as i8;
    let minute = ascii_u32_2(b, 14)? as i8;
    let second = ascii_u32_2(b, 17)? as i8;
    let frac_millis = digit(b[20])? * 100 + digit(b[21])? * 10 + digit(b[22])?;

    DateTime::new(
        year,
        month,
        day,
        hour,
        minute,
        second,
        frac_millis as i32 * 1_000_000,
    )
    .ok()
}

fn parse_with_strptime(s: &str) -> Result<DateTime, jiff::Error> {
    DateTime::strptime("%Y-%m-%dT%H:%M:%S%.f", s)
        .or_else(|_| DateTime::strptime("%Y-%m-%d %H:%M:%S%.f", s))
        .or_else(|_| DateTime::strptime("%Y-%m-%dT%H:%M:%S", s))
        .or_else(|_| DateTime::strptime("%Y-%m-%d %H:%M:%S", s))
        .or_else(|_| DateTime::strptime("%Y-%m-%dT%H:%M:%S%.fZ", s))
        .or_else(|_| DateTime::strptime("%Y-%m-%d %H:%M:%S%.f UTC", s))
        .or_else(|_| Date::strptime("%Y-%m-%d", s).map(|date| date.to_datetime(Time::midnight())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd_hmsn(y: i16, m: i8, d: i8, h: i8, mi: i8, s: i8, n: i32) -> DateTime {
        DateTime::constant(y, m, d, h, mi, s, n)
    }

    #[test]
    fn fast_path_accepts_canonical_shape() {
        let cases: &[(&str, DateTime)] = &[
            ("2022-01-01T00:00:00.000", ymd_hmsn(2022, 1, 1, 0, 0, 0, 0)),
            (
                "2024-06-15T13:45:07.123",
                ymd_hmsn(2024, 6, 15, 13, 45, 7, 123_000_000),
            ),
            (
                "9999-12-31T23:59:59.999",
                ymd_hmsn(9999, 12, 31, 23, 59, 59, 999_000_000),
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(parse_fast(input), Some(*expected), "fast path: {}", input);
            assert_eq!(
                parse_date_str(input).unwrap(),
                *expected,
                "wrapper: {}",
                input
            );
        }
    }

    #[test]
    fn fast_path_rejects_non_canonical() {
        // Wrong length / shape — must not be fast-parsed.
        let rejects = [
            "2022",
            "2022-01-01",
            "2022-01-01 00:00:00",
            "2022-01-01T00:00:00",
            "2022-01-01T00:00:00.000Z",
            "2022-01-01 00:00:00.000",
            "2022-01-01T00:00:00.123456",
            "2022-13-01T00:00:00.000",
            "2022-01-32T00:00:00.000",
            "2022/01/01T00:00:00.000",
            "2022-01-01x00:00:00.000",
            "2022-01-01T25:00:00.000",
            "2022-01-01T00:60:00.000",
            "2022-01-01T00:00:60.000",
        ];
        for s in rejects {
            assert!(parse_fast(s).is_none(), "unexpectedly fast-parsed: {:?}", s);
        }
    }

    #[test]
    fn wrapper_handles_other_shapes_via_strptime_fallback() {
        let cases: &[(&str, DateTime)] = &[
            ("2022-01-01", ymd_hmsn(2022, 1, 1, 0, 0, 0, 0)),
            ("2022-01-01 00:00:00", ymd_hmsn(2022, 1, 1, 0, 0, 0, 0)),
            ("2022-01-01T00:00:00", ymd_hmsn(2022, 1, 1, 0, 0, 0, 0)),
            ("2022-01-01T00:00:00.000Z", ymd_hmsn(2022, 1, 1, 0, 0, 0, 0)),
            (
                "2022-01-01 00:00:00.000 UTC",
                ymd_hmsn(2022, 1, 1, 0, 0, 0, 0),
            ),
            (
                "2024-06-15T13:45:07.123456789",
                ymd_hmsn(2024, 6, 15, 13, 45, 7, 123_456_789),
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(
                parse_date_str(input).unwrap(),
                *expected,
                "wrapper: {}",
                input
            );
        }
    }

    #[test]
    fn rejects_propagate_through_wrapper() {
        for s in ["", "2022/01/01", "2022-01-01T00:00:00+02:00"] {
            assert!(parse_date_str(s).is_err(), "should error: {:?}", s);
        }
    }
}

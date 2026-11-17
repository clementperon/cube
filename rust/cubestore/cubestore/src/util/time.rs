//! Date-time helpers shared across the crate.

use jiff::Timestamp;

/// RFC 3339 in UTC with exactly three fractional digits and a `Z` suffix, e.g.
/// `2024-01-01T00:00:00.000Z`. Digits past the millisecond are truncated, not rounded.
pub fn to_rfc3339_millis(timestamp: Timestamp) -> String {
    timestamp.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// `2024-01-01 00:00:00.123 UTC`: how timestamps are rendered in the text columns of query
/// results, e.g. `created_at` in the row `CREATE TABLE` returns. The fraction is left out when
/// zero and otherwise has 3, 6 or 9 digits, whichever is the fewest that loses nothing.
pub fn to_utc_string(timestamp: Timestamp) -> String {
    let subsec_nanos = timestamp.subsec_nanosecond();
    let format = if subsec_nanos == 0 {
        "%Y-%m-%d %H:%M:%S UTC"
    } else if subsec_nanos % 1_000_000 == 0 {
        "%Y-%m-%d %H:%M:%S%.3f UTC"
    } else if subsec_nanos % 1_000 == 0 {
        "%Y-%m-%d %H:%M:%S%.6f UTC"
    } else {
        "%Y-%m-%d %H:%M:%S%.9f UTC"
    };
    timestamp.strftime(format).to_string()
}

/// The instant `unix_nanos` nanoseconds away from the Unix epoch.
pub fn timestamp_from_unix_nanos(unix_nanos: i64) -> Timestamp {
    // Infallible: `i64` nanoseconds only span the years 1677..=2262.
    Timestamp::from_nanosecond(i128::from(unix_nanos))
        .expect("i64 nanoseconds are always a valid Timestamp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_millis() {
        let format = |s: &str| to_rfc3339_millis(s.parse().unwrap());
        assert_eq!(format("2024-01-02T03:04:05Z"), "2024-01-02T03:04:05.000Z");
        assert_eq!(format("2024-01-02T03:04:05.1Z"), "2024-01-02T03:04:05.100Z");
        // Truncates.
        assert_eq!(
            format("2024-01-02T03:04:05.123999999Z"),
            "2024-01-02T03:04:05.123Z"
        );
        assert_eq!(
            format("1969-12-31T23:59:59.999999Z"),
            "1969-12-31T23:59:59.999Z"
        );
        // Always UTC.
        assert_eq!(
            format("2024-01-02T03:04:05.5+02:00"),
            "2024-01-02T01:04:05.500Z"
        );
    }

    #[test]
    fn utc_string() {
        let format = |s: &str| to_utc_string(s.parse().unwrap());
        assert_eq!(format("2024-01-02T03:04:05Z"), "2024-01-02 03:04:05 UTC");
        assert_eq!(
            format("2024-01-02T03:04:05.1Z"),
            "2024-01-02 03:04:05.100 UTC"
        );
        assert_eq!(
            format("2024-01-02T03:04:05.123Z"),
            "2024-01-02 03:04:05.123 UTC"
        );
        assert_eq!(
            format("2024-01-02T03:04:05.1234Z"),
            "2024-01-02 03:04:05.123400 UTC"
        );
        assert_eq!(
            format("2024-01-02T03:04:05.1234567Z"),
            "2024-01-02 03:04:05.123456700 UTC"
        );
        // Before the Unix epoch the sub-second part is negative.
        assert_eq!(
            format("1969-12-31T23:59:59.5Z"),
            "1969-12-31 23:59:59.500 UTC"
        );
        assert_eq!(
            format("1969-12-31T23:59:59.999999Z"),
            "1969-12-31 23:59:59.999999 UTC"
        );
        assert_eq!(format("0099-12-31T23:59:59Z"), "0099-12-31 23:59:59 UTC");
    }

    #[test]
    fn from_unix_nanos_covers_the_whole_i64_range() {
        assert_eq!(
            to_rfc3339_millis(timestamp_from_unix_nanos(i64::MIN)),
            "1677-09-21T00:12:43.145Z"
        );
        assert_eq!(
            to_rfc3339_millis(timestamp_from_unix_nanos(i64::MAX)),
            "2262-04-11T23:47:16.854Z"
        );
        assert_eq!(timestamp_from_unix_nanos(0), Timestamp::UNIX_EPOCH);
    }
}

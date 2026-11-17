use cubenativeutils::CubeError;
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{SignedDuration, Span, Timestamp, ZonedDateTime};
use lazy_static::lazy_static;
use regex::Regex;

pub struct QueryDateTimeHelper {}

lazy_static! {
    static ref DATE_TIME_LOCAL_MS_RE: Regex =
        Regex::new(r"^\d\d\d\d-\d\d-\d\dT\d\d:\d\d:\d\d\.\d\d\d$").unwrap();
    static ref DATE_TIME_LOCAL_U_RE: Regex =
        Regex::new(r"^\d\d\d\d-\d\d-\d\dT\d\d:\d\d:\d\d\.\d\d\d\d\d\d$").unwrap();
    static ref DATE_RE: Regex = Regex::new(r"^\d\d\d\d-\d\d-\d\d$").unwrap();
}

impl QueryDateTimeHelper {
    /// A partitioned pre-aggregation's date range bound, substituted by the
    /// orchestrator in the bound params only, never in the SQL text.
    pub fn is_partition_range(value: &str) -> bool {
        value == "__FROM_PARTITION_RANGE" || value == "__TO_PARTITION_RANGE"
    }

    /// Looks up an IANA time zone by name. Only the canonical spelling is
    /// accepted: jiff matches names case-insensitively and knows the
    /// `Etc/Unknown` placeholder, neither of which is a time zone a query
    /// can name.
    pub fn time_zone(name: &str) -> Option<TimeZone> {
        let tz = TimeZone::get(name).ok()?;
        (tz.iana_name() == Some(name)).then_some(tz)
    }

    pub fn parse_native_date_time(date: &str) -> Result<DateTime, CubeError> {
        let formats = &[
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S%.f",
        ];

        for format in formats {
            if let Ok(dt) = DateTime::strptime(format, date) {
                return Ok(dt);
            }
        }

        if let Ok(d) = Date::strptime("%Y-%m-%d", date) {
            return Ok(d.at(0, 0, 0, 0));
        }

        // Fallback as RFC3339/ISO8601 with 'Z' timezone
        if let Ok(ts) = date.parse::<Timestamp>() {
            return Ok(TimeZone::UTC.to_datetime(ts));
        }

        Err(CubeError::user(format!("Can't parse date: '{}'", date)))
    }

    /// `date` moved by a whole number of days on the calendar, `None` when
    /// the result leaves the supported range.
    pub fn add_days(date: Date, days: i64) -> Option<Date> {
        date.checked_add(Span::new().try_days(days).ok()?).ok()
    }

    /// The instant a local wall-clock time denotes in `tz`. A time that occurs
    /// twice (DST fold) resolves to its first occurrence, one that never occurs
    /// (DST gap) to the first valid local time after it.
    pub fn resolve_local(tz: &TimeZone, date: DateTime) -> Result<ZonedDateTime, CubeError> {
        if Self::is_nonexistent(tz, date) {
            Self::resolve_nonexistent(tz, date)
        } else {
            Self::first_occurrence(tz, date)
        }
    }

    fn is_nonexistent(tz: &TimeZone, date: DateTime) -> bool {
        matches!(
            tz.to_ambiguous_timestamp(date).offset(),
            AmbiguousOffset::Gap { .. }
        )
    }

    fn first_occurrence(tz: &TimeZone, date: DateTime) -> Result<ZonedDateTime, CubeError> {
        tz.to_ambiguous_zoned_datetime(date)
            .earlier()
            .map_err(|e| Self::out_of_range(tz, date, e))
    }

    fn out_of_range(tz: &TimeZone, date: DateTime, e: jiff::Error) -> CubeError {
        CubeError::user(format!(
            "Time {date} in timezone {} is out of the supported range: {e}",
            Self::time_zone_name(tz)
        ))
    }

    pub fn time_zone_name(tz: &TimeZone) -> &str {
        tz.iana_name().unwrap_or("unknown")
    }

    /// Resolves a nonexistent local time (DST gap) using binary search,
    /// mimicking `moment.tz` behavior — finds the nearest valid local time **after or equal to** the input.
    ///
    /// Assumes that any DST gap is less than 2 hours (true for almost all real-world time zones).
    fn resolve_nonexistent(tz: &TimeZone, date: DateTime) -> Result<ZonedDateTime, CubeError> {
        // Max time delta to look ahead — generous enough for most DST gaps
        let max_offset = SignedDuration::from_hours(2);
        let step = |dt: DateTime, by: SignedDuration| {
            dt.checked_add(by)
                .map_err(|e| Self::out_of_range(tz, date, e))
        };

        let mut low = date;
        let mut high = step(low, max_offset)?;

        // Ensure `high` is a valid local time (expand if needed)
        while Self::is_nonexistent(tz, high) {
            high = step(high, max_offset)?;
        }

        // Binary search for the first valid local time >= `naive`
        loop {
            let width = high.duration_since(low);
            if width <= SignedDuration::from_secs(1) {
                break;
            }
            let mid = step(low, width / 2)?;
            if Self::is_nonexistent(tz, mid) {
                // Still in the invalid range — move lower bound up
                low = mid;
            } else {
                // Found a valid or ambiguous time — narrow upper bound
                high = mid;
            }
        }

        // Return the first valid interpretation
        if Self::is_nonexistent(tz, high) {
            return Err(CubeError::user(format!(
                "Could not resolve nonexistent time {date} in timezone {}",
                Self::time_zone_name(tz)
            )));
        }
        Self::first_occurrence(tz, high)
    }

    /// The overlap of two date ranges; bounds are compared normalized but kept
    /// as given.
    pub fn intersect_date_ranges(
        a: &(String, String),
        b: &(String, String),
        precision: u32,
    ) -> Result<(String, String), CubeError> {
        let from = if Self::format_from_date(&a.0, precision)?
            >= Self::format_from_date(&b.0, precision)?
        {
            a.0.clone()
        } else {
            b.0.clone()
        };
        let to = if Self::format_to_date(&a.1, precision)? <= Self::format_to_date(&b.1, precision)?
        {
            a.1.clone()
        } else {
            b.1.clone()
        };
        Ok((from, to))
    }

    pub fn format_from_date(date: &str, precision: u32) -> Result<String, CubeError> {
        if precision == 3 {
            if DATE_TIME_LOCAL_MS_RE.is_match(date) {
                return Ok(date.to_string());
            }
        } else if precision == 6 {
            if date.len() == 23 && DATE_TIME_LOCAL_MS_RE.is_match(date) {
                return Ok(format!("{}000", date));
            } else if date.len() == 26 && DATE_TIME_LOCAL_U_RE.is_match(date) {
                return Ok(date.to_string());
            }
        } else {
            return Err(CubeError::user(format!(
                "Unsupported timestamp precision: {}",
                precision
            )));
        }

        if DATE_RE.is_match(date) {
            return Ok(format!(
                "{}T00:00:00.{}",
                date,
                "0".repeat(precision as usize)
            ));
        }
        Ok(date.to_string())
    }

    pub fn format_to_date(date: &str, precision: u32) -> Result<String, CubeError> {
        if precision == 3 {
            if DATE_TIME_LOCAL_MS_RE.is_match(date) {
                return Ok(date.to_string());
            }
        } else if precision == 6 {
            if date.len() == 23 && DATE_TIME_LOCAL_MS_RE.is_match(date) {
                if date.ends_with(".999") {
                    return Ok(format!("{}999", date));
                }
                return Ok(format!("{}000", date));
            } else if date.len() == 26 && DATE_TIME_LOCAL_U_RE.is_match(date) {
                return Ok(date.to_string());
            }
        } else {
            return Err(CubeError::user(format!(
                "Unsupported timestamp precision: {}",
                precision
            )));
        }

        if DATE_RE.is_match(date) {
            return Ok(format!(
                "{}T23:59:59.{}",
                date,
                "9".repeat(precision as usize)
            ));
        }

        Ok(date.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn time_zone_takes_canonical_names_only() {
        assert_eq!(QueryDateTimeHelper::time_zone("UTC"), Some(TimeZone::UTC));
        assert_eq!(
            QueryDateTimeHelper::time_zone("America/New_York")
                .unwrap()
                .iana_name(),
            Some("America/New_York")
        );
        // An alias of UTC stays a zone of its own.
        assert_ne!(
            QueryDateTimeHelper::time_zone("Etc/UTC"),
            Some(TimeZone::UTC)
        );
        for name in ["utc", "america/new_york", "Etc/Unknown", "Mars/Olympus", ""] {
            assert_eq!(QueryDateTimeHelper::time_zone(name), None, "{name}");
        }
    }

    #[test]
    fn a_time_past_the_supported_range_is_an_error() {
        // Instants end at 9999-12-30T22:00:00.999999999Z.
        let resolve = |tz: &str, date: &str| {
            let date = QueryDateTimeHelper::parse_native_date_time(date).unwrap();
            QueryDateTimeHelper::resolve_local(&TimeZone::get(tz).unwrap(), date)
        };
        assert!(resolve("UTC", "9999-12-30T22:00:00.999").is_ok());
        assert!(resolve("UTC", "9999-12-31T23:59:59.999").is_err());
        assert!(resolve("Asia/Tokyo", "9999-12-31T07:00:00.999").is_ok());
        assert!(resolve("Asia/Tokyo", "9999-12-31T07:00:01.000").is_err());
    }

    #[test]
    fn test_parse_native_date_time() {
        assert_eq!(
            QueryDateTimeHelper::parse_native_date_time("2021-01-01").unwrap(),
            date(2021, 1, 1).at(0, 0, 0, 0)
        );
        assert_eq!(
            QueryDateTimeHelper::parse_native_date_time("2021-01-01T12:10:15").unwrap(),
            date(2021, 1, 1).at(12, 10, 15, 0)
        );
        assert_eq!(
            QueryDateTimeHelper::parse_native_date_time("2021-01-01 12:10:15").unwrap(),
            date(2021, 1, 1).at(12, 10, 15, 0)
        );
        assert_eq!(
            QueryDateTimeHelper::parse_native_date_time("2021-01-01 12:10:15.345").unwrap(),
            date(2021, 1, 1).at(12, 10, 15, 345_000_000)
        );
        assert_eq!(
            QueryDateTimeHelper::parse_native_date_time("2021-01-01T12:10:15.345").unwrap(),
            date(2021, 1, 1).at(12, 10, 15, 345_000_000)
        );
        // Test parsing with 'Z' timezone (UTC)
        assert_eq!(
            QueryDateTimeHelper::parse_native_date_time("2024-01-01T10:15:00Z").unwrap(),
            date(2024, 1, 1).at(10, 15, 0, 0)
        );
        assert_eq!(
            QueryDateTimeHelper::parse_native_date_time("2024-01-01T10:15:00.123Z").unwrap(),
            date(2024, 1, 1).at(10, 15, 0, 123_000_000)
        );
    }
}

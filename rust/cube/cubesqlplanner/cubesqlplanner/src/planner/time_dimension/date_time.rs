use super::{QueryDateTimeHelper, SqlInterval};
use cubenativeutils::CubeError;
use jiff::civil::{Date, DateTime, Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ZonedDateTime};
use std::cmp::Ord;
use std::str::FromStr;

#[derive(Clone, PartialEq, PartialOrd, Eq, Ord, Debug)]
pub struct QueryDateTime {
    date_time: ZonedDateTime,
}

const MIDNIGHT: Time = Time::MIN;

/// The first day with local times that may be past the last instant, at the most negative
/// offset a time zone can have.
const LAST_INSTANT_TAIL: Date = Date::constant(9999, 12, 29);

impl ToString for QueryDateTime {
    fn to_string(&self) -> String {
        self.default_format()
    }
}

/// The marker an interval carries when its side reaches without limit.
pub const UNBOUNDED_INTERVAL: &str = "unbounded";

/// `date` moved by `interval` on the wall clock, or `None` for an `unbounded`
/// side — which no date states. A side with no interval keeps the date as it is.
///
/// Wall clock rather than absolute time because this is what SQL interval
/// arithmetic over a naive local timestamp does, and where a series places its
/// own points; across a daylight-saving transition the two differ by the offset.
pub fn shift_bound_wall_clock(
    tz: &TimeZone,
    date: &str,
    interval: &Option<String>,
    subtract: bool,
) -> Result<Option<String>, CubeError> {
    let interval = match interval.as_deref() {
        Some(UNBOUNDED_INTERVAL) => return Ok(None),
        Some(interval) => SqlInterval::from_str(interval)?,
        None => return Ok(Some(date.to_string())),
    };
    let interval = if subtract {
        interval.inverse()
    } else {
        interval
    };
    Ok(Some(
        QueryDateTime::from_date_str(tz, date)?
            .add_interval_wall_clock(&interval)?
            .default_format(),
    ))
}

impl QueryDateTime {
    pub fn new(date_time: ZonedDateTime) -> QueryDateTime {
        QueryDateTime { date_time }
    }
    /// The current instant, on the wall clock of `tz`.
    pub fn now(tz: &TimeZone) -> QueryDateTime {
        Self::new(Timestamp::now().to_zoned_datetime(tz.clone()))
    }
    pub fn from_date_str(tz: &TimeZone, date: &str) -> Result<Self, CubeError> {
        let local_dt = QueryDateTimeHelper::parse_native_date_time(date)?;
        Self::from_local_date_time(tz, local_dt)
    }

    pub fn from_local_date_time(tz: &TimeZone, date: DateTime) -> Result<Self, CubeError> {
        let date_time = QueryDateTimeHelper::resolve_local(tz, date)?;
        Ok(Self { date_time })
    }

    /// Start of the given predefined granularity containing this instant, on the local wall clock.
    ///
    /// A midnight that a DST gap swallows resolves to whatever instant the gap pushes it to; that
    /// instant is still the start of its day.
    pub fn start_of(&self, granularity: &str) -> Result<Self, CubeError> {
        let dt = self.naive_local();
        let date = dt.date();
        let time = dt.time();

        let (start_date, start_time) = match granularity {
            "year" => (Date::new(date.year(), 1, 1).ok(), Some(MIDNIGHT)),
            "quarter" => (
                Date::new(date.year(), date.month() - (date.month() - 1) % 3, 1).ok(),
                Some(MIDNIGHT),
            ),
            "month" => (Date::new(date.year(), date.month(), 1).ok(), Some(MIDNIGHT)),
            "week" => (
                QueryDateTimeHelper::add_days(
                    date,
                    -(date.weekday().to_monday_zero_offset() as i64),
                ),
                Some(MIDNIGHT),
            ),
            "day" => (Some(date), Some(MIDNIGHT)),
            "hour" => (Some(date), Time::new(time.hour(), 0, 0, 0).ok()),
            "minute" => (Some(date), Time::new(time.hour(), time.minute(), 0, 0).ok()),
            "second" => (
                Some(date),
                Time::new(time.hour(), time.minute(), time.second(), 0).ok(),
            ),
            other => {
                return Err(CubeError::internal(format!(
                    "Unexpected granularity '{other}' in date alignment check"
                )))
            }
        };

        let (start_date, start_time) = start_date.zip(start_time).ok_or_else(|| {
            CubeError::internal(format!("Failed to build start of {granularity} for {dt}"))
        })?;

        Self::from_local_date_time(
            self.date_time.time_zone(),
            DateTime::from_parts(start_date, start_time),
        )
    }

    /// Whether this instant is the first one of the given predefined granularity.
    pub fn is_start_of(&self, granularity: &str) -> Result<bool, CubeError> {
        Ok(self.start_of(granularity)?.naive_local() == self.naive_local())
    }

    /// How far this instant sits past the start of the given predefined granularity, as a calendar
    /// interval rather than a duration. Sub-second parts are dropped.
    pub fn offset_from_start_of(&self, granularity: &str) -> Result<SqlInterval, CubeError> {
        let dt = self.naive_local();

        let (month, day) = match granularity {
            "year" => (dt.month() as i32 - 1, dt.day() as i32 - 1),
            "quarter" => ((dt.month() as i32 - 1) % 3, dt.day() as i32 - 1),
            "month" => (0, dt.day() as i32 - 1),
            "week" => (0, dt.weekday().to_monday_zero_offset() as i32),
            "day" | "hour" | "minute" | "second" => (0, 0),
            other => {
                return Err(CubeError::internal(format!(
                    "Unexpected granularity '{other}' in date alignment check"
                )))
            }
        };

        let (hour, minute, second) = match granularity {
            "hour" => (0, dt.minute(), dt.second()),
            "minute" => (0, 0, dt.second()),
            "second" => (0, 0, 0),
            _ => (dt.hour(), dt.minute(), dt.second()),
        };

        Ok(SqlInterval::new(
            0,
            0,
            month,
            0,
            day,
            hour as i32,
            minute as i32,
            second as i32,
        ))
    }

    pub fn month(&self) -> u32 {
        self.date_time.month() as u32
    }

    pub fn day(&self) -> u32 {
        self.date_time.day() as u32
    }

    /// The whole sub-second part, in nanoseconds.
    pub fn nanosecond(&self) -> u32 {
        self.date_time.subsec_nanosecond() as u32
    }

    pub fn date_time(&self) -> &ZonedDateTime {
        &self.date_time
    }

    pub fn naive_local(&self) -> DateTime {
        self.date_time.datetime()
    }

    pub fn naive_utc(&self) -> DateTime {
        TimeZone::UTC.to_datetime(self.date_time.timestamp())
    }

    pub fn format(&self, format: &str) -> String {
        self.date_time.strftime(format).to_string()
    }

    pub fn default_format(&self) -> String {
        self.date_time.strftime("%Y-%m-%dT%H:%M:%S%.3f").to_string()
    }

    fn time_part(interval: &SqlInterval) -> SignedDuration {
        SignedDuration::from_hours(interval.hour as i64)
            + SignedDuration::from_mins(interval.minute as i64)
            + SignedDuration::from_secs(interval.second as i64)
    }

    fn overflow(&self, interval: &SqlInterval) -> CubeError {
        CubeError::internal(format!(
            "Failed to compute valid date while adding interval {:?} to date {}",
            interval, self.date_time
        ))
    }

    pub fn add_interval(&self, interval: &SqlInterval) -> Result<Self, CubeError> {
        // For time-only intervals (hour, minute, second), use UTC arithmetic to avoid DST issues
        let is_time_only = interval.year == 0
            && interval.quarter == 0
            && interval.month == 0
            && interval.week == 0
            && interval.day == 0;

        if is_time_only {
            // Use UTC-based arithmetic for time intervals
            let new_datetime = self
                .date_time
                .checked_add(Self::time_part(interval))
                .map_err(|_| self.overflow(interval))?;
            return Ok(Self::new(new_datetime));
        }

        // For date-based intervals, use local time arithmetic
        let date = self.naive_local().date();

        // Step 1: add years and months with fallback logic. A quarter is three
        // months — `SqlInterval` keeps it in a field of its own and never folds
        // it in, so anything reading `month` alone would drop it silently.
        let mut year = date.year() as i32 + interval.year;
        let mut month = date.month() as i32 + interval.quarter * 3 + interval.month;

        while month > 12 {
            year += 1;
            month -= 12;
        }
        while month < 1 {
            year -= 1;
            month += 12;
        }

        let day = date.day();
        let ymd = |day: i8| Date::new(i16::try_from(year).ok()?, month as i8, day).ok();
        // Adjust for overflowed day in shorter months (e.g. Feb 30 → Feb 28)
        let adjusted_date = ymd(day)
            .or_else(|| (1..=31).rev().find_map(ymd))
            .ok_or_else(|| self.overflow(interval))?;

        // Step 2: Add weeks and days
        let adjusted_date = QueryDateTimeHelper::add_days(
            adjusted_date,
            interval.week as i64 * 7 + interval.day as i64,
        )
        .ok_or_else(|| self.overflow(interval))?;

        // Step 3: Recombine with original time
        let time = self.naive_local().time();
        let naive = DateTime::from_parts(adjusted_date, time);

        // Step 4: Add time-based parts
        let naive = naive
            .checked_add(Self::time_part(interval))
            .map_err(|_| self.overflow(interval))?;

        Self::from_local_date_time(self.date_time.time_zone(), naive)
    }

    /// `add_interval`, but a time-only interval moves the wall clock rather
    /// than the instant. An hour of interval is an hour of the clock, the way
    /// a series places its points and the way SQL interval arithmetic reads it;
    /// across a daylight-saving transition the two differ by the offset.
    pub fn add_interval_wall_clock(&self, interval: &SqlInterval) -> Result<Self, CubeError> {
        let carries_date = interval.year != 0
            || interval.quarter != 0
            || interval.month != 0
            || interval.week != 0
            || interval.day != 0;
        if carries_date {
            return self.add_interval(interval);
        }
        self.add_duration(Self::time_part(interval))
    }

    pub fn sub_interval(&self, interval: &SqlInterval) -> Result<Self, CubeError> {
        self.add_interval(&interval.inverse())
    }

    pub fn add_duration(&self, duration: SignedDuration) -> Result<Self, CubeError> {
        let native = self.naive_local().checked_add(duration).map_err(|e| {
            CubeError::internal(format!(
                "Failed to add {duration:?} to date {}: {e}",
                self.date_time
            ))
        })?;
        Self::from_local_date_time(self.date_time.time_zone(), native)
    }

    pub fn granularity(&self) -> String {
        Self::wall_clock_granularity(self.naive_local())
    }

    /// Whether a local time is in the last days of year 9999, where a time zone may have no
    /// instant left for it: instants end at `9999-12-30T22:00:00.999999999Z`.
    pub fn is_past_last_instant(local: DateTime) -> bool {
        local.date() >= LAST_INSTANT_TAIL
    }

    /// The granularity started by the instant one millisecond after `end`, the inclusive end of
    /// a date range.
    ///
    /// A range closing in the last days of year 9999 is open-ended in practice (`9999-12-31` is
    /// a common "no upper bound"). Its end may have no instant, and the millisecond after
    /// `9999-12-31T23:59:59.999` is past every date, so there the wall clock is read as is: no
    /// time zone has a transition that late.
    pub fn granularity_after_range_end(tz: &TimeZone, end: &str) -> Result<String, CubeError> {
        let tick = SignedDuration::from_millis(1);
        let local = QueryDateTimeHelper::parse_native_date_time(end)?;
        if !Self::is_past_last_instant(local) {
            return Ok(Self::from_local_date_time(tz, local)?
                .add_duration(tick)?
                .granularity());
        }
        Ok(match local.checked_add(tick) {
            Ok(next) => Self::wall_clock_granularity(next),
            // The next millisecond is in year 10000: its first instant when `end` is the last
            // whole millisecond of year 9999, a fraction of a millisecond into it otherwise.
            Err(_) if local.subsec_nanosecond() == 999_000_000 => "year".to_string(),
            Err(_) => "second".to_string(),
        })
    }

    fn wall_clock_granularity(local: DateTime) -> String {
        let time = local.time();

        let weekday = local.weekday();
        let is_zero_time = |t: Time| {
            t.hour() == 0 && t.minute() == 0 && t.second() == 0 && t.subsec_nanosecond() == 0
        };

        if local.month() == 1 && local.day() == 1 && is_zero_time(time) {
            "year".to_string()
        } else if local.day() == 1 && is_zero_time(time) {
            "month".to_string()
        } else if weekday == Weekday::Monday && is_zero_time(time) {
            "week".to_string()
        } else if is_zero_time(time) {
            "day".to_string()
        } else if time.minute() == 0 && time.second() == 0 && time.subsec_nanosecond() == 0 {
            "hour".to_string()
        } else if time.second() == 0 && time.subsec_nanosecond() == 0 {
            "minute".to_string()
        } else if time.subsec_nanosecond() == 0 {
            "second".to_string()
        } else {
            "second".to_string()
        }
    }

    pub fn align_to_origin(
        &self,
        origin: &Self,
        interval: &SqlInterval,
    ) -> Result<Self, CubeError> {
        let mut aligned = self.clone();
        let mut offset = origin.clone();

        if self < origin {
            while &offset > self {
                offset = offset.sub_interval(interval)?;
            }
            aligned = offset;
        } else {
            while &offset < self {
                aligned = offset.clone();
                offset = offset.add_interval(interval)?;
            }

            if &offset == self {
                aligned = offset;
            }
        }

        Ok(aligned)
    }
}

#[cfg(test)]
mod shift_bound_tests {
    use super::*;

    // A bound folded out of an end-of-day anchor stays an end-of-day, and on a
    // dialect keeping microseconds it has to survive being normalised again:
    // read back as a range start it would lose its sub-second tail, and the
    // window — whose frame this predicate *is* — would silently widen.
    #[test]
    fn an_end_of_day_bound_keeps_its_tail_at_microsecond_precision() {
        let anchor = QueryDateTimeHelper::format_to_date("2024-01-20", 6).unwrap();
        assert_eq!(anchor, "2024-01-20T23:59:59.999999");

        let shifted =
            shift_bound_wall_clock(&TimeZone::UTC, &anchor, &Some("7 day".to_string()), true)
                .unwrap()
                .unwrap();

        assert_eq!(
            QueryDateTimeHelper::format_to_date(&shifted, 6).unwrap(),
            "2024-01-13T23:59:59.999999",
            "normalised as the anchor was"
        );
        assert_eq!(
            QueryDateTimeHelper::format_from_date(&shifted, 6).unwrap(),
            "2024-01-13T23:59:59.999000",
            "normalised as a range start instead, which is what the bug did"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil;

    #[test]
    fn granularity_after_range_end_reads_the_end_of_year_9999_on_the_wall_clock() {
        let after = |zone: &str, end: &str| {
            QueryDateTime::granularity_after_range_end(&TimeZone::get(zone).unwrap(), end).unwrap()
        };
        // Year 10000 starts right after the last millisecond of year 9999, in any time zone.
        for zone in ["UTC", "America/Los_Angeles", "Pacific/Kiritimati"] {
            assert_eq!(after(zone, "9999-12-31T23:59:59.999"), "year", "{zone}");
            assert_eq!(
                after(zone, "9999-12-31T23:59:59.999999"),
                "second",
                "{zone}"
            );
            assert_eq!(after(zone, "9999-12-30T23:59:59.999"), "day", "{zone}");
            assert_eq!(after(zone, "9999-12-31T22:59:59.999"), "hour", "{zone}");
            assert_eq!(after(zone, "9999-11-30T23:59:59.999"), "month", "{zone}");
        }
        // Any other range end resolves in its time zone: Paraguay's 2023-10-01 has no midnight.
        assert_eq!(after("UTC", "2023-09-30T23:59:59.999"), "month");
        assert_eq!(after("America/Asuncion", "2023-09-30T23:59:59.999"), "hour");
    }

    #[test]
    fn is_start_of_accepts_a_midnight_swallowed_by_a_dst_gap() {
        // Paraguay springs forward at midnight, so 2023-10-01T00:00 does not exist locally and
        // resolves to 01:00. It is still the first instant of that day, month and quarter.
        let tz = TimeZone::get("America/Asuncion").unwrap();
        let d = QueryDateTime::from_date_str(&tz, "2023-10-01").unwrap();

        assert_eq!(d.default_format(), "2023-10-01T01:00:00.000");
        assert!(d.is_start_of("day").unwrap());
        assert!(d.is_start_of("month").unwrap());
        assert!(d.is_start_of("quarter").unwrap());
        assert!(!d.is_start_of("year").unwrap());
    }

    #[test]
    fn canadian_zones_stay_on_daylight_time_after_2026_11_01() {
        // tzdb 2026b-2026e cancel the 2026-11-01 fall-back; a stale bundled tzdb is an hour late.
        for (zone, day, utc) in [
            ("America/Vancouver", "2026-11-02", "2026-11-02T07:00:00"),
            ("America/Edmonton", "2026-11-02", "2026-11-02T06:00:00"),
            ("America/Inuvik", "2026-11-02", "2026-11-02T06:00:00"),
            ("America/Winnipeg", "2026-11-02", "2026-11-02T05:00:00"),
            ("America/Vancouver", "2025-11-03", "2025-11-03T08:00:00"),
        ] {
            let d = QueryDateTime::from_date_str(&TimeZone::get(zone).unwrap(), day).unwrap();
            assert_eq!(
                d.naive_utc().strftime("%Y-%m-%dT%H:%M:%S").to_string(),
                utc,
                "{zone} {day}"
            );
        }
    }

    #[test]
    fn is_start_of_still_rejects_a_real_time_component() {
        let tz = TimeZone::get("America/Asuncion").unwrap();
        // 2023-10-02 has an ordinary midnight, so 01:00 on it is genuinely mid-day.
        let d = QueryDateTime::from_date_str(&tz, "2023-10-02T01:00:00").unwrap();
        assert!(!d.is_start_of("day").unwrap());
        assert!(d.is_start_of("hour").unwrap());
    }

    #[test]
    fn offset_from_start_of_measures_the_local_wall_clock() {
        let tz = TimeZone::get("Etc/GMT-3").unwrap();
        let d = QueryDateTime::from_date_str(&tz, "2024-04-15T06:30:15").unwrap();

        assert_eq!(
            d.offset_from_start_of("year").unwrap(),
            SqlInterval::new(0, 0, 3, 0, 14, 6, 30, 15)
        );
        assert_eq!(
            d.offset_from_start_of("quarter").unwrap(),
            SqlInterval::new(0, 0, 0, 0, 14, 6, 30, 15)
        );
        assert_eq!(
            d.offset_from_start_of("month").unwrap(),
            SqlInterval::new(0, 0, 0, 0, 14, 6, 30, 15)
        );
        // 2024-04-15 is a Monday.
        assert_eq!(
            d.offset_from_start_of("week").unwrap(),
            SqlInterval::new(0, 0, 0, 0, 0, 6, 30, 15)
        );
        assert_eq!(
            d.offset_from_start_of("day").unwrap(),
            SqlInterval::new(0, 0, 0, 0, 0, 6, 30, 15)
        );
        assert_eq!(
            d.offset_from_start_of("hour").unwrap(),
            SqlInterval::new(0, 0, 0, 0, 0, 0, 30, 15)
        );
        assert_eq!(
            d.offset_from_start_of("minute").unwrap(),
            SqlInterval::new(0, 0, 0, 0, 0, 0, 0, 15)
        );
    }

    #[test]
    fn test_parse_date_time() {
        let tz = TimeZone::get("Etc/GMT-3").unwrap();
        let parsed = QueryDateTime::from_date_str(&tz, "2021-01-01").unwrap();
        assert_eq!(
            parsed.naive_utc(),
            civil::date(2020, 12, 31).at(21, 0, 0, 0)
        );

        let tz = TimeZone::get("Etc/GMT-3").unwrap();
        let parsed = QueryDateTime::from_date_str(&tz, "2021-01-01 03:15:20").unwrap();

        assert_eq!(parsed.naive_utc(), civil::date(2021, 1, 1).at(0, 15, 20, 0));

        //Ambiguous time
        let tz = TimeZone::get("America/New_York").unwrap();
        let parsed = QueryDateTime::from_date_str(&tz, "2024-11-03 01:30:00").unwrap();
        assert_eq!(parsed.naive_utc(), civil::date(2024, 11, 3).at(5, 30, 0, 0));
        //Not exist time
        let tz = TimeZone::get("America/New_York").unwrap();
        let parsed = QueryDateTime::from_date_str(&tz, "2024-03-10 02:30:00").unwrap();
        assert_eq!(parsed.naive_utc(), civil::date(2024, 3, 10).at(7, 0, 0, 0));
    }
    #[test]
    fn test_start_of_year() {
        let tz = TimeZone::get("Etc/GMT-3").unwrap();
        let date = QueryDateTime::from_date_str(&tz, "2024-11-03 01:30:00").unwrap();
        let start = date.start_of("year").unwrap();
        assert_eq!(start.naive_utc(), civil::date(2023, 12, 31).at(21, 0, 0, 0));
    }
    #[test]
    fn test_add_interval() {
        let tz = TimeZone::get("America/Los_Angeles").unwrap();

        let date = QueryDateTime::from_date_str(&tz, "2024-03-10T03:00:00").unwrap();
        let interval = "1 hour".parse::<SqlInterval>().unwrap();
        let result = date.add_interval(&interval).unwrap().naive_utc();
        assert_eq!(result, civil::date(2024, 3, 10).at(11, 0, 0, 0));

        let tz = TimeZone::get("Etc/GMT-3").unwrap();

        let date = QueryDateTime::from_date_str(&tz, "2024-11-03 01:30:00").unwrap();
        let interval = "4 hours 2 minutes 10 second"
            .parse::<SqlInterval>()
            .unwrap();
        let result = date.add_interval(&interval).unwrap().naive_utc();
        assert_eq!(result, civil::date(2024, 11, 3).at(2, 32, 10, 0));

        let date = QueryDateTime::from_date_str(&tz, "2024-11-03 01:30:00").unwrap();
        let interval = "2 hours -2 minutes 10 second"
            .parse::<SqlInterval>()
            .unwrap();
        let result = date.add_interval(&interval).unwrap().naive_utc();
        assert_eq!(result, civil::date(2024, 11, 3).at(0, 28, 10, 0));

        let date = QueryDateTime::from_date_str(&tz, "2024-11-03 4:30:00").unwrap();
        let interval = "-4 hours -31 minutes 10 second"
            .parse::<SqlInterval>()
            .unwrap();
        let result = date.add_interval(&interval).unwrap().naive_utc();
        assert_eq!(result, civil::date(2024, 11, 2).at(20, 59, 10, 0));

        let date = QueryDateTime::from_date_str(&tz, "2024-02-03 01:30:00").unwrap();
        let interval = "1 week".parse::<SqlInterval>().unwrap();
        let result = date.add_interval(&interval).unwrap().naive_local();
        assert_eq!(result, civil::date(2024, 2, 10).at(1, 30, 0, 0));

        let date = QueryDateTime::from_date_str(&tz, "2024-02-03 01:30:00").unwrap();
        let interval = "1 month 1 week".parse::<SqlInterval>().unwrap();
        let result = date.add_interval(&interval).unwrap().naive_local();
        assert_eq!(result, civil::date(2024, 3, 10).at(1, 30, 0, 0));

        let date = QueryDateTime::from_date_str(&tz, "2024-02-03 01:30:00").unwrap();
        let interval = "1 year 1 month 1 week 3 minute"
            .parse::<SqlInterval>()
            .unwrap();
        let result = date.add_interval(&interval).unwrap().naive_local();
        assert_eq!(result, civil::date(2025, 3, 10).at(1, 33, 0, 0));

        let date = QueryDateTime::from_date_str(&tz, "2024-02-03 01:30:00").unwrap();
        let interval = "11 month 1 week 3 minute".parse::<SqlInterval>().unwrap();
        let result = date.add_interval(&interval).unwrap().naive_local();
        assert_eq!(result, civil::date(2025, 1, 10).at(1, 33, 0, 0));
    }
    #[test]
    fn test_add_interval_age_cases() {
        let tz = TimeZone::get("Etc/GMT-3").unwrap();

        let date = QueryDateTime::from_date_str(&tz, "2024-01-31 01:30:00").unwrap();
        let interval = "1 month 3 minute".parse::<SqlInterval>().unwrap();
        let result = date.add_interval(&interval).unwrap().naive_local();
        assert_eq!(result, civil::date(2024, 2, 29).at(1, 33, 0, 0));

        let date = QueryDateTime::from_date_str(&tz, "2024-02-29 01:30:00").unwrap();
        let interval = "1 year 3 minute".parse::<SqlInterval>().unwrap();
        let result = date.add_interval(&interval).unwrap().naive_local();
        assert_eq!(result, civil::date(2025, 2, 28).at(1, 33, 0, 0));
    }
    #[test]
    fn test_align_to_origin() {
        let tz = TimeZone::get("Etc/GMT-3").unwrap();
        let date = QueryDateTime::from_date_str(&tz, "2024-01-31").unwrap();
        let interval = "1 day".parse::<SqlInterval>().unwrap();
        let origin = date.start_of("year").unwrap();
        let result = date.align_to_origin(&origin, &interval).unwrap();
        assert_eq!(
            result.naive_local(),
            civil::date(2024, 1, 31).at(0, 0, 0, 0)
        );

        let tz = TimeZone::get("Etc/GMT-3").unwrap();
        let date = QueryDateTime::from_date_str(&tz, "2024-01-31").unwrap();
        let interval = "2 day".parse::<SqlInterval>().unwrap();
        let origin = QueryDateTime::from_date_str(&tz, "2024-01-30").unwrap();
        let result = date.align_to_origin(&origin, &interval).unwrap();
        assert_eq!(
            result.naive_local(),
            civil::date(2024, 1, 30).at(0, 0, 0, 0)
        );
        let tz = TimeZone::get("Etc/GMT-3").unwrap();
        let date = QueryDateTime::from_date_str(&tz, "2024-01-31").unwrap();
        let interval = "2 month".parse::<SqlInterval>().unwrap();
        let origin = QueryDateTime::from_date_str(&tz, "2024-05-15").unwrap();
        let result = date.align_to_origin(&origin, &interval).unwrap();
        assert_eq!(
            result.naive_local(),
            civil::date(2024, 1, 15).at(0, 0, 0, 0)
        );

        let tz = TimeZone::get("America/Los_Angeles").unwrap();
        let date = QueryDateTime::from_date_str(&tz, "2017-01-01T00:10:00").unwrap();
        let interval = "1 hour".parse::<SqlInterval>().unwrap();
        let origin = QueryDateTime::from_date_str(&tz, "2025-01-01T00:10:00").unwrap();
        let result = date.align_to_origin(&origin, &interval).unwrap();
        assert_eq!(
            result.naive_local(),
            civil::date(2017, 1, 1).at(0, 10, 0, 0)
        );
    }
}

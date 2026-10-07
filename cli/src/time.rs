//! UTC timestamps, run-id stamps and human durations, without a date crate.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A UTC calendar time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utc {
    /// Year.
    pub year: i64,
    /// Month, 1-12.
    pub month: u32,
    /// Day, 1-31.
    pub day: u32,
    /// Hour.
    pub hour: u32,
    /// Minute.
    pub minute: u32,
    /// Second.
    pub second: u32,
}

impl Utc {
    /// The calendar time of seconds since the Unix epoch.
    pub fn from_unix(secs: i64) -> Utc {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);

        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = yoe + era * 400 + i64::from(month <= 2);

        Utc {
            year,
            month,
            day,
            hour: (rem / 3_600) as u32,
            minute: (rem % 3_600 / 60) as u32,
            second: (rem % 60) as u32,
        }
    }

    /// The current time.
    pub fn now() -> Utc {
        Utc::from_system(SystemTime::now())
    }

    /// The calendar time of a `SystemTime`.
    pub fn from_system(time: SystemTime) -> Utc {
        let secs = match time.duration_since(UNIX_EPOCH) {
            Ok(after) => after.as_secs() as i64,
            Err(before) => -(before.duration().as_secs() as i64),
        };
        Utc::from_unix(secs)
    }

    /// `20261006T210311Z`, the run-id prefix.
    pub fn stamp(&self) -> String {
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `2026-10-06T21:03:11Z`.
    pub fn rfc3339(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// A calendar day (UTC), counted in days since 1970-01-01, for the dated
/// store policy table and release dates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(pub i64);

impl Day {
    /// The day of a calendar date (Howard Hinnant's days_from_civil).
    pub fn from_civil(year: i64, month: u32, day: u32) -> Day {
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let yoe = year.rem_euclid(400);
        let month = i64::from(month);
        let mp = if month > 2 { month - 3 } else { month + 9 };
        let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Day(era * 146_097 + doe - 719_468)
    }

    /// Parses `YYYY-MM-DD`.
    pub fn parse(text: &str) -> Option<Day> {
        let mut parts = text.trim().splitn(3, '-');
        let year: i64 = parts.next()?.parse().ok()?;
        let month: u32 = parts.next()?.parse().ok()?;
        let day: u32 = parts.next()?.parse().ok()?;
        let valid = (1..=12).contains(&month) && (1..=31).contains(&day);
        let parsed = Day::from_civil(year, month, day);
        // Reject days that roll over into the next month (2026-02-30).
        (valid && parsed.to_utc().day == day).then_some(parsed)
    }

    /// Today: `ICM_TODAY` (`YYYY-MM-DD`, for icm's own tests), else the
    /// system clock.
    pub fn today() -> Day {
        std::env::var("ICM_TODAY")
            .ok()
            .and_then(|text| Day::parse(&text))
            .unwrap_or_else(|| {
                let now = Utc::now();
                Day::from_civil(now.year, now.month, now.day)
            })
    }

    /// Midnight of the day.
    pub fn to_utc(self) -> Utc {
        Utc::from_unix(self.0 * 86_400)
    }

    /// The day `days` later.
    pub fn plus(self, days: i64) -> Day {
        Day(self.0 + days)
    }
}

impl std::fmt::Display for Day {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let utc = self.to_utc();
        write!(f, "{:04}-{:02}-{:02}", utc.year, utc.month, utc.day)
    }
}

/// Parses `500ms`, `30s`, `1.5s`, `10m`, `2h` or a bare number of seconds.
pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let input = input.trim();
    let (number, unit) = match input.find(|c: char| c.is_ascii_alphabetic()) {
        Some(index) => input.split_at(index),
        None => (input, "s"),
    };

    let value: f64 = number
        .trim()
        .parse()
        .map_err(|_| format!("`{input}` is not a duration (examples: 500ms, 30s, 10m, 1h)"))?;

    if !value.is_finite() || value < 0.0 {
        return Err(format!("`{input}` is not a non-negative duration"));
    }

    let seconds = match unit.trim() {
        "ms" => value / 1_000.0,
        "s" | "sec" | "secs" => value,
        "m" | "min" | "mins" => value * 60.0,
        "h" | "hr" | "hrs" => value * 3_600.0,
        other => {
            return Err(format!(
                "`{input}` has an unknown unit `{other}` (use ms, s, m or h)"
            ));
        }
    };

    Ok(Duration::from_secs_f64(seconds))
}

/// `41.2s`, `850ms`, `3m05s`.
pub fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else if millis < 60_000 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        let secs = duration.as_secs();
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(Utc::from_unix(0).rfc3339(), "1970-01-01T00:00:00Z");
        assert_eq!(
            Utc::from_unix(951_782_400).rfc3339(),
            "2000-02-29T00:00:00Z"
        );
        // 2026-10-06T21:03:11Z
        assert_eq!(Utc::from_unix(1_791_320_591).stamp(), "20261006T210311Z");
        assert_eq!(Utc::from_unix(-1).rfc3339(), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn days_round_trip() {
        assert_eq!(Day::from_civil(1970, 1, 1), Day(0));
        assert_eq!(Day::parse("2026-10-06").unwrap().to_string(), "2026-10-06");
        assert_eq!(Day::parse("2000-02-29").unwrap().to_string(), "2000-02-29");
        assert_eq!(
            Day::parse("2026-10-06").unwrap().plus(90).to_string(),
            "2027-01-04"
        );
        assert!(Day::parse("2026-02-30").is_none());
        assert!(Day::parse("2026-13-01").is_none());
        assert!(Day::parse("soon").is_none());
        assert!(Day::parse("1969-12-31").unwrap() < Day(0));
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("1.5s"), Ok(Duration::from_millis(1_500)));
        assert_eq!(parse_duration("9m"), Ok(Duration::from_secs(540)));
        assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7_200)));
        assert_eq!(parse_duration("45"), Ok(Duration::from_secs(45)));
        assert!(parse_duration("ten").is_err());
        assert!(parse_duration("5d").is_err());
        assert!(parse_duration("-1s").is_err());
    }

    #[test]
    fn durations_format() {
        assert_eq!(format_duration(Duration::from_millis(850)), "850ms");
        assert_eq!(format_duration(Duration::from_millis(41_230)), "41.2s");
        assert_eq!(format_duration(Duration::from_secs(185)), "3m05s");
    }
}

//! Unix timestamps, time zones and date formatting.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Serialize;

use crate::error::{FormfluxError, Result};

/// Unix timestamp units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeUnit {
    /// Seconds.
    Seconds,
    /// Milliseconds.
    Milliseconds,
    /// Microseconds.
    Microseconds,
}

impl TimeUnit {
    /// Parse a unit name.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "s" | "sec" | "second" | "seconds" => Ok(Self::Seconds),
            "ms" | "milli" | "millisecond" | "milliseconds" => Ok(Self::Milliseconds),
            "us" | "µs" | "micro" | "microsecond" | "microseconds" => Ok(Self::Microseconds),
            other => Err(FormfluxError::invalid(format!(
                "unknown timestamp unit '{other}' (try s, ms or us)"
            ))),
        }
    }

    /// Short name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seconds => "s",
            Self::Milliseconds => "ms",
            Self::Microseconds => "us",
        }
    }
}

/// Convert a Unix timestamp to UTC.
pub fn from_unix(value: i64, unit: TimeUnit) -> Result<DateTime<Utc>> {
    let (seconds, nanos) = match unit {
        TimeUnit::Seconds => (value, 0),
        TimeUnit::Milliseconds => (value.div_euclid(1_000), value.rem_euclid(1_000) * 1_000_000),
        TimeUnit::Microseconds => (
            value.div_euclid(1_000_000),
            value.rem_euclid(1_000_000) * 1_000,
        ),
    };
    DateTime::<Utc>::from_timestamp(seconds, nanos as u32)
        .ok_or_else(|| FormfluxError::invalid(format!("timestamp {value} is out of range")))
}

/// Convert a datetime to a Unix timestamp.
pub fn to_unix(datetime: DateTime<Utc>, unit: TimeUnit) -> i64 {
    match unit {
        TimeUnit::Seconds => datetime.timestamp(),
        TimeUnit::Milliseconds => datetime.timestamp_millis(),
        TimeUnit::Microseconds => datetime.timestamp_micros(),
    }
}

/// Current time in the requested unit.
pub fn now(unit: TimeUnit) -> i64 {
    to_unix(Utc::now(), unit)
}

/// Look up an IANA time zone.
pub fn timezone(name: &str) -> Result<Tz> {
    name.trim()
        .parse::<Tz>()
        .map_err(|_| FormfluxError::invalid(format!("unknown time zone '{name}'")))
}

/// List IANA time zones, optionally filtered by a case-insensitive substring.
pub fn timezones(filter: Option<&str>) -> Vec<&'static str> {
    let filter = filter.map(str::to_ascii_lowercase);
    chrono_tz::TZ_VARIANTS
        .iter()
        .map(|zone| zone.name())
        .filter(|name| {
            filter
                .as_ref()
                .map(|needle| name.to_ascii_lowercase().contains(needle))
                .unwrap_or(true)
        })
        .collect()
}

/// Parsed timestamp in several useful representations.
#[derive(Debug, Clone, Serialize)]
pub struct TimeInfo {
    /// Seconds since the Unix epoch.
    pub unix_seconds: i64,
    /// Milliseconds since the Unix epoch.
    pub unix_milliseconds: i64,
    /// Microseconds since the Unix epoch.
    pub unix_microseconds: i64,
    /// RFC 3339 with the target offset.
    pub rfc3339: String,
    /// Target time zone name.
    pub timezone: String,
    /// UTC offset in seconds.
    pub offset_seconds: i32,
    /// Formatting requested by the caller.
    pub formatted: String,
}

/// Parse an integer timestamp and render it in `zone`.
pub fn convert_timestamp(value: i64, unit: TimeUnit, zone: &str, format: &str) -> Result<TimeInfo> {
    let utc = from_unix(value, unit)?;
    let zone = timezone(zone)?;
    let local = utc.with_timezone(&zone);
    Ok(TimeInfo {
        unix_seconds: utc.timestamp(),
        unix_milliseconds: utc.timestamp_millis(),
        unix_microseconds: utc.timestamp_micros(),
        rfc3339: local.to_rfc3339(),
        timezone: zone.name().to_string(),
        offset_seconds: local.offset().fix().local_minus_utc(),
        formatted: format_datetime(&local, format)?,
    })
}

/// Parse a human date/time in a time zone and render it in another zone.
pub fn convert_datetime(
    input: &str,
    from_zone: &str,
    to_zone: &str,
    format: &str,
) -> Result<TimeInfo> {
    let from = timezone(from_zone)?;
    let parsed = parse_datetime(input, from)?;
    let to = timezone(to_zone)?;
    let local = parsed.with_timezone(&to);
    Ok(TimeInfo {
        unix_seconds: parsed.timestamp(),
        unix_milliseconds: parsed.timestamp_millis(),
        unix_microseconds: parsed.timestamp_micros(),
        rfc3339: local.to_rfc3339(),
        timezone: to.name().to_string(),
        offset_seconds: local.offset().fix().local_minus_utc(),
        formatted: format_datetime(&local, format)?,
    })
}

/// Parse RFC 3339 or common naive date/time spellings in `zone`.
pub fn parse_datetime(input: &str, zone: Tz) -> Result<DateTime<Tz>> {
    let text = input.trim();
    if let Ok(datetime) = DateTime::parse_from_rfc3339(text) {
        return Ok(datetime.with_timezone(&zone));
    }
    if let Ok(datetime) = chrono::DateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S %z") {
        return Ok(datetime.with_timezone(&zone));
    }
    let formats = [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
        "%Y/%m/%d %H:%M:%S",
    ];
    for format in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return zone.from_local_datetime(&naive).single().ok_or_else(|| {
                FormfluxError::invalid(format!(
                    "'{text}' is ambiguous or does not exist in {} due to a DST transition",
                    zone.name()
                ))
            });
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| FormfluxError::invalid("invalid date"))?;
        return zone
            .from_local_datetime(&naive)
            .single()
            .ok_or_else(|| FormfluxError::invalid("date does not exist in this time zone"));
    }
    Err(FormfluxError::invalid(format!(
        "could not parse date/time '{input}' (use RFC 3339 or YYYY-MM-DD HH:MM:SS)"
    )))
}

/// Format a datetime with a named preset or a chrono strftime pattern.
pub fn format_datetime<Tz2: TimeZone>(datetime: &DateTime<Tz2>, format: &str) -> Result<String>
where
    Tz2::Offset: std::fmt::Display,
{
    match format.trim().to_ascii_lowercase().as_str() {
        "" | "rfc3339" | "iso" | "iso8601" => Ok(datetime.to_rfc3339()),
        "human" | "default" => Ok(datetime.format("%Y-%m-%d %H:%M:%S %Z").to_string()),
        "date" => Ok(datetime.format("%Y-%m-%d").to_string()),
        "time" => Ok(datetime.format("%H:%M:%S").to_string()),
        "unix" | "timestamp" => Ok(datetime.timestamp().to_string()),
        _ => Ok(datetime.format(format).to_string()),
    }
}

/// Parse a duration expression in seconds (re-exported helper for CLI).
pub fn parse_seconds(value: &str) -> Result<f64> {
    formflux_shared::units::parse_duration(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_1970() {
        let info = convert_timestamp(0, TimeUnit::Seconds, "UTC", "iso8601").unwrap();
        assert_eq!(info.unix_seconds, 0);
        assert!(info.rfc3339.starts_with("1970-01-01T00:00:00"));
    }

    #[test]
    fn units_convert_between_each_other() {
        let seconds = from_unix(1_700_000_000, TimeUnit::Seconds).unwrap();
        assert_eq!(to_unix(seconds, TimeUnit::Milliseconds), 1_700_000_000_000);
        assert_eq!(
            to_unix(seconds, TimeUnit::Microseconds),
            1_700_000_000_000_000
        );
        assert_eq!(
            from_unix(1_700_000_000_000, TimeUnit::Milliseconds)
                .unwrap()
                .timestamp(),
            1_700_000_000
        );
    }

    #[test]
    fn negative_microseconds_are_euclidean() {
        let datetime = from_unix(-1, TimeUnit::Microseconds).unwrap();
        assert_eq!(datetime.timestamp_micros(), -1);
    }

    #[test]
    fn timezone_conversion_changes_hour() {
        let info = convert_timestamp(
            1_700_000_000,
            TimeUnit::Seconds,
            "America/New_York",
            "iso8601",
        )
        .unwrap();
        assert!(info.rfc3339.contains("-05:00") || info.rfc3339.contains("-04:00"));
        assert_eq!(info.timezone, "America/New_York");
    }

    #[test]
    fn parses_naive_datetime_in_zone() {
        let zone = timezone("Europe/Berlin").unwrap();
        let parsed = parse_datetime("2024-01-15 12:30:00", zone).unwrap();
        assert_eq!(parsed.timestamp(), 1_705_318_200);
    }

    #[test]
    fn format_presets_work() {
        let info = convert_timestamp(0, TimeUnit::Seconds, "UTC", "human").unwrap();
        assert!(info.formatted.starts_with("1970-01-01 00:00:00"));
        assert!(convert_timestamp(0, TimeUnit::Seconds, "UTC", "unix")
            .unwrap()
            .formatted
            .eq("0"));
    }

    #[test]
    fn zones_can_be_filtered() {
        let zones = timezones(Some("tokyo"));
        assert!(zones.contains(&"Asia/Tokyo"));
        assert!(timezone("Not/AZone").is_err());
    }
}

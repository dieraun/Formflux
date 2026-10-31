//! Parsing and formatting of human units: byte sizes, durations, bitrates,
//! frame rates and resolutions.  Both decimal (`MB`) and binary (`MiB`)
//! suffixes are understood so command-line values behave the way users expect.

use crate::error::{FormfluxError, Result};

/// Parse a human byte size such as `10 MB`, `1.5GiB` or `4096`.
///
/// Bare numbers are bytes.  `KB`/`MB`/`GB` are decimal powers of 1000 while
/// `KiB`/`MiB`/`GiB` (and the short forms `K`, `M`, `G`) are powers of 1024.
pub fn parse_bytes(input: &str) -> Result<u64> {
    let text = input.trim().replace(' ', "").to_ascii_lowercase();
    if text.is_empty() {
        return Err(FormfluxError::invalid("empty byte size"));
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    let value: f64 = number
        .parse()
        .map_err(|_| FormfluxError::invalid(format!("invalid byte size '{input}'")))?;
    if !value.is_finite() || value < 0.0 {
        return Err(FormfluxError::invalid(format!(
            "invalid byte size '{input}'"
        )));
    }
    let multiplier: f64 = match suffix {
        "" | "b" => 1.0,
        "k" | "kib" => 1024.0,
        "m" | "mib" => 1024.0 * 1024.0,
        "g" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "p" | "pib" => 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        "pb" => 1_000_000_000_000_000.0,
        other => {
            return Err(FormfluxError::invalid(format!(
                "unknown byte suffix '{other}' in '{input}'"
            )))
        }
    };
    let bytes = value * multiplier;
    if bytes > u64::MAX as f64 {
        return Err(FormfluxError::invalid(format!(
            "byte size '{input}' overflows"
        )));
    }
    Ok(bytes.round() as u64)
}

/// Format a byte count using binary units with at most one decimal.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let formatted = if value >= 100.0 || (value - value.round()).abs() < 0.05 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    };
    format!("{formatted} {}", UNITS[unit])
}

/// Format bytes as an exact human string for progress lines.
pub fn format_rate(bytes_per_second: f64) -> String {
    if !bytes_per_second.is_finite() || bytes_per_second <= 0.0 {
        return "0 B/s".to_string();
    }
    format!("{}/s", format_bytes(bytes_per_second.round() as u64))
}

/// Parse a duration into seconds.
///
/// Accepts `1h30m`, `90s`, `1500ms`, `01:02:03`, `02:03` and bare seconds.
pub fn parse_duration(input: &str) -> Result<f64> {
    let text = input.trim().to_ascii_lowercase();
    if text.is_empty() {
        return Err(FormfluxError::invalid("empty duration"));
    }
    if text.contains(':') {
        return parse_clock_duration(&text);
    }
    let mut total = 0.0f64;
    let mut rest = text.as_str();
    let mut saw_part = false;
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let number_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if number_end == 0 {
            return Err(FormfluxError::invalid(format!(
                "invalid duration '{input}'"
            )));
        }
        let number: f64 = rest[..number_end]
            .parse()
            .map_err(|_| FormfluxError::invalid(format!("invalid duration '{input}'")))?;
        rest = &rest[number_end..];
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        rest = &rest[unit_end..];
        let multiplier = match unit {
            "" | "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "ms" | "millisecond" | "milliseconds" => 0.001,
            "us" | "microsecond" | "microseconds" => 0.000_001,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86_400.0,
            other => {
                return Err(FormfluxError::invalid(format!(
                    "unknown duration unit '{other}' in '{input}'"
                )))
            }
        };
        total += number * multiplier;
        saw_part = true;
    }
    if !saw_part || !total.is_finite() || total < 0.0 {
        return Err(FormfluxError::invalid(format!(
            "invalid duration '{input}'"
        )));
    }
    Ok(total)
}

fn parse_clock_duration(text: &str) -> Result<f64> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return Err(FormfluxError::invalid(format!(
            "too many ':' segments in duration '{text}'"
        )));
    }
    let mut total = 0.0f64;
    for part in &parts {
        let value: f64 = part
            .parse()
            .map_err(|_| FormfluxError::invalid(format!("invalid duration '{text}'")))?;
        if value < 0.0 {
            return Err(FormfluxError::invalid("duration cannot be negative"));
        }
        total = total * 60.0 + value;
    }
    Ok(total)
}

/// Format seconds as a compact duration.
pub fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0 s".to_string();
    }
    if seconds < 1.0 {
        return format!("{:.0} ms", seconds * 1000.0);
    }
    if seconds < 60.0 {
        return format!("{seconds:.1} s");
    }
    let total = seconds.round() as u64;
    let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

/// Format seconds as an ETA (`--:--` when unknown).
pub fn format_eta(seconds: Option<f64>) -> String {
    match seconds {
        Some(value) if value.is_finite() && value >= 0.0 => format_duration(value),
        _ => "--:--".to_string(),
    }
}

/// Parse a bitrate such as `320k`, `5 Mbps`, `800000` into bits per second.
pub fn parse_bitrate(input: &str) -> Result<u64> {
    let text = input.trim().replace(' ', "").to_ascii_lowercase();
    if text.is_empty() {
        return Err(FormfluxError::invalid("empty bitrate"));
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    let value: f64 = number
        .parse()
        .map_err(|_| FormfluxError::invalid(format!("invalid bitrate '{input}'")))?;
    if !value.is_finite() || value < 0.0 {
        return Err(FormfluxError::invalid(format!("invalid bitrate '{input}'")));
    }
    let multiplier = match suffix {
        "" | "b" | "bps" | "b/s" => 1.0,
        "k" | "kb" | "kbps" | "kbit" | "kbit/s" | "kibps" => 1000.0,
        "m" | "mb" | "mbps" | "mbit" | "mbit/s" => 1_000_000.0,
        "g" | "gb" | "gbps" | "gbit" | "gbit/s" => 1_000_000_000.0,
        other => {
            return Err(FormfluxError::invalid(format!(
                "unknown bitrate suffix '{other}' in '{input}'"
            )))
        }
    };
    Ok((value * multiplier).round() as u64)
}

/// Format bits per second as a short bitrate.
pub fn format_bitrate(bits_per_second: u64) -> String {
    if bits_per_second >= 1_000_000 {
        format!("{:.1} Mbps", bits_per_second as f64 / 1_000_000.0)
    } else if bits_per_second >= 1000 {
        format!("{:.0} kbps", bits_per_second as f64 / 1000.0)
    } else {
        format!("{bits_per_second} bps")
    }
}

/// Parse a resolution such as `1920x1080`, `1280*720` or `4k`.
pub fn parse_resolution(input: &str) -> Result<(u32, u32)> {
    let text = input.trim().to_ascii_lowercase();
    match text.as_str() {
        "4k" | "uhd" => return Ok((3840, 2160)),
        "1080p" | "fhd" => return Ok((1920, 1080)),
        "720p" | "hd" => return Ok((1280, 720)),
        "480p" => return Ok((854, 480)),
        _ => {}
    }
    let separator = text.find(['x', '*', ':']).ok_or_else(|| {
        FormfluxError::invalid(format!("resolution '{input}' must look like 1920x1080"))
    })?;
    let width: u32 = text[..separator]
        .trim()
        .parse()
        .map_err(|_| FormfluxError::invalid(format!("invalid width in '{input}'")))?;
    let height: u32 = text[separator + 1..]
        .trim()
        .parse()
        .map_err(|_| FormfluxError::invalid(format!("invalid height in '{input}'")))?;
    if width == 0 || height == 0 {
        return Err(FormfluxError::invalid(
            "resolution dimensions must be positive",
        ));
    }
    Ok((width, height))
}

/// Format a resolution as `WxH`.
pub fn format_resolution(width: u32, height: u32) -> String {
    format!("{width}x{height}")
}

/// Clamp a percentage into `0..=100`, treating NaN as zero.
pub fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_sizes_parse_both_bases() {
        assert_eq!(parse_bytes("1024").unwrap(), 1024);
        assert_eq!(parse_bytes("1 KB").unwrap(), 1000);
        assert_eq!(parse_bytes("1 KiB").unwrap(), 1024);
        assert_eq!(parse_bytes("1.5MiB").unwrap(), 1_572_864);
        assert_eq!(parse_bytes("2 gb").unwrap(), 2_000_000_000);
        assert!(parse_bytes("12 furlongs").is_err());
    }

    #[test]
    fn byte_sizes_format_compactly() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(1024 * 1024 * 3), "3 MiB");
    }

    #[test]
    fn durations_parse_and_format() {
        assert_eq!(parse_duration("90").unwrap(), 90.0);
        assert_eq!(parse_duration("1h30m").unwrap(), 5400.0);
        assert_eq!(parse_duration("1500ms").unwrap(), 1.5);
        assert_eq!(parse_duration("01:02:03").unwrap(), 3723.0);
        assert_eq!(parse_duration("02:03").unwrap(), 123.0);
        assert!(parse_duration("soon").is_err());
        assert_eq!(format_duration(3723.0), "01:02:03");
        assert_eq!(format_duration(123.0), "02:03");
        assert_eq!(format_duration(0.5), "500 ms");
        assert_eq!(format_eta(None), "--:--");
    }

    #[test]
    fn bitrates_parse_and_format() {
        assert_eq!(parse_bitrate("320k").unwrap(), 320_000);
        assert_eq!(parse_bitrate("5 Mbps").unwrap(), 5_000_000);
        assert_eq!(format_bitrate(5_000_000), "5.0 Mbps");
        assert_eq!(format_bitrate(320_000), "320 kbps");
        assert!(parse_bitrate("fast").is_err());
    }

    #[test]
    fn resolutions_parse() {
        assert_eq!(parse_resolution("1920x1080").unwrap(), (1920, 1080));
        assert_eq!(parse_resolution("1280*720").unwrap(), (1280, 720));
        assert_eq!(parse_resolution("4k").unwrap(), (3840, 2160));
        assert!(parse_resolution("0x10").is_err());
        assert!(parse_resolution("big").is_err());
    }

    #[test]
    fn percentages_are_safe() {
        assert_eq!(clamp_percent(-3.0), 0.0);
        assert_eq!(clamp_percent(120.0), 100.0);
        assert_eq!(clamp_percent(f64::NAN), 0.0);
    }
}

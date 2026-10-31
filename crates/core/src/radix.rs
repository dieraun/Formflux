//! Radix conversion and character inspection.
//!
//! Numbers are parsed into `i128`, which covers every value a CLI user can
//! reasonably paste while keeping the implementation dependency-free.  Input
//! accepts `0b`, `0o` and `0x` prefixes, `_` digit separators and a leading
//! sign.  Character inspection reports the Unicode scalar value and its UTF-8
//! and UTF-16 encodings without allocating a code-point table.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{FormfluxError, Result};

/// A supported positional base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Radix {
    /// Base 2.
    Binary,
    /// Base 8.
    Octal,
    /// Base 10.
    Decimal,
    /// Base 16.
    Hexadecimal,
}

impl Radix {
    /// Numeric base.
    pub fn base(self) -> u32 {
        match self {
            Radix::Binary => 2,
            Radix::Octal => 8,
            Radix::Decimal => 10,
            Radix::Hexadecimal => 16,
        }
    }

    /// Short human label.
    pub fn as_str(self) -> &'static str {
        match self {
            Radix::Binary => "bin",
            Radix::Octal => "oct",
            Radix::Decimal => "dec",
            Radix::Hexadecimal => "hex",
        }
    }

    fn prefix(self) -> Option<&'static str> {
        match self {
            Radix::Binary => Some("0b"),
            Radix::Octal => Some("0o"),
            Radix::Decimal => None,
            Radix::Hexadecimal => Some("0x"),
        }
    }

    /// Parse `2`, `8`, `10`, `16`, `bin`, `oct`, `dec`, `hex`, `binary`,
    /// `octal`, `decimal` or `hexadecimal`, case-insensitively.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "2" | "b" | "bin" | "binary" => Ok(Radix::Binary),
            "8" | "o" | "oct" | "octal" => Ok(Radix::Octal),
            "10" | "d" | "dec" | "decimal" => Ok(Radix::Decimal),
            "16" | "h" | "hex" | "hexadecimal" => Ok(Radix::Hexadecimal),
            other => Err(FormfluxError::invalid(format!(
                "unknown radix '{other}' (try 2, 8, 10 or 16)"
            ))),
        }
    }
}

impl fmt::Display for Radix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Radix {
    type Err = FormfluxError;

    fn from_str(value: &str) -> Result<Self> {
        Radix::parse(value)
    }
}

/// Parse a signed integer in `radix`, or detect the base from a prefix when
/// `radix` is `None`.
pub fn parse_value(input: &str, radix: Option<Radix>) -> Result<i128> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(FormfluxError::invalid("no number supplied"));
    }
    let (negative, rest) = match trimmed.as_bytes()[0] {
        b'-' => (true, &trimmed[1..]),
        b'+' => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    if rest.is_empty() {
        return Err(FormfluxError::invalid(format!("'{input}' has no digits")));
    }

    let lower = rest.to_ascii_lowercase();
    let detected = if lower.starts_with("0x") {
        Some(Radix::Hexadecimal)
    } else if lower.starts_with("0b") {
        Some(Radix::Binary)
    } else if lower.starts_with("0o") {
        Some(Radix::Octal)
    } else {
        None
    };
    if let (Some(expected), Some(found)) = (radix, detected) {
        if expected != found {
            return Err(FormfluxError::invalid(format!(
                "prefix {} conflicts with --from {}",
                found.as_str(),
                expected.as_str()
            )));
        }
    }
    let radix = radix.or(detected).unwrap_or(Radix::Decimal);
    let digits = if detected.is_some() { &rest[2..] } else { rest };
    let digits = digits.replace('_', "");
    if digits.is_empty() {
        return Err(FormfluxError::invalid(format!("'{input}' has no digits")));
    }

    let mut value: i128 = 0;
    for character in digits.chars() {
        let digit = character.to_digit(radix.base()).ok_or_else(|| {
            FormfluxError::invalid(format!(
                "'{character}' is not a base-{} digit",
                radix.base()
            ))
        })?;
        value = value
            .checked_mul(radix.base() as i128)
            .and_then(|value| value.checked_add(digit as i128))
            .ok_or_else(|| FormfluxError::invalid("number is too large for 128 bits"))?;
    }
    Ok(if negative { -value } else { value })
}

/// Format a value in `radix` without a prefix.
pub fn format_value(value: i128, radix: Radix) -> String {
    let magnitude = value.unsigned_abs();
    let digits = match radix {
        Radix::Binary => format!("{magnitude:b}"),
        Radix::Octal => format!("{magnitude:o}"),
        Radix::Decimal => magnitude.to_string(),
        Radix::Hexadecimal => format!("{magnitude:x}"),
    };
    if value < 0 {
        format!("-{digits}")
    } else {
        digits
    }
}

/// Format with the conventional `0b`/`0o`/`0x` prefix (decimal unchanged).
pub fn format_prefixed(value: i128, radix: Radix) -> String {
    let digits = format_value(value, radix);
    match radix.prefix() {
        Some(prefix) if value >= 0 => format!("{prefix}{digits}"),
        Some(prefix) => format!("-{prefix}{}", &digits[1..]),
        None => digits,
    }
}

/// Convert a textual number from one base to another.
pub fn convert(input: &str, from: Option<Radix>, to: Radix) -> Result<String> {
    Ok(format_value(parse_value(input, from)?, to))
}

/// Convert several values, preserving order and sign.
pub fn convert_many(inputs: &[String], from: Option<Radix>, to: Radix) -> Result<Vec<String>> {
    inputs
        .iter()
        .map(|input| convert(input, from, to))
        .collect()
}

/// One decoded character and its encodings.
#[derive(Debug, Clone, Serialize)]
pub struct CharInfo {
    /// The character itself.
    pub character: String,
    /// Unicode scalar value.
    pub codepoint: u32,
    /// Canonical `U+XXXX` spelling.
    pub unicode: String,
    /// UTF-8 bytes.
    pub utf8: Vec<u8>,
    /// UTF-16 code units.
    pub utf16: Vec<u16>,
    /// Whether the character is ASCII.
    pub ascii: bool,
}

/// Inspect every character in `text`.
pub fn inspect_chars(text: &str) -> Vec<CharInfo> {
    text.chars()
        .map(|character| {
            let codepoint = character as u32;
            let mut utf16 = [0u16; 2];
            CharInfo {
                character: character.to_string(),
                codepoint,
                unicode: format!("U+{codepoint:04X}"),
                utf8: character.to_string().into_bytes(),
                utf16: character.encode_utf16(&mut utf16).to_vec(),
                ascii: character.is_ascii(),
            }
        })
        .collect()
}

/// Decode characters from a comma/space separated code-point list.
///
/// Entries accept `65`, `0x41`, `U+0041` and `U+1F600`; a decimal integer is
/// assumed otherwise.
pub fn codepoints_to_string(input: &str) -> Result<String> {
    let mut output = String::new();
    for token in input
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|token| !token.is_empty())
    {
        let token = token.trim();
        let value = if let Some(hex) = token
            .strip_prefix("U+")
            .or_else(|| token.strip_prefix("u+"))
        {
            u32::from_str_radix(hex, 16)
                .map_err(|_| FormfluxError::invalid(format!("invalid code point '{token}'")))?
        } else if token
            .trim_start_matches(['-', '+'])
            .to_ascii_lowercase()
            .starts_with("0x")
        {
            let parsed = parse_value(token, Some(Radix::Hexadecimal))?;
            u32::try_from(parsed).map_err(|_| {
                FormfluxError::invalid(format!("code point '{token}' is out of range"))
            })?
        } else {
            token
                .parse::<u32>()
                .map_err(|_| FormfluxError::invalid(format!("invalid code point '{token}'")))?
        };
        let character = char::from_u32(value).ok_or_else(|| {
            FormfluxError::invalid(format!("U+{value:04X} is not a scalar value"))
        })?;
        output.push(character);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_are_detected() {
        assert_eq!(parse_value("0xff", None).unwrap(), 255);
        assert_eq!(parse_value("0b1010", None).unwrap(), 10);
        assert_eq!(parse_value("0o17", None).unwrap(), 15);
        assert_eq!(parse_value("-0x10", None).unwrap(), -16);
        assert_eq!(parse_value("1_000", None).unwrap(), 1000);
    }

    #[test]
    fn conversions_match() {
        assert_eq!(
            convert("255", Some(Radix::Decimal), Radix::Hexadecimal).unwrap(),
            "ff"
        );
        assert_eq!(convert("0xff", None, Radix::Binary).unwrap(), "11111111");
        assert_eq!(
            convert("-15", Some(Radix::Decimal), Radix::Octal).unwrap(),
            "-17"
        );
        assert_eq!(format_prefixed(-255, Radix::Hexadecimal), "-0xff");
    }

    #[test]
    fn bad_digits_and_prefixes_are_rejected() {
        assert!(parse_value("0xfg", None).is_err());
        assert!(parse_value("0x10", Some(Radix::Binary)).is_err());
        assert!(parse_value("   ", None).is_err());
        assert!(parse_value("0x", None).is_err());
    }

    #[test]
    fn characters_are_described() {
        let info = inspect_chars("Aé😀");
        assert_eq!(info.len(), 3);
        assert_eq!(info[0].codepoint, 65);
        assert_eq!(info[0].unicode, "U+0041");
        assert_eq!(info[1].utf8, vec![0xC3, 0xA9]);
        assert_eq!(info[2].utf16.len(), 2);
        assert_eq!(info[2].utf8.len(), 4);
    }

    #[test]
    fn code_point_lists_decode() {
        assert_eq!(codepoints_to_string("65, 0x42 U+0043").unwrap(), "ABC");
        assert_eq!(codepoints_to_string("U+1F600").unwrap(), "😀");
        assert!(codepoints_to_string("0x110000").is_err());
        assert!(codepoints_to_string("0xD800").is_err());
    }
}

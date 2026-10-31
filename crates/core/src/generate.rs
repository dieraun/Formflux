//! QR codes, EAN-13 barcodes, UUIDs, random passwords and strength checks.

use std::path::Path;

use image::{DynamicImage, Rgba, RgbaImage};
use qrcode::types::Color as QrColor;
use qrcode::QrCode;
use rand::seq::SliceRandom;
use rand::Rng;
use serde::Serialize;

use crate::error::{FormfluxError, Result};

/// Render a QR code to an in-memory matrix (quiet zone included).
pub fn qr_matrix(data: &str, border: u32) -> Result<Vec<Vec<bool>>> {
    let code = QrCode::new(data.as_bytes())
        .map_err(|error| FormfluxError::invalid(format!("could not encode QR data: {error}")))?;
    let width = code.width() as u32;
    let colors = code.to_colors();
    let total = width + border * 2;
    let mut matrix = vec![vec![false; total as usize]; total as usize];
    for y in 0..width {
        for x in 0..width {
            let dark = colors[(y * width + x) as usize] == QrColor::Dark;
            matrix[(y + border) as usize][(x + border) as usize] = dark;
        }
    }
    Ok(matrix)
}

/// Render a QR code as an SVG string.
pub fn qr_svg(data: &str, module_size: u32, border: u32) -> Result<String> {
    let matrix = qr_matrix(data, border)?;
    let module = module_size.clamp(1, 64);
    let size = matrix.len() as u32 * module;
    let mut svg = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\" viewBox=\"0 0 {size} {size}\" shape-rendering=\"crispEdges\">\n<rect width=\"100%\" height=\"100%\" fill=\"#FFFFFF\"/>\n<path fill=\"#000000\" d=\""
    );
    for (y, row) in matrix.iter().enumerate() {
        for (x, dark) in row.iter().enumerate() {
            if *dark {
                svg.push_str(&format!(
                    "M{} {}h{}v{}h-{}z",
                    x as u32 * module,
                    y as u32 * module,
                    module,
                    module,
                    module
                ));
            }
        }
    }
    svg.push_str("\"/>\n</svg>\n");
    Ok(svg)
}

/// Render a QR code to PNG bytes.
pub fn qr_png(data: &str, module_size: u32, border: u32) -> Result<Vec<u8>> {
    let matrix = qr_matrix(data, border)?;
    let module = module_size.clamp(1, 64);
    let size = matrix.len() as u32 * module;
    let mut image = RgbaImage::from_pixel(size, size, Rgba([255, 255, 255, 255]));
    for (y, row) in matrix.iter().enumerate() {
        for (x, dark) in row.iter().enumerate() {
            if *dark {
                for dy in 0..module {
                    for dx in 0..module {
                        image.put_pixel(
                            x as u32 * module + dx,
                            y as u32 * module + dy,
                            Rgba([0, 0, 0, 255]),
                        );
                    }
                }
            }
        }
    }
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .map_err(|error| FormfluxError::Image(format!("could not encode QR PNG: {error}")))?;
    Ok(bytes)
}

/// Write a QR code atomically to a `.png` or `.svg` path.
pub fn write_qr(
    data: &str,
    target: &Path,
    module_size: u32,
    border: u32,
    overwrite: bool,
) -> Result<()> {
    if let Some(parent) = target.parent() {
        crate::io::ensure_dir(parent)?;
    }
    match target
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("svg") => {
            crate::io::write_text(target, &qr_svg(data, module_size, border)?, overwrite)?;
            Ok(())
        }
        Some("png") => {
            let bytes = qr_png(data, module_size, border)?;
            let image = image::load_from_memory(&bytes)
                .map_err(|error| FormfluxError::Image(error.to_string()))?;
            crate::image::write_image(
                &DynamicImage::ImageRgba8(image.to_rgba8()),
                target,
                crate::image::OutputFormat::Png,
                95,
                overwrite,
            )
        }
        _ => Err(FormfluxError::unsupported(
            "QR output must be a .png or .svg file",
        )),
    }
}

const EAN_L: [&str; 10] = [
    "0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011",
    "0110111", "0001011",
];
const EAN_G: [&str; 10] = [
    "0100111", "0110011", "0011011", "0100001", "0011101", "0111001", "0000101", "0010001",
    "0001001", "0010111",
];
const EAN_R: [&str; 10] = [
    "1110010", "1100110", "1101100", "1000010", "1011100", "1001110", "1010000", "1000100",
    "1001000", "1110100",
];
const EAN_PARITY: [&str; 10] = [
    "LLLLLL", "LLGLGG", "LLGGLG", "LLGGGL", "LGLLGG", "LGGLLG", "LGGGLL", "LGLGLG", "LGLGGL",
    "LGGLGL",
];

fn ean_check_digit(digits: &[u8]) -> u8 {
    let mut sum = 0u32;
    for (index, digit) in digits.iter().enumerate() {
        let weight = if index % 2 == 0 { 1 } else { 3 };
        sum += *digit as u32 * weight;
    }
    ((10 - (sum % 10)) % 10) as u8
}

fn ean_bits(input: &str) -> Result<Vec<bool>> {
    let digits: Vec<u8> = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| {
            c.to_digit(10)
                .map(|value| value as u8)
                .ok_or_else(|| FormfluxError::invalid("EAN-13 barcodes accept digits only"))
        })
        .collect::<Result<_>>()?;
    let digits = match digits.len() {
        12 => {
            let mut with_check = digits.clone();
            with_check.push(ean_check_digit(&digits));
            with_check
        }
        13 => {
            if ean_check_digit(&digits[..12]) != digits[12] {
                return Err(FormfluxError::invalid("EAN-13 check digit does not match"));
            }
            digits
        }
        _ => {
            return Err(FormfluxError::invalid(
                "EAN-13 barcodes need 12 or 13 digits",
            ))
        }
    };
    let mut bits = String::from("101");
    let parity = EAN_PARITY[digits[0] as usize].as_bytes();
    for index in 0..6 {
        let digit = digits[index + 1] as usize;
        bits.push_str(if parity[index] == b'L' {
            EAN_L[digit]
        } else {
            EAN_G[digit]
        });
    }
    bits.push_str("01010");
    for digit in &digits[7..] {
        bits.push_str(EAN_R[*digit as usize]);
    }
    bits.push_str("101");
    Ok(bits.bytes().map(|byte| byte == b'1').collect())
}

/// Render an EAN-13 barcode as SVG.
pub fn barcode_svg(input: &str, height: u32) -> Result<String> {
    let bits = ean_bits(input)?;
    let height = height.clamp(20, 800);
    let quiet = 10u32;
    let module = 2u32;
    let width = bits.len() as u32 * module + quiet * 2;
    let mut svg = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{}\" viewBox=\"0 0 {width} {}\">\n<rect width=\"100%\" height=\"100%\" fill=\"#FFFFFF\"/>\n<path fill=\"#000000\" d=\"",
        height + 16,
        height + 16
    );
    for (index, dark) in bits.iter().enumerate() {
        if *dark {
            svg.push_str(&format!(
                "M{} 0h{}v{}h-{}z",
                quiet + index as u32 * module,
                module,
                height,
                module
            ));
        }
    }
    svg.push_str("\"/>\n</svg>\n");
    Ok(svg)
}

/// Write an EAN-13 barcode as SVG.
pub fn write_barcode(input: &str, target: &Path, height: u32, overwrite: bool) -> Result<()> {
    crate::io::write_text(target, &barcode_svg(input, height)?, overwrite)?;
    Ok(())
}

/// New random UUID v4.
pub fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// New time-ordered UUID v7.
pub fn uuid_v7() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Parse and normalize a UUID.
pub fn parse_uuid(input: &str) -> Result<String> {
    let uuid = uuid::Uuid::parse_str(input.trim())
        .map_err(|error| FormfluxError::invalid(format!("invalid UUID: {error}")))?;
    Ok(uuid.to_string())
}

/// Password generation controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordOptions {
    /// Desired length (8..=256).
    pub length: usize,
    /// Include uppercase letters.
    pub uppercase: bool,
    /// Include lowercase letters.
    pub lowercase: bool,
    /// Include digits.
    pub digits: bool,
    /// Include symbols.
    pub symbols: bool,
    /// Exclude look-alike characters (`Il1O0`).
    pub exclude_ambiguous: bool,
}

impl Default for PasswordOptions {
    fn default() -> Self {
        Self {
            length: 20,
            uppercase: true,
            lowercase: true,
            digits: true,
            symbols: true,
            exclude_ambiguous: true,
        }
    }
}

const UPPER: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ";
const LOWER: &str = "abcdefghijkmnopqrstuvwxyz";
const DIGITS: &str = "23456789";
const SYMBOLS: &str = "!@#$%^&*()-_=+[]{};:,.?/";

/// Generate a cryptographically seeded random password.
pub fn generate_password(options: &PasswordOptions) -> Result<String> {
    if !(8..=256).contains(&options.length) {
        return Err(FormfluxError::invalid("password length must be 8..=256"));
    }
    let mut groups: Vec<Vec<char>> = Vec::new();
    if options.uppercase {
        groups.push(charset(UPPER, options.exclude_ambiguous));
    }
    if options.lowercase {
        groups.push(charset(LOWER, options.exclude_ambiguous));
    }
    if options.digits {
        groups.push(charset(DIGITS, options.exclude_ambiguous));
    }
    if options.symbols {
        groups.push(SYMBOLS.chars().collect());
    }
    if groups.is_empty() {
        return Err(FormfluxError::invalid(
            "at least one password character class must be enabled",
        ));
    }
    if options.length < groups.len() {
        return Err(FormfluxError::invalid(
            "password length is shorter than the number of enabled classes",
        ));
    }
    let mut rng = rand::thread_rng();
    let mut password: Vec<char> = groups
        .iter()
        .map(|group| group[rng.gen_range(0..group.len())])
        .collect();
    let all: Vec<char> = groups.iter().flatten().copied().collect();
    while password.len() < options.length {
        password.push(all[rng.gen_range(0..all.len())]);
    }
    password.shuffle(&mut rng);
    Ok(password.into_iter().collect())
}

fn charset(source: &str, exclude_ambiguous: bool) -> Vec<char> {
    source
        .chars()
        .filter(|character| !exclude_ambiguous || !"Il1O0".contains(*character))
        .collect()
}

/// Generate a random string from an explicit alphabet.
pub fn random_string(length: usize, alphabet: &str) -> Result<String> {
    if length == 0 || length > 10_000 {
        return Err(FormfluxError::invalid("length must be between 1 and 10000"));
    }
    let characters: Vec<char> = alphabet.chars().collect();
    if characters.is_empty() {
        return Err(FormfluxError::invalid("alphabet is empty"));
    }
    let mut rng = rand::thread_rng();
    Ok((0..length)
        .map(|_| characters[rng.gen_range(0..characters.len())])
        .collect())
}

/// Password strength report (zxcvbn).
#[derive(Debug, Clone, Serialize)]
pub struct PasswordStrength {
    /// 0 (very weak) through 4 (very strong).
    pub score: u8,
    /// Estimated guesses.
    pub guesses: u64,
    /// Warning text.
    pub warning: Option<String>,
    /// Improvement suggestions.
    pub suggestions: Vec<String>,
}

/// Evaluate a password with zxcvbn.
pub fn password_strength(password: &str) -> PasswordStrength {
    let entropy = zxcvbn::zxcvbn(password, &[]);
    let feedback = entropy.feedback();
    PasswordStrength {
        score: entropy.score().into(),
        guesses: entropy.guesses(),
        warning: feedback
            .and_then(|value| value.warning())
            .map(|warning| warning.to_string()),
        suggestions: feedback
            .map(|value| value.suggestions().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_matrix_has_quiet_zone() {
        let matrix = qr_matrix("https://example.invalid", 4).unwrap();
        assert!(matrix.len() > 8);
        assert!(!matrix[0][0]);
        assert!(matrix.iter().all(|row| row.len() == matrix.len()));
    }

    #[test]
    fn qr_svg_contains_svg() {
        let svg = qr_svg("formflux", 4, 2).unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<path"));
    }

    #[test]
    fn ean_check_digit_is_computed() {
        // 400638133393 is a valid EAN-13 prefix with check digit 1.
        assert_eq!(ean_check_digit(&[4, 0, 0, 6, 3, 8, 1, 3, 3, 3, 9, 3]), 1);
        let bits = ean_bits("400638133393").unwrap();
        assert_eq!(bits.len(), 95);
        assert!(bits[0] && !bits[1] && bits[2]);
        assert!(ean_bits("4006381333940").is_err());
    }

    #[test]
    fn barcode_is_svg() {
        let svg = barcode_svg("4006381333931", 60).unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("h2v60"));
    }

    #[test]
    fn uuid_versions_parse() {
        assert!(parse_uuid(&uuid_v4()).is_ok());
        assert!(parse_uuid(&uuid_v7()).is_ok());
        assert!(parse_uuid("not-a-uuid").is_err());
    }

    #[test]
    fn password_has_requested_length_and_classes() {
        let options = PasswordOptions {
            length: 32,
            ..PasswordOptions::default()
        };
        let password = generate_password(&options).unwrap();
        assert_eq!(password.chars().count(), 32);
        assert!(password.chars().any(|c| c.is_ascii_uppercase()));
        assert!(password.chars().any(|c| c.is_ascii_lowercase()));
        assert!(password.chars().any(|c| c.is_ascii_digit()));
        assert!(!password.contains('O'));
    }

    #[test]
    fn strength_reports_score() {
        let weak = password_strength("password");
        assert!(weak.score <= 1);
        let strong = password_strength("v3Ry-Str0ng-Passphrase!42");
        assert!(strong.score >= 2);
    }

    #[test]
    fn random_string_uses_alphabet() {
        let value = random_string(20, "abc").unwrap();
        assert_eq!(value.len(), 20);
        assert!(value.chars().all(|c| "abc".contains(c)));
        assert!(random_string(0, "abc").is_err());
    }
}

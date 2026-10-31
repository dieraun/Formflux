//! Colour-space conversion, WCAG contrast and palette generation.

use serde::Serialize;

use crate::error::{FormfluxError, Result};

/// sRGB colour with an optional alpha channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Rgba {
    /// Red 0..=255.
    pub r: u8,
    /// Green 0..=255.
    pub g: u8,
    /// Blue 0..=255.
    pub b: u8,
    /// Alpha 0..=255.
    pub a: u8,
}

impl Rgba {
    /// Opaque RGB constructor.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Hex string, `#RRGGBB` or `#RRGGBBAA` when translucent.
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
        }
    }

    /// HSL conversion.
    pub fn to_hsl(self) -> Hsl {
        let r = self.r as f64 / 255.0;
        let g = self.g as f64 / 255.0;
        let b = self.b as f64 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let l = (max + min) / 2.0;
        if delta.abs() < f64::EPSILON {
            return Hsl {
                h: 0.0,
                s: 0.0,
                l: l * 100.0,
            };
        }
        let s = if l > 0.5 {
            delta / (2.0 - max - min)
        } else {
            delta / (max + min)
        };
        let mut h = if (max - r).abs() < f64::EPSILON {
            (g - b) / delta + if g < b { 6.0 } else { 0.0 }
        } else if (max - g).abs() < f64::EPSILON {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };
        h *= 60.0;
        Hsl {
            h,
            s: s * 100.0,
            l: l * 100.0,
        }
    }

    /// CMYK conversion (naive, no black generation).
    pub fn to_cmyk(self) -> Cmyk {
        let r = self.r as f64 / 255.0;
        let g = self.g as f64 / 255.0;
        let b = self.b as f64 / 255.0;
        let k = 1.0 - r.max(g).max(b);
        if k >= 1.0 {
            return Cmyk {
                c: 0.0,
                m: 0.0,
                y: 0.0,
                k: 100.0,
            };
        }
        Cmyk {
            c: (1.0 - r - k) / (1.0 - k) * 100.0,
            m: (1.0 - g - k) / (1.0 - k) * 100.0,
            y: (1.0 - b - k) / (1.0 - k) * 100.0,
            k: k * 100.0,
        }
    }

    /// CIE L*a*b* (D65).
    pub fn to_lab(self) -> Lab {
        let (x, y, z) = self.to_xyz();
        let fx = lab_f(x / 0.950_47);
        let fy = lab_f(y);
        let fz = lab_f(z / 1.088_83);
        Lab {
            l: 116.0 * fy - 16.0,
            a: 500.0 * (fx - fy),
            b: 200.0 * (fy - fz),
        }
    }

    fn to_xyz(self) -> (f64, f64, f64) {
        let r = srgb_to_linear(self.r);
        let g = srgb_to_linear(self.g);
        let b = srgb_to_linear(self.b);
        (
            r * 0.412_456_4 + g * 0.357_576_1 + b * 0.180_437_5,
            r * 0.212_672_9 + g * 0.715_152_2 + b * 0.072_175_0,
            r * 0.019_333_9 + g * 0.119_192_0 + b * 0.950_304_1,
        )
    }

    /// WCAG 2.x relative luminance.
    pub fn relative_luminance(self) -> f64 {
        0.2126 * srgb_to_linear(self.r)
            + 0.7152 * srgb_to_linear(self.g)
            + 0.0722 * srgb_to_linear(self.b)
    }
}

fn srgb_to_linear(value: u8) -> f64 {
    let channel = value as f64 / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f64) -> u8 {
    let channel = value.clamp(0.0, 1.0);
    let encoded = if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

fn lab_f(value: f64) -> f64 {
    if value > 0.008_856 {
        value.cbrt()
    } else {
        7.787 * value + 16.0 / 116.0
    }
}

fn lab_f_inverse(value: f64) -> f64 {
    let cube = value * value * value;
    if cube > 0.008_856 {
        cube
    } else {
        (value - 16.0 / 116.0) / 7.787
    }
}

/// HSL colour; hue in degrees, saturation/lightness in percent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Hsl {
    /// Hue 0..360.
    pub h: f64,
    /// Saturation 0..100.
    pub s: f64,
    /// Lightness 0..100.
    pub l: f64,
}

impl Hsl {
    /// Convert to RGB.
    pub fn to_rgb(self) -> Rgba {
        let h = self.h.rem_euclid(360.0) / 360.0;
        let s = (self.s / 100.0).clamp(0.0, 1.0);
        let l = (self.l / 100.0).clamp(0.0, 1.0);
        if s <= f64::EPSILON {
            let value = (l * 255.0).round() as u8;
            return Rgba::rgb(value, value, value);
        }
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        let channel = |t: f64| {
            let t = if t < 0.0 {
                t + 1.0
            } else if t > 1.0 {
                t - 1.0
            } else {
                t
            };
            let value = if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 1.0 / 2.0 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            };
            (value * 255.0).round().clamp(0.0, 255.0) as u8
        };
        Rgba::rgb(channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
    }
}

/// CMYK colour in percent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Cmyk {
    /// Cyan 0..100.
    pub c: f64,
    /// Magenta 0..100.
    pub m: f64,
    /// Yellow 0..100.
    pub y: f64,
    /// Key/black 0..100.
    pub k: f64,
}

impl Cmyk {
    /// Convert to RGB.
    pub fn to_rgb(self) -> Rgba {
        let c = (self.c / 100.0).clamp(0.0, 1.0);
        let m = (self.m / 100.0).clamp(0.0, 1.0);
        let y = (self.y / 100.0).clamp(0.0, 1.0);
        let k = (self.k / 100.0).clamp(0.0, 1.0);
        Rgba::rgb(
            ((1.0 - c) * (1.0 - k) * 255.0).round() as u8,
            ((1.0 - m) * (1.0 - k) * 255.0).round() as u8,
            ((1.0 - y) * (1.0 - k) * 255.0).round() as u8,
        )
    }
}

/// CIE L*a*b*.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Lab {
    /// Lightness 0..100.
    pub l: f64,
    /// Green/red axis.
    pub a: f64,
    /// Blue/yellow axis.
    pub b: f64,
}

impl Lab {
    /// Convert back to sRGB.
    pub fn to_rgb(self) -> Rgba {
        let fy = (self.l + 16.0) / 116.0;
        let fx = fy + self.a / 500.0;
        let fz = fy - self.b / 200.0;
        let x = 0.950_47 * lab_f_inverse(fx);
        let y = lab_f_inverse(fy);
        let z = 1.088_83 * lab_f_inverse(fz);
        let r = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
        let g = -0.969_266_0 * x + 1.876_010_8 * y + 0.041_556_0 * z;
        let b = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;
        Rgba::rgb(linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b))
    }
}

/// Supported output notations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// `#RRGGBB`.
    Hex,
    /// `rgb(r, g, b)`.
    Rgb,
    /// `rgba(r, g, b, a)`.
    Rgba,
    /// `hsl(h, s%, l%)`.
    Hsl,
    /// `hsla(h, s%, l%, a)`.
    Hsla,
    /// `cmyk(c%, m%, y%, k%)`.
    Cmyk,
    /// `lab(l, a, b)`.
    Lab,
}

impl ColorSpace {
    /// Parse a notation name.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hex" => Ok(Self::Hex),
            "rgb" => Ok(Self::Rgb),
            "rgba" => Ok(Self::Rgba),
            "hsl" => Ok(Self::Hsl),
            "hsla" => Ok(Self::Hsla),
            "cmyk" => Ok(Self::Cmyk),
            "lab" => Ok(Self::Lab),
            other => Err(FormfluxError::unsupported(format!(
                "unknown colour space '{other}'"
            ))),
        }
    }
}

/// Parse hex, `rgb(...)`, `rgba(...)`, `hsl(...)`, `hsla(...)`, `cmyk(...)` or
/// `lab(...)`.
pub fn parse(value: &str) -> Result<Rgba> {
    let input = value.trim();
    if input.starts_with('#') {
        return parse_hex(input);
    }
    let lower = input.to_ascii_lowercase();
    if let Some(inner) = lower.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() != 3 {
            return Err(FormfluxError::invalid("rgb() needs three channels"));
        }
        return Ok(Rgba::rgb(
            parse_channel(parts[0])?,
            parse_channel(parts[1])?,
            parse_channel(parts[2])?,
        ));
    }
    if let Some(inner) = lower
        .strip_prefix("rgba(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() != 4 {
            return Err(FormfluxError::invalid("rgba() needs four channels"));
        }
        return Ok(Rgba {
            a: parse_alpha(parts[3])?,
            ..Rgba::rgb(
                parse_channel(parts[0])?,
                parse_channel(parts[1])?,
                parse_channel(parts[2])?,
            )
        });
    }
    if let Some(inner) = lower
        .strip_prefix("hsla(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() != 4 {
            return Err(FormfluxError::invalid("hsla() needs four values"));
        }
        let color = Hsl {
            h: number::<f64>(parts[0].trim_end_matches("deg"))?,
            s: number::<f64>(parts[1].trim_end_matches('%'))?,
            l: number::<f64>(parts[2].trim_end_matches('%'))?,
        }
        .to_rgb();
        return Ok(Rgba {
            a: parse_alpha(parts[3])?,
            ..color
        });
    }
    if let Some(inner) = lower.strip_prefix("hsl(").and_then(|v| v.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() != 3 {
            return Err(FormfluxError::invalid("hsl() needs three values"));
        }
        return Ok(Hsl {
            h: number::<f64>(parts[0].trim_end_matches("deg"))?,
            s: number::<f64>(parts[1].trim_end_matches('%'))?,
            l: number::<f64>(parts[2].trim_end_matches('%'))?,
        }
        .to_rgb());
    }
    if let Some(inner) = lower
        .strip_prefix("cmyk(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() != 4 {
            return Err(FormfluxError::invalid("cmyk() needs four values"));
        }
        return Ok(Cmyk {
            c: percentage(parts[0])?,
            m: percentage(parts[1])?,
            y: percentage(parts[2])?,
            k: percentage(parts[3])?,
        }
        .to_rgb());
    }
    if let Some(inner) = lower.strip_prefix("lab(").and_then(|v| v.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() != 3 {
            return Err(FormfluxError::invalid("lab() needs three values"));
        }
        return Ok(Lab {
            l: percentage_or_number(parts[0])?,
            a: number::<f64>(parts[1])?,
            b: number::<f64>(parts[2])?,
        }
        .to_rgb());
    }
    Err(FormfluxError::invalid(format!(
        "could not parse colour '{value}' (try #RRGGBB, rgb(...), rgba(...), hsl(...), hsla(...), cmyk(...) or lab(...))"
    )))
}

fn parse_alpha(value: &str) -> Result<u8> {
    if let Some(percent) = value.strip_suffix('%') {
        Ok((number::<f64>(percent)? / 100.0 * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8)
    } else if value.contains('.') {
        Ok((number::<f64>(value)? * 255.0).round().clamp(0.0, 255.0) as u8)
    } else {
        Ok(number::<u8>(value)?)
    }
}

fn percentage(value: &str) -> Result<f64> {
    percentage_or_number(value)
}

fn percentage_or_number(value: &str) -> Result<f64> {
    let value = value.trim_end_matches('%');
    Ok(number::<f64>(value)?.clamp(0.0, 100.0))
}

fn parse_channel(value: &str) -> Result<u8> {
    if let Some(percent) = value.strip_suffix('%') {
        let value: f64 = number(percent)?;
        Ok((value / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8)
    } else {
        Ok(number::<u8>(value)?)
    }
}

fn number<T>(value: &str) -> Result<T>
where
    T: std::str::FromStr,
{
    value
        .trim()
        .parse()
        .map_err(|_| FormfluxError::invalid(format!("invalid colour number '{value}'")))
}

/// Parse `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`.
pub fn parse_hex(value: &str) -> Result<Rgba> {
    let hex = value.trim().trim_start_matches('#');
    let digits: Vec<u8> = hex
        .chars()
        .map(|character| {
            character
                .to_digit(16)
                .map(|value| value as u8)
                .ok_or_else(|| FormfluxError::invalid(format!("invalid hex digit '{character}'")))
        })
        .collect::<Result<_>>()?;
    match digits.len() {
        3 => Ok(Rgba::rgb(digits[0] * 17, digits[1] * 17, digits[2] * 17)),
        4 => Ok(Rgba {
            r: digits[0] * 17,
            g: digits[1] * 17,
            b: digits[2] * 17,
            a: digits[3] * 17,
        }),
        6 => Ok(Rgba::rgb(
            digits[0] * 16 + digits[1],
            digits[2] * 16 + digits[3],
            digits[4] * 16 + digits[5],
        )),
        8 => Ok(Rgba {
            r: digits[0] * 16 + digits[1],
            g: digits[2] * 16 + digits[3],
            b: digits[4] * 16 + digits[5],
            a: digits[6] * 16 + digits[7],
        }),
        _ => Err(FormfluxError::invalid(
            "hex colours need 3, 4, 6 or 8 digits",
        )),
    }
}

/// Format a colour in a chosen notation.
pub fn format(color: Rgba, space: ColorSpace) -> String {
    match space {
        ColorSpace::Hex => color.to_hex(),
        ColorSpace::Rgb => format!("rgb({}, {}, {})", color.r, color.g, color.b),
        ColorSpace::Rgba => format!(
            "rgba({}, {}, {}, {:.3})",
            color.r,
            color.g,
            color.b,
            color.a as f64 / 255.0
        ),
        ColorSpace::Hsl => {
            let hsl = color.to_hsl();
            format!("hsl({:.0}, {:.0}%, {:.0}%)", hsl.h, hsl.s, hsl.l)
        }
        ColorSpace::Hsla => {
            let hsl = color.to_hsl();
            format!(
                "hsla({:.0}, {:.0}%, {:.0}%, {:.2})",
                hsl.h,
                hsl.s,
                hsl.l,
                color.a as f64 / 255.0
            )
        }
        ColorSpace::Cmyk => {
            let cmyk = color.to_cmyk();
            format!(
                "cmyk({:.0}%, {:.0}%, {:.0}%, {:.0}%)",
                cmyk.c, cmyk.m, cmyk.y, cmyk.k
            )
        }
        ColorSpace::Lab => {
            let lab = color.to_lab();
            format!("lab({:.2}, {:.2}, {:.2})", lab.l, lab.a, lab.b)
        }
    }
}

/// WCAG contrast result.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct WcagResult {
    /// Contrast ratio, 1..=21.
    pub ratio: f64,
    /// AA normal text (>= 4.5).
    pub aa_normal: bool,
    /// AA large text (>= 3.0).
    pub aa_large: bool,
    /// AAA normal text (>= 7.0).
    pub aaa_normal: bool,
    /// AAA large text (>= 4.5).
    pub aaa_large: bool,
}

/// Compute WCAG contrast for two opaque colours.
pub fn contrast(foreground: Rgba, background: Rgba) -> WcagResult {
    let first = foreground.relative_luminance();
    let second = background.relative_luminance();
    let (lighter, darker) = if first >= second {
        (first, second)
    } else {
        (second, first)
    };
    let ratio = (lighter + 0.05) / (darker + 0.05);
    WcagResult {
        ratio,
        aa_normal: ratio >= 4.5,
        aa_large: ratio >= 3.0,
        aaa_normal: ratio >= 7.0,
        aaa_large: ratio >= 4.5,
    }
}

/// Choose black or white for the best readable text on `background`.
pub fn best_foreground(background: Rgba) -> Rgba {
    let black = Rgba::rgb(0, 0, 0);
    let white = Rgba::rgb(255, 255, 255);
    if contrast(black, background).ratio >= contrast(white, background).ratio {
        black
    } else {
        white
    }
}

/// Interpolate a linear gradient in sRGB.
pub fn gradient(start: Rgba, end: Rgba, steps: usize) -> Vec<Rgba> {
    if steps == 0 {
        return Vec::new();
    }
    if steps == 1 {
        return vec![start];
    }
    (0..steps)
        .map(|index| {
            let t = index as f64 / (steps - 1) as f64;
            let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
            Rgba {
                r: mix(start.r, end.r),
                g: mix(start.g, end.g),
                b: mix(start.b, end.b),
                a: mix(start.a, end.a),
            }
        })
        .collect()
}

/// A named palette entry.
#[derive(Debug, Clone, Serialize)]
pub struct PaletteColor {
    /// Human name.
    pub name: String,
    /// Colour.
    pub color: Rgba,
}

/// Export a GIMP `.gpl` palette.
pub fn palette_gpl(name: &str, colors: &[PaletteColor]) -> String {
    let safe_name = name.replace(['\n', '\r'], " ");
    let mut out = format!("GIMP Palette\nName: {safe_name}\nColumns: 0\n#\n");
    for entry in colors {
        out.push_str(&format!(
            "{:3} {:3} {:3}\t{}\n",
            entry.color.r, entry.color.g, entry.color.b, entry.name
        ));
    }
    out
}

/// Export a JSON palette.
pub fn palette_json(name: &str, colors: &[PaletteColor]) -> Result<String> {
    let value = serde_json::json!({
        "name": name,
        "colors": colors.iter().map(|entry| serde_json::json!({
            "name": entry.name,
            "hex": entry.color.to_hex(),
            "rgb": [entry.color.r, entry.color.g, entry.color.b],
            "alpha": entry.color.a,
        })).collect::<Vec<_>>(),
    });
    Ok(serde_json::to_string_pretty(&value)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn hex_round_trips() {
        for input in ["#ff0000", "#0f08", "#11223344", "#abc"] {
            let color = parse(input).unwrap();
            let reparsed = parse(&color.to_hex()).unwrap();
            assert_eq!(color, reparsed);
        }
    }

    #[test]
    fn red_hsl_is_zero() {
        let hsl = Rgba::rgb(255, 0, 0).to_hsl();
        assert!(close(hsl.h, 0.0, 0.01));
        assert!(close(hsl.s, 100.0, 0.01));
        assert!(close(hsl.l, 50.0, 0.01));
    }

    #[test]
    fn lab_red_matches_reference() {
        let lab = Rgba::rgb(255, 0, 0).to_lab();
        assert!(close(lab.l, 53.24, 0.1));
        assert!(close(lab.a, 80.09, 0.2));
        assert!(close(lab.b, 67.20, 0.2));
        let back = lab.to_rgb();
        assert!(close(back.r as f64, 255.0, 1.0));
        assert!(close(back.g as f64, 0.0, 1.0));
        assert!(close(back.b as f64, 0.0, 1.0));
    }

    #[test]
    fn black_white_contrast_is_21() {
        let result = contrast(Rgba::rgb(0, 0, 0), Rgba::rgb(255, 255, 255));
        assert!(close(result.ratio, 21.0, 0.01));
        assert!(result.aaa_normal);
    }

    #[test]
    fn cmyk_round_trip_is_reasonable() {
        let cmyk = Rgba::rgb(255, 0, 0).to_cmyk();
        assert!(close(cmyk.c, 0.0, 0.01));
        assert!(close(cmyk.m, 100.0, 0.01));
        assert_eq!(cmyk.to_rgb(), Rgba::rgb(255, 0, 0));
    }

    #[test]
    fn gradient_has_endpoints() {
        let colors = gradient(Rgba::rgb(0, 0, 0), Rgba::rgb(255, 255, 255), 3);
        assert_eq!(colors.first().unwrap().r, 0);
        assert_eq!(colors.last().unwrap().r, 255);
        assert_eq!(colors.len(), 3);
    }

    #[test]
    fn parses_rgb_and_hsl() {
        assert_eq!(parse("rgb(255, 0, 0)").unwrap(), Rgba::rgb(255, 0, 0));
        assert_eq!(parse("hsl(0, 100%, 50%)").unwrap(), Rgba::rgb(255, 0, 0));
        assert_eq!(
            parse("hsla(0, 100%, 50%, 0.5)").unwrap(),
            Rgba {
                a: 128,
                ..Rgba::rgb(255, 0, 0)
            }
        );
        assert_eq!(
            parse("cmyk(0%, 100%, 100%, 0%)").unwrap(),
            Rgba::rgb(255, 0, 0)
        );
        let lab = Rgba::rgb(255, 0, 0).to_lab();
        let back = parse(&format!("lab({:.3}, {:.3}, {:.3})", lab.l, lab.a, lab.b)).unwrap();
        assert!(close(back.r as f64, 255.0, 1.0));
        assert!(close(back.g as f64, 0.0, 1.0));
        assert!(close(back.b as f64, 0.0, 1.0));
        assert!(parse("not-a-colour").is_err());
    }

    #[test]
    fn palette_formats_export() {
        let colors = vec![PaletteColor {
            name: "Red".to_string(),
            color: Rgba::rgb(255, 0, 0),
        }];
        assert!(palette_gpl("Test", &colors).contains("255   0   0"));
        assert!(palette_json("Test", &colors).unwrap().contains("#FF0000"));
    }
}

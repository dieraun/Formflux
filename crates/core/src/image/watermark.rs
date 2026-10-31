//! Text and image watermarks rendered onto an RGBA canvas.

use std::path::PathBuf;

use font8x8::{UnicodeFonts, BASIC_FONTS};
use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::error::{FormfluxError, Result};

/// Where a watermark is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    /// Top left.
    TopLeft,
    /// Top right.
    TopRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom right.
    BottomRight,
    /// Exact centre.
    Center,
}

impl Anchor {
    /// Parse a CLI anchor.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "top-left" | "topleft" | "tl" => Ok(Self::TopLeft),
            "top-right" | "topright" | "tr" => Ok(Self::TopRight),
            "bottom-left" | "bottomleft" | "bl" => Ok(Self::BottomLeft),
            "bottom-right" | "bottomright" | "br" => Ok(Self::BottomRight),
            "center" | "centre" | "c" => Ok(Self::Center),
            other => Err(FormfluxError::invalid(format!(
                "unknown watermark anchor '{other}'"
            ))),
        }
    }

    fn origin(self, canvas: (u32, u32), overlay: (u32, u32), margin: u32) -> (i64, i64) {
        let (cw, ch) = (canvas.0 as i64, canvas.1 as i64);
        let (ow, oh) = (overlay.0 as i64, overlay.1 as i64);
        let m = margin as i64;
        match self {
            Self::TopLeft => (m, m),
            Self::TopRight => (cw - ow - m, m),
            Self::BottomLeft => (m, ch - oh - m),
            Self::BottomRight => (cw - ow - m, ch - oh - m),
            Self::Center => ((cw - ow) / 2, (ch - oh) / 2),
        }
    }
}

/// Watermark content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WatermarkKind {
    /// Rendered with a built-in 8x8 bitmap font.
    Text {
        /// Text to draw.
        text: String,
        /// Pixel height of a glyph (minimum 8).
        size: u32,
        /// RGBA colour.
        color: [u8; 4],
    },
    /// Another image, scaled to a percentage of the canvas width.
    Image {
        /// Source image.
        path: PathBuf,
        /// Width as a percentage of the canvas (1..=100).
        scale_percent: u32,
    },
}

/// A watermark request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watermark {
    /// Content.
    pub kind: WatermarkKind,
    /// Placement.
    pub anchor: Anchor,
    /// Distance from the canvas edge in pixels.
    pub margin: u32,
    /// 0..=1 blend factor.
    pub opacity: f32,
}

impl Watermark {
    /// Validate fields.
    pub fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(FormfluxError::invalid(
                "watermark opacity must be between 0 and 1",
            ));
        }
        match &self.kind {
            WatermarkKind::Text { text, size, .. } => {
                if text.is_empty() {
                    return Err(FormfluxError::invalid("watermark text is empty"));
                }
                if *size == 0 || *size > 512 {
                    return Err(FormfluxError::invalid(
                        "watermark text size must be between 1 and 512",
                    ));
                }
            }
            WatermarkKind::Image { scale_percent, .. } => {
                if *scale_percent == 0 || *scale_percent > 100 {
                    return Err(FormfluxError::invalid(
                        "watermark image scale must be between 1 and 100 percent",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Apply a watermark, returning an RGBA image.
pub fn apply(image: DynamicImage, watermark: &Watermark) -> Result<DynamicImage> {
    watermark.validate()?;
    let mut canvas = image.to_rgba8();
    match &watermark.kind {
        WatermarkKind::Text { text, size, color } => {
            draw_text(&mut canvas, text, *size, *color, watermark);
        }
        WatermarkKind::Image {
            path,
            scale_percent,
        } => {
            let overlay = super::load(path, false)?;
            let target_width = ((canvas.width() as f32) * (*scale_percent as f32) / 100.0)
                .round()
                .max(1.0) as u32;
            let overlay = super::ops::resize(
                overlay,
                &super::ops::ResizeOptions {
                    width: Some(target_width),
                    height: None,
                    mode: super::ops::ResizeMode::Fit,
                },
            )?;
            let (ow, oh) = overlay.dimensions();
            let mut overlay = overlay.to_rgba8();
            let opacity = watermark.opacity.clamp(0.0, 1.0);
            for pixel in overlay.pixels_mut() {
                pixel[3] = (pixel[3] as f32 * opacity).round() as u8;
            }
            let (x, y) = watermark.anchor.origin(
                (canvas.width(), canvas.height()),
                (ow, oh),
                watermark.margin,
            );
            let x = x.clamp(0, canvas.width().saturating_sub(1) as i64);
            let y = y.clamp(0, canvas.height().saturating_sub(1) as i64);
            image::imageops::overlay(&mut canvas, &overlay, x, y);
        }
    }
    Ok(DynamicImage::ImageRgba8(canvas))
}

fn draw_text(canvas: &mut RgbaImage, text: &str, size: u32, color: [u8; 4], watermark: &Watermark) {
    let scale = (size / 8).max(1);
    let glyph_width = 8 * scale;
    let spacing = scale;
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return;
    }
    let width = chars.len() as u32 * glyph_width + (chars.len() as u32 - 1) * spacing;
    let height = 8 * scale;
    let (x, y) = watermark.anchor.origin(
        (canvas.width(), canvas.height()),
        (width, height),
        watermark.margin,
    );
    let opacity = watermark.opacity.clamp(0.0, 1.0);
    for (index, character) in chars.into_iter().enumerate() {
        let upper = character.to_ascii_uppercase();
        let glyph = BASIC_FONTS
            .get(upper)
            .or_else(|| BASIC_FONTS.get('?'))
            .unwrap_or([0u8; 8]);
        let origin_x = x + index as i64 * (glyph_width + spacing) as i64;
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..8u32 {
                if (*bits) & (1u8 << column) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = origin_x + (column * scale + dx) as i64;
                        let py = y + (row as u32 * scale + dy) as i64;
                        blend_pixel(canvas, px, py, color, opacity);
                    }
                }
            }
        }
    }
}

fn blend_pixel(canvas: &mut RgbaImage, x: i64, y: i64, color: [u8; 4], opacity: f32) {
    if x < 0 || y < 0 || x >= canvas.width() as i64 || y >= canvas.height() as i64 {
        return;
    }
    let alpha = (color[3] as f32 / 255.0) * opacity;
    let pixel = canvas.get_pixel_mut(x as u32, y as u32);
    for channel in 0..3 {
        pixel[channel] = (color[channel] as f32 * alpha + pixel[channel] as f32 * (1.0 - alpha))
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    pixel[3] = 255;
}

/// Convenience constructor for a text watermark.
pub fn text(text: impl Into<String>, anchor: Anchor) -> Watermark {
    Watermark {
        kind: WatermarkKind::Text {
            text: text.into(),
            size: 24,
            color: [255, 255, 255, 230],
        },
        anchor,
        margin: 12,
        opacity: 0.85,
    }
}

/// Convenience constructor for an image watermark.
pub fn image(path: impl Into<PathBuf>, anchor: Anchor) -> Watermark {
    Watermark {
        kind: WatermarkKind::Image {
            path: path.into(),
            scale_percent: 20,
        },
        anchor,
        margin: 12,
        opacity: 0.85,
    }
}

/// Draw a single RGBA pixel; exposed for tests and simple overlays.
pub fn put_pixel(canvas: &mut RgbaImage, x: u32, y: u32, color: Rgba<u8>) {
    if x < canvas.width() && y < canvas.height() {
        canvas.put_pixel(x, y, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    fn sample() -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(80, 60, Rgba([10, 20, 30, 255])))
    }

    #[test]
    fn text_changes_pixels() {
        let options = Watermark {
            kind: WatermarkKind::Text {
                text: "HELLO".to_string(),
                size: 16,
                color: [255, 255, 255, 255],
            },
            anchor: Anchor::Center,
            margin: 0,
            opacity: 1.0,
        };
        let output = apply(sample(), &options).unwrap();
        assert_ne!(output.to_rgba8().as_raw(), sample().to_rgba8().as_raw());
    }

    #[test]
    fn image_watermark_is_scaled_and_blended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logo.png");
        RgbaImage::from_pixel(10, 10, Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        let options = Watermark {
            kind: WatermarkKind::Image {
                path,
                scale_percent: 50,
            },
            anchor: Anchor::TopLeft,
            margin: 0,
            opacity: 1.0,
        };
        let output = apply(sample(), &options).unwrap();
        assert_eq!(output.dimensions(), (80, 60));
        assert_eq!(output.to_rgba8().get_pixel(0, 0)[0], 255);
    }

    #[test]
    fn anchors_parse() {
        assert_eq!(Anchor::parse("br").unwrap(), Anchor::BottomRight);
        assert!(Anchor::parse("middle").is_err());
        assert!(Anchor::parse("center").is_ok());
    }
}

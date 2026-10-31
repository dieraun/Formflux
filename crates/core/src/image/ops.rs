//! Geometry and colour operations for images.

use image::{DynamicImage, GenericImageView, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::error::{FormfluxError, Result};

/// How a target box is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResizeMode {
    /// Fit inside the box while preserving aspect ratio (default).
    Fit,
    /// Force the exact box, possibly changing aspect ratio.
    Exact,
}

/// Resize request; at least one dimension must be present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeOptions {
    /// Target width.
    pub width: Option<u32>,
    /// Target height.
    pub height: Option<u32>,
    /// Interpretation of the box.
    pub mode: ResizeMode,
}

impl ResizeOptions {
    /// Validate bounds.
    pub fn validate(&self) -> Result<()> {
        if self.width.is_none() && self.height.is_none() {
            return Err(FormfluxError::invalid(
                "resize needs a width, a height, or both",
            ));
        }
        for value in [self.width, self.height].into_iter().flatten() {
            if value == 0 || value > 65_535 {
                return Err(FormfluxError::invalid(
                    "resize dimensions must be between 1 and 65535",
                ));
            }
        }
        Ok(())
    }
}

/// Image colour adjustments.  Defaults are identity values.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ImageAdjustments {
    /// Brightness offset, -100..=100.
    pub brightness: i32,
    /// Contrast offset, -100..=100.
    pub contrast: f32,
    /// Saturation multiplier, 0..=2 (1 is unchanged).
    pub saturation: f32,
    /// Gamma, 0.1..=5 (1 is unchanged).
    pub gamma: f32,
}

impl Default for ImageAdjustments {
    fn default() -> Self {
        Self {
            brightness: 0,
            contrast: 0.0,
            saturation: 1.0,
            gamma: 1.0,
        }
    }
}

impl ImageAdjustments {
    /// True when no adjustment would change pixels.
    pub fn is_identity(&self) -> bool {
        self.brightness == 0
            && self.contrast.abs() < f32::EPSILON
            && (self.saturation - 1.0).abs() < f32::EPSILON
            && (self.gamma - 1.0).abs() < f32::EPSILON
    }

    /// Validate ranges.
    pub fn validate(&self) -> Result<()> {
        if !(-100..=100).contains(&self.brightness) {
            return Err(FormfluxError::invalid(
                "brightness must be between -100 and 100",
            ));
        }
        if !(-100.0..=100.0).contains(&self.contrast) {
            return Err(FormfluxError::invalid(
                "contrast must be between -100 and 100",
            ));
        }
        if !(0.0..=2.0).contains(&self.saturation) {
            return Err(FormfluxError::invalid("saturation must be between 0 and 2"));
        }
        if !(0.1..=5.0).contains(&self.gamma) {
            return Err(FormfluxError::invalid("gamma must be between 0.1 and 5"));
        }
        Ok(())
    }
}

/// Resize an image.
pub fn resize(image: DynamicImage, options: &ResizeOptions) -> Result<DynamicImage> {
    options.validate()?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(FormfluxError::Image(
            "cannot resize an empty image".to_string(),
        ));
    }
    let resized = match (options.width, options.height, options.mode) {
        (Some(w), Some(h), ResizeMode::Fit) => {
            image.resize(w, h, image::imageops::FilterType::Lanczos3)
        }
        (Some(w), Some(h), ResizeMode::Exact) => {
            image.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        }
        (Some(w), None, _) => {
            let h = ((height as f64) * (w as f64) / (width as f64))
                .round()
                .max(1.0) as u32;
            image.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        }
        (None, Some(h), _) => {
            let w = ((width as f64) * (h as f64) / (height as f64))
                .round()
                .max(1.0) as u32;
            image.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        }
        (None, None, _) => return Err(FormfluxError::invalid("resize dimensions are missing")),
    };
    Ok(resized)
}

/// Crop an image by absolute coordinates.
pub fn crop(image: DynamicImage, x: u32, y: u32, width: u32, height: u32) -> Result<DynamicImage> {
    let (w, h) = image.dimensions();
    let right = x.checked_add(width);
    let bottom = y.checked_add(height);
    match (right, bottom) {
        (Some(right), Some(bottom)) if width > 0 && height > 0 && right <= w && bottom <= h => {
            Ok(image.crop_imm(x, y, width, height))
        }
        _ => Err(FormfluxError::invalid(format!(
            "crop {width}x{height}+{x}+{y} exceeds the {w}x{h} image"
        ))),
    }
}

/// Rotate clockwise by 0/90/180/270 degrees.
pub fn rotate(image: DynamicImage, degrees: u16) -> Result<DynamicImage> {
    match degrees {
        0 => Ok(image),
        90 => Ok(image.rotate90()),
        180 => Ok(image.rotate180()),
        270 => Ok(image.rotate270()),
        other => Err(FormfluxError::invalid(format!(
            "rotation must be 0, 90, 180 or 270 (got {other})"
        ))),
    }
}

/// Flip horizontally and/or vertically.
pub fn flip(image: DynamicImage, horizontal: bool, vertical: bool) -> DynamicImage {
    let mut image = image;
    if horizontal {
        image = image.fliph();
    }
    if vertical {
        image = image.flipv();
    }
    image
}

/// Apply brightness, contrast, saturation and gamma.
pub fn adjust(image: DynamicImage, adjustments: &ImageAdjustments) -> Result<DynamicImage> {
    adjustments.validate()?;
    if adjustments.is_identity() {
        return Ok(image);
    }
    let mut rgba: RgbaImage = image.to_rgba8();
    let brightness = adjustments.brightness as f32 * 2.55;
    let contrast_value = adjustments.contrast * 2.55;
    let contrast_factor = (259.0 * (contrast_value + 255.0)) / (255.0 * (259.0 - contrast_value));
    let gamma = adjustments.gamma.max(0.01);
    let saturation = adjustments.saturation;
    let mut lut = [0u8; 256];
    for (value, slot) in lut.iter_mut().enumerate() {
        let normalized = value as f32 / 255.0;
        let corrected = normalized.powf(1.0 / gamma) * 255.0;
        let contrasted = contrast_factor * (corrected - 128.0) + 128.0;
        *slot = (contrasted + brightness).clamp(0.0, 255.0).round() as u8;
    }
    for pixel in rgba.pixels_mut() {
        let r = lut[pixel[0] as usize] as f32;
        let g = lut[pixel[1] as usize] as f32;
        let b = lut[pixel[2] as usize] as f32;
        let luma = 0.299 * r + 0.587 * g + 0.114 * b;
        pixel[0] = (luma + (r - luma) * saturation).clamp(0.0, 255.0) as u8;
        pixel[1] = (luma + (g - luma) * saturation).clamp(0.0, 255.0) as u8;
        pixel[2] = (luma + (b - luma) * saturation).clamp(0.0, 255.0) as u8;
    }
    Ok(DynamicImage::ImageRgba8(rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn sample() -> DynamicImage {
        let mut image = RgbaImage::new(20, 10);
        for (x, _y, pixel) in image.enumerate_pixels_mut() {
            *pixel = Rgba([(x * 10) as u8, 100, 50, 255]);
        }
        DynamicImage::ImageRgba8(image)
    }

    #[test]
    fn preserves_aspect_ratio_with_one_dimension() {
        let resized = resize(
            sample(),
            &ResizeOptions {
                width: Some(40),
                height: None,
                mode: ResizeMode::Fit,
            },
        )
        .unwrap();
        assert_eq!(resized.dimensions(), (40, 20));
    }

    #[test]
    fn exact_changes_aspect_ratio() {
        let resized = resize(
            sample(),
            &ResizeOptions {
                width: Some(40),
                height: Some(40),
                mode: ResizeMode::Exact,
            },
        )
        .unwrap();
        assert_eq!(resized.dimensions(), (40, 40));
    }

    #[test]
    fn invalid_crop_rejected() {
        assert!(crop(sample(), 15, 0, 10, 10).is_err());
        assert!(crop(sample(), 0, 0, 10, 10).is_ok());
    }

    #[test]
    fn rotate_swaps_dimensions() {
        assert_eq!(rotate(sample(), 90).unwrap().dimensions(), (10, 20));
        assert!(rotate(sample(), 45).is_err());
    }

    #[test]
    fn adjustments_change_pixels() {
        let adjusted = adjust(
            sample(),
            &ImageAdjustments {
                brightness: 20,
                contrast: 10.0,
                saturation: 1.5,
                gamma: 0.9,
            },
        )
        .unwrap();
        assert_eq!(adjusted.dimensions(), (20, 10));
        assert!(adjustments_change(sample(), adjusted));
    }

    fn adjustments_change(before: DynamicImage, after: DynamicImage) -> bool {
        before.to_rgba8().as_raw() != after.to_rgba8().as_raw()
    }

    #[test]
    fn identity_is_noop() {
        assert!(ImageAdjustments::default().is_identity());
    }
}

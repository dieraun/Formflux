//! Image decoding, processing and atomic encoding.
//!
//! The baseline build supports JPEG, PNG, WebP, BMP, TIFF, ICO and GIF.  AVIF
//! is available behind the `avif` feature.  HEIC and RAW are advertised as
//! explicit build variants but require external codecs; this module refuses
//! them with a clear message instead of silently producing broken files.

pub mod exif;
pub mod ops;
pub mod watermark;

pub use exif::{read_exif, strip_exif, ExifField, ExifSummary};
pub use ops::{adjust, crop, flip, resize, rotate, ImageAdjustments, ResizeMode, ResizeOptions};
pub use watermark::{Anchor, Watermark, WatermarkKind};

use std::io::{BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use formflux_shared::fs as sfs;
use formflux_shared::limits::{MemoryBudget, MemoryPermit};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat, RgbaImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cancel::CancelToken;
use crate::error::{FormfluxError, Result};
use crate::io;
use crate::progress::{ProgressEvent, ProgressSink};

/// Baseline output formats.
pub const FORMATS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "bmp", "tiff", "tif", "ico", "gif",
];

/// Output format selected by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// JPEG.
    Jpeg,
    /// PNG.
    Png,
    /// WebP.
    WebP,
    /// BMP.
    Bmp,
    /// TIFF.
    Tiff,
    /// ICO.
    Ico,
    /// GIF.
    Gif,
    /// AVIF (feature-gated).
    Avif,
}

impl OutputFormat {
    /// Parse a user extension.
    pub fn parse(value: &str) -> Result<Self> {
        match value
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "jpg" | "jpeg" | "jpe" => Ok(Self::Jpeg),
            "png" => Ok(Self::Png),
            "webp" => Ok(Self::WebP),
            "bmp" => Ok(Self::Bmp),
            "tif" | "tiff" => Ok(Self::Tiff),
            "ico" => Ok(Self::Ico),
            "gif" => Ok(Self::Gif),
            "avif" if cfg!(feature = "avif") => Ok(Self::Avif),
            "avif" => Err(FormfluxError::unsupported(
                "AVIF support is not compiled in; rebuild with --features avif",
            )),
            "heic" | "heif" => Err(FormfluxError::unsupported(
                "HEIC/HEIF decoding needs libheif and is only available in a full build",
            )),
            "raw" | "dng" | "cr2" | "nef" | "arw" => Err(FormfluxError::unsupported(
                "RAW decoding needs the raw feature and a full build",
            )),
            other => Err(FormfluxError::unsupported(format!(
                "unsupported image format '{other}'"
            ))),
        }
    }

    /// Canonical extension.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::WebP => "webp",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Ico => "ico",
            Self::Gif => "gif",
            Self::Avif => "avif",
        }
    }

    /// Map to the `image` crate format.
    pub fn to_image_format(self) -> ImageFormat {
        match self {
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Png => ImageFormat::Png,
            Self::WebP => ImageFormat::WebP,
            Self::Bmp => ImageFormat::Bmp,
            Self::Tiff => ImageFormat::Tiff,
            Self::Ico => ImageFormat::Ico,
            Self::Gif => ImageFormat::Gif,
            Self::Avif => ImageFormat::Avif,
        }
    }
}

/// One conversion or processing request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageOptions {
    /// Target extension (`png`, `jpg`, ...).
    pub format: String,
    /// Encoder quality, 1..=100 (JPEG/WebP).
    pub quality: u8,
    /// Resize before encoding.
    pub resize: Option<ResizeOptions>,
    /// Crop rectangle.
    pub crop: Option<CropSpec>,
    /// Clockwise rotation.
    pub rotate: u16,
    /// Horizontal flip.
    pub flip_horizontal: bool,
    /// Vertical flip.
    pub flip_vertical: bool,
    /// Colour adjustments.
    pub adjustments: ImageAdjustments,
    /// Watermark overlay.
    pub watermark: Option<Watermark>,
    /// Apply EXIF orientation automatically.
    pub auto_orient: bool,
    /// Remove metadata by re-encoding (always true for non-JPEG outputs).
    pub strip_metadata: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            format: String::new(),
            quality: 90,
            resize: None,
            crop: None,
            rotate: 0,
            flip_horizontal: false,
            flip_vertical: false,
            adjustments: ImageAdjustments::default(),
            watermark: None,
            auto_orient: true,
            strip_metadata: true,
        }
    }
}

/// Serialisable crop rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropSpec {
    /// Left offset.
    pub x: u32,
    /// Top offset.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl ImageOptions {
    /// Validate dimensions and quality.
    pub fn validate(&self) -> Result<()> {
        if !(1..=100).contains(&self.quality) {
            return Err(FormfluxError::invalid("quality must be between 1 and 100"));
        }
        if !matches!(self.rotate, 0 | 90 | 180 | 270) {
            return Err(FormfluxError::invalid("rotation must be 0, 90, 180 or 270"));
        }
        if let Some(crop) = self.crop {
            if crop.width == 0 || crop.height == 0 {
                return Err(FormfluxError::invalid(
                    "crop width and height must be positive",
                ));
            }
        }
        if let Some(resize) = &self.resize {
            resize.validate()?;
        }
        Ok(())
    }
}

/// Result of one image operation.
#[derive(Debug, Clone, Serialize)]
pub struct ImageOutcome {
    /// Input file.
    pub input: PathBuf,
    /// Written file.
    pub output: PathBuf,
    /// Output format.
    pub format: String,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Input size.
    pub input_bytes: u64,
    /// Output size.
    pub output_bytes: u64,
}

/// Basic image facts used by the GUI preview.
#[derive(Debug, Clone, Serialize)]
pub struct ImageInfo {
    /// Canonical path.
    pub path: PathBuf,
    /// Detected format.
    pub format: String,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Colour type label.
    pub color: String,
    /// File size.
    pub size_bytes: u64,
}

/// Decode an image, applying EXIF orientation when requested.
pub fn load(path: &Path, auto_orient: bool) -> Result<DynamicImage> {
    let path = io::existing_file(path)?;
    let reader = image::ImageReader::open(&path)
        .map_err(|error| {
            FormfluxError::Image(format!("could not open '{}': {error}", path.display()))
        })?
        .with_guessed_format()
        .map_err(|error| FormfluxError::Image(format!("could not detect image format: {error}")))?;
    let mut image = reader.decode().map_err(|error| {
        FormfluxError::Image(format!("could not decode '{}': {error}", path.display()))
    })?;
    if auto_orient {
        image = apply_orientation(image, exif::orientation(&path));
    }
    Ok(image)
}

fn apply_orientation(image: DynamicImage, orientation: Option<u16>) -> DynamicImage {
    match orientation {
        Some(2) => image.fliph(),
        Some(3) => image.rotate180(),
        Some(4) => image.flipv(),
        Some(5) => image.rotate90().fliph(),
        Some(6) => image.rotate90(),
        Some(7) => image.rotate270().fliph(),
        Some(8) => image.rotate270(),
        _ => image,
    }
}

/// Inspect an image without retaining its pixels.
pub fn info(path: &Path) -> Result<ImageInfo> {
    let path = io::existing_file(path)?;
    let reader = image::ImageReader::open(&path)
        .map_err(|error| FormfluxError::Image(format!("could not open image: {error}")))?
        .with_guessed_format()
        .map_err(|error| FormfluxError::Image(format!("could not detect image format: {error}")))?;
    let format = reader
        .format()
        .map(|format| {
            format
                .extensions_str()
                .first()
                .copied()
                .unwrap_or("unknown")
        })
        .unwrap_or("unknown")
        .to_string();
    let decoder = reader
        .into_decoder()
        .map_err(|error| FormfluxError::Image(format!("could not start decoder: {error}")))?;
    let (width, height) = decoder.dimensions();
    let color = format!("{:?}", decoder.color_type()).to_lowercase();
    Ok(ImageInfo {
        size_bytes: path.metadata().map(|m| m.len()).unwrap_or(0),
        path,
        format,
        width,
        height,
        color,
    })
}

/// Apply every requested pixel operation in a stable order.
fn process(mut image: DynamicImage, options: &ImageOptions) -> Result<DynamicImage> {
    if let Some(crop) = options.crop {
        image = crop_image(image, crop)?;
    }
    if options.rotate != 0 {
        image = rotate(image, options.rotate)?;
    }
    if options.flip_horizontal {
        image = image.fliph();
    }
    if options.flip_vertical {
        image = image.flipv();
    }
    if let Some(resize_options) = &options.resize {
        image = resize(image, resize_options)?;
    }
    if options.adjustments.is_identity() {
        // no-op
    } else {
        image = adjust(image, &options.adjustments)?;
    }
    if let Some(watermark) = &options.watermark {
        image = watermark::apply(image, watermark)?;
    }
    Ok(image)
}

fn crop_image(image: DynamicImage, crop: CropSpec) -> Result<DynamicImage> {
    let (width, height) = image.dimensions();
    let right = crop.x.checked_add(crop.width);
    let bottom = crop.y.checked_add(crop.height);
    match (right, bottom) {
        (Some(right), Some(bottom)) if right <= width && bottom <= height => {
            Ok(image.crop_imm(crop.x, crop.y, crop.width, crop.height))
        }
        _ => Err(FormfluxError::invalid(format!(
            "crop {}x{}+{}+{} exceeds the {}x{} image",
            crop.width, crop.height, crop.x, crop.y, width, height
        ))),
    }
}

/// Process-wide budget for decoded image buffers.  The default matches the
/// shipped configuration (512 MiB); a single oversized request is rejected
/// instead of letting the process be killed by the OOM killer.
fn image_budget() -> &'static MemoryBudget {
    static BUDGET: OnceLock<MemoryBudget> = OnceLock::new();
    BUDGET.get_or_init(|| MemoryBudget::new(512 * 1024 * 1024))
}

/// Rough decoded-size estimate: compressed bytes times twelve, clamped so a
/// tiny icon reserves something sensible and a huge scan cannot monopolise the
/// whole budget.  The estimate is deliberately conservative; it bounds
/// concurrency, not exact allocation.
fn estimate_image_memory(input: &Path) -> u64 {
    const MIB: u64 = 1024 * 1024;
    input
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0)
        .saturating_mul(12)
        .clamp(16 * MIB, 384 * MIB)
}

fn reserve_image_memory(input: &Path) -> Result<MemoryPermit<'static>> {
    let estimate = estimate_image_memory(input);
    image_budget()
        .reserve_timeout(estimate, Some(std::time::Duration::from_secs(30)))
        .map_err(|error| match error {
            FormfluxError::Limit(message) => {
                FormfluxError::Limit(format!("waiting for image memory budget: {message}"))
            }
            other => other,
        })
}

/// Convert/process one image into `output_dir`.
pub fn convert(
    input: &Path,
    output_dir: &Path,
    options: &ImageOptions,
    overwrite: bool,
) -> Result<ImageOutcome> {
    options.validate()?;
    let input = io::existing_file(input)?;
    io::ensure_dir(output_dir)?;
    let _memory = reserve_image_memory(&input)?;
    let format = if options.format.trim().is_empty() {
        let extension = input
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| FormfluxError::invalid("input has no extension; specify --format"))?;
        OutputFormat::parse(extension)?
    } else {
        OutputFormat::parse(&options.format)?
    };
    let image = load(&input, options.auto_orient)?;
    let image = process(image, options)?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(FormfluxError::Image(
            "the decoded image is empty".to_string(),
        ));
    }
    let target = sfs::destination_for(&input, output_dir, Some(format.extension()), overwrite)?;
    write_image(&image, &target, format, options.quality, overwrite)?;
    Ok(ImageOutcome {
        input: input.clone(),
        output: target.clone(),
        format: format.extension().to_string(),
        width,
        height,
        input_bytes: input.metadata().map(|m| m.len()).unwrap_or(0),
        output_bytes: target.metadata().map(|m| m.len()).unwrap_or(0),
    })
}

/// Compress an image into the same format (or `options.format`).
pub fn compress(
    input: &Path,
    output_dir: &Path,
    options: &ImageOptions,
    overwrite: bool,
) -> Result<ImageOutcome> {
    convert(input, output_dir, options, overwrite)
}

/// Encode `image` atomically to `target`.
pub fn write_image(
    image: &DynamicImage,
    target: &Path,
    format: OutputFormat,
    quality: u8,
    overwrite: bool,
) -> Result<()> {
    if let Some(parent) = target.parent() {
        io::ensure_dir(parent)?;
    }
    let writer = sfs::AtomicWriter::new(target, overwrite)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, writer);
    match format {
        OutputFormat::Jpeg => {
            // JPEG has no alpha channel; drop it explicitly instead of letting
            // the encoder reject RGBA input.
            let rgb = image.to_rgb8();
            let encoder = JpegEncoder::new_with_quality(&mut writer, quality.clamp(1, 100));
            encoder
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|error| FormfluxError::Image(format!("JPEG encoding failed: {error}")))?;
        }
        OutputFormat::Png => {
            let encoder = PngEncoder::new_with_quality(
                &mut writer,
                CompressionType::Default,
                FilterType::Adaptive,
            );
            encoder
                .write_image(
                    image.as_bytes(),
                    image.width(),
                    image.height(),
                    image.color().into(),
                )
                .map_err(|error| FormfluxError::Image(format!("PNG encoding failed: {error}")))?;
        }
        OutputFormat::WebP => {
            let encoder = WebPEncoder::new_lossless(&mut writer);
            encoder
                .write_image(
                    image.as_bytes(),
                    image.width(),
                    image.height(),
                    image.color().into(),
                )
                .map_err(|error| FormfluxError::Image(format!("WebP encoding failed: {error}")))?;
        }
        other => {
            let mut buffer = Cursor::new(Vec::new());
            image
                .write_to(&mut buffer, other.to_image_format())
                .map_err(|error| {
                    FormfluxError::Image(format!("{other:?} encoding failed: {error}"))
                })?;
            writer.write_all(buffer.get_ref())?;
        }
    }
    writer
        .flush()
        .map_err(|error| FormfluxError::Io(std::io::Error::new(error.kind(), error)))?;
    let atomic = writer
        .into_inner()
        .map_err(|error| FormfluxError::Io(error.into_error()))?;
    atomic.finish()?;
    Ok(())
}

/// Batch convert images in parallel.  `jobs == 0` uses logical CPUs.
pub fn batch_convert(
    inputs: &[PathBuf],
    output_dir: &Path,
    options: &ImageOptions,
    overwrite: bool,
    jobs: usize,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<Vec<ImageOutcome>> {
    if inputs.is_empty() {
        return Err(FormfluxError::invalid("no input images"));
    }
    options.validate()?;
    io::ensure_dir(output_dir)?;
    let total = inputs.len();
    let done = AtomicUsize::new(0);
    let threads = if jobs == 0 {
        formflux_shared::platform::logical_cpus()
    } else {
        jobs.clamp(1, 256)
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|error| {
            FormfluxError::internal(format!("could not build worker pool: {error}"))
        })?;
    let results: Vec<Result<ImageOutcome>> = pool.install(|| {
        inputs
            .par_iter()
            .map(|input| {
                if cancel.is_cancelled() {
                    return Err(FormfluxError::Cancelled);
                }
                let result = convert(input, output_dir, options, overwrite);
                let completed = done.fetch_add(1, Ordering::Relaxed) + 1;
                progress.report(
                    ProgressEvent::new("image", completed as f64 / total as f64 * 100.0)
                        .with_message(input.display().to_string()),
                );
                result
            })
            .collect()
    });
    let mut outcomes = Vec::with_capacity(results.len());
    let mut first_error = None;
    for result in results {
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(outcomes)
}

/// Read the raw RGBA buffer of an image (used by the GUI preview).
pub fn rgba(path: &Path) -> Result<RgbaImage> {
    Ok(load(path, true)?.to_rgba8())
}

/// Read a file as a DynamicImage without EXIF handling.
pub fn decode(path: &Path) -> Result<DynamicImage> {
    load(path, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use std::fs;

    fn sample(dir: &Path) -> PathBuf {
        let mut image = RgbaImage::new(40, 30);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = Rgba([(x * 6) as u8, (y * 8) as u8, 128, 255]);
        }
        let path = dir.join("sample.png");
        image.save(&path).unwrap();
        path
    }

    #[test]
    fn converts_to_jpeg_and_resizes() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        let options = ImageOptions {
            format: "jpg".to_string(),
            quality: 80,
            resize: Some(ResizeOptions {
                width: Some(20),
                height: None,
                mode: ResizeMode::Fit,
            }),
            ..ImageOptions::default()
        };
        let outcome = convert(&input, dir.path(), &options, false).unwrap();
        assert_eq!(outcome.width, 20);
        assert_eq!(outcome.height, 15);
        assert!(outcome.output.exists());
        assert_eq!(outcome.format, "jpg");
    }

    #[test]
    fn refuses_crop_outside_image() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        let options = ImageOptions {
            format: "png".to_string(),
            crop: Some(CropSpec {
                x: 30,
                y: 0,
                width: 20,
                height: 10,
            }),
            ..ImageOptions::default()
        };
        assert!(convert(&input, dir.path(), &options, false).is_err());
    }

    #[test]
    fn refused_overwrite_leaves_original() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        let original = std::fs::read(&input).unwrap();
        let options = ImageOptions {
            format: "png".to_string(),
            ..ImageOptions::default()
        };
        let first = convert(&input, dir.path(), &options, false).unwrap();
        assert_ne!(first.output, input);
        let second = convert(&input, dir.path(), &options, false).unwrap();
        assert_ne!(second.output, first.output);
        assert_eq!(std::fs::read(&input).unwrap(), original);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn rotation_and_flip_are_applied() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        let options = ImageOptions {
            format: "png".to_string(),
            rotate: 90,
            flip_horizontal: true,
            ..ImageOptions::default()
        };
        let outcome = convert(&input, dir.path(), &options, false).unwrap();
        assert_eq!((outcome.width, outcome.height), (30, 40));
    }

    #[test]
    fn batch_handles_multiple_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut inputs = Vec::new();
        for index in 0..3 {
            let mut image = RgbaImage::new(10, 10);
            image.put_pixel(0, 0, Rgba([index, 2, 3, 255]));
            let path = dir.path().join(format!("in{index}.png"));
            image.save(&path).unwrap();
            inputs.push(path);
        }
        let output = dir.path().join("out");
        let options = ImageOptions {
            format: "jpg".to_string(),
            ..ImageOptions::default()
        };
        let outcomes = batch_convert(
            &inputs,
            &output,
            &options,
            false,
            2,
            &CancelToken::new(),
            &crate::progress::NoopProgress,
        )
        .unwrap();
        assert_eq!(outcomes.len(), 3);
    }

    #[test]
    fn format_parsing_is_explicit() {
        assert_eq!(OutputFormat::parse("JPG").unwrap(), OutputFormat::Jpeg);
        assert!(OutputFormat::parse("heic").is_err());
        assert!(OutputFormat::parse("exe").is_err());
    }

    #[test]
    fn info_reads_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        let info = info(&input).unwrap();
        assert_eq!((info.width, info.height), (40, 30));
        assert_eq!(info.format, "png");
    }

    #[test]
    fn text_watermark_encodes() {
        let dir = tempfile::tempdir().unwrap();
        let input = sample(dir.path());
        let options = ImageOptions {
            format: "png".to_string(),
            watermark: Some(Watermark {
                kind: WatermarkKind::Text {
                    text: "FORMFLUX".to_string(),
                    size: 12,
                    color: [255, 255, 255, 220],
                },
                anchor: Anchor::BottomRight,
                margin: 2,
                opacity: 0.8,
            }),
            ..ImageOptions::default()
        };
        let outcome = convert(&input, dir.path(), &options, false).unwrap();
        assert!(outcome.output.exists());
    }
}

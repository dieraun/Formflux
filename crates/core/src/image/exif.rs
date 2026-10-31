//! EXIF inspection and metadata stripping.
//!
//! `kamadak-exif` is a reader; stripping is therefore implemented by decoding
//! and re-encoding the pixels, which also removes ICC/comment metadata.  This
//! is deliberate for a privacy tool: nothing from the source container is
//! copied unless the user explicitly asks for a metadata-preserving workflow.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use exif::{In, Reader, Tag};
use serde::Serialize;

use crate::error::{FormfluxError, Result};
use crate::io;

/// One displayed EXIF field.
#[derive(Debug, Clone, Serialize)]
pub struct ExifField {
    /// Tag name.
    pub tag: String,
    /// Rendered value.
    pub value: String,
}

/// Readable EXIF summary.
#[derive(Debug, Clone, Serialize)]
pub struct ExifSummary {
    /// Source path.
    pub path: PathBuf,
    /// Whether any EXIF fields were found.
    pub has_exif: bool,
    /// Orientation tag (1..=8).
    pub orientation: Option<u16>,
    /// Camera/software fields.
    pub camera_make: Option<String>,
    /// Camera model.
    pub camera_model: Option<String>,
    /// Original capture time.
    pub captured_at: Option<String>,
    /// GPS coordinates, when present.
    pub gps: Option<String>,
    /// All fields.
    pub fields: Vec<ExifField>,
}

/// Read EXIF without modifying the file.
pub fn read_exif(path: &Path) -> Result<ExifSummary> {
    let path = io::existing_file(path)?;
    let file = File::open(&path)?;
    let mut reader = BufReader::new(file);
    let exif = match Reader::new().read_from_container(&mut reader) {
        Ok(exif) => exif,
        Err(exif::Error::NotFound(_)) => {
            return Ok(ExifSummary {
                path,
                has_exif: false,
                orientation: None,
                camera_make: None,
                camera_model: None,
                captured_at: None,
                gps: None,
                fields: Vec::new(),
            });
        }
        Err(error) => {
            return Err(FormfluxError::invalid(format!(
                "could not read EXIF: {error}"
            )));
        }
    };
    let mut fields = Vec::new();
    for field in exif.fields() {
        fields.push(ExifField {
            tag: field.tag.to_string(),
            value: field.display_value().with_unit(&exif).to_string(),
        });
    }
    let orientation = orientation_from(&exif);
    let camera_make = text_field(&exif, Tag::Make);
    let camera_model = text_field(&exif, Tag::Model);
    let captured_at =
        text_field(&exif, Tag::DateTimeOriginal).or_else(|| text_field(&exif, Tag::DateTime));
    let gps = gps_text(&exif);
    Ok(ExifSummary {
        has_exif: !fields.is_empty(),
        orientation,
        camera_make,
        camera_model,
        captured_at,
        gps,
        fields,
        path,
    })
}

/// Internal orientation lookup used during decoding.
pub(crate) fn orientation(path: &Path) -> Option<u16> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let exif = Reader::new().read_from_container(&mut reader).ok()?;
    orientation_from(&exif)
}

fn orientation_from(exif: &exif::Exif) -> Option<u16> {
    exif.get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| (1..=8).contains(value))
}

fn text_field(exif: &exif::Exif, tag: Tag) -> Option<String> {
    exif.get_field(tag, In::PRIMARY)
        .map(|field| field.display_value().with_unit(exif).to_string())
        .map(|value| value.trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
}

fn gps_text(exif: &exif::Exif) -> Option<String> {
    let latitude = exif
        .get_field(Tag::GPSLatitude, In::PRIMARY)
        .map(|field| field.display_value().to_string());
    let longitude = exif
        .get_field(Tag::GPSLongitude, In::PRIMARY)
        .map(|field| field.display_value().to_string());
    match (latitude, longitude) {
        (Some(lat), Some(lon)) => Some(format!("{lat}, {lon}")),
        _ => None,
    }
}

/// Strip all metadata by re-encoding pixels into `output_dir`.
pub fn strip_exif(input: &Path, output_dir: &Path, overwrite: bool) -> Result<PathBuf> {
    let input = io::existing_file(input)?;
    io::ensure_dir(output_dir)?;
    let extension = input
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| FormfluxError::invalid("input has no extension"))?;
    let format = super::OutputFormat::parse(extension)?;
    let image = super::load(&input, false)?;
    let target = formflux_shared::fs::destination_for(
        &input,
        output_dir,
        Some(format.extension()),
        overwrite,
    )?;
    super::write_image(&image, &target, format, 95, overwrite)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn missing_exif_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.png");
        RgbaImage::from_pixel(4, 4, Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        let summary = read_exif(&path).unwrap();
        assert!(!summary.has_exif);
        assert!(summary.orientation.is_none());
    }

    #[test]
    fn strip_reencodes_without_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        image::RgbImage::from_pixel(8, 8, image::Rgb([100, 120, 140]))
            .save(&path)
            .unwrap();
        let output = dir.path().join("out");
        let target = strip_exif(&path, &output, false).unwrap();
        assert!(target.exists());
        assert_eq!(target.file_name().unwrap(), "photo.jpg");
    }
}

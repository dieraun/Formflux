//! Cross-module integration tests using only generated files.

use std::path::Path;

use formflux_core::cancel::CancelToken;
use formflux_core::hash::{self, HashAlgorithm};
use formflux_core::image::{self as imglib, ImageOptions};
use formflux_core::progress::NoopProgress;

fn write_png(path: &Path) {
    let mut buffer = image::RgbaImage::new(16, 12);
    for (x, y, pixel) in buffer.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x * 8) as u8, (y * 12) as u8, 180, 255]);
    }
    buffer.save(path).expect("write fixture png");
}

#[test]
fn image_conversion_and_manifest_verification() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("fixture.png");
    write_png(&input);

    let options = ImageOptions {
        format: "jpg".to_string(),
        quality: 82,
        ..ImageOptions::default()
    };
    let outcome = imglib::convert(&input, dir.path(), &options, false).unwrap();
    assert_eq!(outcome.format, "jpg");
    assert!(outcome.output.exists());
    assert_eq!((outcome.width, outcome.height), (16, 12));

    let file = hash::hash_file(&outcome.output, HashAlgorithm::Sha256).unwrap();
    let name = outcome.output.file_name().unwrap().to_string_lossy();
    let manifest = format!("{}  {name}\n", file.digest);

    let results =
        hash::verify_manifest(&manifest, dir.path(), &CancelToken::new(), &NoopProgress).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].ok, "{results:?}");

    let wrong = format!("{}  {name}\n", "0".repeat(64));
    let results =
        hash::verify_manifest(&wrong, dir.path(), &CancelToken::new(), &NoopProgress).unwrap();
    assert!(!results[0].ok);
}

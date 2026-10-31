//! Offline OCR through an external Tesseract binary.
//!
//! No model or engine is bundled.  If Tesseract is missing the command fails
//! with an actionable hint while the rest of Formflux keeps working.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;

use crate::detect;
use crate::error::{FormfluxError, Result};
use crate::external::{hidden_tokio_command, tail};
use crate::io;

/// OCR request.
#[derive(Debug, Clone)]
pub struct OcrOptions {
    /// Tesseract language codes, e.g. `eng` or `eng+deu`.
    pub language: String,
    /// Page segmentation mode (0..=13).
    pub psm: u8,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            language: "eng".to_string(),
            psm: 3,
        }
    }
}

/// OCR output.
#[derive(Debug, Clone, Serialize)]
pub struct OcrResult {
    /// Source image.
    pub input: PathBuf,
    /// Recognized text.
    pub text: String,
    /// Language used.
    pub language: String,
    /// Confidence is not exposed by the plain-text mode; `None` means unknown.
    pub confidence: Option<f32>,
}

/// Run Tesseract on one image.
pub async fn ocr(tesseract: &Path, input: &Path, options: &OcrOptions) -> Result<OcrResult> {
    let input = io::existing_file(input)?;
    if options.language.trim().is_empty() {
        return Err(FormfluxError::invalid("OCR language is empty"));
    }
    if options.psm > 13 {
        return Err(FormfluxError::invalid("Tesseract PSM must be 0..=13"));
    }
    let output = hidden_tokio_command(tesseract)
        .arg(&input)
        .arg("stdout")
        .args(["-l", options.language.trim()])
        .args(["--psm", &options.psm.to_string()])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| FormfluxError::ToolUnavailable {
            tool: "tesseract".to_string(),
            hint: format!(
                "could not start {}: {error}; install Tesseract or set tools.tesseract_path",
                tesseract.display()
            ),
        })?;
    if !output.status.success() {
        return Err(FormfluxError::ExternalToolFailed {
            tool: "tesseract".to_string(),
            code: output.status.code().unwrap_or(-1),
            message: tail(&String::from_utf8_lossy(&output.stderr), 8192),
        });
    }
    Ok(OcrResult {
        input,
        text: String::from_utf8_lossy(&output.stdout).into_owned(),
        language: options.language.clone(),
        confidence: None,
    })
}

/// Convenience wrapper using the configured Tesseract path.
pub async fn ocr_with_config(
    config: &formflux_shared::config::AppConfig,
    input: &Path,
    language: Option<&str>,
    psm: u8,
) -> Result<OcrResult> {
    let tesseract = detect::tesseract(config)?;
    let options = OcrOptions {
        language: language.unwrap_or(&config.tools.ocr_lang).to_string(),
        psm,
    };
    ocr(&tesseract, input, &options).await
}

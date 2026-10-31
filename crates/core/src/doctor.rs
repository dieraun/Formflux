//! Runtime diagnostics for `formflux doctor`.

use formflux_shared::config::AppConfig;
use formflux_shared::platform;
use serde::Serialize;

use crate::detect::{self, ToolInfo};
use crate::error::Result;

/// Complete environment report.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    /// Formflux version.
    pub version: String,
    /// Operating system.
    pub os: String,
    /// CPU architecture.
    pub arch: String,
    /// Rust target triple used for the build.
    pub target: String,
    /// Logical CPU count.
    pub cpus: usize,
    /// Effective worker count.
    pub jobs: usize,
    /// FFmpeg status.
    pub ffmpeg: ToolInfo,
    /// FFprobe status.
    pub ffprobe: ToolInfo,
    /// Tesseract status.
    pub tesseract: ToolInfo,
    /// CUDA status.
    pub cuda: formflux_cuda::CudaInfo,
    /// Human-readable warnings, empty when everything is usable.
    pub warnings: Vec<String>,
}

impl DoctorReport {
    /// Collect the report.  Never fails for a missing optional tool.
    pub fn collect(config: &AppConfig) -> Self {
        let ffmpeg = detect::inspect("ffmpeg", config.tools.ffmpeg_path.as_deref(), &["-version"]);
        let ffprobe = detect::inspect(
            "ffprobe",
            config.tools.ffprobe_path.as_deref(),
            &["-version"],
        );
        let tesseract = detect::inspect(
            "tesseract",
            config.tools.tesseract_path.as_deref(),
            &["--version"],
        );
        let cuda = formflux_cuda::CudaInfo::detect(config.tools.cuda);
        let mut warnings = Vec::new();
        if !ffmpeg.available || !ffprobe.available {
            warnings.push(
                "FFmpeg/ffprobe not available: media features are disabled, everything else works"
                    .to_string(),
            );
        }
        if !tesseract.available {
            warnings.push(
                "Tesseract not available: OCR is disabled until it is installed or configured"
                    .to_string(),
            );
        }
        if cuda.build_feature && !cuda.active {
            warnings.push(cuda.detail.clone());
        }
        Self {
            version: formflux_shared::VERSION.to_string(),
            os: platform::os_name().to_string(),
            arch: platform::arch().to_string(),
            target: platform::target_triple().to_string(),
            cpus: platform::logical_cpus(),
            jobs: config.effective_jobs(),
            ffmpeg,
            ffprobe,
            tesseract,
            cuda,
            warnings,
        }
    }

    /// Collect using the default configuration, ignoring load failures.
    pub fn collect_default() -> Self {
        let config = AppConfig::load_default().unwrap_or_default();
        Self::collect(&config)
    }

    /// True when media operations can run.
    pub fn media_ready(&self) -> bool {
        self.ffmpeg.available && self.ffprobe.available
    }

    /// One-line summary.
    pub fn headline(&self) -> String {
        let media = if self.media_ready() {
            "media ready"
        } else {
            "media disabled"
        };
        format!(
            "Formflux {} on {} ({}), {} CPU threads, {media}",
            self.version, self.os, self.arch, self.cpus
        )
    }
}

/// Apply CLI/config overrides for tool paths before collection.
pub fn with_overrides(
    config: &AppConfig,
    ffmpeg: Option<std::path::PathBuf>,
    ffprobe: Option<std::path::PathBuf>,
    tesseract: Option<std::path::PathBuf>,
) -> AppConfig {
    let mut config = config.clone();
    if let Some(path) = ffmpeg {
        config.tools.ffmpeg_path = Some(path);
    }
    if let Some(path) = ffprobe {
        config.tools.ffprobe_path = Some(path);
    }
    if let Some(path) = tesseract {
        config.tools.tesseract_path = Some(path);
    }
    config
}

/// Ensure the local configuration directory exists and return its path.
pub fn ensure_config_dir() -> Result<std::path::PathBuf> {
    let path = AppConfig::default_path()?;
    if let Some(parent) = path.parent() {
        formflux_shared::fs::ensure_dir(parent)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_collects_without_panicking() {
        let config = AppConfig::default();
        let report = DoctorReport::collect(&config);
        assert!(report.cpus >= 1);
        assert!(!report.version.is_empty());
        assert!(report.headline().contains("Formflux"));
    }
}

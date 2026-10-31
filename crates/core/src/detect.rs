//! Detection of optional external runtimes.

use std::path::{Path, PathBuf};

use formflux_shared::config::AppConfig;
use serde::Serialize;

use crate::error::{FormfluxError, Result};
use crate::external::hidden_command;

/// Result of probing one optional executable.
#[derive(Debug, Clone, Serialize)]
pub struct ToolInfo {
    /// Tool name (`ffmpeg`).
    pub name: String,
    /// True when the path came from configuration rather than `PATH`.
    pub configured: bool,
    /// Resolved executable path.
    pub path: Option<PathBuf>,
    /// Whether the executable exists and answered `--version`.
    pub available: bool,
    /// First version line, when available.
    pub version: Option<String>,
    /// Extra detail or a recovery hint.
    pub detail: Option<String>,
}

impl ToolInfo {
    /// One-line summary for doctor output.
    pub fn summary(&self) -> String {
        match (&self.path, self.available) {
            (Some(path), true) => {
                let version = self.version.as_deref().unwrap_or("unknown version");
                format!("{}: {version} ({})", self.name, path.display())
            }
            (Some(path), false) => format!(
                "{}: configured at {} but not runnable",
                self.name,
                path.display()
            ),
            (None, _) => format!("{}: not found", self.name),
        }
    }
}

/// Locate and version-check one executable.
///
/// This function is synchronous and is only used during startup/doctor; the
/// child process is short-lived.
pub fn inspect(name: &str, configured: Option<&Path>, version_args: &[&str]) -> ToolInfo {
    let path = crate::external::locate(configured, name);
    let Some(path) = path else {
        return ToolInfo {
            name: name.to_string(),
            configured: configured.is_some(),
            path: None,
            available: false,
            version: None,
            detail: Some(if configured.is_some() {
                "the configured path does not exist or is not a file".to_string()
            } else {
                format!("install {name} and make sure it is on PATH")
            }),
        };
    };
    let output = hidden_command(&path).args(version_args).output();
    match output {
        Ok(output)
            if output.status.success()
                || !output.stdout.is_empty()
                || !output.stderr.is_empty() =>
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let first = stdout
                .lines()
                .chain(stderr.lines())
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string);
            ToolInfo {
                name: name.to_string(),
                configured: configured.is_some(),
                path: Some(path),
                available: output.status.success() || first.is_some(),
                version: first,
                detail: None,
            }
        }
        Ok(output) => ToolInfo {
            name: name.to_string(),
            configured: configured.is_some(),
            path: Some(path),
            available: false,
            version: None,
            detail: Some(format!("'{name}' exited with status {}", output.status)),
        },
        Err(error) => ToolInfo {
            name: name.to_string(),
            configured: configured.is_some(),
            path: Some(path),
            available: false,
            version: None,
            detail: Some(format!("could not start '{name}': {error}")),
        },
    }
}

/// Resolved FFmpeg pair required by the media module.
#[derive(Debug, Clone)]
pub struct FfmpegTools {
    /// FFmpeg executable.
    pub ffmpeg: PathBuf,
    /// FFprobe executable.
    pub ffprobe: PathBuf,
    /// Version banner.
    pub version: Option<String>,
}

impl FfmpegTools {
    /// Resolve both executables or explain what is missing.
    pub fn require(config: &AppConfig) -> Result<Self> {
        let ffmpeg = inspect("ffmpeg", config.tools.ffmpeg_path.as_deref(), &["-version"]);
        let ffprobe = inspect(
            "ffprobe",
            config.tools.ffprobe_path.as_deref(),
            &["-version"],
        );
        let mut missing = Vec::new();
        if !ffmpeg.available {
            missing.push("ffmpeg");
        }
        if !ffprobe.available {
            missing.push("ffprobe");
        }
        if !missing.is_empty() {
            return Err(FormfluxError::ToolUnavailable {
                tool: missing.join(" and "),
                hint: "install FFmpeg (which includes ffprobe) and add it to PATH, or set tools.ffmpeg_path and tools.ffprobe_path in config.toml".to_string(),
            });
        }
        Ok(Self {
            ffmpeg: ffmpeg.path.expect("available tool has a path"),
            ffprobe: ffprobe.path.expect("available tool has a path"),
            version: ffmpeg.version,
        })
    }

    /// Whether both binaries look usable (cheap check, no subprocess).
    pub fn is_available(config: &AppConfig) -> bool {
        inspect("ffmpeg", config.tools.ffmpeg_path.as_deref(), &["-version"]).available
            && inspect(
                "ffprobe",
                config.tools.ffprobe_path.as_deref(),
                &["-version"],
            )
            .available
    }
}

/// Locate Tesseract when OCR is requested.
pub fn tesseract(config: &AppConfig) -> Result<PathBuf> {
    let info = inspect(
        "tesseract",
        config.tools.tesseract_path.as_deref(),
        &["--version"],
    );
    if info.available {
        Ok(info.path.expect("available tool has a path"))
    } else {
        Err(FormfluxError::ToolUnavailable {
            tool: "tesseract".to_string(),
            hint: info.detail.unwrap_or_else(|| {
                "install Tesseract OCR and add it to PATH, or set tools.tesseract_path".to_string()
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_explicit_path_is_reported() {
        let info = inspect(
            "definitely-not-installed",
            Some(Path::new("definitely-not-a-real-tool-xyz")),
            &["--version"],
        );
        assert!(!info.available);
        assert!(info.configured);
        assert!(info.path.is_none());
        assert!(info.summary().contains("not found"));
    }
}

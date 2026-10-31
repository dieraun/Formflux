//! Versioned TOML configuration stored in the platform config directory.
//!
//! A missing file is not an error: callers get [`AppConfig::default`] and can
//! persist it on first run.  A malformed file *is* reported, so users are
//! never silently running with unexpected settings.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::{FormfluxError, Result};
use crate::fs as ffs;

/// Environment variable that overrides the configuration file location.
pub const CONFIG_ENV: &str = "FORMFLUX_CONFIG";

/// Interface language.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// English.
    #[default]
    En,
    /// Simplified Chinese.
    Zh,
}

impl Language {
    /// Stable identifier used by the CLI and JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Zh => "zh",
        }
    }

    /// Parse `en`, `en-US`, `zh`, `zh-CN` and similar tags.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.to_ascii_lowercase();
        if value.starts_with("zh") {
            Ok(Self::Zh)
        } else if value.starts_with("en") {
            Ok(Self::En)
        } else {
            Err(FormfluxError::invalid(format!(
                "unknown language '{value}' (expected en or zh)"
            )))
        }
    }

    /// Pick the English or Chinese variant.
    pub fn pick<'a>(self, en: &'a str, zh: &'a str) -> &'a str {
        match self {
            Self::En => en,
            Self::Zh => zh,
        }
    }
}

/// Visual theme preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// Follow the operating system's light/dark setting.
    #[default]
    System,
    /// Force the dark theme.
    Dark,
    /// Force the light theme.
    Light,
}

impl ThemePreference {
    /// Stable lower-case identifier used by the CLI and JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// Parse a user-supplied theme name.
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "system" | "auto" => Ok(Self::System),
            "dark" => Ok(Self::Dark),
            "light" => Ok(Self::Light),
            other => Err(FormfluxError::invalid(format!(
                "unknown theme '{other}' (expected system, dark or light)"
            ))),
        }
    }
}

/// Paths to optional external tools.  `None` means "auto-detect from PATH".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// Explicit `ffmpeg` executable path.
    pub ffmpeg_path: Option<PathBuf>,
    /// Explicit `ffprobe` executable path.
    pub ffprobe_path: Option<PathBuf>,
    /// Explicit Tesseract executable path.
    pub tesseract_path: Option<PathBuf>,
    /// Default OCR language packs, for example `eng+deu`.
    pub ocr_lang: String,
    /// Enable the optional CUDA path when a compatible device is present.
    pub cuda: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            ffmpeg_path: None,
            ffprobe_path: None,
            tesseract_path: None,
            ocr_lang: "eng".to_string(),
            cuda: false,
        }
    }
}

/// Execution and resource settings shared by CLI and GUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// Worker count; `0` means "logical CPU count".
    pub jobs: usize,
    /// Soft memory budget for concurrent jobs, in MiB.
    pub memory_budget_mb: u64,
    /// Allow replacing existing outputs by default.
    pub overwrite: bool,
    /// Keep temporary/intermediate files for diagnostics.
    pub keep_temp: bool,
    /// Granularity of progress events in percent.
    pub progress_step: u8,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            jobs: 0,
            memory_budget_mb: 512,
            overwrite: false,
            keep_temp: false,
            progress_step: 1,
        }
    }
}

/// User-interface preferences.  Only the GUI reads these, but the CLI may
/// display them through `formflux config`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Interface language.
    pub language: Language,
    /// Light/dark theme mode.
    pub theme: ThemePreference,
    /// Extra scale factor applied to the whole UI (0.75 - 2.5).
    pub scale: f32,
    /// Last directory used for outputs.
    pub last_output_dir: Option<PathBuf>,
    /// Open the application maximized.
    pub start_maximized: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            language: Language::En,
            theme: ThemePreference::System,
            scale: 1.0,
            last_output_dir: None,
            start_maximized: false,
        }
    }
}

/// Top-level Formflux configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Schema version, currently `1`.
    pub version: u32,
    /// External tool configuration.
    pub tools: ToolsConfig,
    /// Runtime/resource configuration.
    pub runtime: RuntimeConfig,
    /// UI configuration.
    pub ui: UiConfig,
    /// Logging filter (`error`, `warn`, `info`, `debug`, `trace`).
    pub log_level: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            tools: ToolsConfig::default(),
            runtime: RuntimeConfig::default(),
            ui: UiConfig::default(),
            log_level: "info".to_string(),
        }
    }
}

impl AppConfig {
    /// Resolve the platform configuration file path, honouring
    /// `FORMFLUX_CONFIG`.
    pub fn default_path() -> Result<PathBuf> {
        if let Some(value) = std::env::var_os(CONFIG_ENV) {
            if !value.is_empty() {
                return Ok(PathBuf::from(value));
            }
        }
        let dirs = ProjectDirs::from("io", "formflux", "formflux").ok_or_else(|| {
            FormfluxError::Config(
                "could not determine a platform configuration directory".to_string(),
            )
        })?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Load configuration from an explicit path.  A missing file yields the
    /// defaults.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = ffs::read_text_limited(path, 1 << 20)?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    /// Load the platform configuration file.
    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path()?)
    }

    /// Load the platform configuration, creating a commented default file on
    /// first launch.  A broken existing file is still reported as an error
    /// instead of being silently replaced.
    pub fn load_or_create_default() -> Result<Self> {
        let path = Self::default_path()?;
        if path.exists() {
            Self::load(&path)
        } else {
            let config = Self::default();
            config.save(&path)?;
            Ok(config)
        }
    }

    /// Save configuration to an explicit path using an atomic replace.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            ffs::ensure_dir(parent)?;
        }
        let body = toml::to_string_pretty(self)?;
        let body = format!(
            "# Formflux configuration. Every field is optional.\n\
             # FFmpeg and Tesseract are external runtime dependencies and are never bundled.\n\
             # Formflux never makes network requests.\n\n{body}"
        );
        ffs::atomic_write(path, body.as_bytes(), true)
    }

    /// Save to the platform config location.
    pub fn save_default(&self) -> Result<PathBuf> {
        let path = Self::default_path()?;
        self.save(&path)?;
        Ok(path)
    }

    /// Ensure values are inside supported ranges.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(FormfluxError::config(format!(
                "unsupported config version {} (this build understands version 1)",
                self.version
            )));
        }
        if self.runtime.jobs > 256 {
            return Err(FormfluxError::config("runtime.jobs must be <= 256"));
        }
        if !(64..=1_048_576).contains(&self.runtime.memory_budget_mb) {
            return Err(FormfluxError::config(
                "runtime.memory_budget_mb must be between 64 and 1048576",
            ));
        }
        if !(1..=100).contains(&self.runtime.progress_step) {
            return Err(FormfluxError::config(
                "runtime.progress_step must be between 1 and 100",
            ));
        }
        if !(0.75..=2.5).contains(&self.ui.scale) {
            return Err(FormfluxError::config(
                "ui.scale must be between 0.75 and 2.5",
            ));
        }
        match self.log_level.to_ascii_lowercase().as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => {}
            _ => {
                return Err(FormfluxError::config(
                    "log_level must be one of error, warn, info, debug, trace",
                ))
            }
        }
        Ok(())
    }

    /// Effective worker count, never zero.
    pub fn effective_jobs(&self) -> usize {
        if self.runtime.jobs == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            self.runtime.jobs
        }
    }

    /// Effective soft memory budget in bytes.
    pub fn memory_budget_bytes(&self) -> u64 {
        self.runtime.memory_budget_mb.saturating_mul(1024 * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        AppConfig::default().validate().unwrap();
    }

    #[test]
    fn round_trips_through_toml() {
        let mut config = AppConfig::default();
        config.runtime.jobs = 3;
        config.ui.theme = ThemePreference::Dark;
        config.tools.ffmpeg_path = Some(PathBuf::from("C:/tools/ffmpeg.exe"));
        let encoded = toml::to_string_pretty(&config).unwrap();
        let decoded: AppConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(config, decoded);
    }

    #[test]
    fn saves_and_loads_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        let mut config = AppConfig::default();
        config.runtime.jobs = 7;
        config.save(&path).unwrap();
        assert_eq!(AppConfig::load(&path).unwrap().runtime.jobs, 7);
    }

    #[test]
    fn rejects_absurd_jobs() {
        let mut config = AppConfig::default();
        config.runtime.jobs = 10_000;
        assert!(config.validate().is_err());
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig::load(dir.path().join("absent.toml")).unwrap();
        assert_eq!(config, AppConfig::default());
    }

    #[test]
    fn theme_parser_is_case_insensitive() {
        assert_eq!(
            ThemePreference::parse("DARK").unwrap(),
            ThemePreference::Dark
        );
        assert!(ThemePreference::parse("neon").is_err());
    }
}

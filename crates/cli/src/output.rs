//! Shared CLI output and progress helpers.

use std::sync::Mutex;

use formflux_core::progress::{ProgressEvent, ProgressSink};
use formflux_shared::config::AppConfig;
use serde::Serialize;

/// Global output settings.
pub struct Context {
    /// JSON mode.
    pub json: bool,
    /// Quiet mode.
    pub quiet: bool,
    /// Dry-run mode.
    pub dry_run: bool,
    /// Overwrite existing files.
    pub force: bool,
    /// Confirmation bypass.
    pub yes: bool,
    /// Worker count override.
    pub jobs: usize,
}

impl Context {
    /// Emit a serializable value or a human string.
    pub fn emit<T: Serialize>(
        &self,
        value: &T,
        human: impl FnOnce() -> String,
    ) -> anyhow::Result<()> {
        if self.json {
            let value = serde_json::to_value(value)?;
            println!("{}", serde_json::to_string_pretty(&clean_paths(value))?);
        } else {
            println!("{}", human());
        }
        Ok(())
    }

    /// Emit plain text (wrapped as JSON when requested).
    pub fn text(&self, text: &str) -> anyhow::Result<()> {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&clean_paths(serde_json::json!({ "text": text })))?
            );
        } else {
            println!("{text}");
        }
        Ok(())
    }

    /// Emit one path.
    pub fn path(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&clean_paths(serde_json::json!({ "path": path })))?
            );
        } else {
            println!("{}", path.display());
        }
        Ok(())
    }

    /// Progress sink honouring `--quiet`.
    pub fn progress(&self, label: &'static str) -> CliProgress {
        CliProgress {
            label,
            quiet: self.quiet || self.json,
            last: Mutex::new(-10.0),
        }
    }
}

/// Threshold-based stderr progress.
pub struct CliProgress {
    label: &'static str,
    quiet: bool,
    last: Mutex<f64>,
}

impl ProgressSink for CliProgress {
    fn report(&self, event: ProgressEvent) {
        if self.quiet {
            return;
        }
        let Ok(mut last) = self.last.lock() else {
            return;
        };
        if event.percent < 100.0 && event.percent - *last < 10.0 {
            return;
        }
        *last = event.percent;
        let mut line = format!("{}: {:>5.1}%", self.label, event.percent);
        if !event.stage.is_empty() && event.stage != self.label {
            line.push_str(&format!(" ({})", event.stage));
        }
        if let Some(message) = &event.message {
            line.push_str(&format!("  {message}"));
        }
        if let Some(speed) = &event.speed {
            line.push_str(&format!("  {speed}"));
        }
        if let Some(eta) = event.eta_seconds {
            line.push_str(&format!("  ETA {}", formflux_core::format_eta(Some(eta))));
        }
        eprintln!("{line}");
    }
}

/// Make Windows canonical paths readable in JSON output.
///
/// `std::fs::canonicalize` returns verbatim paths such as `\\?\D:\media`.
/// They are useful internally but noisy for scripts and humans.
fn clean_paths(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(clean_path_text(text)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(clean_paths).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, clean_paths(value)))
                .collect(),
        ),
        other => other,
    }
}

fn clean_path_text(text: String) -> String {
    clean_text(&text)
}

/// Strip Windows verbatim prefixes from any message or path string.
pub(crate) fn clean_text(text: &str) -> String {
    let text = text.replace(r"\\?\UNC\", r"\\");
    text.replace(r"\\?\", "")
}

/// Load configuration, creating a default file on first launch and applying
/// CLI overrides without rewriting the file.
pub fn load_config(
    config_path: Option<&std::path::Path>,
    jobs: Option<usize>,
) -> anyhow::Result<AppConfig> {
    if let Some(path) = config_path {
        std::env::set_var(formflux_shared::config::CONFIG_ENV, path);
    }
    let mut config = AppConfig::load_or_create_default()
        .map_err(|error| anyhow::anyhow!("could not load configuration: {error}"))?;
    if let Some(jobs) = jobs {
        config.runtime.jobs = jobs;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::clean_text;

    #[test]
    fn strips_verbatim_prefixes() {
        assert_eq!(clean_text(r"\\?\D:\media\clip.mp4"), r"D:\media\clip.mp4");
        assert_eq!(
            clean_text(r"\\?\UNC\server\share\clip.mp4"),
            r"\\server\share\clip.mp4"
        );
        assert_eq!(clean_text("plain text"), "plain text");
    }
}

//! CUDA capability detection with a guaranteed CPU fallback.
//!
//! The `cuda` Cargo feature selects the optional build variant.  At runtime
//! Formflux probes the driver and device with `nvidia-smi`.  If no device or
//! driver is present the CPU implementation is used and a warning is logged;
//! a missing GPU can never make the application unusable.
//!
//! This module intentionally performs no network access and never links
//! against the CUDA toolkit at build time, which keeps the default binary
//! portable and strictly single-file.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Result of a CUDA capability check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CudaInfo {
    /// True when the optional `cuda` build feature is compiled in.
    pub build_feature: bool,
    /// True when `nvidia-smi` reports at least one device.
    pub device_present: bool,
    /// True when the build feature and a device are both available *and* the
    /// acceleration path is enabled by configuration.
    pub active: bool,
    /// GPU model name, when detected.
    pub device_name: Option<String>,
    /// NVIDIA driver version, when detected.
    pub driver_version: Option<String>,
    /// Human-readable explanation for logs and `formflux doctor`.
    pub detail: String,
}

impl CudaInfo {
    /// Probe the local machine.  Fast and non-fatal by design.
    pub fn detect(enabled_in_config: bool) -> Self {
        let build_feature = cfg!(feature = "cuda");
        if !build_feature {
            return Self {
                build_feature,
                device_present: false,
                active: false,
                device_name: None,
                driver_version: None,
                detail: "built without the optional cuda feature; using the CPU implementation"
                    .to_string(),
            };
        }
        let output = hidden(
            Command::new("nvidia-smi")
                .args(["--query-gpu=name,driver_version", "--format=csv,noheader"]),
        )
        .output();
        match output {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                let first = text.lines().find(|line| !line.trim().is_empty());
                let (device, driver) = first.map(parse_smi_line).unwrap_or_else(|| (None, None));
                let detected = device.is_some();
                let active = detected && enabled_in_config;
                let detail = if detected {
                    if enabled_in_config {
                        "CUDA device detected; the GPU path is selected where available".to_string()
                    } else {
                        "CUDA device detected, but tools.cuda is false; using CPU".to_string()
                    }
                } else {
                    "nvidia-smi ran but reported no GPU; using CPU".to_string()
                };
                Self {
                    build_feature,
                    device_present: detected,
                    active,
                    device_name: device,
                    driver_version: driver,
                    detail,
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Self {
                    build_feature,
                    device_present: false,
                    active: false,
                    device_name: None,
                    driver_version: None,
                    detail: format!(
                        "nvidia-smi failed ({}); using CPU",
                        stderr.lines().next().unwrap_or("no output")
                    ),
                }
            }
            Err(error) => Self {
                build_feature,
                device_present: false,
                active: false,
                device_name: None,
                driver_version: None,
                detail: format!("nvidia-smi unavailable ({error}); using CPU"),
            },
        }
    }

    /// A one-line status suitable for a banner.
    pub fn summary(&self) -> String {
        if self.active {
            format!(
                "CUDA active: {} (driver {})",
                self.device_name.as_deref().unwrap_or("unknown GPU"),
                self.driver_version.as_deref().unwrap_or("unknown")
            )
        } else if self.device_present {
            format!(
                "CUDA device present: {} (CPU path selected)",
                self.device_name.as_deref().unwrap_or("unknown GPU")
            )
        } else {
            "CUDA unavailable; CPU path active".to_string()
        }
    }
}

fn parse_smi_line(line: &str) -> (Option<String>, Option<String>) {
    let mut parts = line.split(',').map(str::trim);
    let name = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
    let driver = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
    (name, driver)
}

#[cfg(windows)]
fn hidden(command: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn hidden(command: &mut Command) -> &mut Command {
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_is_graceful() {
        let info = CudaInfo::detect(false);
        assert!(!info.active);
        assert!(!info.detail.is_empty());
    }

    #[test]
    fn smi_line_parsing() {
        let (name, driver) = parse_smi_line("NVIDIA GeForce RTX 4090, 555.42");
        assert_eq!(name.as_deref(), Some("NVIDIA GeForce RTX 4090"));
        assert_eq!(driver.as_deref(), Some("555.42"));
    }
}

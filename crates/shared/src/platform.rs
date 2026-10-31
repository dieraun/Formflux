//! Small platform facts used by `doctor`, the GUI about panel and filenames.

/// Operating-system family name.
pub fn os_name() -> &'static str {
    std::env::consts::OS
}

/// CPU architecture name.
pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// Target triple this binary was compiled for.
pub fn target_triple() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", _) => "pc-windows-msvc",
            ("macos", "aarch64") => "apple-darwin",
            ("macos", _) => "apple-darwin",
            ("linux", _) => "unknown-linux-gnu",
            (os, arch) => return format!("{arch}-unknown-{os}"),
        }
    )
}

/// Number of logical CPUs available to this process.
pub fn logical_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Platform executable suffix (`` or `.exe`).
pub fn executable_suffix() -> &'static str {
    std::env::consts::EXE_SUFFIX
}

/// True on Windows.
pub fn is_windows() -> bool {
    cfg!(windows)
}

/// True on macOS.
pub fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

/// True on Linux.
pub fn is_linux() -> bool {
    cfg!(target_os = "linux")
}

/// A human-readable one-line platform description.
pub fn describe() -> String {
    format!("{} {} ({} logical CPUs)", os_name(), arch(), logical_cpus())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_without_panicking() {
        assert!(!describe().is_empty());
        assert!(logical_cpus() >= 1);
    }
}

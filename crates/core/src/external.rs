//! Helpers for external processes (FFmpeg, Tesseract, `nvidia-smi`).
//!
//! No shell is ever involved.  On Windows the console window is suppressed so
//! GUI users do not see a flashing terminal.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

/// Create a `std::process::Command` that never allocates a console window on
/// Windows and never inherits stdin.
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Tokio equivalent of [`hidden_command`].
pub fn hidden_tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Return the last `max_chars` characters of `text`, trimmed, with a marker
/// when content was dropped.  Used to keep FFmpeg stderr diagnostics bounded.
pub fn tail(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let skip = trimmed.chars().count() - max_chars;
    let tail: String = trimmed.chars().skip(skip).collect();
    format!("…{tail}")
}

/// Locate an executable either at an explicit path or on `PATH`.
pub fn locate(configured: Option<&Path>, name: &str) -> Option<std::path::PathBuf> {
    match configured {
        Some(path) => {
            if path.is_file() {
                Some(path.to_path_buf())
            } else {
                None
            }
        }
        None => which::which(name).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("abcdef", 10), "abcdef");
        assert!(tail(&"x".repeat(100), 10).starts_with('…'));
        assert_eq!(tail(&"x".repeat(100), 10).chars().count(), 11);
    }

    #[test]
    fn locate_missing_explicit_path_is_none() {
        assert!(locate(Some(Path::new("definitely-not-a-real-tool-xyz")), "x").is_none());
    }
}

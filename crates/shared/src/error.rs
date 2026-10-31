//! The single error type used by every Formflux public API.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Formflux result alias.  Public APIs return this instead of foreign errors
/// so frontends can present consistent diagnostics.
pub type Result<T> = std::result::Result<T, FormfluxError>;

/// Every recoverable failure in Formflux.
#[derive(Debug, Error)]
pub enum FormfluxError {
    /// An operating-system I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// A caller supplied a value that cannot be processed.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The requested operation or format is not supported on this build.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// A path escaped its sandbox or contains a forbidden component.
    #[error("unsafe path '{}': {reason}", path.display())]
    UnsafePath {
        /// The offending path.
        path: PathBuf,
        /// Why the path was rejected.
        reason: String,
    },

    /// A required input path does not exist.
    #[error("path not found: {}", .0.display())]
    NotFound(PathBuf),

    /// An output path already exists and overwriting was not enabled.
    #[error("refusing to overwrite '{}' (pass --force to allow it)", .0.display())]
    AlreadyExists(PathBuf),

    /// An external runtime dependency (FFmpeg, Tesseract, ...) is unavailable.
    #[error("'{tool}' is unavailable: {hint}")]
    ToolUnavailable {
        /// Executable name, for example `ffmpeg`.
        tool: String,
        /// Human-readable recovery hint.
        hint: String,
    },

    /// An external process ran but reported failure.
    #[error("'{tool}' failed (exit code {code}): {message}")]
    ExternalToolFailed {
        /// Executable name.
        tool: String,
        /// Process exit code, or -1 if terminated by a signal.
        code: i32,
        /// Captured stderr tail.
        message: String,
    },

    /// The on-disk configuration is invalid.
    #[error("configuration error: {0}")]
    Config(String),

    /// A serialization/deserialization operation failed.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// Image decoding, processing or encoding failed.
    #[error("image error: {0}")]
    Image(String),

    /// Hashing or checksum verification failed.
    #[error("hash error: {0}")]
    Hash(String),

    /// The user cancelled the operation.
    #[error("operation cancelled")]
    Cancelled,

    /// A configured resource budget would be exceeded.
    #[error("resource limit: {0}")]
    Limit(String),

    /// A requested object does not exist.
    #[error("not found: {0}")]
    Missing(String),

    /// Invariant violation or an unexpected internal state.
    #[error("internal error: {0}")]
    Internal(String),
}

impl FormfluxError {
    /// Build an [`FormfluxError::InvalidInput`] from anything printable.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }

    /// Build an [`FormfluxError::Unsupported`] from anything printable.
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    /// Build an [`FormfluxError::Config`] from anything printable.
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }

    /// Build an [`FormfluxError::Internal`] from anything printable.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// Attach a human-readable context prefix while preserving the original
    /// error source.
    pub fn context(self, context: impl Into<String>) -> Self {
        let context = context.into();
        match self {
            Self::Io(source) => Self::InvalidInput(format!("{context}: {source}")),
            other => Self::InvalidInput(format!("{context}: {other}")),
        }
    }

    /// True when this error is a user cancellation.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// True when this error is an existing-output collision.
    pub fn is_already_exists(&self) -> bool {
        matches!(self, Self::AlreadyExists(_))
    }
}

impl From<serde_json::Error> for FormfluxError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value.to_string())
    }
}

impl From<serde_yaml::Error> for FormfluxError {
    fn from(value: serde_yaml::Error) -> Self {
        Self::Serialization(value.to_string())
    }
}

impl From<toml::de::Error> for FormfluxError {
    fn from(value: toml::de::Error) -> Self {
        Self::Config(value.to_string())
    }
}

impl From<toml::ser::Error> for FormfluxError {
    fn from(value: toml::ser::Error) -> Self {
        Self::Config(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_preserves_text() {
        let err = FormfluxError::invalid("bad frame").context("converting clip");
        assert_eq!(
            err.to_string(),
            "invalid input: converting clip: invalid input: bad frame"
        );
    }

    #[test]
    fn serde_errors_are_serialization_errors() {
        let err: FormfluxError = serde_json::from_str::<serde_json::Value>("{")
            .unwrap_err()
            .into();
        assert!(matches!(err, FormfluxError::Serialization(_)));
    }
}

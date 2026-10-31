//! Foundation crate for Formflux.
//!
//! Everything that every frontend needs but that is not itself a tool lives
//! here: the error type, configuration, logging, path safety, atomic writes,
//! resource limits and human-readable unit parsing.  This crate deliberately
//! has no dependency on any GUI toolkit or on the FFmpeg integration.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod fs;
pub mod limits;
pub mod logging;
pub mod platform;
pub mod units;

pub use config::{AppConfig, ThemePreference};
pub use error::{FormfluxError, Result};
pub use limits::{MemoryBudget, MemoryPermit};

/// Crate version reported by `formflux doctor` and the GUI about panel.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name used in banners and window titles.
pub const PRODUCT: &str = "Formflux";

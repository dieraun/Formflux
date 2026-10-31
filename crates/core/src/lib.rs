//! Frontend-independent Formflux tool implementations.
//!
//! The crate is deliberately free of `egui` and `clap`: the CLI and GUI are
//! thin adapters over these modules.  Every public function returns
//! [`Result`] and all disk access goes through [`io`] or
//! [`formflux_shared::fs`].

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod cancel;
pub mod color;
pub mod data;
pub mod detect;
pub mod doctor;
pub mod error;
pub mod external;
pub mod generate;
pub mod hash;
pub mod image;
pub mod io;
pub mod measure;
pub mod media;
pub mod ocr;
pub mod progress;
pub mod radix;
pub mod text;
pub mod time;
pub mod tools;

pub use formflux_shared::{units, FormfluxError, Result, VERSION};

/// Product name used in banners and window titles.
pub const PRODUCT: &str = "Formflux";

/// Format bytes with binary units (`1.5 MiB`).
pub fn format_bytes(bytes: u64) -> String {
    units::format_bytes(bytes)
}

/// Format a byte rate (`12.4 MiB/s`).
pub fn format_rate(bytes_per_second: f64) -> String {
    units::format_rate(bytes_per_second)
}

/// Format seconds as `mm:ss`/`hh:mm:ss`.
pub fn format_duration(seconds: f64) -> String {
    units::format_duration(seconds)
}

/// Format an optional ETA.
pub fn format_eta(seconds: Option<f64>) -> String {
    units::format_eta(seconds)
}

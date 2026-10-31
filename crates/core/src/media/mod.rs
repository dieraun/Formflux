//! FFmpeg-backed media operations.
//!
//! FFmpeg is never linked or bundled: every operation spawns the user's
//! `ffmpeg`/`ffprobe` binaries with an explicit argument vector (no shell).

pub mod convert;
pub mod probe;

pub use convert::{
    build_args, convert, ConvertOptions, ConvertOutcome, CropRect, Rotation, FORMATS_AUDIO,
    FORMATS_VIDEO,
};
pub use probe::{
    extract_subtitles, extract_subtitles_as, probe, thumbnail, MediaInfo, StreamInfo,
    SubtitleFormat, SubtitleOutcome,
};

//! Media conversion planning and execution.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use formflux_shared::fs as sfs;
use formflux_shared::units;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::cancel::CancelToken;
use crate::detect::FfmpegTools;
use crate::error::{FormfluxError, Result};
use crate::external::{hidden_tokio_command, tail};
use crate::io;
use crate::media::probe;
use crate::progress::{ProgressEvent, ProgressSink};

/// Video-capable output containers.
pub const FORMATS_VIDEO: &[&str] = &[
    "mp4", "webm", "mkv", "avi", "mov", "flv", "ts", "wmv", "gif", "webp",
];
/// Audio-only output containers.
pub const FORMATS_AUDIO: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "aac", "m4a", "alac", "ape", "wma", "opus",
];

/// Clockwise rotation applied before scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rotation {
    /// Keep orientation.
    #[default]
    None,
    /// Rotate 90 degrees clockwise.
    Cw90,
    /// Rotate 180 degrees.
    Cw180,
    /// Rotate 270 degrees clockwise.
    Cw270,
}

impl Rotation {
    /// FFmpeg filter expression, if any.
    pub fn filter(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Cw90 => Some("transpose=1"),
            Self::Cw180 => Some("hflip,vflip"),
            Self::Cw270 => Some("transpose=2"),
        }
    }

    /// Parse a CLI value (`90`, `cw90`, `180`, `270`).
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "0" | "none" | "no" => Ok(Self::None),
            "90" | "cw90" | "right" => Ok(Self::Cw90),
            "180" | "cw180" | "flip" | "upsidedown" => Ok(Self::Cw180),
            "270" | "cw270" | "left" => Ok(Self::Cw270),
            other => Err(FormfluxError::invalid(format!(
                "rotation must be 0, 90, 180 or 270 (got '{other}')"
            ))),
        }
    }
}

/// Crop rectangle in source pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRect {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Left offset.
    pub x: u32,
    /// Top offset.
    pub y: u32,
}

/// Everything that controls one conversion.  Every field is optional except
/// the output format, so the CLI and GUI can share the same structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvertOptions {
    /// Output container/extension (`mp4`, `mp3`, ...).
    pub format: String,
    /// Remux without re-encoding (`-c copy`).  Extremely fast and lossless.
    pub stream_copy: bool,
    /// Explicit video codec (`libx264`, `libvpx-vp9`, ...).
    pub video_codec: Option<String>,
    /// Explicit audio codec.
    pub audio_codec: Option<String>,
    /// Constant quality for encoders that support it (0-63; lower is better).
    pub crf: Option<u8>,
    /// Encoder speed preset (`ultrafast` ... `veryslow`).
    pub preset: Option<String>,
    /// Target video bitrate (`5M`).
    pub video_bitrate: Option<String>,
    /// Target audio bitrate (`192k`).
    pub audio_bitrate: Option<String>,
    /// Scale to this width (height follows aspect ratio).
    pub width: Option<u32>,
    /// Scale to this height (width follows aspect ratio).
    pub height: Option<u32>,
    /// Frame rate cap/duplication.
    pub fps: Option<f32>,
    /// Crop before scaling.
    pub crop: Option<CropRect>,
    /// Rotate before scaling.
    pub rotate: Rotation,
    /// Apply an HDR-to-SDR tone-mapping chain (needs zscale + tonemap).
    pub hdr_to_sdr: bool,
    /// EBU R128 loudness normalisation.
    pub normalize_audio: bool,
    /// FFT denoise before normalisation.
    pub denoise: bool,
    /// Audio sample rate.
    pub sample_rate: Option<u32>,
    /// Audio channel count.
    pub channels: Option<u16>,
    /// Start offset in seconds.
    pub start_seconds: Option<f64>,
    /// Maximum clip length in seconds.
    pub duration_seconds: Option<f64>,
    /// Drop audio entirely.
    pub no_audio: bool,
    /// Drop video entirely (audio extraction).
    pub no_video: bool,
    /// GIF frame rate (default 12).
    pub gif_fps: Option<f32>,
    /// GIF width (default: source width, capped by `width` when given).
    pub gif_width: Option<u32>,
    /// External subtitle file to mux in.
    pub embed_subtitles: Option<PathBuf>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            format: String::new(),
            stream_copy: false,
            video_codec: None,
            audio_codec: None,
            crf: None,
            preset: None,
            video_bitrate: None,
            audio_bitrate: None,
            width: None,
            height: None,
            fps: None,
            crop: None,
            rotate: Rotation::None,
            hdr_to_sdr: false,
            normalize_audio: false,
            denoise: false,
            sample_rate: None,
            channels: None,
            start_seconds: None,
            duration_seconds: None,
            no_audio: false,
            no_video: false,
            gif_fps: None,
            gif_width: None,
            embed_subtitles: None,
        }
    }
}

impl ConvertOptions {
    /// Validate the option combination before spawning FFmpeg.
    pub fn validate(&self) -> Result<()> {
        let format = normalize_format(&self.format)?;
        if self.no_video && matches!(format.as_str(), "gif" | "webp") {
            return Err(FormfluxError::invalid(format!(
                "{format} output requires a video stream"
            )));
        }
        if self.no_audio && self.no_video {
            return Err(FormfluxError::invalid(
                "cannot drop both audio and video; there would be nothing to write",
            ));
        }
        if let Some(crf) = self.crf {
            if crf > 63 {
                return Err(FormfluxError::invalid("crf must be between 0 and 63"));
            }
        }
        if let Some(preset) = &self.preset {
            const ALLOWED: &[&str] = &[
                "ultrafast",
                "superfast",
                "veryfast",
                "faster",
                "fast",
                "medium",
                "slow",
                "slower",
                "veryslow",
                "placebo",
            ];
            if !ALLOWED.contains(&preset.as_str()) {
                return Err(FormfluxError::invalid(format!(
                    "unknown preset '{preset}'; expected one of {}",
                    ALLOWED.join(", ")
                )));
            }
        }
        for (name, value) in [
            ("width", self.width),
            ("height", self.height),
            ("gif width", self.gif_width),
        ] {
            if let Some(value) = value {
                if value == 0 || value > 16_384 {
                    return Err(FormfluxError::invalid(format!(
                        "{name} must be between 1 and 16384"
                    )));
                }
            }
        }
        if let Some(fps) = self.fps {
            if !fps.is_finite() || !(1.0..=240.0).contains(&fps) {
                return Err(FormfluxError::invalid("fps must be between 1 and 240"));
            }
        }
        if let Some(fps) = self.gif_fps {
            if !fps.is_finite() || !(1.0..=60.0).contains(&fps) {
                return Err(FormfluxError::invalid("gif fps must be between 1 and 60"));
            }
        }
        if let Some(crop) = self.crop {
            if crop.width == 0 || crop.height == 0 {
                return Err(FormfluxError::invalid(
                    "crop width and height must be positive",
                ));
            }
        }
        if let Some(start) = self.start_seconds {
            if !start.is_finite() || start < 0.0 {
                return Err(FormfluxError::invalid("start time must be non-negative"));
            }
        }
        if let Some(duration) = self.duration_seconds {
            if !duration.is_finite() || duration <= 0.0 {
                return Err(FormfluxError::invalid("duration must be positive"));
            }
        }
        if let Some(rate) = self.sample_rate {
            if !(8_000..=768_000).contains(&rate) {
                return Err(FormfluxError::invalid(
                    "sample rate must be between 8000 and 768000 Hz",
                ));
            }
        }
        if let Some(channels) = self.channels {
            if channels == 0 || channels > 32 {
                return Err(FormfluxError::invalid("channels must be between 1 and 32"));
            }
        }
        if let Some(rate) = &self.video_bitrate {
            units::parse_bitrate(rate).map_err(|_| {
                FormfluxError::invalid(format!("invalid video bitrate '{rate}' (try 5M)"))
            })?;
        }
        if let Some(rate) = &self.audio_bitrate {
            units::parse_bitrate(rate).map_err(|_| {
                FormfluxError::invalid(format!("invalid audio bitrate '{rate}' (try 192k)"))
            })?;
        }
        if self.stream_copy {
            let transformed = self.crf.is_some()
                || self.video_codec.is_some()
                || self.audio_codec.is_some()
                || self.preset.is_some()
                || self.video_bitrate.is_some()
                || self.audio_bitrate.is_some()
                || self.width.is_some()
                || self.height.is_some()
                || self.fps.is_some()
                || self.crop.is_some()
                || self.rotate != Rotation::None
                || self.hdr_to_sdr
                || self.normalize_audio
                || self.denoise
                || self.sample_rate.is_some()
                || self.channels.is_some()
                || self.embed_subtitles.is_some()
                || self.gif_fps.is_some()
                || self.gif_width.is_some();
            if transformed {
                return Err(FormfluxError::invalid(
                    "stream copy is lossless and cannot be combined with codecs, filters or resizing",
                ));
            }
            if matches!(format.as_str(), "gif" | "webp") {
                return Err(FormfluxError::invalid(
                    "GIF/WebP output needs re-encoding; stream copy is not available",
                ));
            }
        }
        if self.crf.is_some() && self.video_bitrate.is_some() {
            return Err(FormfluxError::invalid(
                "choose either crf or video bitrate, not both",
            ));
        }
        if let Some(subtitles) = &self.embed_subtitles {
            io::existing_file(subtitles)?;
            if self.stream_copy {
                return Err(FormfluxError::invalid(
                    "cannot mux an external subtitle file in stream-copy mode",
                ));
            }
            if !matches!(
                format.as_str(),
                "mp4" | "mkv" | "mov" | "webm" | "ts" | "m4a"
            ) {
                return Err(FormfluxError::unsupported(format!(
                    "the {format} container cannot carry the selected subtitle track"
                )));
            }
        }
        Ok(())
    }

    fn is_audio_only_output(&self) -> bool {
        is_audio_only(&self.format) || self.no_video
    }
}

fn normalize_format(value: &str) -> Result<String> {
    let format = value.trim().trim_start_matches('.').to_ascii_lowercase();
    if format.is_empty() {
        return Err(FormfluxError::invalid("output format is required"));
    }
    if !FORMATS_VIDEO.contains(&format.as_str()) && !FORMATS_AUDIO.contains(&format.as_str()) {
        return Err(FormfluxError::unsupported(format!(
            "unknown output format '{format}'"
        )));
    }
    Ok(format)
}

fn is_audio_only(format: &str) -> bool {
    FORMATS_AUDIO.contains(&format)
}

fn default_video_codec(format: &str) -> Option<&'static str> {
    match format {
        "webm" => Some("libvpx-vp9"),
        "avi" => Some("mpeg4"),
        "wmv" => Some("wmv2"),
        "webp" => Some("libwebp"),
        "gif" => None,
        _ => Some("libx264"),
    }
}

fn default_audio_codec(format: &str) -> Option<&'static str> {
    match format {
        "mp3" => Some("libmp3lame"),
        "flac" => Some("flac"),
        "wav" => Some("pcm_s16le"),
        "ogg" | "oga" => Some("libvorbis"),
        "aac" | "m4a" | "mp4" | "mov" | "mkv" | "flv" | "ts" => Some("aac"),
        "alac" => Some("alac"),
        "webm" | "opus" => Some("libopus"),
        // APE and WMA are decode-oriented FFmpeg builds; leave the encoder to
        // the local installation and surface its error verbatim.
        _ => None,
    }
}

fn subtitle_codec(format: &str) -> Option<&'static str> {
    match format {
        "mp4" | "mov" | "m4a" => Some("mov_text"),
        "mkv" | "ts" => Some("srt"),
        "webm" => Some("webvtt"),
        _ => None,
    }
}

fn supported_pixel_format(codec: &str) -> bool {
    matches!(
        codec,
        "libx264" | "libx265" | "mpeg4" | "wmv2" | "libvpx-vp9" | "libvpx"
    )
}

fn build_filter(options: &ConvertOptions, format: &str) -> Result<Option<String>> {
    let mut chain: Vec<String> = Vec::new();
    if options.hdr_to_sdr {
        chain.push(
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
                .to_string(),
        );
    }
    if let Some(filter) = options.rotate.filter() {
        chain.push(filter.to_string());
    }
    if let Some(crop) = options.crop {
        chain.push(format!(
            "crop={}:{}:{}:{}",
            crop.width, crop.height, crop.x, crop.y
        ));
    }
    let effective_width = if format == "gif" {
        options.gif_width.or(options.width)
    } else {
        options.width
    };
    match (effective_width, options.height) {
        (Some(width), Some(height)) => chain.push(format!(
            "scale={width}:{height}:force_original_aspect_ratio=decrease:flags=lanczos"
        )),
        (Some(width), None) => chain.push(format!("scale={width}:-2:flags=lanczos")),
        (None, Some(height)) => chain.push(format!("scale=-2:{height}:flags=lanczos")),
        (None, None) => {}
    }
    let fps = if format == "gif" {
        Some(options.gif_fps.or(options.fps).unwrap_or(12.0))
    } else {
        options.fps
    };
    if let Some(fps) = fps {
        chain.push(format!("fps={fps}"));
    }
    if format == "gif" {
        let base = if chain.is_empty() {
            "null".to_string()
        } else {
            chain.join(",")
        };
        return Ok(Some(format!(
            "[0:v]{base},split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5[formflux_v]"
        )));
    }
    if chain.is_empty() {
        Ok(None)
    } else {
        Ok(Some(chain.join(",")))
    }
}

/// Build the FFmpeg argument vector.  Paths are local and expected to exist.
pub fn build_args(input: &Path, target: &Path, options: &ConvertOptions) -> Result<Vec<String>> {
    options.validate()?;
    let format = normalize_format(&options.format)?;
    let audio_only = options.is_audio_only_output();
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-protocol_whitelist".into(),
        "file,pipe".into(),
    ];
    if let Some(start) = options.start_seconds {
        args.push("-ss".into());
        args.push(format!("{start:.3}"));
    }
    args.push("-i".into());
    args.push(input.to_string_lossy().into_owned());
    if let Some(subtitles) = &options.embed_subtitles {
        args.push("-i".into());
        args.push(subtitles.to_string_lossy().into_owned());
    }
    if let Some(duration) = options.duration_seconds {
        args.push("-t".into());
        args.push(format!("{duration:.3}"));
    }
    args.push("-map_metadata".into());
    args.push("0".into());

    // GIF is produced by a filter graph that already ends in [formflux_v].
    // Mapping the source video as well would hand two video streams to the
    // GIF muxer, which rejects them ("does not support more than one stream
    // of type video").
    let gif_filter_output = format == "gif" && !options.stream_copy;
    if audio_only {
        args.push("-map".into());
        args.push("0:a?".into());
        args.push("-vn".into());
    } else if options.no_audio {
        if !gif_filter_output {
            args.push("-map".into());
            args.push("0:v?".into());
        }
        args.push("-an".into());
    } else if !gif_filter_output {
        args.push("-map".into());
        args.push("0:v?".into());
        args.push("-map".into());
        args.push("0:a?".into());
    }

    if options.embed_subtitles.is_some() {
        args.push("-map".into());
        args.push("1:s:0?".into());
    } else if options.stream_copy && !audio_only && !options.no_audio {
        args.push("-map".into());
        args.push("0:s?".into());
    }

    if options.stream_copy {
        args.push("-c".into());
        args.push("copy".into());
        if matches!(format.as_str(), "mp4" | "mov" | "m4a") {
            args.push("-movflags".into());
            args.push("+faststart".into());
        }
    } else {
        let filter = build_filter(options, &format)?;
        if format == "gif" {
            if let Some(filter) = &filter {
                args.push("-filter_complex".into());
                args.push(filter.clone());
                args.push("-map".into());
                args.push("[formflux_v]".into());
            }
            args.push("-an".into());
            args.push("-loop".into());
            args.push("0".into());
        } else {
            if let Some(filter) = &filter {
                args.push("-vf".into());
                args.push(filter.clone());
            }
            if matches!(format.as_str(), "webp") {
                args.push("-an".into());
                args.push("-loop".into());
                args.push("0".into());
            }
            if let Some(codec) = options
                .video_codec
                .clone()
                .or_else(|| default_video_codec(&format).map(str::to_string))
            {
                if !audio_only {
                    args.push("-c:v".into());
                    args.push(codec.clone());
                    if supported_pixel_format(&codec) {
                        args.push("-pix_fmt".into());
                        args.push("yuv420p".into());
                    }
                    if let Some(crf) = options.crf {
                        args.push("-crf".into());
                        args.push(crf.to_string());
                    }
                    if let Some(preset) = &options.preset {
                        args.push("-preset".into());
                        args.push(preset.clone());
                    }
                    if let Some(rate) = &options.video_bitrate {
                        args.push("-b:v".into());
                        args.push(rate.clone());
                    }
                }
            }
            if let Some(codec) = options
                .audio_codec
                .clone()
                .or_else(|| default_audio_codec(&format).map(str::to_string))
            {
                if !options.no_audio {
                    args.push("-c:a".into());
                    args.push(codec);
                    if let Some(crf) = options.crf {
                        if format == "mp3" {
                            args.push("-q:a".into());
                            args.push(crf.to_string());
                        }
                    }
                    if let Some(rate) = &options.audio_bitrate {
                        args.push("-b:a".into());
                        args.push(rate.clone());
                    }
                }
            }
            if matches!(format.as_str(), "mp4" | "mov" | "m4a") {
                args.push("-movflags".into());
                args.push("+faststart".into());
            }
            if options.embed_subtitles.is_some() {
                if let Some(codec) = subtitle_codec(&format) {
                    args.push("-c:s".into());
                    args.push(codec.into());
                }
            }
        }
        let mut audio_filters: Vec<&str> = Vec::new();
        if options.denoise {
            audio_filters.push("afftdn=nf=-25");
        }
        if options.normalize_audio {
            audio_filters.push("loudnorm=I=-16:TP=-1.5:LRA=11");
        }
        if !audio_filters.is_empty() && !options.no_audio {
            args.push("-af".into());
            args.push(audio_filters.join(","));
        }
        if let Some(rate) = options.sample_rate {
            args.push("-ar".into());
            args.push(rate.to_string());
        }
        if let Some(channels) = options.channels {
            args.push("-ac".into());
            args.push(channels.to_string());
        }
    }

    args.push("-progress".into());
    args.push("pipe:1".into());
    args.push("-nostats".into());
    args.push(target.to_string_lossy().into_owned());
    Ok(args)
}

/// Result of a successful conversion.
#[derive(Debug, Clone, Serialize)]
pub struct ConvertOutcome {
    /// Canonical source.
    pub input: PathBuf,
    /// Written file.
    pub output: PathBuf,
    /// Container extension.
    pub format: String,
    /// Wall-clock duration.
    pub elapsed_seconds: f64,
    /// Source size.
    pub input_bytes: u64,
    /// Written size.
    pub output_bytes: u64,
    /// Whether streams were copied.
    pub stream_copy: bool,
}

#[derive(Debug, Default)]
struct FfmpegProgress {
    total_seconds: Option<f64>,
    out_seconds: f64,
    speed: Option<f64>,
    last_emit: Option<Instant>,
    ended: bool,
}

impl FfmpegProgress {
    fn new(total_seconds: Option<f64>) -> Self {
        Self {
            total_seconds,
            ..Self::default()
        }
    }

    fn feed(&mut self, line: &str) -> Option<ProgressEvent> {
        let (key, value) = line.split_once('=')?;
        match key.trim() {
            "out_time_us" => {
                if let Ok(micros) = value.trim().parse::<i64>() {
                    if micros >= 0 {
                        self.out_seconds = micros as f64 / 1_000_000.0;
                    }
                }
            }
            "out_time_ms" => {
                // FFmpeg historically reports microseconds under this name.
                if let Ok(micros) = value.trim().parse::<i64>() {
                    if micros >= 0 {
                        self.out_seconds = micros as f64 / 1_000_000.0;
                    }
                }
            }
            "speed" => {
                self.speed = value.trim().trim_end_matches('x').parse::<f64>().ok();
                if self
                    .speed
                    .is_some_and(|speed| !speed.is_finite() || speed <= 0.0)
                {
                    self.speed = None;
                }
            }
            "progress" => {
                if value.trim() == "end" {
                    self.ended = true;
                }
            }
            _ => return None,
        }
        if self.ended {
            return Some(ProgressEvent::new("convert", 100.0).with_message("finished"));
        }
        if let Some(last) = self.last_emit {
            if last.elapsed() < Duration::from_millis(120) {
                return None;
            }
        }
        self.last_emit = Some(Instant::now());
        Some(self.event())
    }

    fn event(&self) -> ProgressEvent {
        let percent = match self.total_seconds {
            Some(total) if total > 0.0 => (self.out_seconds / total * 100.0).clamp(0.0, 100.0),
            _ => 0.0,
        };
        let mut message = crate::format_duration(self.out_seconds);
        if let Some(total) = self.total_seconds {
            message.push_str(&format!(" / {}", crate::format_duration(total)));
        }
        if let Some(speed) = self.speed {
            message.push_str(&format!("  {speed:.2}x"));
            if let Some(total) = self.total_seconds {
                let remaining = (total - self.out_seconds).max(0.0);
                if speed > 0.0 {
                    message.push_str(&format!(
                        "  ETA {}",
                        crate::format_eta(Some(remaining / speed))
                    ));
                }
            }
        }
        ProgressEvent::new("convert", percent).with_message(message)
    }
}

/// Convert one file.
#[allow(clippy::too_many_arguments)]
pub async fn convert(
    tools: &FfmpegTools,
    input: &Path,
    output_dir: &Path,
    options: &ConvertOptions,
    overwrite: bool,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<ConvertOutcome> {
    options.validate()?;
    let format = normalize_format(&options.format)?;
    let input = io::existing_file(input)?;
    io::ensure_dir(output_dir)?;
    cancel.check()?;
    let info = probe(tools, &input).await.ok();
    let source_duration = info.as_ref().and_then(|info| info.duration_seconds);
    let total_duration = options.duration_seconds.or_else(|| {
        source_duration.map(|duration| (duration - options.start_seconds.unwrap_or(0.0)).max(0.0))
    });
    let input_bytes = input.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let target = sfs::destination_for(&input, output_dir, Some(&format), overwrite)?;
    let temp = tempfile::Builder::new()
        .prefix(".formflux-media-")
        .suffix(&format!(".{format}"))
        .tempfile_in(output_dir)?;
    let temp_path = temp.into_temp_path();
    let args = build_args(&input, temp_path.as_ref(), options)?;
    let started = Instant::now();
    progress.report(ProgressEvent::new("convert", 0.0).with_message("starting ffmpeg"));

    let mut child = hidden_tokio_command(&tools.ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            FormfluxError::Io(error).context(format!(
                "could not start ffmpeg at {}",
                tools.ffmpeg.display()
            ))
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| FormfluxError::internal("ffmpeg stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| FormfluxError::internal("ffmpeg stderr was not captured"))?;
    let stderr_task = tokio::spawn(async move {
        // Drain stderr continuously: stopping at a byte cap would fill the pipe
        // and deadlock a chatty FFmpeg. Keep only the tail for diagnostics.
        const KEEP: usize = 256 * 1024;
        let mut reader = BufReader::new(stderr);
        let mut collected: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) => break,
                Ok(read) => {
                    collected.extend_from_slice(&chunk[..read]);
                    if collected.len() > KEEP {
                        let excess = collected.len() - KEEP;
                        collected.drain(..excess);
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&collected).into_owned()
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut state = FfmpegProgress::new(total_duration);
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(event) = state.feed(&line) {
                            progress.report(event);
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        stderr_task.abort();
                        return Err(FormfluxError::Io(error));
                    }
                }
            }
            _ = ticker.tick() => {
                if cancel.is_cancelled() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    stderr_task.abort();
                    return Err(FormfluxError::Cancelled);
                }
            }
        }
    }
    let status = child.wait().await?;
    let stderr_text = stderr_task.await.unwrap_or_default();
    if !status.success() {
        if cancel.is_cancelled() {
            return Err(FormfluxError::Cancelled);
        }
        return Err(FormfluxError::ExternalToolFailed {
            tool: "ffmpeg".to_string(),
            code: status.code().unwrap_or(-1),
            message: tail(&stderr_text, 8192),
        });
    }
    let output_bytes = temp_path
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if output_bytes == 0 {
        return Err(FormfluxError::ExternalToolFailed {
            tool: "ffmpeg".to_string(),
            code: 0,
            message: tail(&stderr_text, 8192),
        });
    }
    if overwrite && target.exists() {
        std::fs::remove_file(&target)?;
    }
    let persisted = if overwrite {
        temp_path.persist(&target)
    } else {
        temp_path.persist_noclobber(&target)
    };
    match persisted {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(FormfluxError::AlreadyExists(target));
        }
        Err(error) => return Err(FormfluxError::Io(error.error)),
    }
    progress.report(ProgressEvent::new("convert", 100.0).with_message("saved"));
    Ok(ConvertOutcome {
        input,
        output: target,
        format,
        elapsed_seconds: started.elapsed().as_secs_f64(),
        input_bytes,
        output_bytes,
        stream_copy: options.stream_copy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ConvertOptions {
        ConvertOptions {
            format: "mp4".to_string(),
            ..ConvertOptions::default()
        }
    }

    #[test]
    fn basic_command_is_local_only() {
        let args = build_args(Path::new("in.mkv"), Path::new("out.mp4"), &base()).unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-protocol_whitelist", "file,pipe"]));
        assert!(args.contains(&"-c:v".to_string()));
        assert!(args.contains(&"libx264".to_string()));
        assert!(args.contains(&"+faststart".to_string()));
        assert!(!args.iter().any(|arg| arg.contains("http")));
    }

    #[test]
    fn stream_copy_adds_copy() {
        let options = ConvertOptions {
            stream_copy: true,
            ..base()
        };
        let args = build_args(Path::new("in.mkv"), Path::new("out.mp4"), &options).unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-c", "copy"]));
        assert!(!args.contains(&"-c:v".to_string()));
    }

    #[test]
    fn stream_copy_rejects_filters() {
        let options = ConvertOptions {
            stream_copy: true,
            width: Some(1280),
            ..base()
        };
        assert!(options.validate().is_err());
    }

    #[test]
    fn gif_uses_palette_filter() {
        let options = ConvertOptions {
            format: "gif".to_string(),
            gif_fps: Some(10.0),
            gif_width: Some(480),
            ..ConvertOptions::default()
        };
        let args = build_args(Path::new("in.mp4"), Path::new("out.gif"), &options).unwrap();
        assert!(args.iter().any(|arg| arg.contains("palettegen")));
        assert!(args.windows(2).any(|pair| pair == ["-map", "[formflux_v]"]));
        assert_eq!(
            args.iter().filter(|arg| *arg == "0:v?").count(),
            0,
            "the source video must not be mapped alongside [formflux_v]"
        );
        assert!(args.windows(2).any(|pair| pair == ["-loop", "0"]));
    }

    #[test]
    fn audio_extraction_forces_no_video() {
        let options = ConvertOptions {
            format: "mp3".to_string(),
            ..ConvertOptions::default()
        };
        let args = build_args(Path::new("in.mp4"), Path::new("out.mp3"), &options).unwrap();
        assert!(args.contains(&"-vn".to_string()));
        assert!(args.contains(&"libmp3lame".to_string()));
    }

    #[test]
    fn audio_filters_are_combined() {
        let options = ConvertOptions {
            normalize_audio: true,
            denoise: true,
            format: "mkv".to_string(),
            ..ConvertOptions::default()
        };
        let args = build_args(Path::new("in.mp4"), Path::new("out.mkv"), &options).unwrap();
        let position = args.iter().position(|arg| arg == "-af").unwrap();
        assert_eq!(
            args[position + 1],
            "afftdn=nf=-25,loudnorm=I=-16:TP=-1.5:LRA=11"
        );
    }

    #[test]
    fn progress_parser_reports_percent_and_eta() {
        let mut progress = FfmpegProgress::new(Some(100.0));
        assert!(progress.feed("out_time_us=25000000").is_some());
        // Speed is normally reported in the same burst; it updates the field
        // even when the 120 ms progress throttle suppresses another event.
        progress.feed("speed=2.0x");
        assert_eq!(progress.out_seconds, 25.0);
        assert_eq!(progress.speed, Some(2.0));
        let event = progress.event();
        assert_eq!(event.percent, 25.0);
        assert!(event.message.unwrap().contains("ETA 37.5 s"));
        let end = progress.feed("progress=end").unwrap();
        assert_eq!(end.percent, 100.0);
    }

    #[test]
    fn format_validation() {
        let mut options = base();
        options.format = "exe".to_string();
        assert!(options.validate().is_err());
        options.format = "mp3".to_string();
        options.no_video = false;
        assert!(options.validate().is_ok());
    }

    #[test]
    fn rejects_crf_and_bitrate_together() {
        let mut options = base();
        options.crf = Some(20);
        options.video_bitrate = Some("5M".to_string());
        assert!(options.validate().is_err());
    }

    #[test]
    fn rotation_parser() {
        assert_eq!(Rotation::parse("90").unwrap(), Rotation::Cw90);
        assert_eq!(Rotation::parse("left").unwrap(), Rotation::Cw270);
        assert!(Rotation::parse("45").is_err());
    }

    #[test]
    fn crop_and_scale_filter_order() {
        let options = ConvertOptions {
            width: Some(1280),
            crop: Some(CropRect {
                width: 1920,
                height: 800,
                x: 0,
                y: 140,
            }),
            rotate: Rotation::Cw90,
            format: "mp4".to_string(),
            ..ConvertOptions::default()
        };
        let filter = build_filter(&options, "mp4").unwrap().unwrap();
        assert!(filter.starts_with("transpose=1,crop=1920:800:0:140,scale=1280:-2"));
    }
}

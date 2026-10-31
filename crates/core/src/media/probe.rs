//! FFprobe metadata, media thumbnails and subtitle extraction.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use formflux_shared::fs as sfs;
use serde::{Deserialize, Serialize};

use crate::detect::FfmpegTools;
use crate::error::{FormfluxError, Result};
use crate::external::{hidden_tokio_command, tail};
use crate::io;

/// One audio, video, subtitle or data stream.
#[derive(Debug, Clone, Serialize)]
pub struct StreamInfo {
    /// Absolute stream index in the container.
    pub index: usize,
    /// `video`, `audio`, `subtitle`, `data`, ...
    pub codec_type: String,
    /// Short codec name (`h264`, `aac`, `subrip`).
    pub codec_name: Option<String>,
    /// Human codec description.
    pub codec_long_name: Option<String>,
    /// Pixel width for video.
    pub width: Option<u32>,
    /// Pixel height for video.
    pub height: Option<u32>,
    /// Sample rate in Hz for audio.
    pub sample_rate: Option<u32>,
    /// Channel count for audio.
    pub channels: Option<u16>,
    /// Bitrate in bits per second.
    pub bit_rate: Option<u64>,
    /// Frame rate as a float.
    pub frame_rate: Option<f64>,
    /// Language tag.
    pub language: Option<String>,
    /// Stream title.
    pub title: Option<String>,
    /// Stream duration in seconds.
    pub duration_seconds: Option<f64>,
    /// Pixel format.
    pub pixel_format: Option<String>,
}

impl StreamInfo {
    /// Whether this is a video stream.
    pub fn is_video(&self) -> bool {
        self.codec_type == "video"
    }

    /// Whether this is an audio stream.
    pub fn is_audio(&self) -> bool {
        self.codec_type == "audio"
    }

    /// Whether this is a subtitle stream.
    pub fn is_subtitle(&self) -> bool {
        self.codec_type == "subtitle"
    }
}

/// Probe result for one file.
#[derive(Debug, Clone, Serialize)]
pub struct MediaInfo {
    /// Canonical path.
    pub path: PathBuf,
    /// Container short name (`mov,mp4,m4a,3gp,3g2,mj2`).
    pub format_name: String,
    /// Container long name.
    pub format_long_name: Option<String>,
    /// Duration in seconds.
    pub duration_seconds: Option<f64>,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Overall bitrate.
    pub bit_rate: Option<u64>,
    /// All streams.
    pub streams: Vec<StreamInfo>,
}

impl MediaInfo {
    /// Video streams.
    pub fn video_streams(&self) -> impl Iterator<Item = &StreamInfo> {
        self.streams.iter().filter(|stream| stream.is_video())
    }

    /// Audio streams.
    pub fn audio_streams(&self) -> impl Iterator<Item = &StreamInfo> {
        self.streams.iter().filter(|stream| stream.is_audio())
    }

    /// Subtitle streams.
    pub fn subtitle_streams(&self) -> impl Iterator<Item = &StreamInfo> {
        self.streams.iter().filter(|stream| stream.is_subtitle())
    }

    /// First video stream.
    pub fn first_video(&self) -> Option<&StreamInfo> {
        self.video_streams().next()
    }

    /// Human-readable duration.
    pub fn duration_label(&self) -> String {
        self.duration_seconds
            .map(crate::format_duration)
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[derive(Debug, Deserialize)]
struct RawProbe {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: Option<RawFormat>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    format_name: Option<String>,
    format_long_name: Option<String>,
    duration: Option<String>,
    size: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawStream {
    index: usize,
    codec_type: Option<String>,
    codec_name: Option<String>,
    codec_long_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    sample_rate: Option<String>,
    channels: Option<u16>,
    bit_rate: Option<String>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
    pix_fmt: Option<String>,
    tags: Option<RawTags>,
}

#[derive(Debug, Deserialize)]
struct RawTags {
    language: Option<String>,
    title: Option<String>,
}

fn parse_f64(value: Option<&String>) -> Option<f64> {
    value
        .and_then(|text| text.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn parse_u64(value: Option<&String>) -> Option<u64> {
    value.and_then(|text| text.parse::<u64>().ok())
}

fn parse_rate(value: Option<&String>) -> Option<f64> {
    let text = value?;
    if let Some((numerator, denominator)) = text.split_once('/') {
        let numerator = numerator.parse::<f64>().ok()?;
        let denominator = denominator.parse::<f64>().ok()?;
        if denominator <= 0.0 {
            return None;
        }
        let rate = numerator / denominator;
        (rate.is_finite() && rate > 0.0).then_some(rate)
    } else {
        text.parse::<f64>().ok().filter(|rate| *rate > 0.0)
    }
}

/// Probe a media file with `ffprobe`.
pub async fn probe(tools: &FfmpegTools, path: &Path) -> Result<MediaInfo> {
    let path = io::existing_file(path)?;
    let output = hidden_tokio_command(&tools.ffprobe)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| {
            FormfluxError::Io(error).context(format!(
                "could not start ffprobe at {}",
                tools.ffprobe.display()
            ))
        })?;
    if !output.status.success() {
        return Err(FormfluxError::ExternalToolFailed {
            tool: "ffprobe".to_string(),
            code: output.status.code().unwrap_or(-1),
            message: tail(&String::from_utf8_lossy(&output.stderr), 8192),
        });
    }
    let raw: RawProbe = serde_json::from_slice(&output.stdout)
        .map_err(|error| FormfluxError::Serialization(format!("ffprobe JSON: {error}")))?;
    let format = raw.format.unwrap_or(RawFormat {
        format_name: None,
        format_long_name: None,
        duration: None,
        size: None,
        bit_rate: None,
    });
    let streams = raw
        .streams
        .into_iter()
        .map(|stream| StreamInfo {
            index: stream.index,
            codec_type: stream.codec_type.unwrap_or_else(|| "unknown".to_string()),
            codec_name: stream.codec_name,
            codec_long_name: stream.codec_long_name,
            width: stream.width,
            height: stream.height,
            sample_rate: stream
                .sample_rate
                .as_deref()
                .and_then(|value| value.parse().ok()),
            channels: stream.channels,
            bit_rate: parse_u64(stream.bit_rate.as_ref()),
            frame_rate: parse_rate(stream.avg_frame_rate.as_ref())
                .or_else(|| parse_rate(stream.r_frame_rate.as_ref())),
            language: stream.tags.as_ref().and_then(|tags| tags.language.clone()),
            title: stream.tags.as_ref().and_then(|tags| tags.title.clone()),
            duration_seconds: parse_f64(stream.duration.as_ref()),
            pixel_format: stream.pix_fmt,
        })
        .collect();
    let size_bytes = parse_u64(format.size.as_ref())
        .unwrap_or_else(|| path.metadata().map(|metadata| metadata.len()).unwrap_or(0));
    Ok(MediaInfo {
        path,
        format_name: format.format_name.unwrap_or_else(|| "unknown".to_string()),
        format_long_name: format.format_long_name,
        duration_seconds: parse_f64(format.duration.as_ref()),
        size_bytes,
        bit_rate: parse_u64(format.bit_rate.as_ref()),
        streams,
    })
}

/// Render the first available video frame as PNG bytes (for GUI previews).
pub async fn thumbnail(
    tools: &FfmpegTools,
    path: &Path,
    at_seconds: f64,
    width: u32,
) -> Result<Vec<u8>> {
    let path = io::existing_file(path)?;
    let seek = if at_seconds.is_finite() && at_seconds > 0.0 {
        at_seconds
    } else {
        0.0
    };
    let width = width.clamp(32, 1920);
    let output = hidden_tokio_command(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(["-ss", &format!("{seek:.3}")])
        .arg("-i")
        .arg(&path)
        .args([
            "-frames:v",
            "1",
            "-an",
            "-sn",
            "-vf",
            &format!("scale={width}:-2:flags=lanczos"),
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| {
            FormfluxError::Io(error).context("could not start ffmpeg for a thumbnail")
        })?;
    if !output.status.success() {
        return Err(FormfluxError::ExternalToolFailed {
            tool: "ffmpeg".to_string(),
            code: output.status.code().unwrap_or(-1),
            message: tail(&String::from_utf8_lossy(&output.stderr), 4096),
        });
    }
    if output.stdout.is_empty() {
        return Err(FormfluxError::invalid(
            "ffmpeg produced no frame (the file may have no video stream)",
        ));
    }
    Ok(output.stdout)
}

/// Text subtitle output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleFormat {
    /// SubRip (`.srt`).
    Srt,
    /// Advanced SubStation Alpha (`.ass`).
    Ass,
}

impl SubtitleFormat {
    /// File extension.
    pub fn extension(self) -> &'static str {
        match self {
            SubtitleFormat::Srt => "srt",
            SubtitleFormat::Ass => "ass",
        }
    }

    /// FFmpeg subtitle codec name.
    pub fn codec(self) -> &'static str {
        match self {
            SubtitleFormat::Srt => "srt",
            SubtitleFormat::Ass => "ass",
        }
    }

    /// Parse `srt` or `ass`.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "srt" | "subrip" => Ok(SubtitleFormat::Srt),
            "ass" | "ssa" => Ok(SubtitleFormat::Ass),
            other => Err(FormfluxError::unsupported(format!(
                "unsupported subtitle format '{other}' (srt or ass)"
            ))),
        }
    }
}

/// Extracted subtitle result.
#[derive(Debug, Clone, Serialize)]
pub struct SubtitleOutcome {
    /// Source file.
    pub input: PathBuf,
    /// Written subtitle file.
    pub output: PathBuf,
    /// Absolute stream index selected from the source.
    pub stream_index: usize,
    /// Stream language tag, when present.
    pub language: Option<String>,
    /// Written format.
    pub format: SubtitleFormat,
}

/// Extract one subtitle stream to SRT.
pub async fn extract_subtitles(
    tools: &FfmpegTools,
    input: &Path,
    stream: Option<usize>,
    output_dir: &Path,
    overwrite: bool,
) -> Result<SubtitleOutcome> {
    extract_subtitles_as(
        tools,
        input,
        stream,
        output_dir,
        SubtitleFormat::Srt,
        overwrite,
    )
    .await
}

/// Extract one subtitle stream to SRT or ASS.
pub async fn extract_subtitles_as(
    tools: &FfmpegTools,
    input: &Path,
    stream: Option<usize>,
    output_dir: &Path,
    format: SubtitleFormat,
    overwrite: bool,
) -> Result<SubtitleOutcome> {
    let input = io::existing_file(input)?;
    io::ensure_dir(output_dir)?;
    let info = probe(tools, &input).await?;
    let available: Vec<&StreamInfo> = info.subtitle_streams().collect();
    if available.is_empty() {
        return Err(FormfluxError::invalid("the input has no subtitle streams"));
    }
    let selected = match stream {
        Some(index) => available
            .iter()
            .find(|candidate| candidate.index == index)
            .copied()
            .ok_or_else(|| {
                FormfluxError::invalid(format!(
                    "subtitle stream {index} does not exist (available: {})",
                    available
                        .iter()
                        .map(|candidate| candidate.index.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?,
        None => available[0],
    };
    let target = sfs::destination_for(&input, output_dir, Some(format.extension()), overwrite)?;
    let temp = tempfile::Builder::new()
        .prefix(".formflux-subs-")
        .suffix(&format!(".{}", format.extension()))
        .tempfile_in(output_dir)?;
    let temp_path = temp.into_temp_path();
    let output = hidden_tokio_command(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(&input)
        .args(["-map", &format!("0:{}", selected.index)])
        .args(["-c:s", format.codec()])
        .arg(temp_path.as_os_str())
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| {
            FormfluxError::Io(error).context(format!(
                "could not start ffmpeg at {}",
                tools.ffmpeg.display()
            ))
        })?;
    if !output.status.success() {
        return Err(FormfluxError::ExternalToolFailed {
            tool: "ffmpeg".to_string(),
            code: output.status.code().unwrap_or(-1),
            message: tail(&String::from_utf8_lossy(&output.stderr), 8192),
        });
    }
    if temp_path
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0)
        == 0
    {
        return Err(FormfluxError::invalid(
            "the selected subtitle stream could not be converted to SRT (image-based subtitles are not supported)",
        ));
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
    Ok(SubtitleOutcome {
        input,
        output: target,
        stream_index: selected.index,
        language: selected.language.clone(),
        format,
    })
}

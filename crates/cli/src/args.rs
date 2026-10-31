//! Command-line grammar.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Formflux: offline media, image, document and developer tools.
#[derive(Debug, Parser)]
#[command(
    name = "formflux",
    version,
    about = "Offline media, image, text, hash, colour and data tools",
    long_about = "Formflux performs every operation locally. It never uploads files, never \
                  contacts a network service and never bundles FFmpeg or Tesseract."
)]
pub struct Cli {
    /// Machine-readable JSON on stdout.
    #[arg(long, global = true)]
    pub json: bool,
    /// Suppress progress output on stderr.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Skip confirmation prompts (required for destructive operations).
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
    /// Plan the operation without writing anything.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Overwrite existing outputs.
    #[arg(long, global = true)]
    pub force: bool,
    /// Worker count (0 = logical CPUs).
    #[arg(short = 'j', long, global = true)]
    pub jobs: Option<usize>,
    /// Alternate configuration file.
    #[arg(long, global = true, env = "FORMFLUX_CONFIG")]
    pub config: Option<PathBuf>,
    /// Increase log verbosity (repeatable).
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Enable trace logging.
    #[arg(long, global = true)]
    pub trace: bool,
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level groups mirror the GUI tabs.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Report FFmpeg, Tesseract, CUDA and platform status.
    Doctor,
    /// Video and audio conversion.
    #[command(subcommand)]
    Media(MediaCmd),
    /// Image conversion, compression and EXIF.
    #[command(subcommand)]
    Image(ImageCmd),
    /// Hashing and checksum manifests.
    #[command(subcommand)]
    Hash(HashCmd),
    /// Text, documents, encodings and OCR.
    #[command(subcommand)]
    Text(TextCmd),
    /// JSON, YAML and XML.
    #[command(subcommand)]
    Data(DataCmd),
    /// Colour conversion and accessibility.
    #[command(subcommand)]
    Color(ColorCmd),
    /// QR codes, barcodes, UUIDs and passwords.
    #[command(subcommand)]
    Gen(GenCmd),
    /// Timestamps and time zones.
    #[command(subcommand)]
    Time(TimeCmd),
    /// File toolbox.
    #[command(subcommand)]
    Tools(ToolsCmd),
}

/// Media subcommands.
#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum MediaCmd {
    /// Convert a video or audio file with FFmpeg.
    Convert(MediaConvertArgs),
    /// Print container and stream metadata.
    Probe {
        /// Media file.
        input: PathBuf,
    },
    /// Extract one subtitle stream as SRT or ASS.
    ExtractSubs {
        /// Media file.
        input: PathBuf,
        /// Absolute subtitle stream index (default: first).
        #[arg(long)]
        stream: Option<usize>,
        /// Output directory.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        /// Subtitle format (`srt` or `ass`).
        #[arg(long, default_value = "srt")]
        format: String,
    },
}

/// Options shared by media conversion.
#[derive(Debug, Args)]
pub struct MediaConvertArgs {
    /// Input media file.
    pub input: PathBuf,
    /// Output directory.
    #[arg(short, long, default_value = ".")]
    pub output: PathBuf,
    /// Target container/format (`mp4`, `webm`, `mp3`, ...).
    #[arg(long)]
    pub format: String,
    /// Stream-copy without re-encoding where possible.
    #[arg(long)]
    pub stream_copy: bool,
    /// Video encoder.
    #[arg(long)]
    pub video_codec: Option<String>,
    /// Audio encoder.
    #[arg(long)]
    pub audio_codec: Option<String>,
    /// Constant rate factor.
    #[arg(long)]
    pub crf: Option<u8>,
    /// Encoder preset.
    #[arg(long)]
    pub preset: Option<String>,
    /// Video bitrate (`2M`, `800k`).
    #[arg(long)]
    pub video_bitrate: Option<String>,
    /// Audio bitrate (`192k`).
    #[arg(long)]
    pub audio_bitrate: Option<String>,
    /// Output width.
    #[arg(long)]
    pub width: Option<u32>,
    /// Output height.
    #[arg(long)]
    pub height: Option<u32>,
    /// Output frame rate.
    #[arg(long)]
    pub fps: Option<f32>,
    /// Crop as `width:height:x:y`.
    #[arg(long)]
    pub crop: Option<String>,
    /// Rotation: 0, 90, 180 or 270 clockwise.
    #[arg(long, default_value_t = 0)]
    pub rotate: u16,
    /// Tone-map HDR to SDR.
    #[arg(long)]
    pub hdr_to_sdr: bool,
    /// EBU R128 loudness normalization.
    #[arg(long)]
    pub normalize_audio: bool,
    /// FFmpeg afftdn denoise.
    #[arg(long)]
    pub denoise: bool,
    /// Audio sample rate.
    #[arg(long)]
    pub sample_rate: Option<u32>,
    /// Audio channel count.
    #[arg(long)]
    pub channels: Option<u16>,
    /// Start time (`10`, `00:01:30`).
    #[arg(long)]
    pub start: Option<String>,
    /// Duration (`30`, `1m30s`).
    #[arg(long)]
    pub duration: Option<String>,
    /// Drop audio.
    #[arg(long)]
    pub no_audio: bool,
    /// Drop video.
    #[arg(long)]
    pub no_video: bool,
    /// GIF frames per second.
    #[arg(long, default_value_t = 12)]
    pub gif_fps: u8,
    /// GIF width.
    #[arg(long)]
    pub gif_width: Option<u32>,
    /// External subtitle file to embed.
    #[arg(long)]
    pub subtitle: Option<PathBuf>,
}

/// Image subcommands.
#[derive(Debug, Subcommand)]
pub enum ImageCmd {
    /// Convert one or more images.
    Convert(ImageArgs),
    /// Re-encode an image at a target quality.
    Compress(ImageArgs),
    /// Print image dimensions and format.
    Info {
        /// Image file.
        input: PathBuf,
    },
    /// Read or strip EXIF.
    Exif {
        /// Image file.
        input: PathBuf,
        /// Re-encode without metadata.
        #[arg(long)]
        strip: bool,
        /// Output directory for `--strip`.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
}

/// Image processing options.
#[derive(Debug, Args)]
pub struct ImageArgs {
    /// Input images or directories.
    #[arg(required = true)]
    pub input: Vec<PathBuf>,
    /// Output directory.
    #[arg(short, long, default_value = ".")]
    pub output: PathBuf,
    /// Output format (`jpg`, `png`, `webp`, `bmp`, `tiff`, `ico`, `gif`, `avif`).
    #[arg(long)]
    pub format: Option<String>,
    /// Encoder quality 1..=100.
    ///
    /// Long-only because `-q` is the global `--quiet` switch.
    #[arg(long, default_value_t = 90)]
    pub quality: u8,
    /// Target width.
    #[arg(long)]
    pub width: Option<u32>,
    /// Target height.
    #[arg(long)]
    pub height: Option<u32>,
    /// Resize mode: fit or exact.
    #[arg(long, default_value = "fit")]
    pub resize_mode: String,
    /// Crop as `width:height:x:y`.
    #[arg(long)]
    pub crop: Option<String>,
    /// Rotate 0/90/180/270.
    #[arg(long, default_value_t = 0)]
    pub rotate: u16,
    /// Flip horizontally.
    #[arg(long)]
    pub flip_h: bool,
    /// Flip vertically.
    #[arg(long)]
    pub flip_v: bool,
    /// Brightness -100..=100.
    #[arg(long, default_value_t = 0)]
    pub brightness: i32,
    /// Contrast -100..=100.
    #[arg(long, default_value_t = 0.0)]
    pub contrast: f32,
    /// Saturation 0..=2.
    #[arg(long, default_value_t = 1.0)]
    pub saturation: f32,
    /// Gamma 0.1..=5.
    #[arg(long, default_value_t = 1.0)]
    pub gamma: f32,
    /// Text watermark.
    #[arg(long)]
    pub watermark_text: Option<String>,
    /// Image watermark.
    #[arg(long)]
    pub watermark_image: Option<PathBuf>,
    /// Watermark anchor.
    #[arg(long, default_value = "bottom-right")]
    pub watermark_anchor: String,
    /// Watermark opacity 0..=1.
    #[arg(long, default_value_t = 0.85)]
    pub watermark_opacity: f32,
    /// Do not apply EXIF orientation.
    #[arg(long)]
    pub no_auto_orient: bool,
}

/// Hash subcommands.
#[derive(Debug, Subcommand)]
pub enum HashCmd {
    /// Hash files or directories.
    File(HashArgs),
    /// Verify a checksum manifest.
    Verify {
        /// Manifest file.
        manifest: PathBuf,
        /// Base directory for relative paths.
        #[arg(long, default_value = ".")]
        base: PathBuf,
    },
}

/// Hash options.
#[derive(Debug, Args)]
pub struct HashArgs {
    /// Files or directories.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// md5, sha1, sha256, sha384, sha512 or blake2b.
    #[arg(long, default_value = "sha256")]
    pub algorithm: String,
    /// Recurse into directories.
    #[arg(long)]
    pub recursive: bool,
    /// Include dotfiles.
    #[arg(long)]
    pub hidden: bool,
    /// Write a checksum manifest.
    #[arg(long)]
    pub manifest: Option<PathBuf>,
}

/// Text subcommands.
#[derive(Debug, Subcommand)]
pub enum TextCmd {
    /// Convert Markdown, HTML and plain text.
    Convert {
        /// Input document.
        input: PathBuf,
        /// Target format: md, html or txt.
        #[arg(long)]
        to: String,
        /// Output directory.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    /// Convert CSV and TSV.
    Delimited {
        /// Input CSV/TSV.
        input: PathBuf,
        /// Target extension: csv or tsv.
        #[arg(long)]
        to: String,
        /// Output directory.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    /// Re-encode a text file (GBK/UTF-8/Latin-1).
    Transcode {
        /// Input file.
        input: PathBuf,
        /// Source encoding.
        #[arg(long, default_value = "gbk")]
        from: String,
        /// Target encoding.
        #[arg(long, default_value = "utf8")]
        to: String,
        /// Output file.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Base64/Base32/URL/Unicode encode or decode.
    Codec {
        /// Literal value (or use --file).
        value: Option<String>,
        /// Read the value from a file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Codec name.
        #[arg(long, default_value = "base64")]
        codec: String,
        /// Decode instead of encode.
        #[arg(long)]
        decode: bool,
    },
    /// Unified diff of two text files.
    Diff {
        /// Old file.
        old: PathBuf,
        /// New file.
        new: PathBuf,
        /// Context lines.
        #[arg(long, default_value_t = 3)]
        context: usize,
    },
    /// Regex replacement in a text file.
    Replace {
        /// Input file.
        input: PathBuf,
        /// Regular expression.
        #[arg(long)]
        pattern: String,
        /// Replacement (`$1` groups supported).
        #[arg(long, default_value = "")]
        replacement: String,
        /// Replace every occurrence.
        #[arg(long)]
        all: bool,
        /// Output directory.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    /// Collapse redundant whitespace in a text file.
    Whitespace {
        /// Input file.
        input: PathBuf,
        /// Decode as utf8, gbk or latin1.
        #[arg(long, default_value = "utf8")]
        encoding: String,
        /// Write to this file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Convert letter case.
    Case {
        /// Input text.
        value: String,
        /// upper, lower, title, snake, kebab or camel.
        #[arg(long)]
        style: String,
    },
    /// Convert integers between bases 2, 8, 10 and 16.
    Radix {
        /// One or more values (prefixes such as 0xff are inferred).
        #[arg(required = true, num_args = 1..)]
        values: Vec<String>,
        /// Source base: 2, 8, 10 or 16 (inferred from a prefix when omitted).
        #[arg(long)]
        from: Option<String>,
        /// Target base: 2, 8, 10 or 16.
        #[arg(long, default_value = "16")]
        to: String,
    },
    /// Inspect characters, or decode a list of code points.
    Ascii {
        /// Text to inspect, or code points with --decode.
        value: String,
        /// Interpret the value as a comma/space separated code-point list.
        #[arg(long)]
        decode: bool,
    },
    /// OCR an image with external Tesseract.
    Ocr {
        /// Image file.
        input: PathBuf,
        /// Tesseract language codes.
        #[arg(long)]
        lang: Option<String>,
        /// Page segmentation mode.
        #[arg(long, default_value_t = 3)]
        psm: u8,
        /// Optional output text file.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

/// Data subcommands.
#[derive(Debug, Subcommand)]
pub enum DataCmd {
    /// Convert JSON, YAML and XML.
    Convert {
        /// Input document.
        input: PathBuf,
        /// Target format: json, yaml or xml.
        #[arg(long)]
        to: String,
        /// Output directory.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        /// Pretty-print where supported.
        #[arg(long)]
        pretty: bool,
    },
    /// Validate a document.
    Validate {
        /// Input document.
        input: PathBuf,
    },
    /// jq-style path query (`.a.b[0]`).
    Query {
        /// Input document.
        input: PathBuf,
        /// Query path.
        #[arg(long)]
        path: String,
    },
    /// Pretty-print or minify JSON.
    Format {
        /// Input JSON.
        input: PathBuf,
        /// Minify instead of pretty-printing.
        #[arg(long)]
        minify: bool,
    },
}

/// Colour subcommands.
#[derive(Debug, Subcommand)]
pub enum ColorCmd {
    /// Convert a colour to another notation.
    Convert {
        /// Colour (`#ff8800`, `rgb(...)`, `hsl(...)`).
        value: String,
        /// Target: hex, rgb, rgba, hsl, hsla, cmyk or lab.
        #[arg(long, default_value = "hex")]
        to: String,
    },
    /// WCAG contrast ratio.
    Contrast {
        /// Foreground colour.
        foreground: String,
        /// Background colour.
        background: String,
    },
    /// Linear gradient swatches.
    Gradient {
        /// Start colour.
        from: String,
        /// End colour.
        to: String,
        /// Number of swatches.
        #[arg(long, default_value_t = 10)]
        steps: usize,
    },
    /// Export a palette.
    Palette {
        /// Named colours (`name=#hex` or `#hex`).
        #[arg(required = true)]
        colors: Vec<String>,
        /// Palette name.
        #[arg(long, default_value = "Formflux")]
        name: String,
        /// json or gpl.
        #[arg(long, default_value = "gpl")]
        format: String,
        /// Output file.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

/// Generation subcommands.
#[derive(Debug, Subcommand)]
pub enum GenCmd {
    /// Generate a QR code.
    Qr {
        /// Payload.
        data: String,
        /// Output `.png` or `.svg`.
        #[arg(short, long)]
        output: PathBuf,
        /// Module size in pixels.
        #[arg(long, default_value_t = 8)]
        size: u32,
        /// Quiet-zone modules.
        #[arg(long, default_value_t = 4)]
        border: u32,
    },
    /// Generate an EAN-13 barcode SVG.
    Barcode {
        /// 12 or 13 digits.
        data: String,
        /// Output `.svg`.
        #[arg(short, long)]
        output: PathBuf,
        /// Bar height in pixels.
        #[arg(long, default_value_t = 80)]
        height: u32,
    },
    /// Generate UUIDs.
    Uuid {
        /// 4 or 7.
        #[arg(long, default_value_t = 4)]
        version: u8,
        /// Number of UUIDs.
        #[arg(long, default_value_t = 1)]
        count: usize,
    },
    /// Generate a random password.
    Password {
        /// Length.
        #[arg(long, default_value_t = 20)]
        length: usize,
        /// Exclude symbols.
        #[arg(long)]
        no_symbols: bool,
        /// Exclude digits.
        #[arg(long)]
        no_digits: bool,
    },
    /// Check password strength with zxcvbn.
    Strength {
        /// Password to evaluate.
        password: String,
    },
}

/// Time subcommands.
#[derive(Debug, Subcommand)]
pub enum TimeCmd {
    /// Current timestamps.
    Now {
        /// s, ms or us.
        #[arg(long, default_value = "s")]
        unit: String,
        /// IANA time zone.
        #[arg(long, default_value = "UTC")]
        tz: String,
    },
    /// Convert a Unix timestamp.
    Convert {
        /// Timestamp value.
        value: i64,
        /// s, ms or us.
        #[arg(long, default_value = "s")]
        unit: String,
        /// Target IANA time zone.
        #[arg(long, default_value = "UTC")]
        tz: String,
        /// iso8601, human, date, time or a strftime pattern.
        #[arg(long, default_value = "iso8601")]
        format: String,
    },
    /// Convert a date/time between zones.
    Datetime {
        /// Date/time text.
        value: String,
        /// Source zone.
        #[arg(long, default_value = "UTC")]
        from: String,
        /// Target zone.
        #[arg(long, default_value = "UTC")]
        to: String,
        /// Output format.
        #[arg(long, default_value = "iso8601")]
        format: String,
    },
    /// List IANA time zones.
    Zones {
        /// Case-insensitive filter.
        #[arg(long)]
        filter: Option<String>,
    },
}

/// File toolbox subcommands.
#[derive(Debug, Subcommand)]
pub enum ToolsCmd {
    /// Batch rename files.
    Rename(RenameArgs),
    /// Find duplicate files by content.
    Dedupe {
        /// Files or directories.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Hash algorithm.
        #[arg(long, default_value = "sha256")]
        algorithm: String,
        /// Delete redundant copies (requires --yes).
        #[arg(long)]
        delete: bool,
    },
    /// Split a file into chunks.
    Split {
        /// Source file.
        file: PathBuf,
        /// Chunk size (`100M`, `1GiB`).
        #[arg(long)]
        size: String,
        /// Output directory.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    /// Merge chunks.
    Merge {
        /// Parts in order.
        #[arg(required = true)]
        parts: Vec<PathBuf>,
        /// Output file.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Remove empty directories.
    CleanEmpty {
        /// Root directory.
        dir: PathBuf,
        /// Actually delete (otherwise dry-run).
        #[arg(long)]
        apply: bool,
    },
    /// Show file metadata.
    Info {
        /// File or directory.
        path: PathBuf,
    },
    /// Convert length, mass, temperature, data, time or speed units.
    Units {
        /// Numeric value.
        value: f64,
        /// Source unit (for example `km`, `c`, `MiB`, `h`).
        #[arg(long)]
        from: String,
        /// Target unit.
        #[arg(long)]
        to: String,
    },
}

/// Rename options (exactly one mode).
#[derive(Debug, Args)]
pub struct RenameArgs {
    /// Files to rename.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Pattern with `{name}`, `{ext}`, `{n}`, `{n:03}`, `{date}`.
    #[arg(long)]
    pub pattern: Option<String>,
    /// Prefix for the stem.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Suffix for the stem.
    #[arg(long)]
    pub suffix: Option<String>,
    /// Regex applied to the stem.
    #[arg(long)]
    pub regex: Option<String>,
    /// Replacement for --regex.
    #[arg(long, default_value = "")]
    pub replacement: String,
    /// Use EXIF capture time; value is a chrono format.
    #[arg(long)]
    pub exif_time: Option<String>,
    /// First sequence number.
    #[arg(long, default_value_t = 0)]
    pub start: u64,
    /// Sequence step.
    #[arg(long, default_value_t = 1)]
    pub step: u64,
}

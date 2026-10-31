# Changelog

All notable changes to Formflux are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions use [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-31

First public release. Windows x64 binaries; Linux musl and macOS build in CI.

### Added

#### Media
- Video conversion across MP4, WEBM, MKV, AVI, MOV, FLV, TS and WMV.
- Animated GIF and WebP export with palette generation and configurable FPS.
- Stream-copy mode and re-encoding with CRF, bitrate, preset, FPS, resolution,
  crop and rotation controls.
- HDR→SDR tone mapping and subtitle mux/extract (SRT and ASS).
- Audio conversion across MP3, FLAC, WAV, OGG, AAC, M4A, ALAC, APE and WMA,
  with loudness normalisation, denoise, trim, sample-rate and channel control.
- FFmpeg probe, first-frame thumbnails and real-time progress parsing
  (`-progress pipe:1`), including speed and ETA.

#### Images
- JPG, PNG, WebP, BMP, TIFF, ICO and GIF conversion, batch compress and resize.
- Crop, rotate, flip, brightness, contrast, saturation and gamma adjustments.
- EXIF read and strip; optional AVIF build feature.
- Text and image watermarks with anchor, margin and opacity.
- GUI image preview and batch queue with memory-budget admission.

#### Documents and text
- Markdown, HTML and plain-text conversion; CSV/TSV conversion.
- Base64, Base64URL, Base32, URL and Unicode codecs.
- GBK/UTF-8/Latin-1 transcoding, whitespace cleanup and case conversion.
- Unified diff, regex replace and offline OCR through external Tesseract.

#### Hashes and verification
- MD5, SHA-1, SHA-256, SHA-384, SHA-512 and BLAKE2b.
- Single-file, directory and batch hashing.
- Manifest export, parsing and verification with traversal protection.

#### Colour, toolbox, data, generation and time
- HEX, RGB, RGBA, HSL, HSLA, CMYK and LAB conversion; WCAG contrast; gradients;
  GPL and JSON palette export.
- Batch rename (sequence, prefix, suffix, regex, EXIF time), hash dedupe,
  split/merge, empty-directory cleanup, metadata and unit conversion.
- JSON, YAML and XML format/validate/convert plus jq-style path queries.
- QR codes, EAN-13 barcodes, UUID v4/v7, password generation and strength.
- Unix timestamps in s/ms/µs, IANA time zones and formatted datetime conversion.

#### Applications
- `formflux` CLI with structured `--json`, `--quiet`, `--dry-run`, `--force`,
  `-j/--jobs`, `--config` and `doctor` diagnostics.
- `formflux-gui` desktop app with drag and drop, nine task tabs, output browser,
  task queue, cancel/retry, previews and settings.
- Bilingual English / 简体中文 CLI and GUI, plus About metadata.
- Modern GUI styling: accent actions, layered panels, filled sidebar selection,
  drop zone, status dots and responsive task empty state.

#### Engineering
- Workspace split into `shared`, `core`, `cli`, `gui` and optional `cuda`.
- Central `Result<T, FormfluxError>` error type and path-safe IO.
- Atomic writes, read-only sources, no-overwrite defaults and memory budget.
- 141 tests: 105 core, 1 integration, 2 CUDA, 32 shared, 1 CLI.
- CI for Windows, Linux musl and macOS; cargo-deny and offline-source checks.
- README, architecture notes, GUI screenshots and CLI demo GIF.

### Security
- No HTTP client, telemetry, analytics, cloud sync, ads or watermark code.
- External paths validated with `safe_join`; manifest traversal rejected.
- FFmpeg/Tesseract are external runtime dependencies, never bundled or linked.

### Known limitations
- HEIC and RAW are reserved feature names and return `Unsupported`.
- CUDA is detection plus CPU fallback; acceleration kernels are roadmap work.
- OCR requires external Tesseract and was not end-to-end tested on the release
  machine (Tesseract was not installed).
- EXIF supports read/strip, not arbitrary tag writing.
- No system-wide screen colour picker (avoids unsafe global hooks).
- The 10 GB video / 300 MB RSS target has not been benchmarked yet.
- Windows was smoke-tested locally; Linux musl and macOS were built by CI but
  not executed on this machine.

[0.1.0]: https://github.com/dieraun/Formflux/releases/tag/v0.1.0

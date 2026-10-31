# Formflux

Formflux is a local, offline toolbox for media, images, documents, hashes,
colours, data and time. It replaces ad-supported online converters: nothing is
uploaded, there is no telemetry, no watermark and no network client in the
dependency tree.

- **CLI**: `formflux <command>` (script-friendly, `--json` everywhere)
- **Desktop GUI**: `formflux-gui` (egui, drag and drop, task queue)
- **Platforms**: Windows, Linux, macOS
- **License**: MIT

## Build

```sh
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The workspace produces two binaries:

| Binary | Entry point |
| --- | --- |
| `formflux` | `crates/cli` |
| `formflux-gui` | `crates/gui` |

Minimum supported Rust version: 1.85 (edition 2021).

### Feature variants

| Feature | Effect | Notes |
| --- | --- | --- |
| *(default)* | `formflux-min`: JPEG/PNG/WebP/BMP/TIFF/ICO/GIF | Fully static build possible |
| `avif` | AVIF decoding/encoding through the `image` crate | Adds C/assembly codec code |
| `heic` | Reserved for libheif integration | Currently reports `Unsupported` |
| `raw` | Reserved for rawloader integration | Currently reports `Unsupported` |
| `cuda` | NVIDIA device detection with CPU fallback | Detection today; kernels are roadmap work |

```sh
cargo build --release --features avif          # non-strict single file
cargo build --release --features cuda
```

Builds with `avif`/`heic`/`raw`/`cuda` are distributed as `formflux-full` and
are not strict single-file binaries.

### Static distribution

- **Linux** (`x86_64-unknown-linux-musl`):
  ```sh
  RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target x86_64-unknown-linux-musl -p formflux-cli
  ```
- **Windows** (`x86_64-pc-windows-msvc`): `RUSTFLAGS="-C target-feature=+crt-static"`
- **macOS**: universal or per-arch binaries; `.app` packaging is done in CI.

### Platform matrix

| Target | CI build | Notes |
| --- | --- | --- |
| `x86_64-pc-windows-msvc` | `formflux-min`, `formflux-full` | static CRT, Windows subsystem for GUI |
| `x86_64-unknown-linux-musl` | `formflux-min`, `formflux-full` | static musl binary |
| `aarch64-apple-darwin` | `formflux-min` | Apple Silicon |
| `x86_64-apple-darwin` | `formflux-min` | Intel macOS |

### Feature coverage

| Area | CLI | GUI |
| --- | --- | --- |
| Media convert/probe/subtitles | yes | convert, queue, progress |
| Image convert/compress/EXIF/watermark | yes | convert, preview |
| Documents, OCR, codecs, radix, encodings | yes | document convert, OCR |
| Hashes and manifests | yes | hashes |
| Colour, WCAG, gradients, palettes | yes | convert, picker, WCAG |
| File toolbox, units | yes | split, cleanup, radix |
| Data, generate, time | yes | convert, UUID/password/QR, timestamps |

Screen colour picking is intentionally not implemented with a hidden global hook;
the GUI picker starts from the current value and the CLI accepts any colour
string. This keeps the no-unsafe-code policy intact.

### Current limits

- HEIC and RAW are reserved feature names and currently return `Unsupported`.
- CUDA support is device detection plus CPU fallback; accelerated kernels are
  roadmap work.
- EXIF metadata can be read and stripped; arbitrary tag writing is not exposed.
- The GUI covers the common convert/hash/OCR/generate workflows. The complete
  tool matrix, including manifests, subtitles, rename plans and palette export,
  is available through the CLI.
- The 300 MB RSS target applies to Formflux itself; FFmpeg runs as a separate
  process and has its own memory use.

## External runtime dependencies

Formflux never bundles or links these tools. It probes `PATH` and the configured
path, then enables the relevant features only when they exist.

| Tool | Used for | Configuration key |
| --- | --- | --- |
| FFmpeg + ffprobe | media conversion, probing, thumbnails, subtitles | `[tools] ffmpeg_path`, `ffprobe_path` |
| Tesseract | offline OCR | `[tools] tesseract_path`, `ocr_lang` |

`formflux doctor` prints detection results, versions and warnings.

FFmpeg is called as a subprocess (`-progress pipe:1`) and is licensed by its own
project (LGPL/GPL depending on the build). Tesseract is Apache-2.0. Neither is
redistributed by Formflux.

## Usage

```sh
formflux doctor
formflux media probe movie.mkv
formflux media convert movie.mkv --format mp4 --crf 20
formflux media convert clip.mov --format gif --gif-fps 12
formflux media extract-subs movie.mkv --output subs/
formflux media extract-subs movie.mkv --format ass --output subs/
formflux image convert photo.jpg --format webp --quality 82
formflux image compress *.png --output out/ --width 1600
formflux image exif photo.jpg
formflux image exif photo.jpg --strip --output clean/
formflux hash file src/ --algorithm sha256 --manifest SHA256SUMS
formflux hash verify SHA256SUMS --base .
formflux text convert notes.md --to html
formflux text transcode legacy.txt --from gbk --to utf8 --output notes.txt
formflux text whitespace draft.txt --output clean.txt
formflux text codec --value "hello" --codec base64
formflux text radix 0xff 0b1010 777 --to 16
formflux text ascii "A中"
formflux data convert config.json --to yaml
formflux color convert "#ff8800" --to hsl
formflux gen qr "https://example.invalid" --output qr.png
formflux time now --tz Europe/Berlin
formflux time convert 1705318200 --unit s --tz America/New_York
formflux tools units 1.5 --from GiB --to MiB
formflux tools rename *.jpg --pattern "trip-{n:03}"
formflux tools dedupe photos/ --algorithm blake2b
```

Global flags: `--json`, `--quiet`, `--yes`, `--dry-run`, `--force`,
`-j/--jobs`, `--config`, `-v`, `--trace`.

Output files are written atomically (temporary file + rename), source files are
never modified, and existing destinations are not overwritten unless `--force`
is given.

## Configuration

Created on first run at the platform config directory, for example:

- Windows: `%APPDATA%\formflux\formflux\config.toml`
- Linux: `~/.config/formflux/config.toml`
- macOS: `~/Library/Application Support/io.formflux.formflux/config.toml`

Override with `--config <path>` or `FORMFLUX_CONFIG`.

```toml
version = 1
log_level = "info"

[tools]
ffmpeg_path = "C:/tools/ffmpeg/bin/ffmpeg.exe"
ffprobe_path = "C:/tools/ffmpeg/bin/ffprobe.exe"
tesseract_path = "C:/Program Files/Tesseract-OCR/tesseract.exe"
ocr_lang = "eng"
cuda = false

[runtime]
jobs = 0          # 0 = logical CPU count
memory_budget_mb = 512
overwrite = false
keep_temp = false
progress_step = 1

[ui]
theme = "system"  # system | dark | light
scale = 1.0
start_maximized = false
```

## Architecture

```
crates/
├── shared/  # errors, config, path safety, atomic IO, limits, logging, units
├── core/    # all functionality; no egui and no clap
├── cli/     # clap front end
├── gui/     # egui front end
└── cuda/    # optional NVIDIA detection / acceleration layer
```

Every public core API returns `Result<T, FormfluxError>`. Filesystem access goes
through `shared::fs` and `core::io`; external paths are validated with
`safe_join`, which rejects `..` and absolute escapes.

The GUI runs work on a Tokio multi-thread runtime and communicates through an
unbounded channel; the paint thread never blocks. Concurrent jobs are bounded by
a semaphore configured from `runtime.jobs`.

## Security and privacy

- No HTTP client, telemetry, analytics, cloud sync or ads in the source tree or
  dependency graph; `deny.toml` enforces the ban.
- Source files are opened read-only.
- Atomic output writes prevent half-written files.
- Batch limits are held in `shared::limits`; oversized requests fail cleanly
  instead of exhausting memory.
- Media and file copies are streamed in chunks, never loaded whole.
- `verify_manifest` rejects absolute and parent-traversal entries.

## Tests

```sh
cargo test --workspace
```

Unit tests live next to each core module and cover path safety, atomic writes,
hash vectors, encoding, EXIF stripping, colour maths, time-zone conversion,
manifest verification and media argument construction.

## CI / release

`.github/workflows/release.yml` builds Windows, Linux (musl) and macOS on every
tag matching `v*`, runs format/clippy/tests, strips binaries, archives them and
attaches `formflux-min` and `formflux-full` artifacts to the GitHub Release.

## License

MIT. See [LICENSE](LICENSE). FFmpeg and Tesseract remain under their own
licenses and are external runtime dependencies.

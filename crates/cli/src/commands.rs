//! Command implementations.

use std::path::PathBuf;

use anyhow::{bail, Context as _};
use formflux_core::cancel::CancelToken;
use formflux_core::color as colorlib;
use formflux_core::data::{self, DataFormat};
use formflux_core::detect::FfmpegTools;
use formflux_core::generate;
use formflux_core::hash::{self, HashAlgorithm};
use formflux_core::image::watermark::{Anchor, Watermark, WatermarkKind};
use formflux_core::image::{
    self as imglib, CropSpec, ImageAdjustments, ImageOptions, ResizeMode, ResizeOptions,
};
use formflux_core::media::{self, ConvertOptions, CropRect, Rotation, SubtitleFormat};
use formflux_core::text::{self, CaseStyle, Codec, TextEncoding, TextFormat};
use formflux_core::time as timelib;
use formflux_core::tools::{self, RenameMode};
use formflux_shared::config::AppConfig;
use formflux_shared::fs as sfs;
use formflux_shared::units;
use serde_json::json;

use crate::args::*;
use crate::output::Context;

/// Dispatch a parsed command.
pub async fn run(command: Command, config: &AppConfig, ctx: &Context) -> anyhow::Result<()> {
    match command {
        Command::About => about(ctx),
        Command::Doctor => doctor(config, ctx),
        Command::Media(cmd) => media_cmd(cmd, config, ctx).await,
        Command::Image(cmd) => image_cmd(cmd, ctx),
        Command::Hash(cmd) => hash_cmd(cmd, ctx),
        Command::Text(cmd) => text_cmd(cmd, config, ctx).await,
        Command::Data(cmd) => data_cmd(cmd, ctx),
        Command::Color(cmd) => color_cmd(cmd, ctx),
        Command::Gen(cmd) => gen_cmd(cmd, ctx),
        Command::Time(cmd) => time_cmd(cmd, ctx),
        Command::Tools(cmd) => tools_cmd(cmd, ctx),
    }
}

fn about(ctx: &Context) -> anyhow::Result<()> {
    let description = ctx.lang.pick(
        "Offline media, image, document, hash, colour, data and developer tools.",
        "离线多媒体、图像、文档、哈希、颜色、数据与开发者工具箱。",
    );
    let payload = json!({
        "name": formflux_shared::PRODUCT,
        "version": formflux_shared::VERSION,
        "author": formflux_shared::AUTHOR,
        "repository": formflux_shared::REPOSITORY,
        "license": formflux_shared::LICENSE,
        "offline": true,
        "description": description,
    });
    let human = match ctx.lang {
        formflux_shared::config::Language::Zh => format!(
            "Formflux {}
作者: {}
仓库: {}
许可证: {}
离线: 是 / 完全离线

             Formflux 离线多媒体、图像、文档、哈希、颜色与数据工具箱。
             作者：dieraun    仓库：{}
",
            formflux_shared::VERSION,
            formflux_shared::AUTHOR,
            formflux_shared::REPOSITORY,
            formflux_shared::LICENSE,
            formflux_shared::REPOSITORY
        ),
        formflux_shared::config::Language::En => format!(
            "Formflux {}
Author: {}
Repository: {}
License: {}
Offline: yes / 完全离线

             An offline media, image, document, hash, colour, data and developer toolbox.
             Author: dieraun    Repository: {}
",
            formflux_shared::VERSION,
            formflux_shared::AUTHOR,
            formflux_shared::REPOSITORY,
            formflux_shared::LICENSE,
            formflux_shared::REPOSITORY
        ),
    };
    ctx.emit(&payload, || human)
}

fn doctor(config: &AppConfig, ctx: &Context) -> anyhow::Result<()> {
    let report = formflux_core::doctor::DoctorReport::collect(config);
    ctx.emit(&report, || {
        let mut text = match ctx.lang {
            formflux_shared::config::Language::Zh => format!(
                "Formflux {} · {} ({}) · {} 个 CPU 线程 · {}",
                report.version,
                report.os,
                report.arch,
                report.cpus,
                if report.media_ready() {
                    "媒体功能可用"
                } else {
                    "媒体功能不可用（需要 FFmpeg）"
                }
            ),
            _ => report.headline(),
        };
        text.push('\n');
        text.push_str(&format!(
            "{}: {} / {} / {}\n{}: {}  {}: {}\n",
            ctx.t("Platform", "平台"),
            report.os,
            report.arch,
            report.target,
            ctx.t("CPUs", "处理器"),
            report.cpus,
            ctx.t("jobs", "并发"),
            report.jobs
        ));
        text.push_str(&format!(
            "FFmpeg: {}\nFFprobe: {}\nTesseract: {}\nCUDA: {}\n",
            report.ffmpeg.summary(),
            report.ffprobe.summary(),
            report.tesseract.summary(),
            report.cuda.summary()
        ));
        if !report.warnings.is_empty() {
            text.push_str(&format!("{}:\n", ctx.t("Warnings", "警告")));
            for warning in &report.warnings {
                text.push_str(&format!("  - {warning}\n"));
            }
        }
        text
    })
}

async fn media_cmd(cmd: MediaCmd, config: &AppConfig, ctx: &Context) -> anyhow::Result<()> {
    let tools = FfmpegTools::require(config)?;
    match cmd {
        MediaCmd::Probe { input } => {
            let info = media::probe(&tools, &input).await?;
            ctx.emit(&info, || {
                let mut text = format!(
                    "{}\nformat: {}  duration: {}  size: {}\n",
                    info.path.display(),
                    info.format_long_name.as_deref().unwrap_or("unknown"),
                    info.duration_label(),
                    formflux_core::format_bytes(info.size_bytes)
                );
                for stream in &info.streams {
                    text.push_str(&format!(
                        "  #{} {} {} {}\n",
                        stream.index,
                        stream.codec_type,
                        stream.codec_name.as_deref().unwrap_or("-"),
                        stream
                            .width
                            .zip(stream.height)
                            .map(|(w, h)| format!("{w}x{h}"))
                            .unwrap_or_default()
                    ));
                }
                text
            })
        }
        MediaCmd::ExtractSubs {
            input,
            stream,
            output,
            format,
        } => {
            let format = SubtitleFormat::parse(&format)?;
            let outcome =
                media::extract_subtitles_as(&tools, &input, stream, &output, format, ctx.force)
                    .await?;
            ctx.emit(&outcome, || {
                format!(
                    "{} (stream {}, {})",
                    outcome.output.display(),
                    outcome.stream_index,
                    outcome.language.as_deref().unwrap_or("unknown language")
                )
            })
        }
        MediaCmd::Convert(args) => media_convert(args, config, ctx).await,
    }
}

async fn media_convert(
    args: MediaConvertArgs,
    config: &AppConfig,
    ctx: &Context,
) -> anyhow::Result<()> {
    let tools = FfmpegTools::require(config)?;
    let options = ConvertOptions {
        format: args.format.clone(),
        stream_copy: args.stream_copy,
        video_codec: args.video_codec,
        audio_codec: args.audio_codec,
        crf: args.crf,
        preset: args.preset,
        video_bitrate: args.video_bitrate,
        audio_bitrate: args.audio_bitrate,
        width: args.width,
        height: args.height,
        fps: args.fps,
        crop: args.crop.as_deref().map(parse_media_crop).transpose()?,
        rotate: Rotation::parse(&args.rotate.to_string())?,
        hdr_to_sdr: args.hdr_to_sdr,
        normalize_audio: args.normalize_audio,
        denoise: args.denoise,
        sample_rate: args.sample_rate,
        channels: args.channels,
        start_seconds: args
            .start
            .as_deref()
            .map(units::parse_duration)
            .transpose()?,
        duration_seconds: args
            .duration
            .as_deref()
            .map(units::parse_duration)
            .transpose()?,
        no_audio: args.no_audio,
        no_video: args.no_video,
        gif_fps: args
            .format
            .eq_ignore_ascii_case("gif")
            .then_some(args.gif_fps as f32),
        gif_width: args.gif_width,
        embed_subtitles: args.subtitle.clone(),
    };
    options.validate()?;
    formflux_core::io::ensure_dir(&args.output)?;
    if ctx.dry_run {
        let stem = args
            .input
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("output");
        let target = args.output.join(format!("{stem}.{}", options.format));
        let command = media::build_args(&args.input, &target, &options)?;
        return ctx.emit(
            &json!({ "command": command, "target": target, "options": options }),
            || format!("ffmpeg {}", command.join(" ")),
        );
    }
    let progress = ctx.progress("media");
    let outcome = media::convert(
        &tools,
        &args.input,
        &args.output,
        &options,
        ctx.force,
        &CancelToken::never(),
        &progress,
    )
    .await?;
    ctx.emit(&outcome, || {
        format!(
            "{} -> {} ({}, {} in {:.1}s)",
            outcome.input.display(),
            outcome.output.display(),
            outcome.format,
            formflux_core::format_bytes(outcome.output_bytes),
            outcome.elapsed_seconds
        )
    })
}

fn parse_media_crop(value: &str) -> anyhow::Result<CropRect> {
    let parts: Vec<&str> = value.split([':', 'x']).map(str::trim).collect();
    if parts.len() != 4 {
        bail!("crop must be width:height:x:y");
    }
    Ok(CropRect {
        width: parts[0].parse().context("invalid crop width")?,
        height: parts[1].parse().context("invalid crop height")?,
        x: parts[2].parse().context("invalid crop x")?,
        y: parts[3].parse().context("invalid crop y")?,
    })
}

fn image_cmd(cmd: ImageCmd, ctx: &Context) -> anyhow::Result<()> {
    match cmd {
        ImageCmd::Info { input } => {
            let info = imglib::info(&input)?;
            ctx.emit(&info, || {
                format!(
                    "{}: {} {}x{} {:?} ({})",
                    info.path.display(),
                    info.format,
                    info.width,
                    info.height,
                    info.color,
                    formflux_core::format_bytes(info.size_bytes)
                )
            })
        }
        ImageCmd::Exif {
            input,
            strip,
            output,
        } => {
            if strip {
                let target = imglib::exif::strip_exif(&input, &output, ctx.force)?;
                ctx.path(&target)
            } else {
                let summary = imglib::exif::read_exif(&input)?;
                ctx.emit(&summary, || {
                    serde_json::to_string_pretty(&summary).unwrap_or_default()
                })
            }
        }
        ImageCmd::Convert(args) | ImageCmd::Compress(args) => image_convert(args, ctx),
    }
}

fn image_convert(args: ImageArgs, ctx: &Context) -> anyhow::Result<()> {
    let inputs = expand_inputs(&args.input, false)?;
    if inputs.is_empty() {
        bail!("no input images");
    }
    let resize = if args.width.is_some() || args.height.is_some() {
        Some(ResizeOptions {
            width: args.width,
            height: args.height,
            mode: match args.resize_mode.to_ascii_lowercase().as_str() {
                "fit" => ResizeMode::Fit,
                "exact" | "stretch" => ResizeMode::Exact,
                other => bail!("unknown resize mode '{other}' (fit or exact)"),
            },
        })
    } else {
        None
    };
    let crop = args.crop.as_deref().map(parse_image_crop).transpose()?;
    let watermark = match (args.watermark_text, args.watermark_image) {
        (Some(text), None) => Some(Watermark {
            kind: WatermarkKind::Text {
                text,
                size: 24,
                color: [0, 0, 0, 210],
            },
            anchor: Anchor::parse(&args.watermark_anchor)?,
            margin: 16,
            opacity: args.watermark_opacity,
        }),
        (None, Some(path)) => Some(Watermark {
            kind: WatermarkKind::Image {
                path,
                scale_percent: 20,
            },
            anchor: Anchor::parse(&args.watermark_anchor)?,
            margin: 16,
            opacity: args.watermark_opacity,
        }),
        (Some(_), Some(_)) => bail!("choose either --watermark-text or --watermark-image"),
        (None, None) => None,
    };
    let options = ImageOptions {
        format: args.format.unwrap_or_default(),
        quality: args.quality,
        resize,
        crop,
        rotate: args.rotate,
        flip_horizontal: args.flip_h,
        flip_vertical: args.flip_v,
        adjustments: ImageAdjustments {
            brightness: args.brightness,
            contrast: args.contrast,
            saturation: args.saturation,
            gamma: args.gamma,
        },
        watermark,
        auto_orient: !args.no_auto_orient,
        strip_metadata: true,
    };
    options.validate()?;
    if ctx.dry_run {
        return ctx.emit(
            &json!({ "inputs": inputs, "output": args.output, "options": options }),
            || {
                format!(
                    "would process {} image(s) into {}",
                    inputs.len(),
                    args.output.display()
                )
            },
        );
    }
    let progress = ctx.progress("image");
    let outcomes = imglib::batch_convert(
        &inputs,
        &args.output,
        &options,
        ctx.force,
        ctx.jobs,
        &CancelToken::never(),
        &progress,
    )?;
    ctx.emit(&outcomes, || {
        outcomes
            .iter()
            .map(|outcome| {
                format!(
                    "{} -> {} ({}x{}, {})",
                    outcome.input.display(),
                    outcome.output.display(),
                    outcome.width,
                    outcome.height,
                    formflux_core::format_bytes(outcome.output_bytes)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn parse_image_crop(value: &str) -> anyhow::Result<CropSpec> {
    let parts: Vec<&str> = value.split([':', 'x']).map(str::trim).collect();
    if parts.len() != 4 {
        bail!("crop must be width:height:x:y");
    }
    Ok(CropSpec {
        width: parts[0].parse().context("invalid crop width")?,
        height: parts[1].parse().context("invalid crop height")?,
        x: parts[2].parse().context("invalid crop x")?,
        y: parts[3].parse().context("invalid crop y")?,
    })
}

fn hash_cmd(cmd: HashCmd, ctx: &Context) -> anyhow::Result<()> {
    match cmd {
        HashCmd::File(args) => {
            let algorithm = HashAlgorithm::parse(&args.algorithm)?;
            let progress = ctx.progress("hash");
            let hashes = hash::hash_paths(
                &args.paths,
                algorithm,
                args.recursive,
                args.hidden,
                &CancelToken::never(),
                &progress,
            )?;
            if let Some(manifest) = &args.manifest {
                if !ctx.dry_run {
                    if let Some(parent) = manifest.parent() {
                        formflux_core::io::ensure_dir(parent)?;
                    }
                    formflux_core::io::write_text(
                        manifest,
                        &hash::format_manifest(&hashes),
                        ctx.force,
                    )?;
                }
            }
            ctx.emit(&hashes, || {
                hashes
                    .iter()
                    .map(|item| {
                        format!(
                            "{}  {}  ({})",
                            item.digest,
                            item.path.display(),
                            formflux_core::format_bytes(item.size)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        HashCmd::Verify { manifest, base } => {
            let text = formflux_core::io::read_text(&manifest, formflux_core::io::TEXT_LIMIT)?;
            let progress = ctx.progress("verify");
            let results = hash::verify_manifest(&text, &base, &CancelToken::never(), &progress)?;
            let failed = results.iter().filter(|result| !result.ok).count();
            if !ctx.json {
                for result in &results {
                    println!(
                        "{}  {}",
                        if result.ok { "OK" } else { "FAIL" },
                        result.path.display()
                    );
                }
            } else {
                ctx.emit(&results, String::new)?;
            }
            if failed > 0 {
                bail!(
                    "{failed} manifest entr{} failed",
                    if failed == 1 { "y" } else { "ies" }
                );
            }
            Ok(())
        }
    }
}

async fn text_cmd(cmd: TextCmd, config: &AppConfig, ctx: &Context) -> anyhow::Result<()> {
    match cmd {
        TextCmd::Convert { input, to, output } => {
            let target = TextFormat::parse(&to)?;
            let path = text::convert_document(&input, &output, target, ctx.force)?;
            ctx.path(&path)
        }
        TextCmd::Delimited { input, to, output } => {
            let path = text::convert_delimited(&input, &output, &to, ctx.force)?;
            ctx.path(&path)
        }
        TextCmd::Transcode {
            input,
            from,
            to,
            output,
        } => {
            let from = TextEncoding::parse(&from)?;
            let to = TextEncoding::parse(&to)?;
            let path = text::transcode_file(&input, &output, from, to, ctx.force)?;
            ctx.path(&path)
        }
        TextCmd::Codec {
            value,
            file,
            codec,
            decode,
        } => {
            let codec = Codec::parse(&codec)?;
            let input = match (value, file) {
                (Some(value), None) => value,
                (None, Some(path)) => {
                    formflux_core::io::read_text(&path, formflux_core::io::TEXT_LIMIT)?
                }
                (Some(_), Some(_)) => bail!("use either a literal value or --file"),
                (None, None) => bail!("provide a value or --file"),
            };
            let output = if decode {
                codec.decode(&input)?
            } else {
                codec.encode(&input)
            };
            ctx.text(&output)
        }
        TextCmd::Diff { old, new, context } => {
            let old_text = formflux_core::io::read_text(&old, formflux_core::io::TEXT_LIMIT)?;
            let new_text = formflux_core::io::read_text(&new, formflux_core::io::TEXT_LIMIT)?;
            let result = text::diff(&old_text, &new_text, context);
            ctx.emit(&result, || result.unified.clone())
        }
        TextCmd::Replace {
            input,
            pattern,
            replacement,
            all,
            output,
        } => {
            let source = formflux_core::io::read_text(&input, formflux_core::io::TEXT_LIMIT)?;
            let replaced = text::regex_replace(&source, &pattern, &replacement, all)?;
            let extension = input.extension().and_then(|value| value.to_str());
            let target = sfs::destination_for(&input, &output, extension, ctx.force)?;
            formflux_core::io::write_text(&target, &replaced, ctx.force)?;
            ctx.path(&target)
        }
        TextCmd::Whitespace {
            input,
            encoding,
            output,
        } => {
            let encoding = TextEncoding::parse(&encoding)?;
            let source = text::read_text(&input, encoding, formflux_core::io::TEXT_LIMIT)?;
            let cleaned = text::normalize_whitespace(&source);
            match output {
                Some(path) => {
                    formflux_core::io::write_text(&path, &cleaned, ctx.force)?;
                    ctx.path(&path)
                }
                None => ctx.text(&cleaned),
            }
        }
        TextCmd::Case { value, style } => {
            let style = CaseStyle::parse(&style)?;
            ctx.text(&style.apply(&value))
        }
        TextCmd::Radix { values, from, to } => {
            let from = from
                .as_deref()
                .map(formflux_core::radix::Radix::parse)
                .transpose()?;
            let to = formflux_core::radix::Radix::parse(&to)?;
            let results = formflux_core::radix::convert_many(&values, from, to)?;
            ctx.emit(&results, || results.join("\n"))
        }
        TextCmd::Ascii { value, decode } => {
            if decode {
                let decoded = formflux_core::radix::codepoints_to_string(&value)?;
                ctx.text(&decoded)
            } else {
                let characters = formflux_core::radix::inspect_chars(&value);
                ctx.emit(&characters, || {
                    characters
                        .iter()
                        .map(|info| {
                            let bytes = info
                                .utf8
                                .iter()
                                .map(|byte| format!("{byte:02X}"))
                                .collect::<Vec<_>>()
                                .join(" ");
                            format!("{}  {}  utf8={bytes}", info.character, info.unicode)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
        }
        TextCmd::Ocr {
            input,
            lang,
            psm,
            output,
        } => {
            let result =
                formflux_core::ocr::ocr_with_config(config, &input, lang.as_deref(), psm).await?;
            if let Some(path) = output {
                formflux_core::io::write_text(&path, &result.text, ctx.force)?;
                ctx.path(&path)
            } else {
                ctx.emit(&result, || result.text.clone())
            }
        }
    }
}

fn data_cmd(cmd: DataCmd, ctx: &Context) -> anyhow::Result<()> {
    match cmd {
        DataCmd::Convert {
            input,
            to,
            output,
            pretty,
        } => {
            let target = DataFormat::parse(&to)?;
            let path = data::convert_file(&input, &output, target, pretty, ctx.force)?;
            ctx.path(&path)
        }
        DataCmd::Validate { input } => {
            let format = DataFormat::from_path(&input)?;
            let text = formflux_core::io::read_text(&input, formflux_core::io::TEXT_LIMIT)?;
            data::validate(&text, format)?;
            ctx.emit(
                &json!({ "valid": true, "format": format.extension() }),
                || format!("{} is valid {}", input.display(), format.extension()),
            )
        }
        DataCmd::Query { input, path } => {
            let format = DataFormat::from_path(&input)?;
            let text = formflux_core::io::read_text(&input, formflux_core::io::TEXT_LIMIT)?;
            let value = data::query(&text, format, &path)?;
            ctx.emit(&value, || {
                serde_json::to_string_pretty(&value).unwrap_or_default()
            })
        }
        DataCmd::Format { input, minify } => {
            let text = formflux_core::io::read_text(&input, formflux_core::io::TEXT_LIMIT)?;
            let value = data::parse_json(&text)?;
            let rendered = data::to_json(&value, !minify)?;
            ctx.text(&rendered)
        }
    }
}

fn color_cmd(cmd: ColorCmd, ctx: &Context) -> anyhow::Result<()> {
    match cmd {
        ColorCmd::Convert { value, to } => {
            let color = colorlib::parse(&value)?;
            let space = colorlib::ColorSpace::parse(&to)?;
            let rendered = colorlib::format(color, space);
            ctx.emit(
                &json!({
                    "hex": color.to_hex(),
                    "rgb": [color.r, color.g, color.b, color.a],
                    "hsl": color.to_hsl(),
                    "cmyk": color.to_cmyk(),
                    "lab": color.to_lab(),
                    "formatted": rendered,
                }),
                || rendered,
            )
        }
        ColorCmd::Contrast {
            foreground,
            background,
        } => {
            let foreground = colorlib::parse(&foreground)?;
            let background = colorlib::parse(&background)?;
            let result = colorlib::contrast(foreground, background);
            let best = colorlib::best_foreground(background);
            ctx.emit(
                &json!({ "result": result, "best_foreground": best.to_hex() }),
                || {
                    format!(
                        "contrast {:.2}:1  AA normal: {}  AAA normal: {}  suggested text: {}",
                        result.ratio,
                        result.aa_normal,
                        result.aaa_normal,
                        best.to_hex()
                    )
                },
            )
        }
        ColorCmd::Gradient { from, to, steps } => {
            let start = colorlib::parse(&from)?;
            let end = colorlib::parse(&to)?;
            let colors = colorlib::gradient(start, end, steps);
            let hex: Vec<String> = colors.iter().map(|color| color.to_hex()).collect();
            ctx.emit(&hex, || hex.join("  "))
        }
        ColorCmd::Palette {
            colors,
            name,
            format,
            output,
        } => {
            let mut palette = Vec::new();
            for (index, entry) in colors.iter().enumerate() {
                let (label, value) = entry
                    .split_once('=')
                    .map(|(name, value)| (name.to_string(), value))
                    .unwrap_or_else(|| (format!("color-{}", index + 1), entry.as_str()));
                palette.push(colorlib::PaletteColor {
                    name: label,
                    color: colorlib::parse(value)?,
                });
            }
            let rendered = match format.to_ascii_lowercase().as_str() {
                "json" => colorlib::palette_json(&name, &palette)?,
                "gpl" | "gimp" => colorlib::palette_gpl(&name, &palette),
                other => bail!("unknown palette format '{other}' (json or gpl)"),
            };
            if let Some(path) = output {
                formflux_core::io::write_text(&path, &rendered, ctx.force)?;
                ctx.path(&path)
            } else {
                ctx.text(&rendered)
            }
        }
    }
}

fn gen_cmd(cmd: GenCmd, ctx: &Context) -> anyhow::Result<()> {
    match cmd {
        GenCmd::Qr {
            data,
            output,
            size,
            border,
        } => {
            if ctx.dry_run {
                return ctx.text(&format!("would write QR to {}", output.display()));
            }
            generate::write_qr(&data, &output, size, border, ctx.force)?;
            ctx.path(&output)
        }
        GenCmd::Barcode {
            data,
            output,
            height,
        } => {
            if ctx.dry_run {
                return ctx.text(&format!("would write barcode to {}", output.display()));
            }
            if let Some(parent) = output.parent() {
                formflux_core::io::ensure_dir(parent)?;
            }
            generate::write_barcode(&data, &output, height, ctx.force)?;
            ctx.path(&output)
        }
        GenCmd::Uuid { version, count } => {
            if count == 0 || count > 1000 {
                bail!("count must be between 1 and 1000");
            }
            let values: Vec<String> = (0..count)
                .map(|_| match version {
                    4 => generate::uuid_v4(),
                    7 => generate::uuid_v7(),
                    other => format!("invalid-version-{other}"),
                })
                .collect();
            if version != 4 && version != 7 {
                bail!("UUID version must be 4 or 7");
            }
            ctx.emit(&values, || values.join("\n"))
        }
        GenCmd::Password {
            length,
            no_symbols,
            no_digits,
        } => {
            let options = generate::PasswordOptions {
                length,
                symbols: !no_symbols,
                digits: !no_digits,
                ..generate::PasswordOptions::default()
            };
            let password = generate::generate_password(&options)?;
            ctx.emit(&json!({ "password": password }), || password)
        }
        GenCmd::Strength { password } => {
            let strength = generate::password_strength(&password);
            ctx.emit(&strength, || {
                format!(
                    "score {}/4  guesses {}  {}{}",
                    strength.score,
                    strength.guesses,
                    strength.warning.clone().unwrap_or_default(),
                    if strength.suggestions.is_empty() {
                        String::new()
                    } else {
                        format!("\nsuggestions: {}", strength.suggestions.join("; "))
                    }
                )
            })
        }
    }
}

fn time_cmd(cmd: TimeCmd, ctx: &Context) -> anyhow::Result<()> {
    match cmd {
        TimeCmd::Now { unit, tz } => {
            let unit = timelib::TimeUnit::parse(&unit)?;
            let info = timelib::convert_timestamp(timelib::now(unit), unit, &tz, "iso8601")?;
            ctx.emit(&info, || {
                format!(
                    "{}\n{}: {}\nunix s/ms/us: {} / {} / {}",
                    info.formatted,
                    info.timezone,
                    info.rfc3339,
                    info.unix_seconds,
                    info.unix_milliseconds,
                    info.unix_microseconds
                )
            })
        }
        TimeCmd::Convert {
            value,
            unit,
            tz,
            format,
        } => {
            let unit = timelib::TimeUnit::parse(&unit)?;
            let info = timelib::convert_timestamp(value, unit, &tz, &format)?;
            ctx.emit(&info, || info.formatted.clone())
        }
        TimeCmd::Datetime {
            value,
            from,
            to,
            format,
        } => {
            let info = timelib::convert_datetime(&value, &from, &to, &format)?;
            ctx.emit(&info, || info.formatted.clone())
        }
        TimeCmd::Zones { filter } => {
            let zones = timelib::timezones(filter.as_deref());
            ctx.emit(&zones, || zones.join("\n"))
        }
    }
}

fn tools_cmd(cmd: ToolsCmd, ctx: &Context) -> anyhow::Result<()> {
    match cmd {
        ToolsCmd::Rename(args) => {
            let mode = rename_mode(&args)?;
            let plans = tools::plan_rename(&args.files, &mode, args.start, args.step)?;
            if ctx.dry_run {
                ctx.emit(&plans, || {
                    plans
                        .iter()
                        .map(|plan| {
                            format!("{} -> {}", plan.source.display(), plan.target.display())
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            } else {
                tools::execute_rename(&plans)?;
                ctx.emit(&plans, || format!("renamed {} file(s)", plans.len()))
            }
        }
        ToolsCmd::Dedupe {
            paths,
            algorithm,
            delete,
        } => {
            let files = expand_inputs(&paths, true)?;
            let algorithm = HashAlgorithm::parse(&algorithm)?;
            let progress = ctx.progress("dedupe");
            let report =
                tools::find_duplicates(&files, algorithm, &CancelToken::never(), &progress)?;
            if delete {
                if !ctx.yes {
                    bail!("--delete requires --yes");
                }
                if ctx.dry_run {
                    ctx.emit(&report, || {
                        format!("would remove {} duplicate(s)", report.redundant)
                    })?;
                    return Ok(());
                }
                let removed = tools::remove_duplicates(&report, false)?;
                ctx.emit(&json!({ "report": report, "removed": removed }), || {
                    format!("removed {} duplicate(s)", removed.len())
                })
            } else {
                ctx.emit(&report, || {
                    if report.groups.is_empty() {
                        "no duplicates".to_string()
                    } else {
                        report
                            .groups
                            .iter()
                            .map(|group| {
                                format!(
                                    "{} ({}) x{}",
                                    group.digest,
                                    formflux_core::format_bytes(group.size),
                                    group.files.len()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                })
            }
        }
        ToolsCmd::Split { file, size, output } => {
            let size = units::parse_bytes(&size)?;
            let progress = ctx.progress("split");
            let parts = formflux_core::io::split_file(
                &file,
                &output,
                size,
                &CancelToken::never(),
                &progress,
            )?;
            ctx.emit(&parts, || {
                parts
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        ToolsCmd::Merge { parts, output } => {
            let parts = expand_inputs(&parts, false)?;
            let progress = ctx.progress("merge");
            let bytes = formflux_core::io::merge_files(
                &parts,
                &output,
                ctx.force,
                &CancelToken::never(),
                &progress,
            )?;
            ctx.emit(&json!({ "output": output, "bytes": bytes }), || {
                format!("merged {bytes} bytes into {}", output.display())
            })
        }
        ToolsCmd::CleanEmpty { dir, apply } => {
            let removed = tools::clean_empty_dirs(&dir, ctx.dry_run || !apply)?;
            ctx.emit(&removed, || {
                if removed.is_empty() {
                    "no empty directories".to_string()
                } else {
                    removed
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            })
        }
        ToolsCmd::Units { value, from, to } => {
            let converted = formflux_core::measure::convert(value, &from, &to)?;
            let human = format!("{converted} {to}");
            ctx.emit(
                &json!({ "value": value, "from": from, "to": to, "result": converted }),
                || human,
            )
        }
        ToolsCmd::Info { path } => {
            let facts = formflux_core::io::file_facts(&path)?;
            ctx.emit(&facts, || {
                format!(
                    "{}\n{}  modified: {}  readonly: {}",
                    facts.path.display(),
                    formflux_core::format_bytes(facts.size),
                    facts
                        .modified_unix
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                    facts.readonly
                )
            })
        }
    }
}

fn rename_mode(args: &RenameArgs) -> anyhow::Result<RenameMode> {
    let mut modes = 0;
    if args.pattern.is_some() {
        modes += 1;
    }
    if args.prefix.is_some() {
        modes += 1;
    }
    if args.suffix.is_some() {
        modes += 1;
    }
    if args.regex.is_some() {
        modes += 1;
    }
    if args.exif_time.is_some() {
        modes += 1;
    }
    if modes != 1 {
        bail!("choose exactly one of --pattern, --prefix, --suffix, --regex or --exif-time");
    }
    Ok(if let Some(pattern) = &args.pattern {
        RenameMode::Pattern {
            pattern: pattern.clone(),
        }
    } else if let Some(prefix) = &args.prefix {
        RenameMode::Prefix(prefix.clone())
    } else if let Some(suffix) = &args.suffix {
        RenameMode::Suffix(suffix.clone())
    } else if let Some(regex) = &args.regex {
        RenameMode::Regex {
            pattern: regex.clone(),
            replacement: args.replacement.clone(),
        }
    } else {
        RenameMode::ExifTime {
            format: args
                .exif_time
                .clone()
                .unwrap_or_else(|| "%Y%m%d_%H%M%S".to_string()),
        }
    })
}

fn expand_inputs(paths: &[PathBuf], recursive: bool) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            files.extend(formflux_core::io::list_files(path, recursive, true)?);
        } else if path.is_file() {
            files.push(path.clone());
        } else {
            bail!("'{}' does not exist", path.display());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

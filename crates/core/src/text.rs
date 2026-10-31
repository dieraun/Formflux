//! Offline text, document, encoding and diff tools.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use formflux_shared::fs as sfs;
use pulldown_cmark::{html, Options, Parser};
use regex::Regex;
use similar::TextDiff;

use crate::error::{FormfluxError, Result};
use crate::io;

/// Text document formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    /// Markdown.
    Markdown,
    /// HTML fragment/document.
    Html,
    /// Plain text.
    Plain,
}

impl TextFormat {
    /// File extension.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Html => "html",
            Self::Plain => "txt",
        }
    }

    /// Parse a format name or extension.
    pub fn parse(value: &str) -> Result<Self> {
        match value
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "md" | "markdown" => Ok(Self::Markdown),
            "html" | "htm" => Ok(Self::Html),
            "txt" | "text" => Ok(Self::Plain),
            other => Err(FormfluxError::unsupported(format!(
                "unknown text format '{other}' (try md, html or txt)"
            ))),
        }
    }

    /// Detect from a path extension.
    pub fn from_path(path: &Path) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| FormfluxError::invalid("input has no extension"))?;
        Self::parse(extension)
    }
}

/// Character encodings supported for text IO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    /// UTF-8 (with or without BOM).
    Utf8,
    /// GBK / GB18030 (Chinese Windows legacy).
    Gbk,
    /// Latin-1 / ISO-8859-1.
    Latin1,
}

impl TextEncoding {
    /// Parse a user name.
    pub fn parse(value: &str) -> Result<Self> {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_'], "")
            .as_str()
        {
            "utf8" | "utf" => Ok(Self::Utf8),
            "gbk" | "gb18030" | "gb2312" | "cp936" => Ok(Self::Gbk),
            "latin1" | "iso88591" | "windows1252" | "cp1252" => Ok(Self::Latin1),
            other => Err(FormfluxError::unsupported(format!(
                "unsupported text encoding '{other}'"
            ))),
        }
    }

    /// Decode bytes, replacing invalid sequences.
    pub fn decode(self, bytes: &[u8]) -> String {
        match self {
            Self::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            Self::Gbk => encoding_rs::GBK.decode(bytes).0.into_owned(),
            Self::Latin1 => bytes.iter().map(|byte| *byte as char).collect(),
        }
    }

    /// Encode text.
    pub fn encode(self, text: &str) -> Result<Vec<u8>> {
        match self {
            Self::Utf8 => Ok(text.as_bytes().to_vec()),
            Self::Gbk => {
                let (bytes, _, had_errors) = encoding_rs::GBK.encode(text);
                if had_errors {
                    return Err(FormfluxError::invalid(
                        "text contains characters that GBK cannot represent",
                    ));
                }
                Ok(bytes.into_owned())
            }
            Self::Latin1 => {
                let mut bytes = Vec::with_capacity(text.len());
                for character in text.chars() {
                    let code = character as u32;
                    if code > 255 {
                        return Err(FormfluxError::invalid(
                            "text contains characters that Latin-1 cannot represent",
                        ));
                    }
                    bytes.push(code as u8);
                }
                Ok(bytes)
            }
        }
    }
}

/// Read a text file with an explicit encoding and a hard size limit.
pub fn read_text(path: &Path, encoding: TextEncoding, limit: u64) -> Result<String> {
    let bytes = io::read_bytes(path, limit)?;
    let text = encoding.decode(&bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Convert a text document to another format.
pub fn convert_document(
    input: &Path,
    output_dir: &Path,
    target: TextFormat,
    overwrite: bool,
) -> Result<PathBuf> {
    let input = io::existing_file(input)?;
    io::ensure_dir(output_dir)?;
    let source = TextFormat::from_path(&input)?;
    let text = read_text(&input, TextEncoding::Utf8, io::TEXT_LIMIT)?;
    let rendered = match (source, target) {
        (TextFormat::Markdown, TextFormat::Markdown)
        | (TextFormat::Html, TextFormat::Html)
        | (TextFormat::Plain, TextFormat::Plain) => text,
        (TextFormat::Markdown, TextFormat::Html) => markdown_to_html(&text),
        (TextFormat::Markdown, TextFormat::Plain) => markdown_to_text(&text),
        (TextFormat::Html, TextFormat::Markdown) => html_to_markdown(&text),
        (TextFormat::Html, TextFormat::Plain) => html_to_text(&text),
        (TextFormat::Plain, TextFormat::Markdown) => text_to_markdown(&text),
        (TextFormat::Plain, TextFormat::Html) => text_to_html(&text),
    };
    let output = sfs::destination_for(&input, output_dir, Some(target.extension()), overwrite)?;
    io::write_text(&output, &rendered, overwrite)?;
    Ok(output)
}

/// Re-encode a text file (for example GBK to UTF-8) into an explicit path.
pub fn transcode_file(
    input: &Path,
    output: &Path,
    from: TextEncoding,
    to: TextEncoding,
    overwrite: bool,
) -> Result<PathBuf> {
    let input = io::existing_file(input)?;
    let text = read_text(&input, from, io::TEXT_LIMIT)?;
    if input == output && from != to {
        return Err(FormfluxError::invalid(
            "refusing to transcode a file onto itself",
        ));
    }
    let bytes = to.encode(&text)?;
    io::write_bytes(output, &bytes, overwrite)?;
    Ok(output.to_path_buf())
}

/// Render Markdown to a complete HTML document.
pub fn markdown_to_html(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(markdown, options);
    let mut body = String::new();
    html::push_html(&mut body, parser);
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>Formflux document</title>\n</head>\n<body>\n{body}</body>\n</html>\n"
    )
}

/// Strip HTML tags to readable text.
pub fn html_to_text(html: &str) -> String {
    let scripts = Regex::new(r"(?is)<script\b[^>]*>.*?</script>|<style\b[^>]*>.*?</style>")
        .expect("static regex");
    let comments = Regex::new(r"(?s)<!--.*?-->").expect("static regex");
    let breaks =
        Regex::new(r"(?i)<br\s*/?>|</p>|</div>|</h[1-6]>|</li>|</tr>").expect("static regex");
    let tags = Regex::new(r"(?s)<[^>]+>").expect("static regex");
    let text = scripts.replace_all(html, "");
    let text = comments.replace_all(&text, "");
    let text = breaks.replace_all(&text, "\n");
    let text = tags.replace_all(&text, "");
    let text = decode_entities(&text);
    normalize_blank_lines(&text)
}

/// Very small HTML-to-Markdown converter (headings, links, emphasis, lists).
pub fn html_to_markdown(html: &str) -> String {
    let mut text = html_to_text(html);
    let headings = Regex::new(r"(?im)^\s*(.+)\s*$").expect("static regex");
    // Preserve paragraph structure rather than guessing heading levels; the
    // result is deterministic and valid Markdown.
    text = headings.replace_all(&text, "$1").into_owned();
    text
}

/// Convert Markdown to plain text by removing common markers.
pub fn markdown_to_text(markdown: &str) -> String {
    let links = Regex::new(r"\[([^\]]+)\]\([^)]+\)").expect("static regex");
    let images = Regex::new(r"!\[([^\]]*)\]\([^)]*\)").expect("static regex");
    let code = Regex::new(r"`{1,3}([^`]*)`{1,3}").expect("static regex");
    let emphasis = Regex::new(r"[*_~]{1,3}").expect("static regex");
    let headings = Regex::new(r"(?m)^\s{0,3}#{1,6}\s*").expect("static regex");
    let quotes = Regex::new(r"(?m)^\s{0,3}>\s?").expect("static regex");
    let text = images.replace_all(markdown, "$1");
    let text = links.replace_all(&text, "$1");
    let text = code.replace_all(&text, "$1");
    let text = emphasis.replace_all(&text, "");
    let text = headings.replace_all(&text, "");
    let text = quotes.replace_all(&text, "");
    normalize_blank_lines(&text)
}

/// Escape HTML special characters.
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Convert plain text into paragraphs of HTML.
pub fn text_to_html(text: &str) -> String {
    let body = text
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| format!("<p>{}</p>", escape_html(paragraph).replace('\n', "<br>")))
        .collect::<Vec<_>>()
        .join("\n");
    format!("<!doctype html>\n<html lang=\"en\">\n<head><meta charset=\"utf-8\"><title>Formflux document</title></head>\n<body>\n{body}\n</body>\n</html>\n")
}

/// Convert plain text into simple Markdown paragraphs.
pub fn text_to_markdown(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn normalize_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.replace("\r\n", "\n").lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank <= 1 {
                out.push('\n');
            }
        } else {
            blank = 0;
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    out.trim().to_string()
}

fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// Convert CSV to TSV or vice versa.
pub fn convert_delimited(
    input: &Path,
    output_dir: &Path,
    output_extension: &str,
    overwrite: bool,
) -> Result<PathBuf> {
    let input = io::existing_file(input)?;
    io::ensure_dir(output_dir)?;
    let from = input
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let to = output_extension
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let (from_delimiter, to_delimiter) = match (from.as_str(), to.as_str()) {
        ("csv", "tsv") => (b',', b'\t'),
        ("tsv", "csv") => (b'\t', b','),
        ("csv", "csv") => (b',', b','),
        ("tsv", "tsv") => (b'\t', b'\t'),
        _ => {
            return Err(FormfluxError::unsupported(format!(
                "cannot convert '{from}' to '{to}' (csv/tsv only)"
            )))
        }
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(from_delimiter)
        .flexible(true)
        .from_path(&input)
        .map_err(|error| FormfluxError::invalid(format!("could not read CSV: {error}")))?;
    let target = sfs::destination_for(&input, output_dir, Some(&to), overwrite)?;
    let atomic = sfs::AtomicWriter::new(&target, overwrite)?;
    let mut writer = csv::WriterBuilder::new()
        .delimiter(to_delimiter)
        .from_writer(atomic);
    for record in reader.records() {
        let record =
            record.map_err(|error| FormfluxError::invalid(format!("invalid CSV row: {error}")))?;
        writer
            .write_record(&record)
            .map_err(|error| FormfluxError::invalid(format!("could not write CSV: {error}")))?;
    }
    writer
        .flush()
        .map_err(|error| FormfluxError::Io(std::io::Error::new(error.kind(), error)))?;
    writer
        .into_inner()
        .map_err(|error| FormfluxError::Io(error.into_error()))?
        .finish()?;
    Ok(target)
}

/// Encode a string with one of the common codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Standard Base64 with padding.
    Base64,
    /// URL-safe Base64 without padding.
    Base64Url,
    /// RFC 4648 Base32.
    Base32,
    /// Percent-encode every non-alphanumeric byte.
    Url,
    /// Escape non-ASCII characters as `\u{...}`.
    Unicode,
}

impl Codec {
    /// Parse a codec name.
    pub fn parse(value: &str) -> Result<Self> {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_'], "")
            .as_str()
        {
            "base64" | "b64" => Ok(Self::Base64),
            "base64url" => Ok(Self::Base64Url),
            "base32" | "b32" => Ok(Self::Base32),
            "url" | "percent" | "urlencode" => Ok(Self::Url),
            "unicode" | "escape" => Ok(Self::Unicode),
            other => Err(FormfluxError::unsupported(format!(
                "unknown codec '{other}'"
            ))),
        }
    }

    /// Encode text.
    pub fn encode(self, text: &str) -> String {
        match self {
            Self::Base64 => base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
            Self::Base64Url => {
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes())
            }
            Self::Base32 => data_encoding::BASE32.encode(text.as_bytes()),
            Self::Url => {
                percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
            }
            Self::Unicode => unicode_escape(text),
        }
    }

    /// Decode text.
    pub fn decode(self, text: &str) -> Result<String> {
        match self {
            Self::Base64 => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(text.trim())
                    .or_else(|_| {
                        base64::engine::general_purpose::STANDARD_NO_PAD.decode(text.trim())
                    })
                    .map_err(|error| FormfluxError::invalid(format!("invalid Base64: {error}")))?;
                String::from_utf8(bytes).map_err(|error| {
                    FormfluxError::invalid(format!("Base64 is not UTF-8: {error}"))
                })
            }
            Self::Base64Url => {
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(text.trim())
                    .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(text.trim()))
                    .map_err(|error| {
                        FormfluxError::invalid(format!("invalid Base64URL: {error}"))
                    })?;
                String::from_utf8(bytes).map_err(|error| {
                    FormfluxError::invalid(format!("Base64URL is not UTF-8: {error}"))
                })
            }
            Self::Base32 => {
                let bytes = data_encoding::BASE32
                    .decode(text.trim().to_ascii_uppercase().as_bytes())
                    .map_err(|error| FormfluxError::invalid(format!("invalid Base32: {error}")))?;
                String::from_utf8(bytes).map_err(|error| {
                    FormfluxError::invalid(format!("Base32 is not UTF-8: {error}"))
                })
            }
            Self::Url => percent_encoding::percent_decode_str(text.trim())
                .decode_utf8()
                .map(|value| value.into_owned())
                .map_err(|error| FormfluxError::invalid(format!("invalid URL encoding: {error}"))),
            Self::Unicode => unicode_unescape(text),
        }
    }
}

/// Escape non-ASCII characters as `\u{XXXX}`.
pub fn unicode_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii() {
            out.push(character);
        } else {
            out.push_str(&format!("\\u{{{:X}}}", character as u32));
        }
    }
    out
}

/// Decode `\u{XXXX}` and `\uXXXX` escapes.
pub fn unicode_unescape(text: &str) -> Result<String> {
    let regex = Regex::new(r"\\u\{([0-9a-fA-F]{1,6})\}|\\u([0-9a-fA-F]{4})").expect("static regex");
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for capture in regex.captures_iter(text) {
        let whole = capture.get(0).expect("whole match");
        out.push_str(&text[last..whole.start()]);
        let hex = capture
            .get(1)
            .or_else(|| capture.get(2))
            .expect("one capture")
            .as_str();
        let value = u32::from_str_radix(hex, 16)
            .map_err(|error| FormfluxError::invalid(format!("invalid unicode escape: {error}")))?;
        let character = char::from_u32(value)
            .ok_or_else(|| FormfluxError::invalid(format!("invalid code point U+{value:X}")))?;
        out.push(character);
        last = whole.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// Unified diff between two strings.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffResult {
    /// Unified diff text.
    pub unified: String,
    /// Number of changed lines.
    pub changes: usize,
}

/// Compute a line-oriented unified diff.
pub fn diff(old: &str, new: &str, context: usize) -> DiffResult {
    let diff = TextDiff::from_lines(old, new);
    let unified = diff
        .unified_diff()
        .context_radius(context)
        .header("old", "new")
        .to_string();
    let changes = diff
        .iter_all_changes()
        .filter(|change| change.tag() != similar::ChangeTag::Equal)
        .count();
    DiffResult { unified, changes }
}

/// Replace with a regular expression.  Replacement supports `$1` groups.
pub fn regex_replace(input: &str, pattern: &str, replacement: &str, all: bool) -> Result<String> {
    let regex = Regex::new(pattern)
        .map_err(|error| FormfluxError::invalid(format!("invalid regex: {error}")))?;
    Ok(if all {
        regex.replace_all(input, replacement).into_owned()
    } else {
        regex.replace(input, replacement).into_owned()
    })
}

/// Collapse runs of whitespace and trim each line.
pub fn normalize_whitespace(input: &str) -> String {
    let whitespace = Regex::new(r"[ \t]+").expect("static regex");
    input
        .replace("\r\n", "\n")
        .lines()
        .map(|line| whitespace.replace_all(line.trim(), " ").into_owned())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Case conversion styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseStyle {
    /// UPPERCASE.
    Upper,
    /// lowercase.
    Lower,
    /// Title Case.
    Title,
    /// snake_case.
    Snake,
    /// kebab-case.
    Kebab,
    /// camelCase.
    Camel,
}

impl CaseStyle {
    /// Parse a style name.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "upper" | "uppercase" => Ok(Self::Upper),
            "lower" | "lowercase" => Ok(Self::Lower),
            "title" => Ok(Self::Title),
            "snake" | "snakecase" => Ok(Self::Snake),
            "kebab" | "kebabcase" | "dash" => Ok(Self::Kebab),
            "camel" | "camelcase" => Ok(Self::Camel),
            other => Err(FormfluxError::invalid(format!(
                "unknown case style '{other}'"
            ))),
        }
    }

    /// Apply the style.
    pub fn apply(self, input: &str) -> String {
        match self {
            Self::Upper => input.to_uppercase(),
            Self::Lower => input.to_lowercase(),
            Self::Title => input
                .split_whitespace()
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        Some(first) => {
                            first.to_uppercase().collect::<String>()
                                + &chars.as_str().to_lowercase()
                        }
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            Self::Snake | Self::Kebab => {
                let separator = if self == Self::Snake { '_' } else { '-' };
                let words = split_words(input);
                words.join(&separator.to_string()).to_lowercase()
            }
            Self::Camel => {
                let words = split_words(input);
                let mut out = String::new();
                for (index, word) in words.iter().enumerate() {
                    let mut chars = word.chars();
                    let Some(first) = chars.next() else { continue };
                    if index == 0 {
                        out.push(first.to_ascii_lowercase());
                    } else {
                        out.push(first.to_ascii_uppercase());
                    }
                    out.push_str(&chars.as_str().to_ascii_lowercase());
                }
                out
            }
        }
    }
}

fn split_words(input: &str) -> Vec<String> {
    let regex = Regex::new(r"[^A-Za-z0-9]+").expect("static regex");
    regex
        .split(input)
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn markdown_renders_html() {
        let html = markdown_to_html("# Hello\n\n**bold** and `code`");
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn html_text_strips_scripts() {
        let text = html_to_text("<p>Hello <b>world</b></p><script>alert(1)</script>");
        assert!(text.contains("Hello world"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn codecs_round_trip() {
        for codec in [
            Codec::Base64,
            Codec::Base64Url,
            Codec::Base32,
            Codec::Url,
            Codec::Unicode,
        ] {
            let encoded = codec.encode("héllo 世界");
            assert_eq!(codec.decode(&encoded).unwrap(), "héllo 世界", "{codec:?}");
        }
    }

    #[test]
    fn unicode_escape_is_correct() {
        assert_eq!(unicode_escape("aé"), "a\\u{E9}");
        assert_eq!(unicode_unescape("a\\u{E9}").unwrap(), "aé");
        assert_eq!(unicode_unescape("\\u0041").unwrap(), "A");
    }

    #[test]
    fn diff_counts_changes() {
        let result = diff("a\nb\n", "a\nc\n", 3);
        assert!(result.changes >= 2);
        assert!(result.unified.contains("--- old"));
        assert!(result.unified.contains("+++ new"));
    }

    #[test]
    fn regex_and_case_tools() {
        assert_eq!(regex_replace("a1 b2", r"\d", "x", true).unwrap(), "ax bx");
        assert_eq!(CaseStyle::Snake.apply("Hello World"), "hello_world");
        assert_eq!(CaseStyle::Camel.apply("hello_world"), "helloWorld");
        assert_eq!(CaseStyle::Kebab.apply("Hello World"), "hello-world");
    }

    #[test]
    fn whitespace_is_normalized() {
        assert_eq!(normalize_whitespace("  a   b  \n c "), "a b\nc");
    }

    #[test]
    fn document_conversion_writes_html() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("note.md");
        fs::write(&input, "# Title\n\nBody").unwrap();
        let output = convert_document(&input, dir.path(), TextFormat::Html, false).unwrap();
        assert!(output.exists());
        assert!(fs::read_to_string(output)
            .unwrap()
            .contains("<h1>Title</h1>"));
    }

    #[test]
    fn gbk_encoding_round_trip() {
        let text = "你好";
        let bytes = TextEncoding::Gbk.encode(text).unwrap();
        assert_eq!(TextEncoding::Gbk.decode(&bytes), text);
        assert!(TextEncoding::Latin1.encode("世界").is_err());
    }

    #[test]
    fn csv_to_tsv() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("data.csv");
        fs::write(&input, "name,age\nalice,30\n").unwrap();
        let output = convert_delimited(&input, dir.path(), "tsv", false).unwrap();
        assert!(fs::read_to_string(output).unwrap().contains('\t'));
    }
}

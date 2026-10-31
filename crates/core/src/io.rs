//! The only layer that performs file IO.
//!
//! Tool modules call these helpers instead of touching [`std::fs`] directly so
//! that streaming, cancellation, byte limits and atomic replacement stay
//! consistent across every feature.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use formflux_shared::fs::{self as sfs, AtomicWriter};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::cancel::CancelToken;
use crate::error::{FormfluxError, Result};
use crate::progress::{ProgressEvent, ProgressSink};

/// Default maximum for text/config inputs loaded into memory (64 MiB).
pub const TEXT_LIMIT: u64 = 64 * 1024 * 1024;
/// Copy buffer size used by streaming operations (4 MiB).
pub const COPY_BUFFER: usize = 4 * 1024 * 1024;

/// Read a whole file after checking a hard byte limit.
pub fn read_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    sfs::read_bytes_limited(path, limit)
}

/// Read a UTF-8 file (BOM stripped) after checking a hard byte limit.
pub fn read_text(path: &Path, limit: u64) -> Result<String> {
    sfs::read_text_limited(path, limit)
}

/// Read stdin up to `limit` bytes.
pub fn read_stdin(limit: u64) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    let read = io::stdin()
        .lock()
        .take(limit + 1)
        .read_to_end(&mut buffer)?;
    if read as u64 > limit {
        return Err(FormfluxError::Limit(format!(
            "stdin exceeded the limit of {} bytes",
            crate::format_bytes(limit)
        )));
    }
    Ok(buffer)
}

/// Atomically write bytes, refusing to clobber unless `overwrite`.
pub fn write_bytes(path: &Path, bytes: &[u8], overwrite: bool) -> Result<PathBuf> {
    let mut writer = AtomicWriter::new(path, overwrite)?;
    writer.write_all(bytes)?;
    writer.finish()
}

/// Atomically write UTF-8 text.
pub fn write_text(path: &Path, text: &str, overwrite: bool) -> Result<PathBuf> {
    write_bytes(path, text.as_bytes(), overwrite)
}

/// Serialize a value as pretty JSON and write it atomically.
pub fn write_json<T: Serialize>(path: &Path, value: &T, overwrite: bool) -> Result<PathBuf> {
    let text = serde_json::to_string_pretty(value)?;
    write_text(path, &(text + "\n"), overwrite)
}

/// Read and deserialize a JSON file.
pub fn read_json<T: DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let text = read_text(path, limit)?;
    Ok(serde_json::from_str(&text)?)
}

/// Stream a source file into `target`, reporting byte progress and honouring
/// cancellation.  The source is opened read-only; the target is atomic.
pub fn copy_with_progress(
    source: &Path,
    target: &Path,
    overwrite: bool,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<u64> {
    let source = sfs::existing_file(source)?;
    let total = source.metadata()?.len();
    let input = File::open(&source)?;
    let mut reader = BufReader::with_capacity(COPY_BUFFER, input);
    let mut writer = BufWriter::with_capacity(COPY_BUFFER, AtomicWriter::new(target, overwrite)?);
    let started = Instant::now();
    let mut copied = 0u64;
    let mut buffer = vec![0u8; COPY_BUFFER];
    let mut last_report = Instant::now() - Duration::from_secs(1);
    loop {
        cancel.check()?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        copied += read as u64;
        if last_report.elapsed() >= Duration::from_millis(100) || copied == total {
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            let percent = if total == 0 {
                100.0
            } else {
                copied as f64 / total as f64 * 100.0
            };
            let speed = copied as f64 / elapsed;
            let eta = if speed > 0.0 && total > copied {
                Some((total - copied) as f64 / speed)
            } else {
                Some(0.0)
            };
            progress.report(
                ProgressEvent::new("copy", percent)
                    .with_message(format!(
                        "{} / {}",
                        crate::format_bytes(copied),
                        crate::format_bytes(total)
                    ))
                    .with_bytes(copied, Some(total))
                    .with_speed(format!("{}/s", crate::format_bytes(speed as u64)))
                    .with_eta(eta),
            );
            last_report = Instant::now();
        }
    }
    writer.flush()?;
    let atomic = writer
        .into_inner()
        .map_err(|e| FormfluxError::Io(e.into_error()))?;
    atomic.finish()?;
    progress.report(
        ProgressEvent::new("copy", 100.0)
            .with_message(format!("copied {}", crate::format_bytes(copied)))
            .with_bytes(copied, Some(total)),
    );
    Ok(copied)
}

/// Copy a file without progress reporting.
pub fn copy_file(source: &Path, target: &Path, overwrite: bool) -> Result<u64> {
    sfs::atomic_copy(source, target, overwrite)
}

/// Ensure a directory exists.
pub fn ensure_dir(path: &Path) -> Result<()> {
    sfs::ensure_dir(path)
}

/// Resolve an existing regular file.
pub fn existing_file(path: &Path) -> Result<PathBuf> {
    sfs::existing_file(path)
}

/// List regular files under a root.
pub fn list_files(root: &Path, recursive: bool, include_hidden: bool) -> Result<Vec<PathBuf>> {
    sfs::collect_files(root, recursive, include_hidden)
}

/// Load a UTF-8 text document, decoding GBK fallback only when requested.
pub fn read_text_with_encoding(path: &Path, limit: u64, fallback_gbk: bool) -> Result<String> {
    let bytes = read_bytes(path, limit)?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()),
        Err(error) if fallback_gbk => {
            let (decoded, _, had_errors) = encoding_rs::GBK.decode(error.as_bytes());
            if had_errors {
                return Err(FormfluxError::invalid(format!(
                    "'{}' is neither UTF-8 nor valid GBK",
                    path.display()
                )));
            }
            Ok(decoded.into_owned())
        }
        Err(error) => Err(FormfluxError::invalid(format!(
            "'{}' is not valid UTF-8: {error}",
            path.display()
        ))),
    }
}

/// Metadata summary used by the file toolbox and previews.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileFacts {
    /// Canonical path.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Last modification as Unix seconds, if available.
    pub modified_unix: Option<i64>,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Read-only bit.
    pub readonly: bool,
}

/// Read metadata for a path without following it into a directory tree.
pub fn file_facts(path: &Path) -> Result<FileFacts> {
    let metadata = fs::metadata(path)?;
    let modified_unix = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64);
    Ok(FileFacts {
        path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        size: metadata.len(),
        modified_unix,
        is_dir: metadata.is_dir(),
        readonly: metadata.permissions().readonly(),
    })
}

/// Split a large file into numbered chunks in `output_dir`.
///
/// Returns the chunk paths.  Chunks are written atomically and the source is
/// never modified.
pub fn split_file(
    source: &Path,
    output_dir: &Path,
    chunk_size: u64,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<Vec<PathBuf>> {
    if chunk_size == 0 {
        return Err(FormfluxError::invalid("chunk size must be positive"));
    }
    let source = existing_file(source)?;
    ensure_dir(output_dir)?;
    let total = source.metadata()?.len();
    let stem = sfs::safe_file_name(&source)?;
    let mut input = BufReader::with_capacity(COPY_BUFFER, File::open(&source)?);
    let mut outputs = Vec::new();
    let mut index = 0u32;
    let mut written_total = 0u64;
    loop {
        cancel.check()?;
        let target = output_dir.join(format!("{stem}.{index:03}"));
        let mut writer = AtomicWriter::new(&target, false)?;
        let mut written = 0u64;
        let mut buffer = vec![0u8; 1024 * 1024];
        while written < chunk_size {
            let want = ((chunk_size - written) as usize).min(buffer.len());
            let read = input.read(&mut buffer[..want])?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
            written += read as u64;
        }
        if written == 0 {
            break;
        }
        writer.finish()?;
        outputs.push(target);
        written_total += written;
        progress.report(
            ProgressEvent::new("split", written_total as f64 / total.max(1) as f64 * 100.0)
                .with_bytes(written_total, Some(total)),
        );
        index += 1;
        if written < chunk_size {
            break;
        }
    }
    Ok(outputs)
}

/// Merge chunk files (sorted by name) into one atomic output.
pub fn merge_files(
    parts: &[PathBuf],
    target: &Path,
    overwrite: bool,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<u64> {
    if parts.is_empty() {
        return Err(FormfluxError::invalid("no input parts to merge"));
    }
    let total: u64 = parts
        .iter()
        .map(|part| part.metadata().map(|m| m.len()).unwrap_or(0))
        .sum();
    let mut writer = BufWriter::with_capacity(COPY_BUFFER, AtomicWriter::new(target, overwrite)?);
    let mut written = 0u64;
    for part in parts {
        let mut reader = BufReader::with_capacity(COPY_BUFFER, File::open(part)?);
        let mut buffer = vec![0u8; COPY_BUFFER];
        loop {
            cancel.check()?;
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
            written += read as u64;
            if total > 0 {
                progress.report(
                    ProgressEvent::new("merge", written as f64 / total as f64 * 100.0)
                        .with_bytes(written, Some(total)),
                );
            }
        }
    }
    writer.flush()?;
    let atomic = writer
        .into_inner()
        .map_err(|e| FormfluxError::Io(e.into_error()))?;
    atomic.finish()?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::NoopProgress;
    use std::sync::Arc;

    #[test]
    fn copy_reports_and_preserves_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        fs::write(&source, vec![9u8; 100_000]).unwrap();
        let target = dir.path().join("target.bin");
        let bytes = copy_with_progress(&source, &target, false, &CancelToken::new(), &NoopProgress)
            .unwrap();
        assert_eq!(bytes, 100_000);
        assert_eq!(fs::read(&target).unwrap(), fs::read(&source).unwrap());
    }

    #[test]
    fn cancelled_copy_leaves_no_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        fs::write(&source, vec![9u8; 100_000]).unwrap();
        let target = dir.path().join("target.bin");
        let token = CancelToken::new();
        token.cancel();
        let err = copy_with_progress(&source, &target, false, &token, &NoopProgress).unwrap_err();
        assert!(err.is_cancelled());
        assert!(!target.exists());
    }

    #[test]
    fn split_and_merge_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("big.bin");
        fs::write(
            &source,
            (0..=255u8).cycle().take(10_000).collect::<Vec<_>>(),
        )
        .unwrap();
        let parts = split_file(
            &source,
            dir.path(),
            4096,
            &CancelToken::new(),
            &NoopProgress,
        )
        .unwrap();
        assert_eq!(parts.len(), 3);
        let merged = dir.path().join("merged.bin");
        merge_files(&parts, &merged, false, &CancelToken::new(), &NoopProgress).unwrap();
        assert_eq!(fs::read(&merged).unwrap(), fs::read(&source).unwrap());
    }

    #[test]
    fn file_facts_reports_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"hello").unwrap();
        assert_eq!(file_facts(&path).unwrap().size, 5);
    }

    #[test]
    fn progress_sink_receives_completion() {
        use std::sync::Mutex;
        #[derive(Default)]
        struct Sink(Mutex<Vec<f64>>);
        impl ProgressSink for Sink {
            fn report(&self, event: ProgressEvent) {
                self.0.lock().unwrap().push(event.percent);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        fs::write(&source, vec![1u8; 10]).unwrap();
        let sink = Arc::new(Sink::default());
        copy_with_progress(
            &source,
            &dir.path().join("b.bin"),
            false,
            &CancelToken::new(),
            sink.as_ref(),
        )
        .unwrap();
        assert_eq!(sink.0.lock().unwrap().last().copied(), Some(100.0));
    }
}

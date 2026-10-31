//! Path safety and durable file output.
//!
//! Formflux never trusts a path that came from a queue file, a dropped folder
//! or an archive.  [`safe_join`] confines a user-supplied relative path to a
//! base directory, following symlinks while resolving so a link cannot be used
//! to escape.  [`AtomicWriter`] writes to a sibling temporary file and renames
//! it only after a successful flush + fsync, so a crash never leaves a
//! half-written deliverable behind.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};

use tempfile::NamedTempFile;

use crate::error::{FormfluxError, Result};

/// Largest config/text file Formflux will read into memory by default (8 MiB).
pub const DEFAULT_TEXT_LIMIT: u64 = 8 * 1024 * 1024;

/// The prefix used for sibling temporary files.
const TEMP_PREFIX: &str = ".formflux-";

/// Resolve `base` to an existing canonical directory.
fn canonical_base(base: &Path) -> Result<PathBuf> {
    if !base.exists() {
        return Err(FormfluxError::NotFound(base.to_path_buf()));
    }
    let canonical = base.canonicalize().map_err(|e| FormfluxError::UnsafePath {
        path: base.to_path_buf(),
        reason: format!("cannot resolve base directory: {e}"),
    })?;
    if !canonical.is_dir() {
        return Err(FormfluxError::UnsafePath {
            path: base.to_path_buf(),
            reason: "base is not a directory".to_string(),
        });
    }
    Ok(canonical)
}

/// Join an untrusted relative path onto `base`, rejecting traversal and
/// symlink escapes.
///
/// Absolute paths, Windows prefixes, `..` components and NUL bytes are
/// rejected. `.` components are ignored.  The base directory must already
/// exist; the joined target does not have to.
pub fn safe_join(base: &Path, untrusted: &Path) -> Result<PathBuf> {
    let base_canon = canonical_base(base)?;
    if untrusted.as_os_str().is_empty() {
        return Err(FormfluxError::UnsafePath {
            path: untrusted.to_path_buf(),
            reason: "empty path".to_string(),
        });
    }
    if untrusted.is_absolute() {
        return Err(FormfluxError::UnsafePath {
            path: untrusted.to_path_buf(),
            reason: "absolute paths are not allowed here".to_string(),
        });
    }

    let mut relative = PathBuf::new();
    for component in untrusted.components() {
        match component {
            Component::Normal(part) => {
                if part.to_string_lossy().contains('\0') {
                    return Err(FormfluxError::UnsafePath {
                        path: untrusted.to_path_buf(),
                        reason: "NUL byte in path".to_string(),
                    });
                }
                relative.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(FormfluxError::UnsafePath {
                    path: untrusted.to_path_buf(),
                    reason: "path traversal component is not allowed".to_string(),
                });
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Ok(base_canon);
    }

    let candidate = base_canon.join(&relative);
    canonical_checked(&base_canon, &candidate, untrusted)
}

/// Canonicalize the longest existing prefix of `candidate` and prove that it
/// stays inside `base`, then append the still-missing tail literally.
fn canonical_checked(base: &Path, candidate: &Path, original: &Path) -> Result<PathBuf> {
    let mut probe = candidate.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !probe.exists() {
        let name = probe.file_name().ok_or_else(|| FormfluxError::UnsafePath {
            path: original.to_path_buf(),
            reason: "could not find an existing ancestor".to_string(),
        })?;
        tail.push(name.to_os_string());
        probe = probe
            .parent()
            .ok_or_else(|| FormfluxError::UnsafePath {
                path: original.to_path_buf(),
                reason: "could not find an existing ancestor".to_string(),
            })?
            .to_path_buf();
    }
    let resolved = probe
        .canonicalize()
        .map_err(|e| FormfluxError::UnsafePath {
            path: original.to_path_buf(),
            reason: format!("cannot resolve path: {e}"),
        })?;
    if !resolved.starts_with(base) {
        return Err(FormfluxError::UnsafePath {
            path: original.to_path_buf(),
            reason: "resolved path escapes the base directory".to_string(),
        });
    }
    let mut result = resolved;
    for name in tail.into_iter().rev() {
        result.push(name);
    }
    Ok(result)
}

/// True when `path` is lexically/canonically inside `base`.
pub fn is_within(base: &Path, path: &Path) -> bool {
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(base)
}

/// Create a directory and all parents, mapping errors with context.
pub fn ensure_dir(path: &Path) -> Result<()> {
    if path.exists() {
        if path.is_dir() {
            return Ok(());
        }
        return Err(FormfluxError::invalid(format!(
            "'{}' exists and is not a directory",
            path.display()
        )));
    }
    fs::create_dir_all(path).map_err(|e| {
        FormfluxError::invalid(format!("cannot create directory '{}': {e}", path.display()))
    })
}

/// Ensure `path` names an existing regular file and return its canonical form.
pub fn existing_file(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        return Err(FormfluxError::NotFound(path.to_path_buf()));
    }
    let canonical = path.canonicalize()?;
    if !canonical.is_file() {
        return Err(FormfluxError::invalid(format!(
            "'{}' is not a regular file",
            path.display()
        )));
    }
    Ok(canonical)
}

/// Validate a file name for portability across Windows, macOS and Linux.
///
/// Rejects separators, control characters, Windows-reserved device names and
/// names that end in a dot or space.
pub fn validate_portable_filename(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(FormfluxError::invalid(
            "file name may not be empty, '.' or '..'",
        ));
    }
    if name.chars().count() > 240 {
        return Err(FormfluxError::invalid(
            "file name is longer than 240 characters",
        ));
    }
    if name.chars().any(|c| {
        matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control()
    }) {
        return Err(FormfluxError::invalid(format!(
            "file name '{name}' contains a reserved character"
        )));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(FormfluxError::invalid(format!(
            "file name '{name}' may not end with a dot or space"
        )));
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err(FormfluxError::invalid(format!(
            "file name '{name}' is reserved on Windows"
        )));
    }
    Ok(())
}

/// Extract and validate just the file name from an input path.
pub fn safe_file_name(path: &Path) -> Result<String> {
    let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        FormfluxError::invalid(format!("'{}' has no usable file name", path.display()))
    })?;
    validate_portable_filename(name)?;
    Ok(name.to_string())
}

/// Choose a non-existing sibling path by appending ` (1)`, ` (2)`, ...
///
/// The original path is returned unchanged when it does not exist.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("output")
        .to_string();
    let ext = path.extension().and_then(OsStr::to_str);
    for index in 1..=9999u32 {
        let name = match ext {
            Some(ext) => format!("{stem} ({index}).{ext}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem}-{}", std::process::id()))
}

/// Build the output path for converting `input` into `output_dir`.
///
/// The source file is never selected as an output, even with `overwrite`.
/// When `overwrite` is false an existing target is an [`FormfluxError::AlreadyExists`].
pub fn destination_for(
    input: &Path,
    output_dir: &Path,
    extension: Option<&str>,
    overwrite: bool,
) -> Result<PathBuf> {
    let input = existing_file(input)?;
    ensure_dir(output_dir)?;
    let name = safe_file_name(&input)?;
    let mut target = output_dir.join(name);
    if let Some(extension) = extension {
        let extension = extension.trim_start_matches('.');
        if extension.is_empty() {
            return Err(FormfluxError::invalid("output extension may not be empty"));
        }
        validate_portable_filename(&format!("x.{extension}"))?;
        target.set_extension(extension);
    }
    let mut target = safe_join(
        output_dir,
        Path::new(
            target
                .file_name()
                .ok_or_else(|| FormfluxError::invalid("output has no file name"))?,
        ),
    )?;
    if target == input {
        // Converting in place (for example PNG -> PNG with a resize) must not
        // overwrite the read-only source.  Pick the next free sibling instead.
        target = unique_path(&target);
    } else if target.exists() && !overwrite {
        return Err(FormfluxError::AlreadyExists(target));
    }
    Ok(target)
}

/// An atomic file writer.  Content goes to a temporary sibling and is renamed
/// into place by [`AtomicWriter::finish`].
pub struct AtomicWriter {
    target: PathBuf,
    overwrite: bool,
    temp: Option<NamedTempFile>,
}

impl AtomicWriter {
    /// Create a writer for `target`, creating parent directories as needed.
    pub fn new(target: impl AsRef<Path>, overwrite: bool) -> Result<Self> {
        let target = target.as_ref().to_path_buf();
        if let Some(parent) = target.parent() {
            ensure_dir(parent)?;
        }
        if target.exists() && !overwrite {
            return Err(FormfluxError::AlreadyExists(target));
        }
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let temp = NamedTempFile::with_prefix_in(TEMP_PREFIX, parent)?;
        Ok(Self {
            target,
            overwrite,
            temp: Some(temp),
        })
    }

    /// Final destination.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Flush, fsync and atomically publish the file.
    pub fn finish(mut self) -> Result<PathBuf> {
        let mut temp = self
            .temp
            .take()
            .ok_or_else(|| FormfluxError::internal("atomic writer already finished"))?;
        temp.as_file_mut().flush()?;
        temp.as_file().sync_all()?;
        let result = if self.overwrite {
            temp.persist(&self.target)
        } else {
            temp.persist_noclobber(&self.target)
        };
        match result {
            Ok(_) => {
                sync_parent(&self.target);
                Ok(self.target)
            }
            Err(error) => {
                if error.error.kind() == io::ErrorKind::AlreadyExists {
                    Err(FormfluxError::AlreadyExists(self.target))
                } else {
                    Err(FormfluxError::Io(error.error))
                }
            }
        }
    }
}

impl Write for AtomicWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.temp.as_mut() {
            Some(temp) => temp.write(buf),
            None => Err(io::Error::other("atomic writer already finished")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.temp.as_mut() {
            Some(temp) => temp.flush(),
            None => Err(io::Error::other("atomic writer already finished")),
        }
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}

/// Atomically write `bytes` to `path`.
pub fn atomic_write(path: impl AsRef<Path>, bytes: &[u8], overwrite: bool) -> Result<()> {
    let mut writer = AtomicWriter::new(path, overwrite)?;
    writer.write_all(bytes)?;
    writer.finish()?;
    Ok(())
}

/// Atomically write UTF-8 text to `path`.
pub fn atomic_write_text(path: impl AsRef<Path>, text: &str, overwrite: bool) -> Result<()> {
    atomic_write(path, text.as_bytes(), overwrite)
}

/// Stream-copy `source` to `target` through a sibling temp file.
///
/// Existing targets are protected unless `overwrite` is set. The source is
/// opened read-only and never modified.
pub fn atomic_copy(source: &Path, target: &Path, overwrite: bool) -> Result<u64> {
    let input = existing_file(source)?;
    let mut reader = BufReader::with_capacity(1024 * 1024, File::open(&input)?);
    let mut writer = BufWriter::with_capacity(1024 * 1024, AtomicWriter::new(target, overwrite)?);
    let bytes = io::copy(&mut reader, &mut writer)?;
    writer.flush()?;
    let atomic = writer
        .into_inner()
        .map_err(|e| FormfluxError::Io(e.into_error()))?;
    atomic.finish()?;
    Ok(bytes)
}

/// Read at most `limit` bytes; larger files produce an error instead of an
/// unbounded allocation.
pub fn read_bytes_limited(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > limit {
        return Err(FormfluxError::Limit(format!(
            "'{}' is {} bytes, above the {} byte limit",
            path.display(),
            metadata.len(),
            limit
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut limited = file.take(limit + 1);
    limited.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(FormfluxError::Limit(format!(
            "'{}' grew past the {} byte limit while reading",
            path.display(),
            limit
        )));
    }
    Ok(bytes)
}

/// Read a UTF-8 text file with a byte limit, stripping a leading BOM.
pub fn read_text_limited(path: &Path, limit: u64) -> Result<String> {
    let bytes = read_bytes_limited(path, limit)?;
    let text = String::from_utf8(bytes).map_err(|e| {
        FormfluxError::invalid(format!("'{}' is not valid UTF-8: {e}", path.display()))
    })?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Open a file for writing, refusing to clobber unless `overwrite` is set.
pub fn create_output(path: &Path, overwrite: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true);
    if overwrite {
        options.truncate(true);
    } else {
        options.create_new(true);
    }
    Ok(options.open(path)?)
}

/// Recursively collect regular files under `root`.
///
/// Symbolic links are not followed, hidden files are skipped unless
/// `include_hidden`, and traversal depth is capped to keep pathological trees
/// from consuming unlimited time.
pub fn collect_files(root: &Path, recursive: bool, include_hidden: bool) -> Result<Vec<PathBuf>> {
    let root = existing_file(root).or_else(|_| {
        if root.is_dir() {
            Ok(root.to_path_buf())
        } else {
            Err(FormfluxError::NotFound(root.to_path_buf()))
        }
    })?;
    if root.is_file() {
        return Ok(vec![root]);
    }
    let mut files = Vec::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 64 {
            return Err(FormfluxError::Limit(format!(
                "directory nesting exceeded 64 levels under '{}'",
                dir.display()
            )));
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            let name = entry.file_name();
            if !include_hidden && name.to_string_lossy().starts_with('.') {
                continue;
            }
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if recursive {
                    stack.push((path, depth + 1));
                }
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// File size in bytes.
pub fn file_size(path: &Path) -> Result<u64> {
    Ok(existing_file(path)?.metadata()?.len())
}

/// Lossy display helper that never fails.
pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_accepts_normal_nested_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = safe_join(dir.path(), Path::new("a/b/c.txt")).unwrap();
        assert!(path.starts_with(dir.path().canonicalize().unwrap()));
        assert!(path.ends_with("c.txt"));
    }

    #[test]
    fn safe_join_rejects_traversal_and_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        assert!(safe_join(dir.path(), Path::new("../escape")).is_err());
        assert!(safe_join(dir.path(), Path::new("a/../../escape")).is_err());
        assert!(safe_join(dir.path(), Path::new("/etc/passwd")).is_err());
        assert!(safe_join(dir.path(), Path::new("")).is_err());
    }

    #[test]
    fn atomic_write_does_not_clobber_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.bin");
        atomic_write(&path, b"one", false).unwrap();
        let err = atomic_write(&path, b"two", false).unwrap_err();
        assert!(err.is_already_exists());
        assert_eq!(fs::read(&path).unwrap(), b"one");
        atomic_write(&path, b"two", true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn atomic_copy_streams_and_protects_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.bin");
        fs::write(&source, vec![7u8; 4096]).unwrap();
        let target = dir.path().join("out.bin");
        assert_eq!(atomic_copy(&source, &target, false).unwrap(), 4096);
        assert_eq!(fs::read(&target).unwrap().len(), 4096);
        assert!(atomic_copy(&source, &target, false).is_err());
    }

    #[test]
    fn unique_path_walks_the_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.txt");
        fs::write(&path, b"x").unwrap();
        let unique = unique_path(&path);
        assert_eq!(unique.file_name().unwrap(), "report (1).txt");
    }

    #[test]
    fn destination_never_targets_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("photo.jpg");
        fs::write(&source, b"x").unwrap();
        let target = destination_for(&source, dir.path(), Some("jpg"), true).unwrap();
        assert_ne!(target, source);
        assert_eq!(target.file_name().unwrap(), "photo (1).jpg");
    }

    #[test]
    fn destination_checks_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("photo.jpg");
        fs::write(&source, b"x").unwrap();
        let output = dir.path().join("out");
        ensure_dir(&output).unwrap();
        let target = destination_for(&source, &output, Some("png"), false).unwrap();
        assert_eq!(target.file_name().unwrap(), "photo.png");
        fs::write(&target, b"y").unwrap();
        assert!(destination_for(&source, &output, Some("png"), false).is_err());
        assert!(destination_for(&source, &output, Some("png"), true).is_ok());
    }

    #[test]
    fn text_reads_are_bounded_and_strip_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "\u{feff}hello").unwrap();
        assert_eq!(read_text_limited(&path, 64).unwrap(), "hello");
        assert!(read_text_limited(&path, 2).is_err());
    }

    #[test]
    fn portable_file_names_reject_windows_traps() {
        assert!(validate_portable_filename("normal.png").is_ok());
        assert!(validate_portable_filename("CON.txt").is_err());
        assert!(validate_portable_filename("bad?.txt").is_err());
        assert!(validate_portable_filename("trailing.").is_err());
        assert!(validate_portable_filename("a/b").is_err());
    }

    #[test]
    fn collect_files_skips_hidden_and_links() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("visible.txt"), b"x").unwrap();
        fs::write(dir.path().join(".hidden"), b"x").unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("deep.txt"), b"x").unwrap();
        let files = collect_files(dir.path(), true, false).unwrap();
        assert_eq!(files.len(), 2);
        let files = collect_files(dir.path(), true, true).unwrap();
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn writer_streams_large_payload() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("stream.bin");
        let mut writer = AtomicWriter::new(&target, false).unwrap();
        let chunk = vec![42u8; 64 * 1024];
        for _ in 0..8 {
            writer.write_all(&chunk).unwrap();
        }
        writer.finish().unwrap();
        assert_eq!(fs::metadata(&target).unwrap().len(), 8 * 64 * 1024);
    }
}

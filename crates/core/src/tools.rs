//! File toolbox: batch rename, duplicate discovery, empty directories and facts.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use formflux_shared::fs as sfs;
use serde::Serialize;

use crate::cancel::CancelToken;
use crate::error::{FormfluxError, Result};
use crate::hash::{self, FileHash, HashAlgorithm};
use crate::io;
use crate::progress::{NoopProgress, ProgressSink};

/// One planned rename.
#[derive(Debug, Clone, Serialize)]
pub struct RenamePlan {
    /// Current path.
    pub source: PathBuf,
    /// New path.
    pub target: PathBuf,
}

/// Batch rename strategy.
#[derive(Debug, Clone)]
pub enum RenameMode {
    /// Pattern with `{name}`, `{ext}`, `{n}`, `{n:03}` and `{date}` tokens.
    Pattern {
        /// Pattern text.
        pattern: String,
    },
    /// Add a prefix to the stem.
    Prefix(String),
    /// Add a suffix to the stem.
    Suffix(String),
    /// Regular-expression replacement on the stem.
    Regex {
        /// Pattern.
        pattern: String,
        /// Replacement, `$1` supported.
        replacement: String,
    },
    /// Rename from the EXIF capture time.
    ExifTime {
        /// chrono format used for the timestamp.
        format: String,
    },
}

/// Build a rename plan without touching the filesystem.
pub fn plan_rename(
    files: &[PathBuf],
    mode: &RenameMode,
    start: u64,
    step: u64,
) -> Result<Vec<RenamePlan>> {
    if files.is_empty() {
        return Err(FormfluxError::invalid("no files to rename"));
    }
    let mut occupied: HashSet<String> = HashSet::new();
    let sources: HashSet<PathBuf> = files.iter().cloned().collect();
    for file in files {
        if let Some(parent) = file.parent() {
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && !sources.contains(&path) {
                        occupied.insert(normalize_key(&path));
                    }
                }
            }
        }
    }
    let mut plans = Vec::with_capacity(files.len());
    let mut targets = HashSet::new();
    let mut sequence = start;
    let date = chrono::Local::now().format("%Y%m%d").to_string();
    for file in files {
        let parent = file
            .parent()
            .ok_or_else(|| FormfluxError::invalid("input has no parent directory"))?;
        let stem = file
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| FormfluxError::invalid("input has no valid stem"))?;
        let extension = file
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let new_stem = match mode {
            RenameMode::Pattern { pattern } => {
                render_pattern(pattern, stem, extension, sequence, &date)?
            }
            RenameMode::Prefix(prefix) => format!("{prefix}{stem}"),
            RenameMode::Suffix(suffix) => format!("{stem}{suffix}"),
            RenameMode::Regex {
                pattern,
                replacement,
            } => {
                let regex = regex::Regex::new(pattern).map_err(|error| {
                    FormfluxError::invalid(format!("invalid rename regex: {error}"))
                })?;
                regex.replace_all(stem, replacement.as_str()).into_owned()
            }
            RenameMode::ExifTime { format } => exif_stem(file, format)?,
        };
        sequence += step;
        let new_stem = new_stem.trim();
        if new_stem.is_empty() {
            return Err(FormfluxError::invalid(
                "rename would produce an empty filename",
            ));
        }
        let filename = if extension.is_empty() {
            new_stem.to_string()
        } else {
            format!("{new_stem}.{extension}")
        };
        sfs::validate_portable_filename(&filename)?;
        let target = parent.join(&filename);
        let key = normalize_key(&target);
        if !targets.insert(key.clone()) {
            return Err(FormfluxError::invalid(format!(
                "rename plan has a collision at '{}'",
                target.display()
            )));
        }
        if occupied.contains(&key) {
            return Err(FormfluxError::AlreadyExists(target));
        }
        plans.push(RenamePlan {
            source: file.clone(),
            target,
        });
    }
    Ok(plans)
}

fn normalize_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn render_pattern(
    pattern: &str,
    stem: &str,
    extension: &str,
    sequence: u64,
    date: &str,
) -> Result<String> {
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '{' {
            out.push(character);
            continue;
        }
        let mut token = String::new();
        let mut closed = false;
        for next in chars.by_ref() {
            if next == '}' {
                closed = true;
                break;
            }
            token.push(next);
        }
        if !closed {
            return Err(FormfluxError::invalid("unclosed '{' in rename pattern"));
        }
        let (name, width) = match token.split_once(':') {
            Some((name, width)) => (
                name,
                Some(
                    width
                        .parse::<usize>()
                        .map_err(|_| FormfluxError::invalid("invalid sequence width"))?,
                ),
            ),
            None => (token.as_str(), None),
        };
        match name {
            "name" => out.push_str(stem),
            "ext" => out.push_str(extension),
            "n" | "index" => {
                if let Some(width) = width {
                    out.push_str(&format!("{sequence:0width$}"));
                } else {
                    out.push_str(&sequence.to_string());
                }
            }
            "date" => out.push_str(date),
            other => {
                return Err(FormfluxError::invalid(format!(
                    "unknown rename token '{{{other}}}'"
                )))
            }
        }
    }
    Ok(out)
}

fn exif_stem(file: &Path, format: &str) -> Result<String> {
    let summary = crate::image::exif::read_exif(file)?;
    let captured = summary
        .captured_at
        .ok_or_else(|| FormfluxError::invalid(format!("'{}' has no EXIF date", file.display())))?;
    let captured = captured.trim_matches('"');
    let datetime = chrono::NaiveDateTime::parse_from_str(captured, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(captured, "%Y:%m:%d %H:%M:%S"))
        .map_err(|error| {
            FormfluxError::invalid(format!("could not parse EXIF date '{captured}': {error}"))
        })?;
    Ok(datetime.format(format).to_string())
}

/// Execute a rename plan.  The operation is two-phase: every source is moved
/// to a temporary name in its own directory, then to the final name, so swaps
/// and chains do not clobber each other.
pub fn execute_rename(plans: &[RenamePlan]) -> Result<()> {
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(plans.len());
    for (index, plan) in plans.iter().enumerate() {
        let parent = plan
            .source
            .parent()
            .ok_or_else(|| FormfluxError::invalid("source has no parent"))?;
        let temp = parent.join(format!(".formflux-rename-{index}.tmp"));
        if temp.exists() {
            std::fs::remove_file(&temp)?;
        }
        std::fs::rename(&plan.source, &temp)?;
        staged.push((temp, plan.target.clone()));
    }
    for (temp, target) in staged {
        if let Err(error) = std::fs::rename(&temp, &target) {
            return Err(FormfluxError::Io(error).context(format!(
                "could not finish rename; temporary file remains at {}",
                temp.display()
            )));
        }
    }
    Ok(())
}

/// Rename files according to `mode`.
pub fn rename_batch(
    files: &[PathBuf],
    mode: &RenameMode,
    dry_run: bool,
) -> Result<Vec<RenamePlan>> {
    let plans = plan_rename(files, mode, 0, 1)?;
    if !dry_run {
        execute_rename(&plans)?;
    }
    Ok(plans)
}

/// A group of byte-identical files.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    /// SHA-256 (or selected algorithm) digest.
    pub digest: String,
    /// Algorithm used.
    pub algorithm: String,
    /// Size of each file.
    pub size: u64,
    /// All files with this digest, first seen first.
    pub files: Vec<PathBuf>,
}

/// Result of duplicate discovery.
#[derive(Debug, Clone, Serialize)]
pub struct DedupeReport {
    /// Files scanned.
    pub scanned: usize,
    /// Groups with more than one file.
    pub groups: Vec<DuplicateGroup>,
    /// Number of redundant copies (total files in groups minus groups).
    pub redundant: usize,
}

/// Find duplicate files by size first, then content hash.
pub fn find_duplicates(
    files: &[PathBuf],
    algorithm: HashAlgorithm,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<DedupeReport> {
    let mut by_size: BTreeMap<u64, Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        cancel.check()?;
        let metadata = std::fs::metadata(file)?;
        if metadata.is_file() {
            by_size
                .entry(metadata.len())
                .or_default()
                .push(file.clone());
        }
    }
    let candidates = by_size
        .into_values()
        .filter(|group| group.len() > 1)
        .flatten()
        .collect::<Vec<_>>();
    let hashes: Vec<FileHash> =
        hash::hash_paths(&candidates, algorithm, false, false, cancel, progress)?;
    let mut by_digest: BTreeMap<(u64, String), Vec<PathBuf>> = BTreeMap::new();
    for item in hashes {
        by_digest
            .entry((item.size, item.digest))
            .or_default()
            .push(item.path);
    }
    let mut groups = Vec::new();
    let mut redundant = 0;
    for ((size, digest), files) in by_digest {
        if files.len() > 1 {
            redundant += files.len() - 1;
            groups.push(DuplicateGroup {
                digest,
                algorithm: algorithm.as_str().to_string(),
                size,
                files,
            });
        }
    }
    Ok(DedupeReport {
        scanned: files.len(),
        groups,
        redundant,
    })
}

/// Delete duplicates, keeping the first file in each group.
pub fn remove_duplicates(report: &DedupeReport, dry_run: bool) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for group in &report.groups {
        for file in group.files.iter().skip(1) {
            if !dry_run {
                std::fs::remove_file(file)?;
            }
            removed.push(file.clone());
        }
    }
    Ok(removed)
}

/// Remove empty directories below `root`, deepest first.
pub fn clean_empty_dirs(root: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
    let root = io::existing_file(root).or_else(|_| {
        if root.is_dir() {
            Ok(root.to_path_buf())
        } else {
            Err(FormfluxError::NotFound(root.to_path_buf()))
        }
    })?;
    let mut dirs = Vec::new();
    collect_dirs(&root, &mut dirs)?;
    dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    let mut removed = Vec::new();
    for dir in dirs {
        if dir == root {
            continue;
        }
        let empty = std::fs::read_dir(&dir)?.next().is_none();
        if empty {
            if !dry_run {
                std::fs::remove_dir(&dir)?;
            }
            removed.push(dir);
        }
    }
    Ok(removed)
}

fn collect_dirs(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        out.push(path.clone());
        collect_dirs(&path, out)?;
    }
    Ok(())
}

/// A convenient no-progress duplicate scan (used by tests and small folders).
pub fn duplicates_simple(files: &[PathBuf], algorithm: HashAlgorithm) -> Result<DedupeReport> {
    find_duplicates(files, algorithm, &CancelToken::never(), &NoopProgress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, b"x").unwrap();
        path
    }

    #[test]
    fn pattern_rename_keeps_extension() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![touch(dir.path(), "a.txt"), touch(dir.path(), "b.txt")];
        let plans = plan_rename(
            &files,
            &RenameMode::Pattern {
                pattern: "photo-{n:03}".to_string(),
            },
            5,
            1,
        )
        .unwrap();
        assert!(plans[0].target.ends_with("photo-005.txt"));
        assert!(plans[1].target.ends_with("photo-006.txt"));
    }

    #[test]
    fn collisions_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![touch(dir.path(), "a.txt"), touch(dir.path(), "b.txt")];
        let result = plan_rename(&files, &RenameMode::Prefix("same-".to_string()), 0, 1);
        // Prefix produces distinct names; force a collision with a pattern.
        assert!(result.is_ok());
        let collision = plan_rename(
            &files,
            &RenameMode::Pattern {
                pattern: "same".to_string(),
            },
            0,
            0,
        );
        assert!(collision.is_err());
    }

    #[test]
    fn rename_executes_two_phase() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![touch(dir.path(), "a.txt"), touch(dir.path(), "b.txt")];
        let plans = plan_rename(
            &files,
            &RenameMode::Pattern {
                pattern: "renamed-{n}".to_string(),
            },
            1,
            1,
        )
        .unwrap();
        execute_rename(&plans).unwrap();
        assert!(!files[0].exists());
        assert!(plans[0].target.exists());
    }

    #[test]
    fn duplicates_are_grouped() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.bin");
        let second = dir.path().join("b.bin");
        let third = dir.path().join("c.bin");
        fs::write(&first, b"same").unwrap();
        fs::write(&second, b"same").unwrap();
        fs::write(&third, b"different").unwrap();
        let report = duplicates_simple(&[first, second, third], HashAlgorithm::Sha256).unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].files.len(), 2);
        assert_eq!(report.redundant, 1);
    }

    #[test]
    fn empty_directories_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let removed = clean_empty_dirs(dir.path(), false).unwrap();
        assert!(removed.len() >= 2);
        assert!(!dir.path().join("a").exists());
    }
}

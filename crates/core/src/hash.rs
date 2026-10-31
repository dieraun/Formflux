//! Streaming file hashing, manifests and verification.
//!
//! Files are read in fixed-size chunks; a 10 GB video never enters memory.
//! Multi-file jobs report aggregate byte progress.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use blake2::Blake2b512;
use formflux_shared::fs as sfs;
use md5::Md5;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::cancel::CancelToken;
use crate::error::{FormfluxError, Result};
use crate::io;
use crate::progress::{ProgressEvent, ProgressSink};

/// Chunk size used for hashing (1 MiB).
pub const HASH_BUFFER: usize = 1024 * 1024;

/// Supported digest algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    /// MD5 (legacy compatibility only).
    Md5,
    /// SHA-1 (legacy compatibility only).
    Sha1,
    /// SHA-256.
    Sha256,
    /// SHA-384.
    Sha384,
    /// SHA-512.
    Sha512,
    /// BLAKE2b-512.
    Blake2b,
}

impl HashAlgorithm {
    /// Every algorithm in display order.
    pub const ALL: &'static [HashAlgorithm] = &[
        Self::Md5,
        Self::Sha1,
        Self::Sha256,
        Self::Sha384,
        Self::Sha512,
        Self::Blake2b,
    ];

    /// CLI/API name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
            Self::Blake2b => "blake2b",
        }
    }

    /// Parse a user-supplied algorithm name.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "md5" => Ok(Self::Md5),
            "sha1" => Ok(Self::Sha1),
            "sha256" => Ok(Self::Sha256),
            "sha384" => Ok(Self::Sha384),
            "sha512" => Ok(Self::Sha512),
            "blake2" | "blake2b" | "blake2b512" => Ok(Self::Blake2b),
            other => Err(FormfluxError::unsupported(format!(
                "unknown hash algorithm '{other}' (try md5, sha1, sha256, sha384, sha512 or blake2b)"
            ))),
        }
    }

    /// Digest length in bytes.
    pub fn digest_len(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 | Self::Blake2b => 64,
        }
    }

    /// Infer an algorithm from a hex digest length.
    pub fn from_digest_hex(value: &str) -> Option<Self> {
        match value.len() {
            32 => Some(Self::Md5),
            40 => Some(Self::Sha1),
            64 => Some(Self::Sha256),
            96 => Some(Self::Sha384),
            128 => Some(Self::Sha512),
            _ => None,
        }
    }
}

impl std::fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for HashAlgorithm {
    type Err = FormfluxError;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

enum Hasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
    Blake2b(Blake2b512),
}

impl Hasher {
    fn new(algorithm: HashAlgorithm) -> Self {
        match algorithm {
            HashAlgorithm::Md5 => Self::Md5(Md5::new()),
            HashAlgorithm::Sha1 => Self::Sha1(Sha1::new()),
            HashAlgorithm::Sha256 => Self::Sha256(Sha256::new()),
            HashAlgorithm::Sha384 => Self::Sha384(Sha384::new()),
            HashAlgorithm::Sha512 => Self::Sha512(Sha512::new()),
            HashAlgorithm::Blake2b => Self::Blake2b(Blake2b512::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Md5(hasher) => hasher.update(bytes),
            Self::Sha1(hasher) => hasher.update(bytes),
            Self::Sha256(hasher) => hasher.update(bytes),
            Self::Sha384(hasher) => hasher.update(bytes),
            Self::Sha512(hasher) => hasher.update(bytes),
            Self::Blake2b(hasher) => hasher.update(bytes),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            Self::Md5(hasher) => hasher.finalize().to_vec(),
            Self::Sha1(hasher) => hasher.finalize().to_vec(),
            Self::Sha256(hasher) => hasher.finalize().to_vec(),
            Self::Sha384(hasher) => hasher.finalize().to_vec(),
            Self::Sha512(hasher) => hasher.finalize().to_vec(),
            Self::Blake2b(hasher) => hasher.finalize().to_vec(),
        }
    }
}

/// Hash an in-memory byte slice.
pub fn hash_bytes(algorithm: HashAlgorithm, bytes: &[u8]) -> String {
    let mut hasher = Hasher::new(algorithm);
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// One hashed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHash {
    /// Absolute path.
    pub path: PathBuf,
    /// File size.
    pub size: u64,
    /// Algorithm.
    pub algorithm: HashAlgorithm,
    /// Lowercase hex digest.
    pub digest: String,
}

/// Hash one file without progress reporting.
pub fn hash_file(path: &Path, algorithm: HashAlgorithm) -> Result<FileHash> {
    hash_file_with_progress(
        path,
        algorithm,
        &CancelToken::new(),
        &crate::progress::NoopProgress,
    )
}

/// Hash one file, streaming and reporting byte progress.
pub fn hash_file_with_progress(
    path: &Path,
    algorithm: HashAlgorithm,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<FileHash> {
    let path = io::existing_file(path)?;
    let size = path.metadata()?.len();
    let mut reader = BufReader::with_capacity(HASH_BUFFER, File::open(&path)?);
    let mut hasher = Hasher::new(algorithm);
    let mut buffer = vec![0u8; HASH_BUFFER];
    let mut read_total = 0u64;
    loop {
        cancel.check()?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        read_total += read as u64;
        if size > 0 {
            progress.report(
                ProgressEvent::new("hash", read_total as f64 / size as f64 * 100.0)
                    .with_bytes(read_total, Some(size)),
            );
        }
    }
    progress.report(
        ProgressEvent::new("hash", 100.0)
            .with_bytes(read_total, Some(size))
            .with_message(format!("{} {}", algorithm, path.display())),
    );
    Ok(FileHash {
        path,
        size: read_total,
        algorithm,
        digest: hex::encode(hasher.finalize()),
    })
}

/// Hash many files (or directories) sequentially, reporting aggregate bytes.
pub fn hash_paths(
    paths: &[PathBuf],
    algorithm: HashAlgorithm,
    recursive: bool,
    include_hidden: bool,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<Vec<FileHash>> {
    if paths.is_empty() {
        return Err(FormfluxError::invalid("no input paths"));
    }
    let mut files = Vec::new();
    for path in paths {
        let canonical = io::existing_file(path).or_else(|_| {
            if path.is_dir() {
                Ok(path.clone())
            } else {
                Err(FormfluxError::NotFound(path.clone()))
            }
        })?;
        if canonical.is_dir() {
            files.extend(io::list_files(&canonical, recursive, include_hidden)?);
        } else {
            files.push(canonical);
        }
    }
    files.sort();
    files.dedup();
    let total: u64 = files
        .iter()
        .filter_map(|file| file.metadata().ok())
        .map(|metadata| metadata.len())
        .sum();
    let mut hashed = Vec::with_capacity(files.len());
    let mut done = 0u64;
    for file in files {
        cancel.check()?;
        let result = hash_file_with_progress(&file, algorithm, cancel, progress)?;
        done += result.size;
        if total > 0 {
            progress.report(
                ProgressEvent::new("hash", done as f64 / total as f64 * 100.0)
                    .with_bytes(done, Some(total))
                    .with_message(file.display().to_string()),
            );
        }
        hashed.push(result);
    }
    Ok(hashed)
}

/// Serialize hashes as a `sha256sum`-compatible manifest.
pub fn format_manifest(hashes: &[FileHash]) -> String {
    let mut out = String::new();
    for hash in hashes {
        let name = sfs::safe_file_name(&hash.path).unwrap_or_else(|_| "file".to_string());
        out.push_str(&format!("{}  {}\n", hash.digest, name));
    }
    out
}

/// A parsed manifest line.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestEntry {
    /// Expected digest.
    pub digest: String,
    /// Path as written in the manifest.
    pub path: String,
    /// Algorithm inferred from digest length.
    pub algorithm: HashAlgorithm,
}

/// Parse checksum manifests (`<hex>  <path>` or JSON array of FileHash).
pub fn parse_manifest(text: &str) -> Result<Vec<ManifestEntry>> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('[') {
        let hashes: Vec<FileHash> = serde_json::from_str(trimmed)?;
        return hashes
            .into_iter()
            .map(|hash| {
                Ok(ManifestEntry {
                    digest: hash.digest,
                    path: hash.path.to_string_lossy().into_owned(),
                    algorithm: hash.algorithm,
                })
            })
            .collect();
    }
    let mut entries = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (digest, rest) = line.split_once(char::is_whitespace).ok_or_else(|| {
            FormfluxError::invalid(format!("manifest line {} has no digest", number + 1))
        })?;
        let digest = digest.trim().to_ascii_lowercase();
        if !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(FormfluxError::invalid(format!(
                "manifest line {} has a non-hex digest",
                number + 1
            )));
        }
        let algorithm = HashAlgorithm::from_digest_hex(&digest).ok_or_else(|| {
            FormfluxError::invalid(format!(
                "manifest line {} has an unsupported digest length ({})",
                number + 1,
                digest.len()
            ))
        })?;
        let path = rest.trim().trim_start_matches('*').trim().to_string();
        if path.is_empty() {
            return Err(FormfluxError::invalid(format!(
                "manifest line {} has no path",
                number + 1
            )));
        }
        entries.push(ManifestEntry {
            digest,
            path,
            algorithm,
        });
    }
    if entries.is_empty() {
        return Err(FormfluxError::invalid("the manifest is empty"));
    }
    Ok(entries)
}

/// Verification outcome for one manifest entry.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyResult {
    /// Manifest path.
    pub path: PathBuf,
    /// Expected digest.
    pub expected: String,
    /// Computed digest, or `None` when the file was missing.
    pub actual: Option<String>,
    /// True when the digests match.
    pub ok: bool,
    /// Error detail for missing/unreadable files.
    pub error: Option<String>,
}

/// Verify a manifest against files resolved under `base_dir`.
pub fn verify_manifest(
    manifest: &str,
    base_dir: &Path,
    cancel: &CancelToken,
    progress: &dyn ProgressSink,
) -> Result<Vec<VerifyResult>> {
    let entries = parse_manifest(manifest)?;
    let total = entries.len().max(1) as f64;
    let mut results = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        cancel.check()?;
        let relative = Path::new(&entry.path);
        if relative.is_absolute() {
            return Err(FormfluxError::UnsafePath {
                path: relative.to_path_buf(),
                reason: "manifest entries must be relative to the manifest directory".to_string(),
            });
        }
        let path = sfs::safe_join(base_dir, relative)?;
        let result = match hash_file(&path, entry.algorithm) {
            Ok(hash) => VerifyResult {
                path,
                expected: entry.digest.clone(),
                actual: Some(hash.digest.clone()),
                ok: hash.digest == entry.digest,
                error: None,
            },
            Err(error) => VerifyResult {
                path,
                expected: entry.digest.clone(),
                actual: None,
                ok: false,
                error: Some(error.to_string()),
            },
        };
        results.push(result);
        progress.report(
            ProgressEvent::new("verify", (index + 1) as f64 / total * 100.0)
                .with_message(entry.path.clone()),
        );
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn known_vectors() {
        assert_eq!(
            hash_bytes(HashAlgorithm::Md5, b"abc"),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hash_bytes(HashAlgorithm::Sha1, b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hash_bytes(HashAlgorithm::Sha256, b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hash_bytes(HashAlgorithm::Sha384, b"abc"),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
    }

    #[test]
    fn file_and_bytes_agree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        fs::write(&path, b"hello world").unwrap();
        let file_hash = hash_file(&path, HashAlgorithm::Sha256).unwrap();
        assert_eq!(
            file_hash.digest,
            hash_bytes(HashAlgorithm::Sha256, b"hello world")
        );
        assert_eq!(file_hash.size, 11);
    }

    #[test]
    fn algorithm_parsing() {
        assert_eq!(
            HashAlgorithm::parse("SHA-256").unwrap(),
            HashAlgorithm::Sha256
        );
        assert_eq!(
            HashAlgorithm::parse("blake2b").unwrap(),
            HashAlgorithm::Blake2b
        );
        assert!(HashAlgorithm::parse("crc32").is_err());
    }

    #[test]
    fn manifest_round_trip_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"content").unwrap();
        let hashes = hash_paths(
            std::slice::from_ref(&path),
            HashAlgorithm::Sha256,
            false,
            false,
            &CancelToken::new(),
            &crate::progress::NoopProgress,
        )
        .unwrap();
        let manifest = format_manifest(&hashes);
        assert!(manifest.contains("a.txt"));
        let results = verify_manifest(
            &manifest,
            dir.path(),
            &CancelToken::new(),
            &crate::progress::NoopProgress,
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].ok);
        assert!(!results[0].path.to_string_lossy().contains(".."));
    }

    #[test]
    fn manifest_detects_mismatch_and_traversal() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"changed").unwrap();
        let digest = hash_bytes(HashAlgorithm::Sha256, b"original");
        let manifest = format!("{digest}  a.txt\n");
        let results = verify_manifest(
            &manifest,
            dir.path(),
            &CancelToken::new(),
            &crate::progress::NoopProgress,
        )
        .unwrap();
        assert!(!results[0].ok);
        assert!(results[0].actual.is_some());
        let traversal = format!("{digest}  ../secret.txt\n");
        assert!(verify_manifest(
            &traversal,
            dir.path(),
            &CancelToken::new(),
            &crate::progress::NoopProgress
        )
        .is_err());
    }

    #[test]
    fn directory_listing_is_hashed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::write(dir.path().join("nested/b.txt"), b"b").unwrap();
        let hashes = hash_paths(
            &[dir.path().to_path_buf()],
            HashAlgorithm::Sha256,
            true,
            false,
            &CancelToken::new(),
            &crate::progress::NoopProgress,
        )
        .unwrap();
        assert_eq!(hashes.len(), 2);
    }
}

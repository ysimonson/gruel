//! Hash of the running compiler binary, memoized across invocations.
//!
//! See ADR-0074 ("Compiler fingerprint") for the rationale. The short
//! version: `CARGO_PKG_VERSION` is too coarse (doesn't change across local
//! `cargo build` cycles), so we hash the binary's bytes instead. Hashing 30+
//! MB on every invocation would itself be a regression, so we memoize the
//! result keyed by `(path, mtime, size)` of the binary.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::fingerprint::{CacheKey, Hasher};

/// Compute or retrieve the cached hash of the compiler binary at
/// `binary_path`. Memoization lives under `memo_dir`; callers typically
/// pass `~/.cache/gruel/binary-hash`.
///
/// On any I/O error reading the memo, falls back to recomputing. The cache
/// is an optimization; correctness comes from the binary hash itself, not
/// from the memo.
pub fn compiler_fingerprint(binary_path: &Path, memo_dir: &Path) -> io::Result<CacheKey> {
    let meta = fs::metadata(binary_path)?;
    let size = meta.len();
    let mtime_nanos = mtime_nanos(&meta);

    let memo_filename = format!("{}-{}-{}.hash", path_slug(binary_path), mtime_nanos, size);
    let memo_path = memo_dir.join(&memo_filename);

    if let Some(cached) = read_cached_hash(&memo_path) {
        debug!(
            binary = %binary_path.display(),
            "compiler_fp: memo hit"
        );
        return Ok(cached);
    }

    debug!(
        binary = %binary_path.display(),
        size = size,
        "compiler_fp: hashing binary"
    );

    let bytes = fs::read(binary_path)?;
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    let key = hasher.finalize();

    if let Err(e) = write_cached_hash(&memo_path, &key) {
        // Don't fail the build over a memo write error — log and continue.
        warn!(
            error = %e,
            memo_path = %memo_path.display(),
            "compiler_fp: failed to write memo, continuing"
        );
    }

    Ok(key)
}

/// Get the path to the currently-running executable, with sensible
/// fallback. Used by callers who haven't been handed an explicit binary
/// path (the common case from the `gruel` CLI).
pub fn current_binary_path() -> io::Result<PathBuf> {
    std::env::current_exe()
}

/// Convert a path into a filename-safe slug. Used so the memo filename is
/// unique per binary location without needing a directory tree.
fn path_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(unix)]
fn mtime_nanos(meta: &fs::Metadata) -> u128 {
    use std::os::unix::fs::MetadataExt;
    let secs = meta.mtime() as i128;
    let nanos = meta.mtime_nsec() as i128;
    (secs.max(0) as u128) * 1_000_000_000 + nanos.max(0) as u128
}

#[cfg(not(unix))]
fn mtime_nanos(meta: &fs::Metadata) -> u128 {
    use std::time::UNIX_EPOCH;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn read_cached_hash(path: &Path) -> Option<CacheKey> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(CacheKey::from_bytes(arr))
}

fn write_cached_hash(path: &Path, key: &CacheKey) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Atomic write: tmp + rename.
    let tmp = path.with_extension("hash.tmp");
    fs::write(&tmp, key.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn fingerprint_is_stable_across_calls() {
        let bin_dir = TempDir::new().unwrap();
        let memo_dir = TempDir::new().unwrap();
        let bin = bin_dir.path().join("fakebin");
        fs::write(&bin, b"hello world").unwrap();

        let a = compiler_fingerprint(&bin, memo_dir.path()).unwrap();
        let b = compiler_fingerprint(&bin, memo_dir.path()).unwrap();
        assert_eq!(a, b, "memo should yield same hash on second call");
    }

    #[test]
    fn fingerprint_changes_when_binary_changes() {
        let bin_dir = TempDir::new().unwrap();
        let memo_dir = TempDir::new().unwrap();
        let bin = bin_dir.path().join("fakebin");

        fs::write(&bin, b"version 1").unwrap();
        let v1 = compiler_fingerprint(&bin, memo_dir.path()).unwrap();

        // Sleep long enough that mtime is guaranteed to differ on coarse
        // filesystems.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&bin, b"version 2 different bytes").unwrap();
        let v2 = compiler_fingerprint(&bin, memo_dir.path()).unwrap();

        assert_ne!(v1, v2);
    }

    #[test]
    fn missing_binary_is_an_error() {
        let memo_dir = TempDir::new().unwrap();
        let result =
            compiler_fingerprint(Path::new("/nonexistent/path/to/binary"), memo_dir.path());
        assert!(result.is_err());
    }
}

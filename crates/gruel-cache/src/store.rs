//! Content-addressed on-disk cache store.
//!
//! Layout (rooted at the cache directory passed to [`CacheStore::open`]):
//!
//! ```text
//! <root>/
//! ├── version              # u32 schema version, decimal text
//! ├── parse/
//! │   └── <hash>.bin
//! ├── air/
//! │   └── <hash>.bin
//! ├── llvm-ir/
//! │   └── <hash>.bc
//! └── tmp/                 # staging for atomic writes
//! ```
//!
//! On [`CacheStore::open`], if the persisted version doesn't match
//! [`crate::CACHE_SCHEMA_VERSION`], the entire cache directory is wiped
//! and recreated. This is the only path by which the store deletes data.
//!
//! Concurrency: writes go to `tmp/<random>` and are renamed into place.
//! Multiple `gruel` processes sharing a cache directory can read and write
//! safely; the worst case is duplicated work (two processes computing the
//! same hash and racing on rename).

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::CACHE_SCHEMA_VERSION;
use crate::fingerprint::CacheKey;

/// Which subdirectory a cache entry belongs in. The variants correspond
/// 1:1 to the pipeline stages that persist results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheKind {
    Parse,
    Air,
    LlvmIr,
}

impl CacheKind {
    fn dir_name(self) -> &'static str {
        match self {
            CacheKind::Parse => "parse",
            CacheKind::Air => "air",
            CacheKind::LlvmIr => "llvm-ir",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            CacheKind::Parse | CacheKind::Air => "bin",
            CacheKind::LlvmIr => "bc",
        }
    }

    /// All known kinds, for iteration in stats / GC code.
    pub fn all() -> [CacheKind; 3] {
        [CacheKind::Parse, CacheKind::Air, CacheKind::LlvmIr]
    }
}

/// Aggregate per-kind statistics for `gruel cache stats`.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: u64,
}

/// Handle to an open cache directory. Cheap to clone if needed (just
/// holds a `PathBuf`); creating a new one with [`CacheStore::open`] is
/// also cheap once the version check has run.
#[derive(Debug, Clone)]
pub struct CacheStore {
    root: PathBuf,
}

impl CacheStore {
    /// Open (or create) a cache rooted at `root`. If a `version` file
    /// exists with a value other than [`CACHE_SCHEMA_VERSION`], the
    /// entire directory is wiped before the store returns.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        let store = Self { root };
        store.ensure_layout()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Look up a blob. Returns `Ok(None)` if the entry is absent;
    /// `Err(_)` only on real I/O failures.
    pub fn get(&self, kind: CacheKind, key: &CacheKey) -> io::Result<Option<Vec<u8>>> {
        let path = self.entry_path(kind, key);
        match fs::File::open(&path) {
            Ok(mut f) => {
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                Ok(Some(buf))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Insert (or overwrite) a blob. Atomic: writes to `tmp/<random>`
    /// and renames into place. Concurrent inserts of the same key are
    /// safe; whichever rename wins is the visible result.
    pub fn put(&self, kind: CacheKind, key: &CacheKey, data: &[u8]) -> io::Result<()> {
        let final_path = self.entry_path(kind, key);
        let tmp_path = self.tmp_path(key, kind.extension());
        {
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        // On Unix, rename atomically replaces the destination.
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// Wipe every cache file. Equivalent to `gruel cache clean`.
    pub fn clean(&self) -> io::Result<()> {
        if !self.root.exists() {
            return Ok(());
        }
        fs::remove_dir_all(&self.root)?;
        self.ensure_layout()?;
        info!(root = %self.root.display(), "cache cleaned");
        Ok(())
    }

    /// Walk the cache directory and accumulate per-kind stats. O(N) in
    /// the number of entries; called only by `gruel cache stats` and
    /// during tests, never on the hot path.
    pub fn stats(&self) -> io::Result<[(CacheKind, CacheStats); 3]> {
        let mut out = [
            (CacheKind::Parse, CacheStats::default()),
            (CacheKind::Air, CacheStats::default()),
            (CacheKind::LlvmIr, CacheStats::default()),
        ];
        for (kind, stats) in &mut out {
            let dir = self.kind_dir(*kind);
            if !dir.exists() {
                continue;
            }
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let meta = entry.metadata()?;
                if meta.is_file() {
                    stats.entries += 1;
                    stats.bytes += meta.len();
                }
            }
        }
        Ok(out)
    }

    fn entry_path(&self, kind: CacheKind, key: &CacheKey) -> PathBuf {
        self.kind_dir(kind)
            .join(format!("{}.{}", key.hex(), kind.extension()))
    }

    fn kind_dir(&self, kind: CacheKind) -> PathBuf {
        self.root.join(kind.dir_name())
    }

    fn tmp_path(&self, key: &CacheKey, ext: &str) -> PathBuf {
        // Use the key + a process-local counter so concurrent puts of
        // different keys don't fight over the same tmp filename.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        self.root.join("tmp").join(format!(
            "{}-{}-{}.{}.tmp",
            std::process::id(),
            n,
            key.hex(),
            ext
        ))
    }

    fn ensure_layout(&self) -> io::Result<()> {
        // Check version file; wipe if mismatched.
        let version_path = self.root.join("version");
        if self.root.exists() && version_path.exists() {
            match fs::read_to_string(&version_path) {
                Ok(s) => {
                    let stored: Option<u32> = s.trim().parse().ok();
                    if stored != Some(CACHE_SCHEMA_VERSION) {
                        warn!(
                            stored = ?stored,
                            current = CACHE_SCHEMA_VERSION,
                            "cache schema version mismatch; wiping"
                        );
                        // Wipe and recreate.
                        fs::remove_dir_all(&self.root)?;
                    }
                }
                Err(_) => {
                    // Unreadable version file → treat as mismatch.
                    fs::remove_dir_all(&self.root)?;
                }
            }
        }

        // Ensure all subdirs exist.
        for kind in CacheKind::all() {
            fs::create_dir_all(self.root.join(kind.dir_name()))?;
        }
        fs::create_dir_all(self.root.join("tmp"))?;

        // Persist current version.
        if !version_path.exists() {
            fs::write(&version_path, CACHE_SCHEMA_VERSION.to_string())?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::blake3_bytes;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, CacheStore) {
        let tmp = TempDir::new().unwrap();
        let store = CacheStore::open(tmp.path().join("cache")).unwrap();
        (tmp, store)
    }

    #[test]
    fn missing_entry_returns_none() {
        let (_tmp, store) = make_store();
        let key = blake3_bytes(b"never inserted");
        assert!(store.get(CacheKind::Parse, &key).unwrap().is_none());
    }

    #[test]
    fn put_then_get_round_trips() {
        let (_tmp, store) = make_store();
        let key = blake3_bytes(b"hello");
        let data = b"some serialized blob";
        store.put(CacheKind::Air, &key, data).unwrap();
        assert_eq!(
            store.get(CacheKind::Air, &key).unwrap().as_deref(),
            Some(data.as_ref())
        );
    }

    #[test]
    fn put_is_atomic_no_partial_files() {
        // After a successful put, the entry file exists and tmp/ contains
        // no leftover .tmp files.
        let (_tmp, store) = make_store();
        let key = blake3_bytes(b"k");
        store.put(CacheKind::Parse, &key, b"data").unwrap();

        let tmp_dir = store.root().join("tmp");
        let leftovers: Vec<_> = fs::read_dir(&tmp_dir).unwrap().collect();
        assert!(
            leftovers.is_empty(),
            "tmp/ should be empty after successful put, found: {:?}",
            leftovers
        );
    }

    #[test]
    fn clean_wipes_everything_then_layout_returns() {
        let (_tmp, store) = make_store();
        let key = blake3_bytes(b"k");
        store.put(CacheKind::Air, &key, b"x").unwrap();
        assert!(store.get(CacheKind::Air, &key).unwrap().is_some());

        store.clean().unwrap();
        assert!(store.get(CacheKind::Air, &key).unwrap().is_none());

        // Layout still usable (subdirs and version file present).
        for kind in CacheKind::all() {
            assert!(store.root().join(kind.dir_name()).is_dir());
        }
        assert!(store.root().join("version").is_file());
    }

    #[test]
    fn version_mismatch_wipes_cache() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("cache");

        // Open once, populate.
        {
            let store = CacheStore::open(&root).unwrap();
            let key = blake3_bytes(b"k");
            store.put(CacheKind::Parse, &key, b"x").unwrap();
        }

        // Corrupt the version file.
        fs::write(root.join("version"), "999").unwrap();

        // Re-opening should wipe and recreate.
        let store = CacheStore::open(&root).unwrap();
        let key = blake3_bytes(b"k");
        assert!(store.get(CacheKind::Parse, &key).unwrap().is_none());
        assert_eq!(
            fs::read_to_string(root.join("version")).unwrap().trim(),
            CACHE_SCHEMA_VERSION.to_string()
        );
    }

    #[test]
    fn stats_reports_entry_counts_and_bytes() {
        let (_tmp, store) = make_store();
        let k1 = blake3_bytes(b"one");
        let k2 = blake3_bytes(b"two");
        store.put(CacheKind::Parse, &k1, b"abcde").unwrap();
        store.put(CacheKind::Parse, &k2, b"xy").unwrap();
        store.put(CacheKind::Air, &k1, b"123").unwrap();

        let stats = store.stats().unwrap();
        let parse = &stats[0].1;
        let air = &stats[1].1;
        let llvm = &stats[2].1;
        assert_eq!(parse.entries, 2);
        assert_eq!(parse.bytes, 7);
        assert_eq!(air.entries, 1);
        assert_eq!(air.bytes, 3);
        assert_eq!(llvm.entries, 0);
    }

    #[test]
    fn put_overwrite_replaces_existing() {
        let (_tmp, store) = make_store();
        let key = blake3_bytes(b"k");
        store.put(CacheKind::Air, &key, b"old").unwrap();
        store.put(CacheKind::Air, &key, b"new").unwrap();
        assert_eq!(
            store.get(CacheKind::Air, &key).unwrap().as_deref(),
            Some(b"new".as_ref())
        );
    }
}

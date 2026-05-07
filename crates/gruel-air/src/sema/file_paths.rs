//! File path management for multi-file compilation.
//!
//! This module handles mapping FileIds to source file paths, which is needed
//! for module resolution and relative imports.

use rustc_hash::FxHashMap as HashMap;

use gruel_util::FileId;

use super::Sema;

impl<'a> Sema<'a> {
    /// Set file paths for module resolution in multi-file compilation.
    ///
    /// This maps FileIds to their corresponding source file paths,
    /// enabling relative import resolution during @import.
    pub fn set_file_paths(&mut self, file_paths: HashMap<FileId, String>) {
        self.file_paths = file_paths;
    }

    /// Get the source file path for a span.
    ///
    /// Looks up the file path using the span's file_id.
    pub(crate) fn get_source_path(&self, span: gruel_util::Span) -> Option<&str> {
        self.file_paths.get(&span.file_id).map(|s| s.as_str())
    }

    /// Get the file path for a given FileId.
    pub(crate) fn get_file_path(&self, file_id: FileId) -> Option<&str> {
        self.file_paths.get(&file_id).map(|s| s.as_str())
    }
}

/// Path-based predicate: the file lives inside the top-level `prelude/`
/// directory. Used by the `@lang(...)` privilege check (Phase 1+).
pub fn is_prelude_path(path: &str) -> bool {
    // Match either the embedded virtual prefix `prelude/` (no leading
    // path component) or any path with a `/prelude/` segment (on-disk
    // workspace paths). Reject paths whose prelude segment sits under
    // `std/prelude/` — that layout was retired in ADR-0079, and a
    // residual `std/prelude/` directory is not the privileged one.
    if path.contains("std/prelude/") || path.contains("std\\prelude\\") {
        return false;
    }
    path.starts_with("prelude/")
        || path.starts_with("prelude\\")
        || path.contains("/prelude/")
        || path.contains("\\prelude\\")
}

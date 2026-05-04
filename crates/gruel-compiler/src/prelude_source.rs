//! Prelude source assembly (ADR-0078).
//!
//! The Gruel prelude lives on disk under `std/prelude/*.gruel`. Each prelude
//! file is loaded as its own source unit with a unique path so visibility
//! resolution treats them as a directory module per ADR-0026 (same-directory
//! private items are mutually visible; only `pub` items leak out to user
//! files in other directories).
//!
//! Resolution order, mirroring `resolve_std_import` in
//! `crates/gruel-air/src/sema/analysis.rs`:
//!
//! 1. `$GRUEL_STD_PATH/prelude/*.gruel` if `GRUEL_STD_PATH` is set
//! 2. Walk up from the binary's manifest directory looking for
//!    `std/prelude/*.gruel`
//! 3. Embedded fallback via `include_str!` — guaranteed to compile and
//!    distribute in the binary, used when no on-disk stdlib is found.
//!
//! The list of prelude files is closed and ordered. To add a new prelude file,
//! append it to `PRELUDE_FILES`.

use std::path::{Path, PathBuf};

/// One on-disk prelude file with both its embedded fallback content and
/// its relative path under `std/prelude/`.
pub struct PreludeFile {
    /// Filename relative to `std/prelude/` (e.g. `"option.gruel"`).
    pub filename: &'static str,
    /// Embedded copy via `include_str!`. Used as a fallback when the on-disk
    /// stdlib isn't available.
    pub embedded: &'static str,
}

/// Embedded copies of every prelude file. Order is the parse order; files
/// later in the list see all symbols declared earlier (intra-prelude
/// references at the type-level resolve through global declaration
/// gathering, not source order, so order is mostly informational).
const PRELUDE_FILES: &[PreludeFile] = &[
    PreludeFile {
        filename: "interfaces.gruel",
        embedded: include_str!("../../../std/prelude/interfaces.gruel"),
    },
    PreludeFile {
        filename: "target.gruel",
        embedded: include_str!("../../../std/prelude/target.gruel"),
    },
    PreludeFile {
        filename: "cmp.gruel",
        embedded: include_str!("../../../std/prelude/cmp.gruel"),
    },
    PreludeFile {
        filename: "option.gruel",
        embedded: include_str!("../../../std/prelude/option.gruel"),
    },
    PreludeFile {
        filename: "result.gruel",
        embedded: include_str!("../../../std/prelude/result.gruel"),
    },
    PreludeFile {
        filename: "char.gruel",
        embedded: include_str!("../../../std/prelude/char.gruel"),
    },
    PreludeFile {
        filename: "string.gruel",
        embedded: include_str!("../../../std/prelude/string.gruel"),
    },
];

/// Resolved prelude source: a path and the corresponding source string. The
/// path is always under `std/prelude/` (real disk path when available,
/// virtual `std/prelude/<filename>` otherwise) so the visibility resolver
/// treats prelude files as a single directory module.
pub struct ResolvedPreludeFile {
    pub path: String,
    pub source: String,
}

/// Resolve every prelude file to (path, source) pairs.
///
/// Tries the on-disk `std/prelude/` first (via `GRUEL_STD_PATH` or a small
/// upward search). On miss — or if any file is missing on disk — falls back
/// to the embedded copies under a virtual `std/prelude/<filename>` path so
/// the directory-based visibility check still works.
pub fn resolved_prelude_files() -> Vec<ResolvedPreludeFile> {
    if let Some(dir) = locate_prelude_dir()
        && let Some(files) = read_prelude_dir(&dir)
    {
        return files;
    }
    embedded_prelude_files()
}

/// Embedded prelude files (the `include_str!` fallback). The returned paths
/// are virtual (`std/prelude/<filename>`) but share the directory structure
/// the visibility check needs.
pub fn embedded_prelude_files() -> Vec<ResolvedPreludeFile> {
    PRELUDE_FILES
        .iter()
        .map(|f| ResolvedPreludeFile {
            path: format!("std/prelude/{}", f.filename),
            source: f.embedded.to_string(),
        })
        .collect()
}

/// Try to locate `std/prelude/` on disk. Returns the directory path on hit.
fn locate_prelude_dir() -> Option<PathBuf> {
    if let Ok(gruel_std) = std::env::var("GRUEL_STD_PATH") {
        let candidate = Path::new(&gruel_std).join("prelude");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    // Walk up from CARGO_MANIFEST_DIR looking for std/prelude/. Stops at the
    // filesystem root. This works for in-repo builds (`cargo run -p gruel`).
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut current: PathBuf = PathBuf::from(manifest);
    loop {
        let candidate = current.join("std").join("prelude");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Read every file listed in `PRELUDE_FILES` from `dir`. Returns `None` if
/// any file is missing — the caller falls back to embedded copies in that
/// case so a partial on-disk stdlib doesn't silently shadow the embedded one.
fn read_prelude_dir(dir: &Path) -> Option<Vec<ResolvedPreludeFile>> {
    let mut out = Vec::with_capacity(PRELUDE_FILES.len());
    for f in PRELUDE_FILES {
        let path = dir.join(f.filename);
        let source = std::fs::read_to_string(&path).ok()?;
        out.push(ResolvedPreludeFile {
            path: path.to_string_lossy().into_owned(),
            source,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_files_cover_canonical_items() {
        let files = embedded_prelude_files();
        let combined: String = files.iter().map(|f| f.source.as_str()).collect();
        assert!(combined.contains("fn Option(comptime T: type)"));
        assert!(combined.contains("fn Result(comptime T: type, comptime E: type)"));
        assert!(combined.contains("fn char__encode_utf8"));
        assert!(combined.contains("fn String__from_utf8"));
        assert!(combined.contains("pub interface Eq"));
        assert!(combined.contains("pub interface Ord"));
    }

    #[test]
    fn embedded_paths_are_under_std_prelude() {
        for f in embedded_prelude_files() {
            assert!(f.path.starts_with("std/prelude/"), "{}", f.path);
        }
    }
}

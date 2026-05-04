//! Prelude and stdlib source resolution (ADR-0078).
//!
//! The Gruel prelude is a regular Gruel module rooted at `std/_prelude.gruel`.
//! That single file is implicitly `@import`-ed by every compilation: its
//! `pub` items become available in user files without an explicit import.
//! Internally, `_prelude.gruel` uses `@import` + `pub const` re-exports to
//! organize itself across `std/prelude/*.gruel` submodules — exactly like
//! `_std.gruel` does for the rest of the standard library.
//!
//! The entire `std/` tree is embedded into the binary via `include_dir!` so
//! the compiler ships with a self-contained stdlib. When `GRUEL_STD_PATH`
//! is set or the binary runs from inside a checked-out repo, on-disk files
//! win over the embedded copy so contributors editing prelude code get
//! their changes without rebuilding the compiler.

use include_dir::{Dir, include_dir};
use std::path::{Path, PathBuf};

/// Embedded copy of the entire `std/` directory tree.
static STD_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../std");

/// Path within `std/` to the prelude root.
const PRELUDE_ROOT_REL: &str = "_prelude.gruel";

/// Order in which prelude submodules must be loaded.
///
/// Today's resolver collects function signatures in source order, so a
/// function whose return type references `Result(...)` has to follow
/// `result.gruel` in the merged AST. This list encodes the dependency
/// order; any unlisted `.gruel` files under `std/prelude/` are loaded
/// alphabetically after the listed ones.
const PRELUDE_SUBMODULE_ORDER: &[&str] = &[
    "prelude/interfaces.gruel",
    "prelude/target.gruel",
    "prelude/cmp.gruel",
    "prelude/option.gruel",
    "prelude/result.gruel",
    "prelude/char.gruel",
    "prelude/string.gruel",
];

/// One stdlib file with its path and source.
pub struct ResolvedPreludeFile {
    /// Path used by the module resolver — disk-absolute when on-disk,
    /// virtual `std/<rel>` otherwise.
    pub path: String,
    /// Source content.
    pub source: String,
}

/// Result of locating the prelude.
///
/// The compiler stages every entry in `aux_files` into the compilation
/// unit's `file_paths` so `@import` resolution finds them. `prelude_dir`
/// lists just the files under `std/prelude/` — the ones referenced by
/// `_prelude.gruel`'s re-exports — so test fixtures that bypass
/// `CompilationUnit::parse` can inline their items without dragging in
/// unrelated stdlib modules (which would have unresolved `@import`
/// references in a test environment).
pub struct ResolvedPrelude {
    /// `_prelude.gruel` itself — the file the compiler implicitly imports.
    pub root: ResolvedPreludeFile,
    /// Files under `std/prelude/`, in dependency-aware order.
    pub prelude_dir: Vec<ResolvedPreludeFile>,
    /// All other stdlib files (e.g. `_std.gruel`, `math.gruel`). Pre-staged
    /// for `@import("std")` and friends. Empty in test fixtures.
    pub other_std_files: Vec<ResolvedPreludeFile>,
}

impl ResolvedPrelude {
    /// Every file other than the root, in load order. Used by the
    /// compilation driver to register all stdlib paths.
    pub fn aux_files(&self) -> impl Iterator<Item = &ResolvedPreludeFile> {
        self.prelude_dir.iter().chain(self.other_std_files.iter())
    }
}

/// Resolve the prelude.
///
/// Tries the on-disk `std/` first (via `GRUEL_STD_PATH` or an upward search
/// from the binary's manifest dir); falls back to the embedded tree
/// otherwise.
pub fn resolved_prelude() -> ResolvedPrelude {
    if let Some(std_dir) = locate_std_dir()
        && let Some(resolved) = read_disk_std(&std_dir)
    {
        return resolved;
    }
    embedded_prelude()
}

/// Embedded prelude (the `include_dir!` fallback).
pub fn embedded_prelude() -> ResolvedPrelude {
    let mut root: Option<ResolvedPreludeFile> = None;
    let mut by_rel: std::collections::HashMap<String, ResolvedPreludeFile> =
        std::collections::HashMap::new();

    for file in walk_dir(&STD_DIR) {
        let rel = file.path().to_string_lossy().to_string();
        let source = match file.contents_utf8() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let path = format!("std/{}", rel);
        let entry = ResolvedPreludeFile { path, source };
        if rel == PRELUDE_ROOT_REL {
            root = Some(entry);
        } else if rel.ends_with(".gruel") {
            by_rel.insert(rel, entry);
        }
    }

    split_prelude_dir(by_rel, root.expect("std/_prelude.gruel must exist"))
}

/// Partition the collected `.gruel` files into prelude-dir submodules
/// (referenced by `_prelude.gruel`) and other stdlib files.
fn split_prelude_dir(
    mut by_rel: std::collections::HashMap<String, ResolvedPreludeFile>,
    root: ResolvedPreludeFile,
) -> ResolvedPrelude {
    let mut prelude_dir = Vec::with_capacity(PRELUDE_SUBMODULE_ORDER.len());
    for &rel in PRELUDE_SUBMODULE_ORDER {
        if let Some(entry) = by_rel.remove(rel) {
            prelude_dir.push(entry);
        }
    }
    // Any leftover prelude/* files (added later, not yet in
    // PRELUDE_SUBMODULE_ORDER) — append alphabetically.
    let mut leftover_prelude: Vec<_> = by_rel
        .keys()
        .filter(|k| k.starts_with("prelude/"))
        .cloned()
        .collect();
    leftover_prelude.sort();
    for rel in leftover_prelude {
        if let Some(entry) = by_rel.remove(&rel) {
            prelude_dir.push(entry);
        }
    }
    // Everything else is "other std files" — sorted alphabetically.
    let mut other_std_files: Vec<_> = by_rel.into_values().collect();
    other_std_files.sort_by(|a, b| a.path.cmp(&b.path));
    ResolvedPrelude {
        root,
        prelude_dir,
        other_std_files,
    }
}

/// Iterate every file (recursive) inside an `include_dir` tree, sorted by
/// path for deterministic ordering. Order matters for declaration
/// resolution: a file that references items from another file must follow
/// the file that defines those items in the merged AST.
fn walk_dir<'a>(dir: &'a Dir<'a>) -> Vec<&'a include_dir::File<'a>> {
    let mut out = Vec::new();
    walk_into(dir, &mut out);
    out.sort_by_key(|f| f.path().to_path_buf());
    out
}

fn walk_into<'a>(dir: &'a Dir<'a>, out: &mut Vec<&'a include_dir::File<'a>>) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::File(f) => out.push(f),
            include_dir::DirEntry::Dir(d) => walk_into(d, out),
        }
    }
}

/// Try to locate `std/` on disk.
fn locate_std_dir() -> Option<PathBuf> {
    if let Ok(gruel_std) = std::env::var("GRUEL_STD_PATH") {
        let candidate = PathBuf::from(&gruel_std);
        if candidate.join(PRELUDE_ROOT_REL).exists() {
            return Some(candidate);
        }
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut current: PathBuf = PathBuf::from(manifest);
    loop {
        let candidate = current.join("std");
        if candidate.join(PRELUDE_ROOT_REL).exists() {
            return Some(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Read every `.gruel` file under `std_dir` recursively. Returns `None` if
/// the prelude root is missing — caller falls back to embedded.
fn read_disk_std(std_dir: &Path) -> Option<ResolvedPrelude> {
    let root_path = std_dir.join(PRELUDE_ROOT_REL);
    let root_source = std::fs::read_to_string(&root_path).ok()?;
    let mut aux_collected = Vec::new();
    collect_gruel_files(std_dir, &mut aux_collected);
    let root = ResolvedPreludeFile {
        path: root_path.to_string_lossy().into_owned(),
        source: root_source,
    };
    // Filter the root out, then arrange by relative path against
    // PRELUDE_SUBMODULE_ORDER for deterministic, dependency-aware order.
    aux_collected.retain(|f| f.path != root.path);
    let by_rel: std::collections::HashMap<String, ResolvedPreludeFile> = aux_collected
        .into_iter()
        .filter_map(|f| {
            let rel = std::path::Path::new(&f.path)
                .strip_prefix(std_dir)
                .ok()?
                .to_str()?
                .to_string();
            Some((rel, f))
        })
        .collect();
    Some(split_prelude_dir(by_rel, root))
}

fn collect_gruel_files(dir: &Path, out: &mut Vec<ResolvedPreludeFile>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_gruel_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("gruel")
            && let Ok(source) = std::fs::read_to_string(&path)
        {
            out.push(ResolvedPreludeFile {
                path: path.to_string_lossy().into_owned(),
                source,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_prelude_root_loadable() {
        let p = embedded_prelude();
        // The prelude root is at least valid (possibly-empty) Gruel.
        assert!(p.root.path.ends_with("_prelude.gruel"));
    }

    #[test]
    fn embedded_prelude_includes_submodules() {
        let p = embedded_prelude();
        assert!(
            p.prelude_dir
                .iter()
                .any(|f| f.path.ends_with("/option.gruel"))
        );
        assert!(p.prelude_dir.iter().any(|f| f.path.ends_with("/cmp.gruel")));
    }

    #[test]
    fn other_std_files_separate_from_prelude_dir() {
        let p = embedded_prelude();
        // Embedded copy includes `_std.gruel` and `math.gruel`; they go in
        // `other_std_files`, not `prelude_dir`.
        assert!(
            p.other_std_files
                .iter()
                .any(|f| f.path.ends_with("_std.gruel"))
        );
    }
}

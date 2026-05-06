//! Prelude and stdlib source resolution (ADR-0079).
//!
//! The Gruel prelude is a top-level module rooted at `prelude/_prelude.gruel`.
//! That single file is implicitly `@import`-ed by every compilation: its
//! `pub` items become available in user files without an explicit import.
//! Internally, `_prelude.gruel` uses `@import` + `pub const` re-exports to
//! organize itself across sibling `prelude/*.gruel` submodules.
//!
//! Stdlib lives at `std/` and is a regular library — reachable via
//! `@import("std")`, with no auto-load semantics. Splitting prelude and
//! stdlib at the file layer (ADR-0079) makes the privilege boundary
//! explicit: only files under `prelude/` are allowed to claim
//! `@lang(...)` bindings.
//!
//! Both trees are embedded into the binary via `include_dir!` so the
//! compiler ships self-contained. When `GRUEL_STD_PATH` /
//! `GRUEL_PRELUDE_PATH` is set or the binary runs from inside a
//! checked-out repo, on-disk files win over the embedded copy so
//! contributors editing prelude or stdlib code get their changes
//! without rebuilding the compiler.

use include_dir::{Dir, include_dir};
use std::path::{Path, PathBuf};

/// Embedded copy of the top-level `prelude/` directory tree.
static PRELUDE_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../prelude");

/// Embedded copy of the top-level `std/` directory tree.
static STD_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../std");

/// Path within `prelude/` to the prelude root.
const PRELUDE_ROOT_REL: &str = "_prelude.gruel";

/// Order in which prelude submodules must be loaded.
///
/// Today's resolver collects function signatures in source order, so a
/// function whose return type references `Result(...)` has to follow
/// `result.gruel` in the merged AST. This list encodes the dependency
/// order; any unlisted `.gruel` files under `prelude/` are loaded
/// alphabetically after the listed ones.
const PRELUDE_SUBMODULE_ORDER: &[&str] = &[
    "interfaces.gruel",
    "target.gruel",
    "type_info.gruel",
    "cmp.gruel",
    "option.gruel",
    "result.gruel",
    "char.gruel",
    "string.gruel",
];

/// One source file with its path and content.
pub struct ResolvedPreludeFile {
    /// Path used by the module resolver — disk-absolute when on-disk,
    /// virtual `prelude/<rel>` or `std/<rel>` otherwise.
    pub path: String,
    /// Source content.
    pub source: String,
}

/// Result of locating the prelude.
///
/// The compiler stages every entry in `aux_files` into the compilation
/// unit's `file_paths` so `@import` resolution finds them. `prelude_dir`
/// lists the files under `prelude/` (excluding the root) — the ones
/// referenced by `_prelude.gruel`'s re-exports — so test fixtures that
/// bypass `CompilationUnit::parse` can inline their items without
/// dragging in unrelated stdlib modules (which would have unresolved
/// `@import` references in a test environment).
pub struct ResolvedPrelude {
    /// `prelude/_prelude.gruel` itself — the file the compiler implicitly imports.
    pub root: ResolvedPreludeFile,
    /// Files under `prelude/`, in dependency-aware order.
    pub prelude_dir: Vec<ResolvedPreludeFile>,
    /// Stdlib files under `std/` (e.g. `_std.gruel`, `math.gruel`).
    /// Pre-staged for `@import("std")` and friends. Empty in test
    /// fixtures.
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
/// Tries on-disk `prelude/` and `std/` first (via `GRUEL_PRELUDE_PATH` /
/// `GRUEL_STD_PATH` or an upward search from the binary's manifest dir);
/// falls back to the embedded trees otherwise.
pub fn resolved_prelude() -> ResolvedPrelude {
    let disk_prelude = locate_dir("prelude", "GRUEL_PRELUDE_PATH", PRELUDE_ROOT_REL);
    let disk_std = locate_dir("std", "GRUEL_STD_PATH", "_std.gruel");
    if let (Some(prelude_dir), Some(std_dir)) = (disk_prelude.as_ref(), disk_std.as_ref())
        && let Some(resolved) = read_disk(prelude_dir, std_dir)
    {
        return resolved;
    }
    embedded_prelude()
}

/// Embedded prelude (the `include_dir!` fallback).
pub fn embedded_prelude() -> ResolvedPrelude {
    let mut root: Option<ResolvedPreludeFile> = None;
    let mut prelude_files: std::collections::HashMap<String, ResolvedPreludeFile> =
        std::collections::HashMap::new();

    for file in walk_dir(&PRELUDE_DIR) {
        let rel = file.path().to_string_lossy().to_string();
        let source = match file.contents_utf8() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let path = format!("prelude/{}", rel);
        let entry = ResolvedPreludeFile { path, source };
        if rel == PRELUDE_ROOT_REL {
            root = Some(entry);
        } else if rel.ends_with(".gruel") {
            prelude_files.insert(rel, entry);
        }
    }

    let mut other_std_files = Vec::new();
    for file in walk_dir(&STD_DIR) {
        let rel = file.path().to_string_lossy().to_string();
        if !rel.ends_with(".gruel") {
            continue;
        }
        let source = match file.contents_utf8() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let path = format!("std/{}", rel);
        other_std_files.push(ResolvedPreludeFile { path, source });
    }
    other_std_files.sort_by(|a, b| a.path.cmp(&b.path));

    arrange_prelude(
        prelude_files,
        root.expect("prelude/_prelude.gruel must exist"),
        other_std_files,
    )
}

/// Arrange prelude submodules into dependency-aware order; pass through
/// the `other_std_files` list as the caller computed it.
fn arrange_prelude(
    mut prelude_files: std::collections::HashMap<String, ResolvedPreludeFile>,
    root: ResolvedPreludeFile,
    other_std_files: Vec<ResolvedPreludeFile>,
) -> ResolvedPrelude {
    let mut prelude_dir = Vec::with_capacity(PRELUDE_SUBMODULE_ORDER.len());
    for &rel in PRELUDE_SUBMODULE_ORDER {
        if let Some(entry) = prelude_files.remove(rel) {
            prelude_dir.push(entry);
        }
    }
    // Any leftover prelude files (added later, not yet in
    // PRELUDE_SUBMODULE_ORDER) — append alphabetically.
    let mut leftover: Vec<_> = prelude_files.keys().cloned().collect();
    leftover.sort();
    for rel in leftover {
        if let Some(entry) = prelude_files.remove(&rel) {
            prelude_dir.push(entry);
        }
    }
    ResolvedPrelude {
        root,
        prelude_dir,
        other_std_files,
    }
}

/// Iterate every file (recursive) inside an `include_dir` tree, sorted by
/// path for deterministic ordering.
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

/// Try to locate a top-level workspace directory (e.g. `prelude/` or
/// `std/`) on disk.
fn locate_dir(name: &str, env_var: &str, witness: &str) -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var(env_var) {
        let candidate = PathBuf::from(&env_path);
        if candidate.join(witness).exists() {
            return Some(candidate);
        }
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut current: PathBuf = PathBuf::from(manifest);
    loop {
        let candidate = current.join(name);
        if candidate.join(witness).exists() {
            return Some(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Read every `.gruel` file under `prelude_dir` and `std_dir`. Returns
/// `None` if the prelude root is missing — caller falls back to embedded.
fn read_disk(prelude_dir: &Path, std_dir: &Path) -> Option<ResolvedPrelude> {
    let root_path = prelude_dir.join(PRELUDE_ROOT_REL);
    let root_source = std::fs::read_to_string(&root_path).ok()?;
    let root = ResolvedPreludeFile {
        path: root_path.to_string_lossy().into_owned(),
        source: root_source,
    };

    let mut prelude_collected = Vec::new();
    collect_gruel_files(prelude_dir, &mut prelude_collected);
    prelude_collected.retain(|f| f.path != root.path);
    let prelude_files: std::collections::HashMap<String, ResolvedPreludeFile> = prelude_collected
        .into_iter()
        .filter_map(|f| {
            let rel = std::path::Path::new(&f.path)
                .strip_prefix(prelude_dir)
                .ok()?
                .to_str()?
                .to_string();
            Some((rel, f))
        })
        .collect();

    let mut other_std_files = Vec::new();
    collect_gruel_files(std_dir, &mut other_std_files);
    other_std_files.sort_by(|a, b| a.path.cmp(&b.path));

    Some(arrange_prelude(prelude_files, root, other_std_files))
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
        assert!(p.root.path.ends_with("_prelude.gruel"));
        // The root path lives under prelude/, not std/.
        assert!(
            p.root.path.contains("prelude/_prelude.gruel")
                || p.root.path.contains("prelude\\_prelude.gruel")
        );
    }

    #[test]
    fn embedded_prelude_includes_submodules() {
        let p = embedded_prelude();
        assert!(
            p.prelude_dir
                .iter()
                .any(|f| f.path.ends_with("/option.gruel") || f.path.ends_with("\\option.gruel"))
        );
        assert!(
            p.prelude_dir
                .iter()
                .any(|f| f.path.ends_with("/cmp.gruel") || f.path.ends_with("\\cmp.gruel"))
        );
    }

    #[test]
    fn other_std_files_separate_from_prelude_dir() {
        let p = embedded_prelude();
        assert!(
            p.other_std_files
                .iter()
                .any(|f| f.path.ends_with("_std.gruel"))
        );
        // Prelude_dir should not contain stdlib files.
        assert!(!p.prelude_dir.iter().any(|f| f.path.ends_with("_std.gruel")));
    }
}

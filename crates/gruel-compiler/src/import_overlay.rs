//! Walks the `@import` graph from an entry `.gruel` file (ADR-0092 Phase 3).
//!
//! [`load_import_closure`] is a thin helper that parses an entry source file,
//! walks every `@import("...")` it contains (transitively), and returns the
//! corresponding [`SourceFile`]s in a stable order suitable for feeding the
//! existing multi-file pipeline ([`parse_all_files_with_preview`],
//! [`merge_symbols`], …).
//!
//! Each file's text is sourced from the optional [`ImportOverlay`] callback
//! first (so the LSP can substitute open-editor buffers) and falls back to
//! the on-disk file content. The CLI typically passes `None` for the
//! overlay; the LSP passes a closure backed by its [`DocState`] map.
//!
//! This sits *above* sema: sema continues to look up imports against an
//! already-loaded `file_paths` map. The overlay's job is to populate that
//! map before sema runs.
//!
//! [`SourceFile`]: crate::SourceFile
//! [`parse_all_files_with_preview`]: crate::parse_all_files_with_preview
//! [`merge_symbols`]: crate::merge_symbols
//! [`DocState`]: https://docs.rs/dashmap

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gruel_air::ModulePath;
use gruel_parser::ast::{Ast, Expr, IntrinsicArg, Item};
use lasso::ThreadedRodeo;

use crate::{FileId, Lexer, Parser, PreviewFeatures};

/// Hook that, given an absolute (or workspace-relative) file path, may
/// return overriding source text for that file.
///
/// CLI callers leave this as `None` and the closure walker reads from disk.
/// The LSP can supply an overlay backed by its open-buffer cache so that
/// `@import` resolution sees in-flight edits, not stale disk contents.
pub type ImportOverlay = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;

/// One file loaded by [`load_import_closure`]: a path, its text, and an
/// assigned [`FileId`]. The caller converts these into [`SourceFile`]s.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    pub path: PathBuf,
    pub text: String,
    pub file_id: FileId,
}

/// A non-fatal load failure for one file (the entry, or a transitively
/// imported file). The walker logs these up to the caller; sema will
/// surface the eventual `ModuleNotFound` diagnostic for unresolved
/// `@import`s.
#[derive(Debug)]
pub enum ImportLoadError {
    /// Filesystem error reading a file (no overlay matched and on-disk
    /// read failed).
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for ImportLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportLoadError::Io { path, source } => {
                write!(f, "failed to read {}: {}", path.display(), source)
            }
        }
    }
}

impl std::error::Error for ImportLoadError {}

/// Load the entry file plus every file it transitively `@import`s.
///
/// The first returned `LoadedFile` is always the entry file with
/// `FileId::new(1)`. Subsequent files get monotonically increasing
/// `FileId`s in the order they're first discovered.
///
/// Resolution mirrors sema's existing rules
/// ([`gruel_air::ModulePath::resolve`]). Imports that can't be resolved
/// to a known file (yet) are simply skipped — sema will emit
/// `ModuleNotFound` once the closure is parsed.
pub fn load_import_closure(
    entry_path: &Path,
    preview_features: &PreviewFeatures,
    overlay: Option<&ImportOverlay>,
) -> Result<Vec<LoadedFile>, ImportLoadError> {
    let entry_text = read_text(entry_path, overlay)?;
    let mut closure: Vec<LoadedFile> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut worklist: Vec<LoadedFile> = vec![LoadedFile {
        path: entry_path.to_path_buf(),
        text: entry_text,
        file_id: FileId::new(1),
    }];
    let mut next_id: u32 = 2;

    // Candidate paths the @import resolver matches against. Grown as we
    // discover new files so suffix-matching resolution can find them.
    let mut candidates: Vec<String> = vec![entry_path.to_string_lossy().into_owned()];

    while let Some(file) = worklist.pop() {
        if !seen.insert(file.path.clone()) {
            continue;
        }
        let imports = discover_imports(&file.text, preview_features);
        let file_path = file.path.clone();
        closure.push(file);

        for import in imports {
            let Some(resolved) = ModulePath::parse(&import).resolve(candidates.iter()) else {
                // Could not resolve against currently-known paths. The walker
                // doesn't enumerate the filesystem; we look for `.gruel`
                // siblings of files we've already loaded instead.
                if let Some(candidate) = try_neighbor_path(&file_path, &import) {
                    let path = candidate;
                    if seen.contains(&path) {
                        continue;
                    }
                    let text = match read_text(&path, overlay) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    candidates.push(path.to_string_lossy().into_owned());
                    worklist.push(LoadedFile {
                        path,
                        text,
                        file_id: FileId::new(next_id),
                    });
                    next_id = next_id.saturating_add(1);
                }
                continue;
            };
            let resolved_path = PathBuf::from(&resolved);
            if seen.contains(&resolved_path) {
                continue;
            }
            let text = match read_text(&resolved_path, overlay) {
                Ok(t) => t,
                Err(_) => continue,
            };
            worklist.push(LoadedFile {
                path: resolved_path,
                text,
                file_id: FileId::new(next_id),
            });
            next_id = next_id.saturating_add(1);
        }
    }

    Ok(closure)
}

/// Heuristic neighbour resolution for `@import("foo.gruel")` when sema's
/// candidate-list lookup fails: try the import path as a sibling of the
/// current file. Matches sema's "explicit path" rule.
fn try_neighbor_path(current: &Path, import: &str) -> Option<PathBuf> {
    if !import.ends_with(".gruel") {
        return None;
    }
    let dir = current.parent()?;
    let candidate = dir.join(import);
    candidate.exists().then_some(candidate)
}

fn read_text(path: &Path, overlay: Option<&ImportOverlay>) -> Result<String, ImportLoadError> {
    if let Some(o) = overlay
        && let Some(text) = o(path)
    {
        return Ok(text);
    }
    std::fs::read_to_string(path).map_err(|err| ImportLoadError::Io {
        path: path.to_path_buf(),
        source: err,
    })
}

/// Parse a file and pluck out the `@import("...")` string-literal paths
/// it references in const initialisers. Best-effort: returns an empty
/// list on parse failure (sema will report the parse error from the
/// real pipeline).
fn discover_imports(text: &str, preview_features: &PreviewFeatures) -> Vec<String> {
    let interner = ThreadedRodeo::new();
    let lexer = Lexer::with_interner_and_file_id(text, interner, FileId::new(1));
    let Ok((tokens, interner)) = lexer.tokenize() else {
        return Vec::new();
    };
    let parser = Parser::new(tokens, interner)
        .with_preview_features(preview_features.clone())
        .with_source(text);
    let Ok((ast, interner)) = parser.parse() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_imports_in_ast(&ast, &interner, &mut out);
    out
}

fn collect_imports_in_ast(ast: &Ast, interner: &ThreadedRodeo, out: &mut Vec<String>) {
    for item in &ast.items {
        if let Item::Const(c) = item {
            collect_imports_in_expr(&c.init, interner, out);
        }
    }
}

fn collect_imports_in_expr(expr: &Expr, interner: &ThreadedRodeo, out: &mut Vec<String>) {
    if let Expr::IntrinsicCall(call) = expr
        && interner.resolve(&call.name.name) == "import"
        && let Some(IntrinsicArg::Expr(Expr::String(s))) = call.args.first()
    {
        out.push(interner.resolve(&s.value).to_string());
    }
}

/// View the closure as `(path, text, FileId)` triples so the caller can
/// construct `SourceFile<'_>` borrows.
pub fn loaded_files_as_view(files: &[LoadedFile]) -> Vec<(String, String, FileId)> {
    files
        .iter()
        .map(|f| {
            (
                f.path.to_string_lossy().into_owned(),
                f.text.clone(),
                f.file_id,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn closure_of_lone_entry_is_just_the_entry() {
        let tmp = TempDir::new().unwrap();
        let main = write(tmp.path(), "main.gruel", "fn main() -> i32 { 0 }\n");
        let files = load_import_closure(&main, &PreviewFeatures::default(), None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, main);
        assert!(files[0].text.contains("fn main"));
        assert_eq!(files[0].file_id.index(), 1);
    }

    #[test]
    fn closure_follows_one_import_from_disk() {
        let tmp = TempDir::new().unwrap();
        let main = write(
            tmp.path(),
            "main.gruel",
            "const math = @import(\"math.gruel\");\nfn main() -> i32 { 0 }\n",
        );
        let math = write(tmp.path(), "math.gruel", "pub fn pi() -> i32 { 3 }\n");
        let files = load_import_closure(&main, &PreviewFeatures::default(), None).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.clone()).collect();
        assert!(paths.contains(&main));
        assert!(paths.contains(&math));
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn overlay_substitutes_imported_file_text() {
        let tmp = TempDir::new().unwrap();
        let main = write(
            tmp.path(),
            "main.gruel",
            "const math = @import(\"math.gruel\");\nfn main() -> i32 { 0 }\n",
        );
        let math = write(tmp.path(), "math.gruel", "pub fn pi() -> i32 { 999 }\n");

        let overlay_target = math.clone();
        let overlay: ImportOverlay = Arc::new(move |p: &Path| {
            if p == overlay_target {
                Some("pub fn pi() -> i32 { 42 }\n".to_string())
            } else {
                None
            }
        });

        let files =
            load_import_closure(&main, &PreviewFeatures::default(), Some(&overlay)).unwrap();
        let math_file = files
            .iter()
            .find(|f| f.path == math)
            .expect("math.gruel should be in closure");
        assert!(math_file.text.contains("42"), "expected overlay text, got: {}", math_file.text);
    }

    #[test]
    fn overlay_substitutes_entry_file_text() {
        let tmp = TempDir::new().unwrap();
        let main = write(tmp.path(), "main.gruel", "fn main() -> i32 { 0 }\n");

        let main_clone = main.clone();
        let overlay: ImportOverlay = Arc::new(move |p: &Path| {
            if p == main_clone {
                Some("fn main() -> i32 { 7 }\n".to_string())
            } else {
                None
            }
        });

        let files =
            load_import_closure(&main, &PreviewFeatures::default(), Some(&overlay)).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].text.contains("7"));
    }

    #[test]
    fn unresolvable_import_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let main = write(
            tmp.path(),
            "main.gruel",
            "const missing = @import(\"nope.gruel\");\nfn main() -> i32 { 0 }\n",
        );
        let files = load_import_closure(&main, &PreviewFeatures::default(), None).unwrap();
        // Walker doesn't crash; just leaves the import unresolved.
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, main);
    }

    #[test]
    fn cycle_terminates() {
        let tmp = TempDir::new().unwrap();
        let a = write(
            tmp.path(),
            "a.gruel",
            "const b = @import(\"b.gruel\");\npub fn from_a() -> i32 { 1 }\n",
        );
        let _b = write(
            tmp.path(),
            "b.gruel",
            "const a = @import(\"a.gruel\");\npub fn from_b() -> i32 { 2 }\n",
        );
        let files = load_import_closure(&a, &PreviewFeatures::default(), None).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn entry_io_error_returned() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("absent.gruel");
        let err = load_import_closure(&missing, &PreviewFeatures::default(), None).unwrap_err();
        assert!(matches!(err, ImportLoadError::Io { .. }));
    }
}

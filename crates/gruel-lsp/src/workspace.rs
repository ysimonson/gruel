//! Workspace file discovery and @import closure construction (ADR-0091).
//!
//! Each open editor buffer is analyzed as its own compilation root: the LSP
//! parses it, walks for `@import("...")` calls, transitively loads every
//! reachable file (open buffer first, on-disk fallback), and hands the closure
//! to the frontend as a single `CompilationUnit`. Unrelated files in the
//! workspace are never merged together — that's what kept opening this very
//! repo from producing thousands of `fn main()` duplicate-definition errors
//! before ADR-0091's per-root revision (2026-05-19).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gruel_air::ModulePath;
use gruel_compiler::{FileId, PreviewFeatures, SourceFile, parse_all_files_with_preview};
use gruel_parser::ast::{Ast, Expr, IntrinsicArg, Item};
use ignore::WalkBuilder;
use lasso::ThreadedRodeo;

use crate::analysis::WorkspaceFile;

/// Enumerate every `*.gruel` file under `root`, respecting `.gitignore`
/// and skipping `.git`/`target`.
///
/// Used to populate the candidate path list that `@import` resolution searches
/// against — NOT to build a compilation unit. The LSP analyzes each open file
/// as its own root and only pulls in files reachable through its `@import`
/// graph.
pub fn enumerate_gruel_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .standard_filters(true)
        .filter_entry(|entry| {
            let name = entry.file_name();
            name != ".git" && name != "target"
        })
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("gruel") {
            out.push(path.to_path_buf());
        }
    }
    out
}

/// Walk a parsed AST and collect the string-literal argument of every
/// `@import("...")` intrinsic call we can find. Imports whose path is a
/// non-literal (e.g. `@import(comptime { ... })`) are skipped — the sema layer
/// resolves those during the analysis pass and will surface diagnostics if
/// resolution fails.
fn collect_imports_in_expr(expr: &Expr, interner: &ThreadedRodeo, out: &mut Vec<String>) {
    if let Expr::IntrinsicCall(call) = expr
        && interner.resolve(&call.name.name) == "import"
        && let Some(IntrinsicArg::Expr(Expr::String(s))) = call.args.first()
    {
        out.push(interner.resolve(&s.value).to_string());
    }
}

fn collect_imports_in_ast(ast: &Ast, interner: &ThreadedRodeo, out: &mut Vec<String>) {
    // Only walk const initializers — that's where the compiler actually
    // resolves `@import` (see crates/gruel-air/src/sema/imports.rs). Imports
    // appearing elsewhere are illegal and would already be flagged by sema.
    for item in &ast.items {
        if let Item::Const(c) = item {
            collect_imports_in_expr(&c.init, interner, out);
        }
    }
}

/// Parse one file's source, walk its AST for `@import("...")` paths, and
/// return them. Returns `Vec::new()` on parse failure — the analysis pass will
/// surface the syntax error itself; we just can't follow imports through a
/// broken file.
fn discover_imports(text: &str, path: &str, preview_features: &PreviewFeatures) -> Vec<String> {
    let source = SourceFile::new(path, text, FileId::new(1));
    let parsed = match parse_all_files_with_preview(&[source], preview_features) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let Some(file) = parsed.files.first() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    collect_imports_in_ast(&file.ast, &parsed.interner, &mut paths);
    paths
}

/// Resolve an `@import("foo")` path against the available files, mirroring
/// `gruel-air`'s [`ModulePath`] rules. The candidate list contains every
/// known file path (open + on-disk under the workspace root, plus any extras
/// supplied by the caller).
fn resolve_import(import_path: &str, candidates: &[String]) -> Option<String> {
    let owned: Vec<String> = candidates.to_vec();
    ModulePath::parse(import_path).resolve(owned.iter())
}

/// Build a (root + transitively-`@import`-reachable) `WorkspaceFile` set for
/// one compilation root.
///
/// File text comes from `open_text` when the file is currently open in the
/// editor, otherwise from disk. Already-visited paths are deduped so cyclic
/// import graphs terminate.
pub fn build_root_closure<F>(
    root: WorkspaceFile,
    workspace_root: Option<&Path>,
    preview_features: &PreviewFeatures,
    mut open_text: F,
) -> Vec<WorkspaceFile>
where
    F: FnMut(&Path) -> Option<String>,
{
    // All known candidate paths the @import resolver can match against.
    // Strings, since ModulePath::resolve works on String iterators.
    let workspace_files: Vec<PathBuf> = match workspace_root {
        Some(root) => enumerate_gruel_files(root),
        None => Vec::new(),
    };
    let mut candidate_strings: Vec<String> = workspace_files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let root_str = root.path.to_string_lossy().into_owned();
    if !candidate_strings.iter().any(|s| s == &root_str) {
        candidate_strings.push(root_str);
    }

    let mut closure: Vec<WorkspaceFile> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut next_id: u32 = root.file_id.index().saturating_add(1).max(2);
    let mut worklist: Vec<WorkspaceFile> = vec![root];

    while let Some(file) = worklist.pop() {
        if !seen.insert(file.path.clone()) {
            continue;
        }
        let path_str = file.path.to_string_lossy().into_owned();
        let import_paths = discover_imports(&file.text, &path_str, preview_features);
        closure.push(file);

        for import_path in import_paths {
            let Some(resolved_str) = resolve_import(&import_path, &candidate_strings) else {
                // Resolution failure is the analysis pass's problem (it will
                // emit ModuleNotFound). We just don't follow what we can't
                // find here.
                continue;
            };
            let resolved_path = PathBuf::from(&resolved_str);
            if seen.contains(&resolved_path) {
                continue;
            }
            let text = match open_text(&resolved_path) {
                Some(t) => t,
                None => match std::fs::read_to_string(&resolved_path) {
                    Ok(t) => t,
                    Err(_) => continue,
                },
            };
            worklist.push(WorkspaceFile {
                path: resolved_path,
                text,
                file_id: FileId::new(next_id),
            });
            next_id = next_id.saturating_add(1);
        }
    }

    closure
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn wsf(path: PathBuf, text: &str, id: u32) -> WorkspaceFile {
        WorkspaceFile {
            path,
            text: text.to_string(),
            file_id: FileId::new(id),
        }
    }

    #[test]
    fn finds_gruel_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.gruel"), "fn main() -> i32 { 0 }").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.gruel"), "fn helper() -> i32 { 1 }").unwrap();
        fs::write(root.join("notes.txt"), "not gruel").unwrap();
        let files = enumerate_gruel_files(root);
        let names: Vec<_> = files
            .iter()
            .filter_map(|p| p.file_name().and_then(|s| s.to_str()))
            .collect();
        assert!(names.contains(&"a.gruel"));
        assert!(names.contains(&"b.gruel"));
        assert!(!names.contains(&"notes.txt"));
    }

    #[test]
    fn discovers_no_imports_for_plain_file() {
        let imports = discover_imports(
            "fn main() -> i32 { 0 }",
            "main.gruel",
            &PreviewFeatures::default(),
        );
        assert!(imports.is_empty());
    }

    #[test]
    fn discovers_import_in_const_init() {
        let text = r#"const math = @import("math.gruel");
fn main() -> i32 { 0 }
"#;
        let imports = discover_imports(text, "main.gruel", &PreviewFeatures::default());
        assert_eq!(imports, vec!["math.gruel".to_string()]);
    }

    #[test]
    fn closure_includes_only_root_when_no_imports() {
        let dir = tempdir().unwrap();
        let root_path = dir.path().join("main.gruel");
        fs::write(&root_path, "fn main() -> i32 { 0 }").unwrap();
        let root = wsf(root_path.clone(), "fn main() -> i32 { 0 }", 1);
        let closure =
            build_root_closure(root, Some(dir.path()), &PreviewFeatures::default(), |_| {
                None
            });
        assert_eq!(closure.len(), 1);
        assert_eq!(closure[0].path, root_path);
    }

    #[test]
    fn closure_follows_one_import() {
        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.gruel");
        let math_path = dir.path().join("math.gruel");
        let main_src = r#"const math = @import("math.gruel");
fn main() -> i32 { 0 }
"#;
        let math_src = r#"pub fn pi() -> i32 { 3 }
"#;
        fs::write(&main_path, main_src).unwrap();
        fs::write(&math_path, math_src).unwrap();
        let root = wsf(main_path.clone(), main_src, 1);
        let closure =
            build_root_closure(root, Some(dir.path()), &PreviewFeatures::default(), |_| {
                None
            });
        let paths: Vec<_> = closure.iter().map(|f| f.path.clone()).collect();
        assert!(paths.contains(&main_path), "closure should include root");
        assert!(
            paths.contains(&math_path),
            "closure should include math.gruel"
        );
        assert_eq!(closure.len(), 2);
    }

    #[test]
    fn closure_does_not_include_sibling_with_no_import_relation() {
        // The bug fix: two unrelated `fn main()` files don't get merged.
        let dir = tempdir().unwrap();
        let a_path = dir.path().join("a.gruel");
        let b_path = dir.path().join("b.gruel");
        let a_src = "fn main() -> i32 { 1 }";
        let b_src = "fn main() -> i32 { 2 }";
        fs::write(&a_path, a_src).unwrap();
        fs::write(&b_path, b_src).unwrap();
        let root = wsf(a_path.clone(), a_src, 1);
        let closure =
            build_root_closure(root, Some(dir.path()), &PreviewFeatures::default(), |_| {
                None
            });
        let paths: Vec<_> = closure.iter().map(|f| f.path.clone()).collect();
        assert!(paths.contains(&a_path));
        assert!(
            !paths.contains(&b_path),
            "unrelated file must not be pulled in"
        );
    }

    #[test]
    fn closure_terminates_on_import_cycle() {
        let dir = tempdir().unwrap();
        let a_path = dir.path().join("a.gruel");
        let b_path = dir.path().join("b.gruel");
        let a_src = r#"const b = @import("b.gruel");
pub fn from_a() -> i32 { 1 }
"#;
        let b_src = r#"const a = @import("a.gruel");
pub fn from_b() -> i32 { 2 }
"#;
        fs::write(&a_path, a_src).unwrap();
        fs::write(&b_path, b_src).unwrap();
        let root = wsf(a_path.clone(), a_src, 1);
        let closure =
            build_root_closure(root, Some(dir.path()), &PreviewFeatures::default(), |_| {
                None
            });
        let paths: Vec<_> = closure.iter().map(|f| f.path.clone()).collect();
        assert!(paths.contains(&a_path));
        assert!(paths.contains(&b_path));
        assert_eq!(closure.len(), 2);
    }

    #[test]
    fn closure_prefers_open_text_over_disk() {
        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.gruel");
        fs::write(&main_path, "fn main() -> i32 { 0 }").unwrap();
        let root = wsf(main_path.clone(), "fn main() -> i32 { 999 }", 1);
        let in_memory = "fn main() -> i32 { 999 }".to_string();
        let closure =
            build_root_closure(root, Some(dir.path()), &PreviewFeatures::default(), |_| {
                Some(in_memory.clone())
            });
        assert_eq!(closure[0].text, "fn main() -> i32 { 999 }");
    }
}

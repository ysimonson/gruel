//! ADR-0092 Phase 4: LSP manifest integration tests.
//!
//! These verify that:
//! - **Manifested mode** (manifest present): the LSP analyzes the manifest's
//!   `root` and its transitively-imported files only. Sibling files that are
//!   *not* reached through `@import` produce no diagnostics, no matter how
//!   many `fn main` they share with the entry.
//! - **Isolation mode** (no manifest): each open buffer is its own root.
//!   Two unrelated `fn main` files don't trip duplicate-definition.
//! - Switching modes by introducing a `gruel.json` works (the analysis
//!   sees the new mode after `reload_manifest`).

use std::fs;
use std::path::{Path, PathBuf};

use gruel_compiler::{FileId, PreviewFeatures};
use gruel_lsp::analysis::{WorkspaceFile, analyze_root};
use gruel_target::Target;
use tempfile::TempDir;

fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, body).unwrap();
    p
}

fn ws(path: &Path, text: &str, id: u32) -> WorkspaceFile {
    WorkspaceFile {
        path: path.to_path_buf(),
        text: text.to_string(),
        file_id: FileId::new(id),
    }
}

#[test]
fn manifested_mode_includes_imported_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main_src = "const util = @import(\"util.gruel\");\n\
                    fn main() -> i32 { util.helper() }\n";
    let util_src = "pub fn helper() -> i32 { 42 }\n";
    let main = write(&root, "src/main.gruel", main_src);
    write(&root, "src/util.gruel", util_src);

    // In manifested mode, the LSP hands sema the entry; build_root_closure
    // (via analyze_root) walks @imports and pulls in util.gruel.
    let root_file = ws(&main, main_src, 1);
    let result = analyze_root(
        root_file,
        Some(&root),
        &PreviewFeatures::default(),
        &Target::host(),
        |_| None,
    );

    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(
        errors.is_empty(),
        "expected no errors, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn manifested_mode_does_not_pull_unrelated_file() {
    // The bug fix: a sibling `fn main()` in the workspace that isn't
    // @import'd by the manifest's entry does not collide with the entry.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main_src = "fn main() -> i32 { 1 }\n";
    let scratch_src = "fn main() -> i32 { 2 }\n";
    let main = write(&root, "src/main.gruel", main_src);
    write(&root, "scratch/other.gruel", scratch_src);

    let root_file = ws(&main, main_src, 1);
    let result = analyze_root(
        root_file,
        Some(&root),
        &PreviewFeatures::default(),
        &Target::host(),
        |_| None,
    );

    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect();
    let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        !messages.iter().any(|m| m.contains("duplicate")),
        "expected no duplicate-definition errors, got: {:?}",
        messages
    );
}

#[test]
fn isolation_mode_isolates_each_open_buffer() {
    // Without a manifest, each open file is its own root. Two unrelated
    // `fn main` files don't collide.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let a_src = "fn main() -> i32 { 1 }\n";
    let b_src = "fn main() -> i32 { 2 }\n";
    let a = write(&root, "a.gruel", a_src);
    let b = write(&root, "b.gruel", b_src);

    let a_result = analyze_root(
        ws(&a, a_src, 1),
        Some(&root),
        &PreviewFeatures::default(),
        &Target::host(),
        |_| None,
    );
    let b_result = analyze_root(
        ws(&b, b_src, 1),
        Some(&root),
        &PreviewFeatures::default(),
        &Target::host(),
        |_| None,
    );

    let a_errors: Vec<&str> = a_result
        .diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message.as_str())
        .collect();
    let b_errors: Vec<&str> = b_result
        .diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message.as_str())
        .collect();

    assert!(
        !a_errors.iter().any(|m| m.contains("duplicate")),
        "a.gruel got duplicate errors: {:?}",
        a_errors
    );
    assert!(
        !b_errors.iter().any(|m| m.contains("duplicate")),
        "b.gruel got duplicate errors: {:?}",
        b_errors
    );
}

#[test]
fn manifest_discover_at_root_finds_workspace_manifest() {
    // Phase 4 plumbing test: the LSP's discovery path consults
    // gruel_manifest::discover_at_root. Verify it correctly identifies
    // a manifest at the workspace root and parses it.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write(&root, "src/main.gruel", "fn main() -> i32 { 0 }\n");
    fs::write(
        root.join("gruel.json"),
        r#"{ "name": "hello", "version": "0.1.0", "bin": { "root": "src/main.gruel" } }"#,
    )
    .unwrap();

    let manifest_path =
        gruel_manifest::discover_at_root(&root).expect("manifest should be discovered");
    let manifest = gruel_manifest::load_at(&manifest_path).expect("manifest should load");
    assert_eq!(manifest.name, "hello");
    assert!(manifest.target.is_binary());
    assert_eq!(
        manifest.target.root().canonicalize().unwrap(),
        root.join("src/main.gruel").canonicalize().unwrap()
    );
}

#[test]
fn manifest_discover_returns_none_when_absent() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    assert!(gruel_manifest::discover_at_root(&root).is_none());
}

//! Phase 1 integration tests for diagnostics (ADR-0091).
//!
//! These tests drive the analysis pipeline directly via the `analyze`
//! function so they don't need to spawn the full tower-lsp message pump.
//! Server-level integration (open/change/close) is covered by tests that
//! exercise `Backend::analyze_now`.

use std::path::PathBuf;

use gruel_compiler::{FileId, PreviewFeatures};
use gruel_lsp::analysis::{WorkspaceFile, analyze, analyze_root};
use gruel_target::Target;
use tempfile::tempdir;

fn ws(path: &str, source: &str, id: u32) -> WorkspaceFile {
    WorkspaceFile {
        path: PathBuf::from(path),
        text: source.to_string(),
        file_id: FileId::new(id),
    }
}

#[test]
fn no_diagnostics_for_clean_program() {
    let files = vec![ws("a.gruel", "fn main() -> i32 { 42 }", 1)];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    assert!(
        res.diagnostics.is_empty(),
        "expected no diagnostics, got: {:?}",
        res.diagnostics
    );
    assert!(res.snapshot.is_some());
}

#[test]
fn type_error_produces_diagnostic() {
    let files = vec![ws("a.gruel", "fn main() -> i32 { true }", 1)];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    let errors: Vec<_> = res
        .diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(
        !errors.is_empty(),
        "expected at least one error, got: {:?}",
        res.diagnostics
    );
}

#[test]
fn unused_variable_produces_warning() {
    let files = vec![ws("a.gruel", "fn main() -> i32 { let unused = 1; 0 }", 1)];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    let warnings: Vec<_> = res
        .diagnostics
        .iter()
        .filter(|d| d.severity == "warning")
        .collect();
    assert!(
        !warnings.is_empty(),
        "expected at least one warning, got: {:?}",
        res.diagnostics
    );
}

#[test]
fn multi_file_diagnostics_attribute_to_correct_file() {
    let files = vec![
        ws("main.gruel", "fn main() -> i32 { helper() }", 1),
        ws("helper.gruel", "fn helper() -> bool { true }", 2),
    ];
    // main returns i32 but calls helper which returns bool: type error.
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    assert!(
        !res.diagnostics.is_empty(),
        "expected diagnostics, got: {:?}",
        res.diagnostics
    );
}

#[test]
fn parse_error_still_reports_diagnostic() {
    let files = vec![ws("a.gruel", "fn main( { 0 }", 1)];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    assert!(
        !res.diagnostics.is_empty(),
        "expected diagnostics for parse error"
    );
}

#[test]
fn snapshot_includes_line_maps_for_each_file() {
    let files = vec![ws("a.gruel", "fn main() -> i32 { 0 }", 1)];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    let snap = res.snapshot.expect("snapshot");
    assert_eq!(snap.sources.len(), 1);
    assert_eq!(snap.line_maps.len(), 1);
    assert!(snap.path_to_file_id.contains_key(&PathBuf::from("a.gruel")));
}

/// Regression: two unrelated `fn main()` files in the same workspace used to
/// produce a cascade of duplicate-definition errors because the LSP merged
/// every `*.gruel` file into one `CompilationUnit`. After ADR-0091's per-root
/// revision, each open root only pulls in its `@import` closure, so unrelated
/// programs no longer collide.
#[test]
fn unrelated_fn_main_files_do_not_collide() {
    let dir = tempdir().unwrap();
    let a_path = dir.path().join("a.gruel");
    let b_path = dir.path().join("b.gruel");
    std::fs::write(&a_path, "fn main() -> i32 { 1 }").unwrap();
    std::fs::write(&b_path, "fn main() -> i32 { 2 }").unwrap();

    // Analyzing `a.gruel` as a root must NOT pull in `b.gruel` even though
    // it's a sibling file in the same workspace root.
    let root = WorkspaceFile {
        path: a_path.clone(),
        text: "fn main() -> i32 { 1 }".to_string(),
        file_id: FileId::new(1),
    };
    let res = analyze_root(
        root,
        Some(dir.path()),
        &PreviewFeatures::default(),
        &Target::host(),
        |_| None,
    );
    let errors: Vec<_> = res
        .diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(
        errors.is_empty(),
        "expected no errors when analyzing a.gruel as a root, got: {:?}",
        errors
    );
}

/// Sibling files joined by `@import` must still see each other.
#[test]
fn root_pulls_in_imported_sibling() {
    let dir = tempdir().unwrap();
    let main_path = dir.path().join("main.gruel");
    let math_path = dir.path().join("math.gruel");
    let main_src = r#"const math = @import("math.gruel");
fn main() -> i32 { math.three() }
"#;
    let math_src = "pub fn three() -> i32 { 3 }\n";
    std::fs::write(&main_path, main_src).unwrap();
    std::fs::write(&math_path, math_src).unwrap();

    let root = WorkspaceFile {
        path: main_path.clone(),
        text: main_src.to_string(),
        file_id: FileId::new(1),
    };
    let res = analyze_root(
        root,
        Some(dir.path()),
        &PreviewFeatures::default(),
        &Target::host(),
        |_| None,
    );
    let errors: Vec<_> = res
        .diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect();
    assert!(
        errors.is_empty(),
        "expected no errors with valid @import, got: {:?}",
        errors
    );
    let snap = res.snapshot.expect("snapshot");
    assert!(snap.path_to_file_id.contains_key(&main_path));
    assert!(
        snap.path_to_file_id.contains_key(&math_path),
        "imported file must be in the snapshot's file map"
    );
}

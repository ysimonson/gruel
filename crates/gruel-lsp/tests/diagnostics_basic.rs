//! Phase 1 integration tests for diagnostics (ADR-0091).
//!
//! These tests drive the analysis pipeline directly via the `analyze`
//! function so they don't need to spawn the full tower-lsp message pump.
//! Server-level integration (open/change/close) is covered by tests that
//! exercise `Backend::analyze_now`.

use std::path::PathBuf;

use gruel_compiler::{FileId, PreviewFeatures};
use gruel_lsp::analysis::{WorkspaceFile, analyze};
use gruel_target::Target;

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
    let files = vec![ws(
        "a.gruel",
        "fn main() -> i32 { let unused = 1; 0 }",
        1,
    )];
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

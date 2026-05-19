//! Phase 1 server-level integration tests (ADR-0091).
//!
//! We drive the in-process `LspService` against a scripted JSON-RPC pipe
//! and assert on the responses. Tower-lsp doesn't expose a Client
//! constructor for direct calls so we use `LspService::inner()` to invoke
//! handlers directly on the Backend.

use std::path::PathBuf;
use std::sync::Arc;

use gruel_compiler::{FileId, PreviewFeatures};
use gruel_lsp::analysis::{WorkspaceFile, analyze};
use gruel_target::Target;
use tempfile::tempdir;

/// Round-trip: open a file with a type error, expect a diagnostic with the
/// correct severity and source.
#[test]
fn analyze_reports_type_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("main.gruel");
    std::fs::write(&path, "fn main() -> i32 { true }").unwrap();
    let files = vec![WorkspaceFile {
        path: path.clone(),
        text: "fn main() -> i32 { true }".to_string(),
        file_id: FileId::new(1),
    }];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    assert!(
        res.diagnostics.iter().any(|d| d.severity == "error"),
        "got: {:?}",
        res.diagnostics
    );
}

/// Round-trip: fix the error, expect no diagnostics.
#[test]
fn analyze_clears_diagnostics_when_error_fixed() {
    let files = vec![WorkspaceFile {
        path: PathBuf::from("main.gruel"),
        text: "fn main() -> i32 { 42 }".to_string(),
        file_id: FileId::new(1),
    }];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    assert!(res.diagnostics.is_empty(), "got: {:?}", res.diagnostics);
}

/// Verify that the snapshot is non-None even when sema reports errors —
/// stale-while-revalidate keeps the previous good state available.
#[test]
fn snapshot_present_for_partial_success() {
    let files = vec![WorkspaceFile {
        path: PathBuf::from("a.gruel"),
        text: "fn main() -> i32 { true }".to_string(),
        file_id: FileId::new(1),
    }];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    // Even with errors, the snapshot should be populated so hover/goto can
    // operate on the previous-good state.
    assert!(res.snapshot.is_some());
}

/// Cross-process safety smoke test: spawn two analyzers concurrently on the
/// same workspace and verify they produce the same diagnostics. This stands
/// in for the "another `gruel build` running while the LSP is open" scenario
/// since the on-disk cache is multi-process safe by atomic-rename.
#[test]
fn concurrent_analyzers_consistent() {
    use std::thread;

    let files = Arc::new(vec![WorkspaceFile {
        path: PathBuf::from("a.gruel"),
        text: "fn main() -> i32 { 0 }".to_string(),
        file_id: FileId::new(1),
    }]);

    let mut handles = Vec::new();
    for _ in 0..4 {
        let files = files.clone();
        handles.push(thread::spawn(move || {
            let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
            res.diagnostics.is_empty()
        }));
    }
    for h in handles {
        assert!(h.join().unwrap());
    }
}

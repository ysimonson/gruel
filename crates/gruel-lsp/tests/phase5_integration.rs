//! Phase 5 integration tests (ADR-0091).
//!
//! Covers references, workspace symbols, and multi-file diagnostics.

use std::path::PathBuf;

use gruel_compiler::{FileId, PreviewFeatures};
use gruel_lsp::analysis::{WorkspaceFile, analyze};
use gruel_lsp::references::references_at;
use gruel_lsp::workspace_symbols::{SymbolKind, workspace_symbols};
use gruel_target::Target;

fn snap_from(files: &[(String, &str, u32)]) -> gruel_lsp::analysis::Snapshot {
    let workspace: Vec<WorkspaceFile> = files
        .iter()
        .map(|(path, src, id)| WorkspaceFile {
            path: PathBuf::from(path.clone()),
            text: src.to_string(),
            file_id: FileId::new(*id),
        })
        .collect();
    let res = analyze(&workspace, &PreviewFeatures::default(), &Target::host());
    res.snapshot.expect("snapshot")
}

#[test]
fn references_for_function_in_two_files() {
    let main = "fn main() -> i32 { helper() }";
    let lib = "fn helper() -> i32 { 0 }";
    let snap = snap_from(&[
        ("main.gruel".to_string(), main, 1),
        ("lib.gruel".to_string(), lib, 2),
    ]);

    // Find references at the helper call in main.
    let byte = main.find("helper").unwrap() as u32;
    let refs = references_at(&snap.ast, &snap.interner, FileId::new(1), byte, true);
    // 1 def in lib + 1 call in main = 2 references
    assert!(refs.len() >= 2, "got: {:?}", refs);
    // Definitions and callers must come from both files.
    let unique_files: std::collections::HashSet<_> = refs.iter().map(|s| s.file_id).collect();
    assert!(unique_files.len() >= 2);
}

#[test]
fn workspace_symbols_returns_all_top_level() {
    let src = "fn foo() -> i32 { 0 }\nstruct Bar { x: i32 }\nconst N: i32 = 1;";
    let snap = snap_from(&[(String::from("a.gruel"), src, 1)]);

    let syms = workspace_symbols(&snap.ast, &snap.interner, "");
    let names: Vec<_> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"Bar"));
    assert!(names.contains(&"N"));
}

#[test]
fn workspace_symbols_filter_by_query() {
    let src = "fn foo() -> i32 { 0 }\nstruct Bar { x: i32 }";
    let snap = snap_from(&[(String::from("a.gruel"), src, 1)]);
    let syms = workspace_symbols(&snap.ast, &snap.interner, "bar");
    assert_eq!(syms.len(), 1);
    assert_eq!(syms[0].name, "Bar");
    assert_eq!(syms[0].kind, SymbolKind::Struct);
}

#[test]
fn multi_file_diagnostics_appear_in_each_files_namespace() {
    let main = "fn main() -> i32 { helper() }";
    let lib = "fn helper() -> bool { true }";
    let workspace: Vec<WorkspaceFile> = vec![
        WorkspaceFile {
            path: PathBuf::from("main.gruel"),
            text: main.to_string(),
            file_id: FileId::new(1),
        },
        WorkspaceFile {
            path: PathBuf::from("lib.gruel"),
            text: lib.to_string(),
            file_id: FileId::new(2),
        },
    ];
    let res = analyze(&workspace, &PreviewFeatures::default(), &Target::host());
    // main calls helper but helper returns bool — type error in main.
    assert!(!res.diagnostics.is_empty(), "got: {:?}", res.diagnostics);
}

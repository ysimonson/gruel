//! Phase 4 integration tests (ADR-0091).
//!
//! Covers expr-type hover, goto-definition for top-level items + locals,
//! and signatureHelp.

use std::path::PathBuf;

use gruel_compiler::{FileId, PreviewFeatures};
use gruel_lsp::analysis::{WorkspaceFile, analyze};
use gruel_lsp::goto::definition_at;
use gruel_lsp::hover::hover_at_with_expr_types;
use gruel_lsp::signature::signature_help;
use gruel_target::Target;

fn snapshot_for(source: &str) -> gruel_lsp::analysis::Snapshot {
    let files = vec![WorkspaceFile {
        path: PathBuf::from("main.gruel"),
        text: source.to_string(),
        file_id: FileId::new(1),
    }];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    res.snapshot.expect("snapshot")
}

#[test]
fn expr_type_hover_for_let_initializer() {
    let src = "fn main() -> i32 { let x = 42; x }";
    let snap = snapshot_for(src);
    // Cursor on `42` — expression type should be i32 (from AIR).
    let byte = src.find("42").unwrap() as u32 + 1;
    let h = hover_at_with_expr_types(
        &snap.ast,
        &snap.interner,
        &snap.expr_types,
        snap.type_pool.as_deref(),
        FileId::new(1),
        byte,
    );
    assert!(h.is_some(), "hover should return content for `42`");
}

#[test]
fn goto_function_definition() {
    let src = "fn helper() -> i32 { 0 }\nfn main() -> i32 { helper() }";
    let snap = snapshot_for(src);
    // Cursor on `helper` call.
    let byte = src.rfind("helper").unwrap() as u32 + 1;
    let def = definition_at(&snap.ast, &snap.interner, FileId::new(1), byte).unwrap();
    // Definition should be at the `helper` in `fn helper()`.
    let expected_start = src.find("helper").unwrap() as u32;
    assert_eq!(def.start, expected_start);
}

#[test]
fn goto_local_definition() {
    let src = "fn main() -> i32 { let answer = 42; answer }";
    let snap = snapshot_for(src);
    let byte = src.rfind("answer").unwrap() as u32 + 1;
    let def = definition_at(&snap.ast, &snap.interner, FileId::new(1), byte).unwrap();
    let expected_start = src.find("answer").unwrap() as u32;
    assert_eq!(def.start, expected_start);
}

#[test]
fn goto_parameter_definition() {
    let src = "fn double(x: i32) -> i32 { x + x }";
    let snap = snapshot_for(src);
    let byte = src.rfind('x').unwrap() as u32;
    let def = definition_at(&snap.ast, &snap.interner, FileId::new(1), byte).unwrap();
    let expected_start = src.find("x:").unwrap() as u32;
    assert_eq!(def.start, expected_start);
}

#[test]
fn signature_help_returns_callee_signature() {
    let src = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() -> i32 { add(1, 2) }";
    let snap = snapshot_for(src);
    let byte = src.find("add(1").unwrap() as u32 + 4;
    let sig = signature_help(&snap.ast, &snap.interner, FileId::new(1), byte).unwrap();
    assert!(sig.label.contains("a: i32"));
    assert!(sig.label.contains("b: i32"));
    assert_eq!(sig.parameters.len(), 2);
    assert_eq!(sig.active_parameter, 0);
}

#[test]
fn signature_help_advances_on_second_arg() {
    let src = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() -> i32 { add(1, 2) }";
    let snap = snapshot_for(src);
    let byte = src.find("add(1, 2)").unwrap() as u32 + 7;
    let sig = signature_help(&snap.ast, &snap.interner, FileId::new(1), byte).unwrap();
    assert_eq!(sig.active_parameter, 1);
}

#[test]
fn expr_types_side_table_populated() {
    let src = "fn main() -> i32 { 1 + 2 }";
    let snap = snapshot_for(src);
    // We should have type info for at least the `1`, `2`, and `1 + 2` AIR
    // instructions.
    assert!(
        !snap.expr_types.is_empty(),
        "expected expr_types to be populated"
    );
    assert!(snap.type_pool.is_some());
}

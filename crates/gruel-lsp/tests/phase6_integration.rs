//! Phase 6 integration tests (ADR-0091).
//!
//! Covers completion (trigger-character, member access, locals) and inlay
//! hints (inferred-type lets, parameter names).

use std::path::PathBuf;

use gruel_compiler::{FileId, PreviewFeatures};
use gruel_lsp::analysis::{WorkspaceFile, analyze};
use gruel_lsp::completion::{CompletionKind, complete_at};
use gruel_lsp::inlay_hints::{InlayKind, inlay_hints};
use gruel_target::Target;

fn snap_for(source: &str) -> gruel_lsp::analysis::Snapshot {
    let files = vec![WorkspaceFile {
        path: PathBuf::from("main.gruel"),
        text: source.to_string(),
        file_id: FileId::new(1),
    }];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    res.snapshot.expect("snapshot")
}

#[test]
fn at_completion_suggests_intrinsics() {
    let src = "fn main() -> i32 { 0 }";
    let snap = snap_for(src);
    let items = complete_at(&snap.ast, &snap.interner, FileId::new(1), 19, Some('@'));
    assert!(items.iter().any(|i| i.kind == CompletionKind::Intrinsic));
    assert!(items.iter().any(|i| i.label.starts_with("@dbg")));
}

#[test]
fn dot_completion_suggests_struct_fields() {
    let src = r#"struct Point { x: i32, y: i32 }
fn main() -> i32 { 0 }"#;
    let snap = snap_for(src);
    let items = complete_at(&snap.ast, &snap.interner, FileId::new(1), 30, Some('.'));
    let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"x"));
    assert!(labels.contains(&"y"));
}

#[test]
fn generic_completion_includes_locals_and_items() {
    let src = "fn helper() -> i32 { 0 }\nfn main() -> i32 { let answer = 42; answer }";
    let snap = snap_for(src);
    let byte = src.rfind("answer").unwrap() as u32;
    let items = complete_at(&snap.ast, &snap.interner, FileId::new(1), byte, None);
    let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"answer"));
    assert!(labels.contains(&"helper"));
    assert!(labels.contains(&"main"));
    assert!(labels.contains(&"if"));
}

#[test]
fn inlay_hint_for_inferred_let() {
    let src = "fn main() -> i32 { let answer = 42; answer }";
    let snap = snap_for(src);
    let hints = inlay_hints(
        &snap.ast,
        &snap.interner,
        &snap.expr_types,
        snap.type_pool.as_deref(),
        FileId::new(1),
    );
    let type_hints: Vec<_> = hints.iter().filter(|h| h.kind == InlayKind::Type).collect();
    assert!(!type_hints.is_empty(), "expected a type hint");
    assert!(type_hints.iter().any(|h| h.label.contains("i32")));
}

#[test]
fn inlay_hint_for_unnamed_call_args() {
    let src = "fn add(x: i32, y: i32) -> i32 { x + y }\nfn main() -> i32 { add(1, 2) }";
    let snap = snap_for(src);
    let hints = inlay_hints(
        &snap.ast,
        &snap.interner,
        &snap.expr_types,
        snap.type_pool.as_deref(),
        FileId::new(1),
    );
    let param_hints: Vec<_> = hints
        .iter()
        .filter(|h| h.kind == InlayKind::Parameter)
        .collect();
    assert!(param_hints.iter().any(|h| h.label == "x:"));
    assert!(param_hints.iter().any(|h| h.label == "y:"));
}

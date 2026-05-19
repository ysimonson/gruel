//! Phase 3 integration tests for hover (ADR-0091).

use std::path::PathBuf;
use std::sync::Arc;

use gruel_compiler::{FileId, PreviewFeatures};
use gruel_lsp::analysis::{WorkspaceFile, analyze};
use gruel_lsp::hover::hover_at;
use gruel_target::Target;

fn snapshot_for(source: &str) -> (Arc<gruel_lsp::analysis::Snapshot>, FileId, u32) {
    let files = vec![WorkspaceFile {
        path: PathBuf::from("main.gruel"),
        text: source.to_string(),
        file_id: FileId::new(1),
    }];
    let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
    let snap = res.snapshot.expect("snapshot");
    (Arc::new(snap), FileId::new(1), 0)
}

#[test]
fn hover_function_after_full_compile() {
    let src = "/// Entry point.\nfn main() -> i32 { 0 }";
    let (snap, fid, _) = snapshot_for(src);
    let byte = src.find("main").unwrap() as u32;
    let h = hover_at(&snap.ast, &snap.interner, fid, byte).unwrap();
    assert!(h.markdown.contains("fn main"));
    assert!(h.markdown.contains("Entry point"));
}

#[test]
fn hover_struct_with_doc() {
    let src = "/// A 2D point.\nstruct Point { x: i32, y: i32 }";
    let (snap, fid, _) = snapshot_for(src);
    let byte = src.find("Point").unwrap() as u32;
    let h = hover_at(&snap.ast, &snap.interner, fid, byte).unwrap();
    assert!(h.markdown.contains("struct Point"));
    assert!(h.markdown.contains("A 2D point"));
}

#[test]
fn hover_interface_method_sig() {
    let src = r#"
interface Drop {
    /// Releases the resource.
    fn __drop(self);
}
"#;
    let (snap, fid, _) = snapshot_for(src);
    let byte = src.find("__drop").unwrap() as u32;
    let h = hover_at(&snap.ast, &snap.interner, fid, byte).unwrap();
    assert!(h.markdown.contains("__drop"));
    assert!(h.markdown.contains("Releases the resource"));
}

#[test]
fn hover_struct_field_returns_field_signature() {
    let src = "struct Point {\n    /// X coordinate.\n    pub x: i32,\n}";
    let (snap, fid, _) = snapshot_for(src);
    let byte = src.find("x: i32").unwrap() as u32;
    let h = hover_at(&snap.ast, &snap.interner, fid, byte).unwrap();
    assert!(h.markdown.contains("x: i32"), "got: {}", h.markdown);
    assert!(h.markdown.contains("X coordinate"));
}

//! Integration tests for `textDocument/formatting` (ADR-0093 Phase 7).
//!
//! These spin up a real `LspService` so the `Backend` is wired exactly as
//! production, populate the in-memory document store directly, and call
//! `Backend::formatting` end-to-end.

use gruel_compiler::PreviewFeatures;
use gruel_lsp::document::DocState;
use gruel_lsp::server::Backend;
use lsp_types::{
    DocumentFormattingParams, FormattingOptions, TextDocumentIdentifier, TextEdit, Url,
    WorkDoneProgressParams,
};
use tower_lsp::{LanguageServer, LspService};

fn format_params(uri: Url) -> DocumentFormattingParams {
    DocumentFormattingParams {
        text_document: TextDocumentIdentifier { uri },
        options: FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..FormattingOptions::default()
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

/// Apply a list of edits to `original` and return the resulting text. Edits
/// are line-aligned (column 0), so the application is line-based. Applying
/// in *reverse* order keeps earlier edits' line numbers stable as later
/// ones grow/shrink the document.
fn apply_edits(original: &str, mut edits: Vec<TextEdit>) -> String {
    edits.sort_by_key(|e| std::cmp::Reverse(e.range.start.line));
    let mut lines: Vec<String> = original
        .split_inclusive('\n')
        .map(|s| s.to_string())
        .collect();
    for edit in edits {
        let start = edit.range.start.line as usize;
        let end = edit.range.end.line as usize;
        let replacement: Vec<String> = if edit.new_text.is_empty() {
            Vec::new()
        } else {
            edit.new_text
                .split_inclusive('\n')
                .map(|s| s.to_string())
                .collect()
        };
        lines.splice(start..end, replacement);
    }
    lines.concat()
}

#[tokio::test]
async fn formatting_basic_messy_input_produces_canonical_edits() {
    let (service, _socket) =
        LspService::new(|client| Backend::new(client, PreviewFeatures::default()));
    let backend = service.inner();
    let uri = Url::parse("file:///test/basic.gruel").unwrap();
    let original = "fn   main(  )   ->   i32   { 1+2*3 }".to_string();
    backend.docs.insert(
        uri.clone(),
        DocState::new(uri.clone(), original.clone(), 1, true),
    );

    let edits = backend
        .formatting(format_params(uri))
        .await
        .expect("formatting failed")
        .expect("expected Some(edits)");

    let applied = apply_edits(&original, edits);
    assert_eq!(applied, "fn main() -> i32 {\n    1 + 2 * 3\n}\n");
}

#[tokio::test]
async fn formatting_unchanged_returns_empty_edits() {
    let (service, _socket) =
        LspService::new(|client| Backend::new(client, PreviewFeatures::default()));
    let backend = service.inner();
    let uri = Url::parse("file:///test/clean.gruel").unwrap();
    let canonical = "fn main() -> i32 {\n    0\n}\n".to_string();
    backend
        .docs
        .insert(uri.clone(), DocState::new(uri.clone(), canonical, 1, true));

    let edits = backend
        .formatting(format_params(uri))
        .await
        .expect("formatting failed")
        .expect("expected Some(empty edits)");

    assert!(
        edits.is_empty(),
        "already-canonical file should return Some(vec![]); got {} edits",
        edits.len()
    );
}

#[tokio::test]
async fn formatting_parse_error_returns_none() {
    let (service, _socket) =
        LspService::new(|client| Backend::new(client, PreviewFeatures::default()));
    let backend = service.inner();
    let uri = Url::parse("file:///test/broken.gruel").unwrap();
    // Missing closing brace — won't lex/parse cleanly.
    let broken = "fn main() -> i32 { let x = ".to_string();
    backend
        .docs
        .insert(uri.clone(), DocState::new(uri.clone(), broken, 1, true));

    let result = backend
        .formatting(format_params(uri))
        .await
        .expect("formatting returned an LSP error");

    assert!(
        result.is_none(),
        "broken source must format-on-save without clobbering the buffer; got {:?}",
        result
    );
}

#[tokio::test]
async fn formatting_under_utf16_encoding_returns_valid_edits() {
    // The diff helper produces column-0 ranges only, so the negotiated
    // encoding never affects edit ranges. Still: assert behavior matches
    // the UTF-8 path so a future change that touches the ranges (e.g. for
    // trailing-byte adjustments) trips this test.
    let (service, _socket) =
        LspService::new(|client| Backend::new(client, PreviewFeatures::default()));
    let backend = service.inner();
    {
        let mut enc = backend.encoding.lock().await;
        *enc = gruel_lsp::position::PositionEncoding::Utf16;
    }
    let uri = Url::parse("file:///test/utf16.gruel").unwrap();
    // String literal contains a multi-byte character — exercises a real
    // UTF-8 byte vs UTF-16 code-unit distinction in the input buffer.
    let original = "fn main() -> i32 { let s = \"héllo\"; 0 }".to_string();
    backend.docs.insert(
        uri.clone(),
        DocState::new(uri.clone(), original.clone(), 1, true),
    );

    let edits = backend
        .formatting(format_params(uri))
        .await
        .expect("formatting failed")
        .expect("expected Some(edits)");

    let applied = apply_edits(&original, edits);
    assert_eq!(
        applied,
        "fn main() -> i32 {\n    let s = \"héllo\";\n    0\n}\n"
    );
}

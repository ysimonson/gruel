//! Phase 2 integration tests for code actions (ADR-0091).
//!
//! The real compiler produces no `MachineApplicable` suggestions yet — the
//! suggestion plumbing exists end-to-end but Sema doesn't attach any.
//! These tests therefore synthesise diagnostics that mimic what a future
//! suggestion-emitting sema would produce, and verify the LSP layer
//! converts them correctly.

use std::path::PathBuf;

use dashmap::DashMap;
use gruel_compiler::JsonSuggestion;
use gruel_lsp::code_actions::code_actions_for_range;
use gruel_lsp::diagnostics::suggestions_from_diagnostic_data;
use gruel_lsp::document::DocState;
use gruel_lsp::position::PositionEncoding;
use lsp_types::{
    CodeActionKind, CodeActionOrCommand, Diagnostic, DiagnosticSeverity, Position, Range, Url,
};

fn synth_diag(start: u32, end: u32, applicability: &str) -> Diagnostic {
    let suggestion = JsonSuggestion {
        message: "did you mean `i32`?".to_string(),
        file: "main.gruel".to_string(),
        start,
        end,
        replacement: "i32".to_string(),
        applicability: applicability.to_string(),
    };
    Diagnostic {
        range: Range {
            start: Position {
                line: 0,
                character: start,
            },
            end: Position {
                line: 0,
                character: end,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("gruel".to_string()),
        message: "type mismatch".to_string(),
        related_information: None,
        tags: None,
        data: Some(serde_json::to_value(vec![suggestion]).unwrap()),
    }
}

#[test]
fn suggestion_roundtrip_through_diagnostic_data() {
    let diag = synth_diag(13, 16, "MachineApplicable");
    let s = suggestions_from_diagnostic_data(diag.data.as_ref().unwrap());
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].applicability, "MachineApplicable");
    assert_eq!(s[0].replacement, "i32");
}

#[test]
fn machine_applicable_marked_preferred() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("main.gruel");
    std::fs::write(&path, "fn main() -> i64 { 0 }").unwrap();

    let docs = DashMap::new();
    let diags = vec![synth_diag(13, 16, "MachineApplicable")];
    let range = Range {
        start: Position {
            line: 0,
            character: 14,
        },
        end: Position {
            line: 0,
            character: 14,
        },
    };
    let actions = code_actions_for_range(
        &diags,
        range,
        &docs,
        PositionEncoding::Utf8,
        Some(tmp.path()),
    );
    assert_eq!(actions.len(), 1);
    let CodeActionOrCommand::CodeAction(ref a) = actions[0] else {
        panic!("expected CodeAction");
    };
    assert_eq!(a.is_preferred, Some(true));
    assert_eq!(a.kind, Some(CodeActionKind::QUICKFIX));
    assert_eq!(a.title, "did you mean `i32`?");
}

#[test]
fn non_machine_applicable_not_preferred() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("main.gruel");
    std::fs::write(&path, "fn main() -> i64 { 0 }").unwrap();

    let docs = DashMap::new();
    let diags = vec![synth_diag(13, 16, "MaybeIncorrect")];
    let range = Range {
        start: Position {
            line: 0,
            character: 14,
        },
        end: Position {
            line: 0,
            character: 14,
        },
    };
    let actions = code_actions_for_range(
        &diags,
        range,
        &docs,
        PositionEncoding::Utf8,
        Some(tmp.path()),
    );
    assert_eq!(actions.len(), 1);
    let CodeActionOrCommand::CodeAction(ref a) = actions[0] else {
        panic!("expected CodeAction");
    };
    assert!(a.is_preferred.is_none() || a.is_preferred == Some(false));
}

#[test]
fn uses_open_doc_line_map_when_available() {
    let url = Url::parse("file:///tmp/test_lsp/main.gruel").unwrap();
    let text = "fn main() -> i64 {\n    0\n}".to_string();
    let docs = DashMap::new();
    docs.insert(url.clone(), DocState::new(url.clone(), text, 1, true));

    let suggestion = JsonSuggestion {
        message: "fix".to_string(),
        file: "/tmp/test_lsp/main.gruel".to_string(),
        start: 23, // After `\n    ` on line 1, the `0` byte
        end: 24,
        replacement: "42".to_string(),
        applicability: "MachineApplicable".to_string(),
    };
    let diag = Diagnostic {
        range: Range {
            start: Position {
                line: 1,
                character: 4,
            },
            end: Position {
                line: 1,
                character: 5,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("gruel".to_string()),
        message: "fix".to_string(),
        related_information: None,
        tags: None,
        data: Some(serde_json::to_value(vec![suggestion]).unwrap()),
    };

    let range = Range {
        start: Position {
            line: 1,
            character: 4,
        },
        end: Position {
            line: 1,
            character: 4,
        },
    };
    let actions = code_actions_for_range(
        &[diag],
        range,
        &docs,
        PositionEncoding::Utf8,
        Some(&PathBuf::from("/tmp/test_lsp")),
    );
    assert_eq!(actions.len(), 1);
    let CodeActionOrCommand::CodeAction(ref a) = actions[0] else {
        panic!("expected CodeAction");
    };
    let edit = a.edit.as_ref().unwrap();
    let changes = edit.changes.as_ref().unwrap();
    let (_uri, edits) = changes.iter().next().unwrap();
    // Position should be on line 1, character 4 (after `    `).
    assert_eq!(
        edits[0].range.start,
        Position {
            line: 1,
            character: 4
        }
    );
}

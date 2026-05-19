//! Code actions for diagnostic suggestions (ADR-0091 Phase 2).
//!
//! Every `JsonDiagnostic.suggestions` entry the compiler produces becomes
//! a `quickfix` code action when the editor's cursor (or selected range)
//! overlaps the diagnostic's range. We stashed the suggestions on the
//! diagnostic's `data` field in Phase 1, so we just decode them here.

use std::collections::HashMap;
use std::path::Path;

use dashmap::DashMap;
use gruel_compiler::JsonSuggestion;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic, Range, TextEdit, Url,
    WorkspaceEdit,
};

use crate::diagnostics::suggestions_from_diagnostic_data;
use crate::document::DocState;
use crate::position::{PositionEncoding, byte_to_position};

/// Build LSP `CodeAction`s for every `JsonSuggestion` attached to
/// `diagnostics` (via Phase 1's `data` field) whose primary range overlaps
/// the requested `range`.
///
/// `docs` is used to look up `LineMap`s so we can convert the suggestion's
/// byte offsets to LSP positions. Falls back to fetching the source from
/// disk if the file isn't open in the editor.
pub fn code_actions_for_range(
    diagnostics: &[Diagnostic],
    range: Range,
    docs: &DashMap<Url, DocState>,
    encoding: PositionEncoding,
    workspace_root: Option<&Path>,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    for diag in diagnostics {
        if !ranges_overlap(diag.range, range) {
            continue;
        }
        let Some(data) = diag.data.as_ref() else {
            continue;
        };
        for suggestion in suggestions_from_diagnostic_data(data) {
            if let Some(action) =
                build_code_action(diag, &suggestion, docs, encoding, workspace_root)
            {
                actions.push(CodeActionOrCommand::CodeAction(action));
            }
        }
    }
    actions
}

fn build_code_action(
    diag: &Diagnostic,
    suggestion: &JsonSuggestion,
    docs: &DashMap<Url, DocState>,
    encoding: PositionEncoding,
    workspace_root: Option<&Path>,
) -> Option<CodeAction> {
    let abs_path = resolve_path(&suggestion.file, workspace_root)?;
    let uri = Url::from_file_path(&abs_path).ok()?;
    let range = position_range_for_suggestion(&uri, suggestion, docs, encoding)?;

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    changes.insert(
        uri,
        vec![TextEdit {
            range,
            new_text: suggestion.replacement.clone(),
        }],
    );

    let is_preferred = match suggestion.applicability.as_str() {
        "MachineApplicable" => Some(true),
        _ => None,
    };

    Some(CodeAction {
        title: suggestion.message.clone(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }),
        command: None,
        is_preferred,
        disabled: None,
        data: None,
    })
}

fn position_range_for_suggestion(
    uri: &Url,
    suggestion: &JsonSuggestion,
    docs: &DashMap<Url, DocState>,
    encoding: PositionEncoding,
) -> Option<Range> {
    if let Some(doc) = docs.get(uri) {
        let start = byte_to_position(&doc.line_map, &doc.text, suggestion.start, encoding);
        let end = byte_to_position(&doc.line_map, &doc.text, suggestion.end, encoding);
        return Some(Range { start, end });
    }
    // Fallback: read from disk so we can compute positions.
    let path = uri.to_file_path().ok()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let line_map = crate::position::LineMap::new(&text);
    let start = byte_to_position(&line_map, &text, suggestion.start, encoding);
    let end = byte_to_position(&line_map, &text, suggestion.end, encoding);
    Some(Range { start, end })
}

fn ranges_overlap(a: Range, b: Range) -> bool {
    !(a.end < b.start || b.end < a.start)
}

fn resolve_path(raw: &str, workspace_root: Option<&Path>) -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(raw);
    if p.is_absolute() {
        return Some(p);
    }
    if let Some(root) = workspace_root {
        return Some(root.join(p));
    }
    std::env::current_dir().ok().map(|cwd| cwd.join(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{DiagnosticSeverity, Position};

    fn make_diag_with_suggestion(start: u32, end: u32) -> Diagnostic {
        let suggestion = JsonSuggestion {
            message: "did you mean `i32`?".to_string(),
            file: "main.gruel".to_string(),
            start,
            end,
            replacement: "i32".to_string(),
            applicability: "MachineApplicable".to_string(),
        };
        Diagnostic {
            range: Range {
                start: Position { line: 0, character: start },
                end: Position { line: 0, character: end },
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
    fn produces_action_for_overlapping_diagnostic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("main.gruel");
        std::fs::write(&path, "fn main() -> i64 { 0 }").unwrap();

        let docs = DashMap::new();
        let diags = vec![make_diag_with_suggestion(13, 16)];
        let range = Range {
            start: Position { line: 0, character: 13 },
            end: Position { line: 0, character: 13 },
        };
        let actions = code_actions_for_range(
            &diags,
            range,
            &docs,
            PositionEncoding::Utf8,
            Some(tmp.path()),
        );
        assert_eq!(actions.len(), 1, "got: {:?}", actions);
        let CodeActionOrCommand::CodeAction(ref action) = actions[0] else {
            panic!("expected CodeAction");
        };
        assert_eq!(action.is_preferred, Some(true));
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
    }

    #[test]
    fn no_action_when_range_disjoint() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("main.gruel");
        std::fs::write(&path, "fn main() -> i64 { 0 }").unwrap();

        let docs = DashMap::new();
        let diags = vec![make_diag_with_suggestion(13, 16)];
        let range = Range {
            start: Position { line: 5, character: 0 },
            end: Position { line: 5, character: 0 },
        };
        let actions = code_actions_for_range(
            &diags,
            range,
            &docs,
            PositionEncoding::Utf8,
            Some(tmp.path()),
        );
        assert!(actions.is_empty());
    }
}

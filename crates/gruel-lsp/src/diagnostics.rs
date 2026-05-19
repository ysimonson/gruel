//! Convert Gruel `JsonDiagnostic` values to LSP `Diagnostic` values
//! (ADR-0091).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gruel_compiler::{JsonDiagnostic, JsonSpan, JsonSuggestion};
use lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString,
    Position, Range, Url,
};

use crate::position::PositionEncoding;

/// Group of diagnostics keyed by file path string.
pub type DiagnosticsByFile = HashMap<PathBuf, Vec<Diagnostic>>;

fn make_position(line: u32, column: u32) -> Position {
    // JsonSpan uses 1-indexed line/column. LSP uses 0-indexed.
    Position {
        line: line.saturating_sub(1),
        character: column.saturating_sub(1),
    }
}

fn range_from_span(span: &JsonSpan, _encoding: PositionEncoding) -> Range {
    // We do not know the source text here to compute UTF-16 columns; for
    // best fidelity callers should remap spans through `position::byte_to_position`
    // when source is available. The JsonSpan's `line` / `column` are byte-based
    // 1-indexed; we use them as a best-effort UTF-8 mapping (LSP clients that
    // negotiate UTF-8 see correct positions; UTF-16 clients see byte-position
    // approximation, which is upgraded by the worker via `remap_diagnostic_range`).
    Range {
        start: make_position(span.line, span.column),
        end: make_position(span.line, span.column),
    }
}

/// Convert one Gruel `JsonDiagnostic` to an LSP `Diagnostic` and the file
/// path it belongs to. Returns `None` if the diagnostic has no primary span.
pub fn to_lsp_diagnostic(
    diag: &JsonDiagnostic,
    workspace_root: Option<&Path>,
) -> Option<(PathBuf, Diagnostic)> {
    let primary = diag.spans.iter().find(|s| s.primary)?;
    let range = range_from_span(primary, PositionEncoding::Utf8);

    let severity = match diag.severity {
        "error" => Some(DiagnosticSeverity::ERROR),
        "warning" => Some(DiagnosticSeverity::WARNING),
        _ => None,
    };

    let mut message = diag.message.clone();
    for note in &diag.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    for help in &diag.helps {
        message.push_str("\nhelp: ");
        message.push_str(help);
    }

    let related = diag
        .spans
        .iter()
        .filter(|s| !s.primary)
        .filter_map(|s| {
            let path = resolve_path(&s.file, workspace_root)?;
            let uri = Url::from_file_path(&path).ok()?;
            Some(DiagnosticRelatedInformation {
                location: Location {
                    uri,
                    range: range_from_span(s, PositionEncoding::Utf8),
                },
                message: s.label.clone().unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let related = if related.is_empty() {
        None
    } else {
        Some(related)
    };

    let path = resolve_path(&primary.file, workspace_root)?;

    let code = if diag.code.is_empty() {
        None
    } else {
        Some(NumberOrString::String(diag.code.clone()))
    };

    let suggestions_data = serde_json::to_value(&diag.suggestions).ok();

    Some((
        path,
        Diagnostic {
            range,
            severity,
            code,
            code_description: None,
            source: Some("gruel".to_string()),
            message,
            related_information: related,
            tags: None,
            data: suggestions_data,
        },
    ))
}

fn resolve_path(raw: &str, workspace_root: Option<&Path>) -> Option<PathBuf> {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        return Some(p);
    }
    if let Some(root) = workspace_root {
        return Some(root.join(p));
    }
    std::env::current_dir().ok().map(|cwd| cwd.join(p))
}

/// Group LSP diagnostics by file path.
pub fn group_by_file(
    diagnostics: impl IntoIterator<Item = JsonDiagnostic>,
    workspace_root: Option<&Path>,
) -> DiagnosticsByFile {
    let mut out: DiagnosticsByFile = HashMap::new();
    for d in diagnostics {
        if let Some((path, diag)) = to_lsp_diagnostic(&d, workspace_root) {
            out.entry(path).or_default().push(diag);
        }
    }
    out
}

/// Deserialize the `JsonSuggestion[]` from a Diagnostic.data field
/// (Phase 2 carries them so codeAction can read them back).
pub fn suggestions_from_diagnostic_data(value: &serde_json::Value) -> Vec<JsonSuggestion> {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_compiler::JsonSpan;

    fn make_diag() -> JsonDiagnostic {
        JsonDiagnostic {
            code: "E0001".to_string(),
            message: "type mismatch".to_string(),
            severity: "error",
            spans: vec![JsonSpan {
                file: "main.gruel".to_string(),
                start: 10,
                end: 12,
                line: 2,
                column: 5,
                label: None,
                primary: true,
            }],
            suggestions: vec![],
            notes: vec!["expected i32".to_string()],
            helps: vec![],
        }
    }

    #[test]
    fn basic_mapping() {
        let d = make_diag();
        let (path, lsp) = to_lsp_diagnostic(&d, Some(Path::new("/work"))).unwrap();
        assert_eq!(path, PathBuf::from("/work/main.gruel"));
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::ERROR));
        assert!(lsp.message.contains("type mismatch"));
        assert!(lsp.message.contains("note: expected i32"));
        assert_eq!(
            lsp.range.start,
            Position {
                line: 1,
                character: 4
            }
        );
    }
}

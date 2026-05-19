//! Compile analysis worker (ADR-0091).
//!
//! Given a snapshot of the workspace's source files (open buffers + on-disk
//! fallback), compile through the frontend and return per-file diagnostics
//! plus a successful `Snapshot` when possible.

use std::path::PathBuf;
use std::sync::Arc;

use gruel_compiler::{
    FileId, JsonDiagnostic, MultiFileJsonFormatter, PreviewFeatures, SourceFile, SourceInfo,
    compile_frontend_from_ast_with_options_full_target, merge_symbols,
    parse_all_files_with_preview,
};
use gruel_parser::ast::Ast;
use gruel_target::Target;
use rustc_hash::FxHashMap;

use crate::position::LineMap;

/// One source file the worker can see (either an open editor buffer or a
/// file on disk).
#[derive(Debug, Clone)]
pub struct WorkspaceFile {
    pub path: PathBuf,
    pub text: String,
    /// The file_id assigned to this file (stable across compiles within a
    /// single workspace pass).
    pub file_id: FileId,
}

/// Successful compile snapshot (sema completed even if errors were reported).
///
/// The LSP keeps the most recent `Snapshot` available via `ArcSwap` so that
/// hover/goto/references can serve queries while a new compile is in flight.
#[derive(Debug)]
pub struct Snapshot {
    pub ast: Ast,
    /// Shared interner used to resolve identifiers from the AST.
    ///
    /// Phase 1 only walks the AST for hover/goto; the interner is owned here
    /// so later phases can re-resolve `Spur`s without re-parsing.
    pub interner: Arc<lasso::ThreadedRodeo>,
    /// File contents at the time this snapshot was captured.
    pub sources: FxHashMap<FileId, WorkspaceFile>,
    /// Source path -> file_id reverse map.
    pub path_to_file_id: FxHashMap<PathBuf, FileId>,
    /// Line maps for each open file.
    pub line_maps: FxHashMap<FileId, LineMap>,
}

/// Result of one compile pass.
pub struct AnalysisResult {
    pub diagnostics: Vec<JsonDiagnostic>,
    pub snapshot: Option<Snapshot>,
}

/// Compile the given workspace files via the frontend and return
/// diagnostics + an optional successful snapshot.
pub fn analyze(
    files: &[WorkspaceFile],
    preview_features: &PreviewFeatures,
    target: &Target,
) -> AnalysisResult {
    if files.is_empty() {
        return AnalysisResult {
            diagnostics: vec![],
            snapshot: None,
        };
    }

    // Build SourceFile views.
    let sources: Vec<SourceFile<'_>> = files
        .iter()
        .map(|f| SourceFile::new(path_str(&f.path), f.text.as_str(), f.file_id))
        .collect();

    // Source info for diagnostic formatting.
    let source_infos: Vec<(FileId, SourceInfo<'_>)> = files
        .iter()
        .map(|f| (f.file_id, SourceInfo::new(f.text.as_str(), path_str(&f.path))))
        .collect();
    let formatter = MultiFileJsonFormatter::new(source_infos);

    let mut diagnostics = Vec::new();

    // Parse all files with the shared interner.
    let parsed = match parse_all_files_with_preview(&sources, preview_features) {
        Ok(p) => p,
        Err(errors) => {
            for e in errors.iter() {
                diagnostics.push(formatter.format_error(e));
            }
            return AnalysisResult {
                diagnostics,
                snapshot: None,
            };
        }
    };

    // Merge symbols.
    let merged = match merge_symbols(parsed) {
        Ok(m) => m,
        Err(errors) => {
            for e in errors.iter() {
                diagnostics.push(formatter.format_error(e));
            }
            return AnalysisResult {
                diagnostics,
                snapshot: None,
            };
        }
    };

    let ast_for_snapshot = merged.ast.clone();

    let state = match compile_frontend_from_ast_with_options_full_target(
        merged.ast,
        merged.interner,
        preview_features,
        true, // suppress comptime @dbg print
        target,
    ) {
        Ok(state) => state,
        Err(errors) => {
            for e in errors.iter() {
                diagnostics.push(formatter.format_error(e));
            }
            // Sema failed before producing a CompileState; we still need an
            // AST-only snapshot for syntactic queries. Re-parse to recover an
            // interner — simpler than threading a clone through every error
            // path.
            return AnalysisResult {
                diagnostics,
                snapshot: build_ast_snapshot(files, preview_features)
                    .map(|s| s)
                    .ok(),
            };
        }
    };

    for warning in &state.warnings {
        diagnostics.push(formatter.format_warning(warning));
    }

    let interner_for_snapshot = Arc::new(state.interner);

    AnalysisResult {
        diagnostics,
        snapshot: Some(build_snapshot(
            files,
            ast_for_snapshot,
            interner_for_snapshot,
        )),
    }
}

/// Re-parse the workspace once to produce an AST-only snapshot (used when
/// sema fails — diagnostics already cover the errors; we still want the AST
/// for syntactic LSP queries).
fn build_ast_snapshot(
    files: &[WorkspaceFile],
    preview_features: &PreviewFeatures,
) -> Result<Snapshot, ()> {
    let sources: Vec<SourceFile<'_>> = files
        .iter()
        .map(|f| SourceFile::new(path_str(&f.path), f.text.as_str(), f.file_id))
        .collect();
    let parsed = parse_all_files_with_preview(&sources, preview_features).map_err(|_| ())?;
    let merged = merge_symbols(parsed).map_err(|_| ())?;
    let interner = Arc::new(merged.interner);
    Ok(build_snapshot(files, merged.ast, interner))
}

fn build_snapshot(
    files: &[WorkspaceFile],
    ast: Ast,
    interner: Arc<lasso::ThreadedRodeo>,
) -> Snapshot {
    let mut sources: FxHashMap<FileId, WorkspaceFile> = FxHashMap::default();
    let mut path_to_file_id: FxHashMap<PathBuf, FileId> = FxHashMap::default();
    let mut line_maps: FxHashMap<FileId, LineMap> = FxHashMap::default();
    for f in files {
        line_maps.insert(f.file_id, LineMap::new(&f.text));
        path_to_file_id.insert(f.path.clone(), f.file_id);
        sources.insert(f.file_id, f.clone());
    }
    Snapshot {
        ast,
        interner,
        sources,
        path_to_file_id,
        line_maps,
    }
}

fn path_str(path: &std::path::Path) -> &str {
    path.to_str().unwrap_or("<non-utf8>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wsf(path: &str, text: &str, id: u32) -> WorkspaceFile {
        WorkspaceFile {
            path: PathBuf::from(path),
            text: text.to_string(),
            file_id: FileId::new(id),
        }
    }

    #[test]
    fn compiles_clean_program() {
        let files = vec![wsf("main.gruel", "fn main() -> i32 { 0 }", 1)];
        let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
        assert!(
            res.diagnostics.is_empty(),
            "expected no diagnostics, got: {:?}",
            res.diagnostics
        );
        assert!(res.snapshot.is_some());
    }

    #[test]
    fn reports_type_error() {
        let files = vec![wsf("main.gruel", "fn main() -> i32 { true }", 1)];
        let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
        assert!(
            !res.diagnostics.is_empty(),
            "expected diagnostics for type error"
        );
        assert!(res.diagnostics.iter().any(|d| d.severity == "error"));
    }

    #[test]
    fn reports_warnings() {
        let files = vec![wsf(
            "main.gruel",
            "fn main() -> i32 { let x = 42; 0 }",
            1,
        )];
        let res = analyze(&files, &PreviewFeatures::default(), &Target::host());
        // Unused-variable warning.
        assert!(res.diagnostics.iter().any(|d| d.severity == "warning"));
    }
}

//! Spec-corpus diagnostic differential (ADR-0091).
//!
//! For every spec test and UI test, compile the source via:
//!
//! 1. The `gruel check` code path (the canonical pipeline).
//! 2. The in-process LSP backend (`analyze`).
//!
//! Collect each side's diagnostics and assert they agree on the
//! normalized tuple `(file, line, col, severity, code)`. The bodies of
//! the two paths share a sema call, so any divergence is a bug — either
//! the LSP-side mapping dropped a diagnostic, or the side-table
//! population perturbed sema output.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;

use gruel_compiler::{
    FileId, MultiFileJsonFormatter, PreviewFeature, PreviewFeatures, SourceFile, SourceInfo,
    compile_frontend_from_ast_with_options_full_target, merge_symbols,
    parse_all_files_with_preview, prepend_prelude,
};
use gruel_lsp::analysis::{WorkspaceFile, analyze};
use gruel_target::Target;
use gruel_test_runner::{Case, load_test_files};
use rayon::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct NormalizedDiag {
    file: String,
    line: u32,
    column: u32,
    severity: String,
    code: String,
}

fn cli_diagnostics(case: &Case) -> BTreeSet<NormalizedDiag> {
    let path = "case.gruel";
    let source = case.source.as_str();
    let preview = build_preview(case);

    let sources = vec![SourceFile::new(path, source, FileId::new(1))];
    let source_infos = vec![(FileId::new(1), SourceInfo::new(source, path))];
    let formatter = MultiFileJsonFormatter::new(source_infos);

    let parsed = match parse_all_files_with_preview(&sources, &preview) {
        Ok(p) => p,
        Err(errors) => {
            return errors
                .iter()
                .map(|e| normalize(&formatter.format_error(e)))
                .collect();
        }
    };
    let merged = match merge_symbols(parsed) {
        Ok(m) => m,
        Err(errors) => {
            return errors
                .iter()
                .map(|e| normalize(&formatter.format_error(e)))
                .collect();
        }
    };
    let (ast, interner) = match prepend_prelude(merged.ast, merged.interner, &preview) {
        Ok(p) => p,
        Err(errors) => {
            return errors
                .iter()
                .map(|e| normalize(&formatter.format_error(e)))
                .collect();
        }
    };
    let state = match compile_frontend_from_ast_with_options_full_target(
        ast,
        interner,
        &preview,
        true,
        &Target::host(),
    ) {
        Ok(state) => state,
        Err(errors) => {
            return errors
                .iter()
                .map(|e| normalize(&formatter.format_error(e)))
                .collect();
        }
    };
    state
        .warnings
        .iter()
        .map(|w| normalize(&formatter.format_warning(w)))
        .collect()
}

fn lsp_diagnostics(case: &Case) -> BTreeSet<NormalizedDiag> {
    let preview = build_preview(case);
    let files = vec![WorkspaceFile {
        path: PathBuf::from("case.gruel"),
        text: case.source.clone(),
        file_id: FileId::new(1),
    }];
    let res = analyze(&files, &preview, &Target::host());
    res.diagnostics.iter().map(normalize).collect()
}

fn build_preview(case: &Case) -> PreviewFeatures {
    let mut set = PreviewFeatures::default();
    if let Some(name) = &case.preview {
        if let Ok(feature) = PreviewFeature::from_str(name) {
            set.insert(feature);
        }
    }
    set
}

fn normalize(d: &gruel_compiler::JsonDiagnostic) -> NormalizedDiag {
    let primary = d
        .spans
        .iter()
        .find(|s| s.primary)
        .or_else(|| d.spans.first());
    NormalizedDiag {
        file: primary.map(|s| s.file.clone()).unwrap_or_default(),
        line: primary.map(|s| s.line).unwrap_or(0),
        column: primary.map(|s| s.column).unwrap_or(0),
        severity: d.severity.to_string(),
        code: d.code.clone(),
    }
}

// Slow: ~10 minutes serial on a developer laptop, ~2-5 minutes parallel on
// CI. Gated behind `#[ignore]` so `cargo test --tests` (and `make
// quick-test`) skips it; the dedicated `make lsp-diagnostic-differential`
// target runs it via `cargo test -- --ignored`.
#[test]
#[ignore = "expensive: 2k+ spec cases × 2 full compiles each"]
fn lsp_and_cli_diagnostics_agree_on_spec_corpus() {
    // Some sema paths recurse deeply for comptime; build a dedicated rayon
    // pool whose workers each get a 16 MiB stack so per-case compilation
    // can't blow it.
    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(16 * 1024 * 1024)
        .thread_name(|i| format!("diag-differential-{}", i))
        .build()
        .expect("build rayon pool");
    pool.install(run_differential);
}

fn run_differential() {
    let cases_dir = std::env::var("GRUEL_SPEC_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../gruel-spec/cases"));
    let ui_dir = std::env::var("GRUEL_UI_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../gruel-ui-tests/cases"));

    let mut cases: Vec<Case> = Vec::new();
    if cases_dir.exists() {
        for (_id, file) in load_test_files(&cases_dir) {
            cases.extend(file.case);
        }
    }
    if ui_dir.exists() {
        for (_id, file) in load_test_files(&ui_dir) {
            cases.extend(file.case);
        }
    }

    // Skip cases that depend on golden output, runtime behaviour, or
    // preview features whose name we don't recognise — the differential
    // is about diagnostic agreement, not test infrastructure.
    let cases: Vec<&Case> = cases
        .iter()
        .filter(|c| !c.skip)
        .filter(|c| c.expected_tokens.is_none())
        .filter(|c| c.expected_ast.is_none())
        .filter(|c| c.expected_rir.is_none())
        .filter(|c| c.expected_air.is_none())
        .filter(|c| c.expected_cfg.is_none())
        .filter(|c| {
            // Drop tests with preview features that aren't valid: we can't
            // reproduce them.
            c.preview
                .as_ref()
                .map(|p| PreviewFeature::from_str(p).is_ok())
                .unwrap_or(true)
        })
        .collect();

    let total = cases.len();
    let disagreements: Vec<_> = cases
        .par_iter()
        .filter_map(|case| {
            let cli = cli_diagnostics(case);
            let lsp = lsp_diagnostics(case);
            if cli != lsp {
                let extra_in_cli: Vec<_> = cli.difference(&lsp).cloned().collect();
                let extra_in_lsp: Vec<_> = lsp.difference(&cli).cloned().collect();
                Some((case.name.clone(), extra_in_cli, extra_in_lsp))
            } else {
                None
            }
        })
        .collect();

    if !disagreements.is_empty() {
        let mut msg = format!(
            "{}/{} cases agree on diagnostics; {} disagree:\n",
            total - disagreements.len(),
            total,
            disagreements.len()
        );
        for (name, cli, lsp) in disagreements.iter().take(10) {
            msg.push_str(&format!("\n  case `{}`\n", name));
            if !cli.is_empty() {
                msg.push_str(&format!("    only in CLI: {:?}\n", cli));
            }
            if !lsp.is_empty() {
                msg.push_str(&format!("    only in LSP: {:?}\n", lsp));
            }
        }
        if disagreements.len() > 10 {
            msg.push_str(&format!("\n  ... and {} more\n", disagreements.len() - 10));
        }
        panic!("{}", msg);
    }

    eprintln!(
        "spec/UI diagnostic differential: {}/{} cases agree",
        total - disagreements.len(),
        total
    );
}

//! Acceptance-parity check between the canonical `gruel-parser` (chumsky)
//! and this tree-sitter grammar (ADR-0090 Part 5).
//!
//! Walks every TOML case file under `crates/gruel-spec/cases/` and
//! `crates/gruel-ui-tests/cases/`. For each `[[case]]` entry, runs both
//! parsers on the embedded `source` string and asserts that they agree on
//! whether the input is *syntactically* valid.
//!
//! Acceptance only: tree shape, error positions, and recovery strategies
//! are explicitly out of scope. Spec cases marked `compile_fail = true`
//! cover both lexical/parser errors and post-parser failures (sema, type
//! mismatch, etc.); only the former are syntactic and visible to
//! tree-sitter, so cases whose `error_contains` clearly references a
//! non-syntactic error (e.g. "type mismatch", "unused variable") are
//! still checked — they may still parse, which is the point. Disagreement
//! at the parse level is what this test catches.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Deserialize;
use tree_sitter::Parser;
use tree_sitter_gruel::LANGUAGE;
use walkdir::WalkDir;

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    source: String,
    #[serde(default)]
    compile_fail: bool,
    #[serde(default)]
    preview: Option<String>,
    /// Cases with `params = [...]` are templated — the `source` field
    /// contains `{name}` placeholders that the spec runner substitutes
    /// before parsing. Skip these (we'd need to replicate the runner's
    /// expansion logic to compare meaningfully).
    #[serde(default)]
    params: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
struct CaseFile {
    #[serde(rename = "case", default)]
    cases: Vec<Case>,
}

fn workspace_root() -> PathBuf {
    // bindings/rust/Cargo.toml → ../../../
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(3)
        .expect("workspace root")
        .to_path_buf()
}

fn chumsky_accepts(source: &str) -> bool {
    use gruel_lexer::Lexer;
    use gruel_parser::Parser as GParser;

    let lexer = Lexer::new(source);
    let (tokens, interner) = match lexer.tokenize() {
        Ok(v) => v,
        Err(_) => return false,
    };
    GParser::new(tokens, interner).parse().is_ok()
}

fn tree_sitter_accepts(parser: &mut Parser, source: &str) -> bool {
    match parser.parse(source.as_bytes(), None) {
        Some(tree) => !tree.root_node().has_error(),
        None => false,
    }
}

fn collect_cases(dir: &Path) -> Vec<(PathBuf, Case)> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dir) {
        let entry = match entry {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let content = match fs::read_to_string(entry.path()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let parsed: CaseFile = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for case in parsed.cases {
            out.push((entry.path().to_path_buf(), case));
        }
    }
    out
}

#[test]
fn spec_and_ui_cases_agree_on_acceptance() {
    let root = workspace_root();
    let mut parser = Parser::new();
    parser
        .set_language(&LANGUAGE.into())
        .expect("tree-sitter language");

    let mut cases = collect_cases(&root.join("crates/gruel-spec/cases"));
    cases.extend(collect_cases(&root.join("crates/gruel-ui-tests/cases")));

    assert!(!cases.is_empty(), "no cases discovered");

    let total = cases.len();
    let agree = AtomicUsize::new(0);
    let mut disagreements: BTreeMap<PathBuf, Vec<(String, bool, bool, String)>> = BTreeMap::new();

    for (path, case) in cases {
        // Skip preview-gated cases: their source is allowed to be
        // syntactically valid even when sema rejects it, but it can also
        // use experimental syntax that the grammar may legitimately not
        // recognise yet. Compare only stable surface syntax.
        if case.preview.is_some() {
            continue;
        }
        // Skip templated cases (`source` contains `{placeholder}` and
        // a `params` table expands them at run time). We don't replicate
        // the spec runner's substitution; the per-variant expansions are
        // covered by the structural rules these templates target.
        if case.params.is_some() {
            continue;
        }
        let chumsky_ok = chumsky_accepts(&case.source);
        let ts_ok = tree_sitter_accepts(&mut parser, &case.source);
        if chumsky_ok == ts_ok {
            agree.fetch_add(1, Ordering::Relaxed);
        } else {
            disagreements.entry(path.clone()).or_default().push((
                case.name.clone(),
                chumsky_ok,
                ts_ok,
                case.source.clone(),
            ));
        }
    }

    // Known disagreements that are out of scope for an acceptance-only
    // syntactic differential:
    //
    // 1. Reserved-keyword reuse (`let let = 1`, `let i32 = 1`, etc.) —
    //    tree-sitter's `word` mechanism only fires when both the keyword
    //    and the word would be valid in the same parse state, so a
    //    keyword in identifier-position quietly falls back to identifier.
    //    Chumsky's lexer treats keywords as absolute.
    //
    // 2. Semantic-level rejections (refutable `let 1 = 1;`, surrogate
    //    `\u{D800}` char literals, `_` as a value) that the canonical
    //    parser catches after the parse, not during it.
    let known_skip = |name: &str| -> bool {
        // Reserved-keyword cases — see lexical/keywords.toml and
        // lexical/tokens.toml.
        let keyword_skip = (name.starts_with("keyword_") && name.contains("_reserved"))
            || name.starts_with("type_") && name.contains("_as_")
            || name.starts_with("identifier_cannot_be_keyword_")
            || name == "underscore_cannot_be_referenced";
        // Semantic-only rejections that don't show up in pure syntax.
        let semantic_skip = name == "refutable_int_in_let_errors"
            || name == "refutable_nested_int_in_let_errors"
            || name == "char_lit_surrogate_rejected"
            || name == "char_lit_out_of_range_rejected";
        keyword_skip || semantic_skip
    };
    let mut filtered: BTreeMap<PathBuf, Vec<(String, bool, bool, String)>> = BTreeMap::new();
    for (path, items) in disagreements.into_iter() {
        let kept: Vec<_> = items
            .into_iter()
            .filter(|(name, _, _, _)| !known_skip(name))
            .collect();
        if !kept.is_empty() {
            filtered.insert(path, kept);
        }
    }
    let disagreements = filtered;

    eprintln!(
        "spec/UI corpus differential: {}/{} cases agree on acceptance",
        agree.load(Ordering::Relaxed),
        total,
    );

    if !disagreements.is_empty() {
        let mut report = String::new();
        for (path, items) in &disagreements {
            report.push_str(&format!("\n=== {} ===\n", path.display()));
            for (name, chumsky, ts, source) in items {
                report.push_str(&format!(
                    "  case `{}`: chumsky={}, tree-sitter={}\n",
                    name, chumsky, ts
                ));
                report.push_str(&format!(
                    "  source:\n    {}\n",
                    source.replace('\n', "\n    ")
                ));
            }
        }
        panic!(
            "{} parser disagreement(s) between chumsky and tree-sitter:{}",
            disagreements.values().map(Vec::len).sum::<usize>(),
            report,
        );
    }
}

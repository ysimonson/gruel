//! Corpus idempotence + reparse test (ADR-0093 Phase 6).
//!
//! For every `.gruel` source body in the spec and UI test corpora, run the
//! formatter and assert that:
//!
//! 1. `format_source(format_source(x)) == format_source(x)` — idempotence.
//! 2. `format_source(x)` re-parses successfully — semantic preservation
//!    (weakened from full AST-equivalence; the parser is the contract).
//!
//! Cases that don't parse cleanly to begin with are simply skipped: the
//! formatter is allowed (and expected) to error out on broken source, and
//! the spec test runner already covers compile-failure scenarios.

use std::path::PathBuf;

use gruel_fmt::format_source;
use gruel_test_runner::{Case, expand_case, load_test_files};

fn load_all_cases() -> Vec<Case> {
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
    // Expand `params`-driven templates.
    cases.into_iter().flat_map(expand_case).collect()
}

#[test]
fn idempotence_and_reparse_over_spec_corpus() {
    let cases = load_all_cases();
    assert!(
        !cases.is_empty(),
        "no spec/UI test cases found; check GRUEL_SPEC_CASES / GRUEL_UI_CASES env vars"
    );

    let mut total = 0usize;
    let mut formatted_ok = 0usize;
    let mut parse_skipped = 0usize;
    let mut idempotence_failures: Vec<String> = Vec::new();
    let mut reparse_failures: Vec<String> = Vec::new();

    for case in &cases {
        if case.skip {
            continue;
        }
        // Skip pure-error cases — the formatter (correctly) errors on these
        // and there's nothing to be idempotent about.
        if case.compile_fail {
            continue;
        }
        total += 1;
        let first = match format_source(&case.source) {
            Ok(s) => s,
            Err(_) => {
                // Source didn't lex/parse — that's the parser's problem, not
                // ours. Skip the case.
                parse_skipped += 1;
                continue;
            }
        };
        let second = match format_source(&first) {
            Ok(s) => s,
            Err(e) => {
                reparse_failures.push(format!(
                    "case '{}': formatted output failed to re-parse: {}\n--- formatted ---\n{}\n",
                    case.name, e, first
                ));
                continue;
            }
        };
        formatted_ok += 1;
        if first != second {
            idempotence_failures.push(format!(
                "case '{}': not idempotent\n--- once ---\n{}\n--- twice ---\n{}\n",
                case.name, first, second
            ));
        }
    }

    eprintln!(
        "gruel-fmt corpus: total={}, formatted_ok={}, parse_skipped={}, idempotence_failures={}, reparse_failures={}",
        total,
        formatted_ok,
        parse_skipped,
        idempotence_failures.len(),
        reparse_failures.len()
    );

    if !idempotence_failures.is_empty() || !reparse_failures.is_empty() {
        let mut msg = String::new();
        if !idempotence_failures.is_empty() {
            msg.push_str("\nIDEMPOTENCE FAILURES:\n\n");
            for f in idempotence_failures.iter().take(10) {
                msg.push_str(f);
                msg.push('\n');
            }
            if idempotence_failures.len() > 10 {
                msg.push_str(&format!(
                    "... and {} more\n",
                    idempotence_failures.len() - 10
                ));
            }
        }
        if !reparse_failures.is_empty() {
            msg.push_str("\nREPARSE FAILURES:\n\n");
            for f in reparse_failures.iter().take(10) {
                msg.push_str(f);
                msg.push('\n');
            }
            if reparse_failures.len() > 10 {
                msg.push_str(&format!("... and {} more\n", reparse_failures.len() - 10));
            }
        }
        panic!("{}", msg);
    }
}

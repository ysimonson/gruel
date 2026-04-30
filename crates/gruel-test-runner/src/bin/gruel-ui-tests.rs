//! UI tests for Gruel compiler features.
//!
//! These tests verify compiler behavior that is not part of the language specification,
//! such as warnings, diagnostics quality, and compiler flags.

use gruel_test_runner::{
    CacheStore, Case, build_gruel_binary, find_dir, find_gruel_binary, load_test_files,
    run_test_case, should_skip_for_platform,
};
use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use std::path::Path;
use std::sync::Arc;

/// Possible paths for the cases directory.
const CASES_DIR_PATHS: &[&str] = &[
    "crates/gruel-ui-tests/cases",
    "cases",
    "../gruel-ui-tests/cases",
];

/// Wrapper to convert TestResult to libtest2_mimic's RunError type.
fn run_case_wrapper(
    case: &Case,
    gruel_binary: &Path,
    cache: &Arc<CacheStore>,
    skip: bool,
    ctx: RunContext<'_>,
) -> Result<(), RunError> {
    if skip {
        return ctx.ignore_for("marked as skip");
    }
    if let Some(reason) = should_skip_for_platform(&case.only_on) {
        return ctx.ignore_for(reason);
    }
    run_test_case(case, gruel_binary, Some(cache)).map_err(|e| RunError::fail(e.to_string()))
}

fn main() {
    // Build the gruel compiler before running tests
    build_gruel_binary();

    // Find the gruel binary
    let gruel_binary = find_gruel_binary();

    // Set up a result cache keyed on the binary's mtime+size and each test's TOML mtime+size.
    let cache = Arc::new(CacheStore::new(&gruel_binary));

    // Find the cases directory
    let cases_dir = find_dir("GRUEL_UI_CASES", CASES_DIR_PATHS, "cases");

    // Load all test files
    let test_files = load_test_files(&cases_dir);

    // Convert to trials
    let tests: Vec<Trial> = test_files
        .into_iter()
        .flat_map(|(_, test_file)| {
            let section_id = test_file.section.id.clone();
            let gruel_binary = gruel_binary.clone();
            let cache = Arc::clone(&cache);

            test_file.case.into_iter().map(move |case| {
                let test_name = format!("{}::{}", section_id, case.name);
                let skip = case.skip;
                let gruel_binary = gruel_binary.clone();
                let cache = Arc::clone(&cache);

                Trial::test(test_name, move |ctx| {
                    run_case_wrapper(&case, &gruel_binary, &cache, skip, ctx)
                })
            })
        })
        .collect();

    if tests.is_empty() {
        eprintln!("Warning: No UI test cases found in {}", cases_dir.display());
        eprintln!("Make sure test files exist and have the correct format.");
    }

    Harness::with_env().discover(tests).main();
}

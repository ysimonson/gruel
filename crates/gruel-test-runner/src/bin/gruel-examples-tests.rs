//! Integration tests for `examples/`.
//!
//! Compiles every `examples/*.gruel` file with the gruel binary, runs the
//! resulting program, and verifies the exit code (and optional stdout) match
//! the entry in `examples/expected.toml`.
//!
//! Examples without an entry in `expected.toml` cause the test to fail —
//! adding an example is therefore an explicit, traceable act.

use gruel_test_runner::{
    CacheStore, Case, build_gruel_binary, find_dir, find_gruel_binary, run_test_case,
};
use libtest2_mimic::{Harness, Trial};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

const EXAMPLES_DIR_PATHS: &[&str] = &["examples", "../examples", "../../examples"];

#[derive(Debug, Deserialize)]
struct ExpectedEntry {
    exit_code: i32,
    #[serde(default)]
    stdout: Option<String>,
}

fn load_manifest(path: &Path) -> BTreeMap<String, ExpectedEntry> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
    toml::from_str(&text).unwrap_or_else(|e| panic!("Failed to parse {}: {}", path.display(), e))
}

fn collect_example_files(dir: &Path) -> BTreeMap<String, std::path::PathBuf> {
    let mut out = BTreeMap::new();
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("Failed to read examples dir {}: {}", dir.display(), e));
    for entry in entries {
        let entry = entry.expect("read_dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("gruel") {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("utf-8 stem")
                .to_string();
            out.insert(stem, path);
        }
    }
    out
}

fn main() {
    build_gruel_binary();
    let gruel_binary = find_gruel_binary();
    let cache = Arc::new(CacheStore::new(&gruel_binary));

    let examples_dir = find_dir("GRUEL_EXAMPLES_DIR", EXAMPLES_DIR_PATHS, "examples");
    let manifest_path = examples_dir.join("expected.toml");
    let manifest = load_manifest(&manifest_path);
    let example_files = collect_example_files(&examples_dir);

    // Surface drift in either direction as test failures so coverage stays exact.
    let mut tests: Vec<Trial> = Vec::with_capacity(example_files.len() + manifest.len());

    for (name, path) in &example_files {
        if !manifest.contains_key(name) {
            let display = format!(
                "examples::{} (missing from {})",
                name,
                manifest_path.display()
            );
            let path = path.clone();
            tests.push(Trial::test(display, move |_ctx| {
                Err(libtest2_mimic::RunError::fail(format!(
                    "no expected.toml entry for example {}; add one to {}",
                    path.display(),
                    "examples/expected.toml"
                )))
            }));
        }
    }

    for (name, expected) in manifest {
        let Some(source_path) = example_files.get(&name).cloned() else {
            tests.push(Trial::test(
                format!("examples::{} (missing source)", name),
                move |_ctx| {
                    Err(libtest2_mimic::RunError::fail(format!(
                        "expected.toml lists `{}` but examples/{}.gruel does not exist",
                        name, name
                    )))
                },
            ));
            continue;
        };

        let gruel_binary = gruel_binary.clone();
        let cache = Arc::clone(&cache);
        let test_name = format!("examples::{}", name);
        let case_name = name.clone();

        tests.push(Trial::test(test_name, move |_ctx| {
            let source = std::fs::read_to_string(&source_path).map_err(|e| {
                libtest2_mimic::RunError::fail(format!(
                    "Failed to read {}: {}",
                    source_path.display(),
                    e
                ))
            })?;

            let case = Case {
                name: case_name.clone(),
                source,
                exit_code: Some(expected.exit_code),
                expected_stdout: expected.stdout.clone(),
                ..Case::default()
            };

            run_test_case(&case, &gruel_binary, Some(&cache))
                .map_err(libtest2_mimic::RunError::fail)
        }));
    }

    if tests.is_empty() {
        eprintln!(
            "Warning: no example tests found. examples_dir={}",
            examples_dir.display()
        );
    }

    Harness::with_env().discover(tests).main();
}

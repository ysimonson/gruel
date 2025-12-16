use libtest_mimic::{Arguments, Failed, Trial};
use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// A section of the spec containing multiple test cases.
#[derive(Debug, Deserialize)]
struct Section {
    id: String,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    #[serde(default)]
    description: String,
}

/// A single test case.
#[derive(Debug, Clone, Deserialize)]
struct Case {
    name: String,
    source: String,
    exit_code: i32,
    #[serde(default)]
    skip: bool,
}

/// A spec file containing a section and its cases.
#[derive(Debug, Deserialize)]
struct SpecFile {
    section: Section,
    #[serde(default)]
    case: Vec<Case>,
}

/// Load all spec files from the cases directory.
fn load_spec_files(cases_dir: &Path) -> Vec<(String, SpecFile)> {
    let mut specs = Vec::new();

    if !cases_dir.exists() {
        eprintln!("Warning: cases directory not found: {}", cases_dir.display());
        return specs;
    }

    let entries = match fs::read_dir(cases_dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Error reading cases directory: {}", e);
            return specs;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error reading {}: {}", path.display(), e);
                    continue;
                }
            };

            match toml::from_str::<SpecFile>(&content) {
                Ok(spec) => {
                    let filename = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    specs.push((filename, spec));
                }
                Err(e) => {
                    eprintln!("Error parsing {}: {}", path.display(), e);
                }
            }
        }
    }

    // Sort by filename for deterministic ordering
    specs.sort_by(|a, b| a.0.cmp(&b.0));
    specs
}

/// Run a single test case.
fn run_test_case(case: &Case, rue_binary: &Path) -> Result<(), Failed> {
    // Create a temporary directory for this test
    let temp_dir = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {}", e))?;
    let source_path = temp_dir.path().join("test.rue");
    let output_path = temp_dir.path().join("test");

    // Write source to file
    let mut source_file =
        fs::File::create(&source_path).map_err(|e| format!("Failed to create source file: {}", e))?;
    source_file
        .write_all(case.source.as_bytes())
        .map_err(|e| format!("Failed to write source: {}", e))?;

    // Compile with rue
    let compile_output = Command::new(rue_binary)
        .arg(&source_path)
        .arg(&output_path)
        .output()
        .map_err(|e| format!("Failed to run rue compiler: {}", e))?;

    if !compile_output.status.success() {
        return Err(format!(
            "Compilation failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&compile_output.stdout),
            String::from_utf8_lossy(&compile_output.stderr)
        )
        .into());
    }

    // Run the compiled binary
    let run_output = Command::new(&output_path)
        .output()
        .map_err(|e| format!("Failed to run compiled binary: {}", e))?;

    let actual_exit_code = run_output.status.code().unwrap_or(-1);
    let expected_exit_code = case.exit_code;

    if actual_exit_code != expected_exit_code {
        return Err(format!(
            "Exit code mismatch:\n  expected: {}\n  actual: {}\n  source: {}",
            expected_exit_code, actual_exit_code, case.source
        )
        .into());
    }

    Ok(())
}

fn main() {
    let args = Arguments::from_args();

    // Find the rue binary - it should be built alongside this test runner
    // For now, we'll look for it relative to the current directory or via an env var
    let rue_binary = std::env::var("RUE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            // Try to find it in common buck output locations
            let possible_paths = [
                "buck-out/v2/gen/root/crates/rue/__rue__/rue",
                "../rue/rue",
                "./rue",
            ];
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            // Default - will likely fail but with a clear error
            Path::new("rue").to_path_buf()
        });

    // Find the cases directory
    let cases_dir = std::env::var("RUE_SPEC_CASES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            // Try to find it relative to the current directory
            let possible_paths = [
                "crates/rue-spec/cases",
                "cases",
                "../rue-spec/cases",
            ];
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            Path::new("cases").to_path_buf()
        });

    // Load all spec files
    let specs = load_spec_files(&cases_dir);

    // Convert to trials
    let tests: Vec<Trial> = specs
        .into_iter()
        .flat_map(|(_, spec)| {
            let section_id = spec.section.id.clone();
            let rue_binary = rue_binary.clone();

            spec.case.into_iter().map(move |case| {
                let test_name = format!("{}::{}", section_id, case.name);
                let skip = case.skip;
                let rue_binary = rue_binary.clone();

                let mut trial = Trial::test(test_name, move || run_test_case(&case, &rue_binary));

                if skip {
                    trial = trial.with_ignored_flag(true);
                }

                trial
            })
        })
        .collect();

    if tests.is_empty() {
        eprintln!("Warning: No test cases found in {}", cases_dir.display());
        eprintln!("Make sure spec files exist and have the correct format.");
    }

    libtest_mimic::run(&args, tests).exit();
}

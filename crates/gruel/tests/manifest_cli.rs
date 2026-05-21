//! End-to-end CLI tests covering ADR-0092 manifest-based invocations.
//!
//! These shell out to the compiled `gruel` binary so we exercise the full
//! arg-parsing + manifest-loading + compilation path. Each test sets up an
//! isolated tempdir so they don't trip each other up.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Locate the `gruel` binary produced by Cargo. The test binary itself
/// lives in `target/<profile>/deps/`; the CLI is at `../gruel` relative
/// to that.
fn gruel_bin() -> PathBuf {
    let test_exe = std::env::current_exe().expect("current exe");
    let mut dir = test_exe.parent().expect("test dir").to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join("gruel")
}

/// Set up a minimal binary package on disk and return the package dir.
fn write_bin_package(name: &str, version: &str, root_rel: &str, source: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    write_target(dir.path(), name, version, "bin", root_rel, source);
    dir
}

fn write_lib_package(name: &str, version: &str, root_rel: &str, source: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    write_target(dir.path(), name, version, "lib", root_rel, source);
    dir
}

fn write_target(dir: &Path, name: &str, version: &str, kind: &str, root_rel: &str, source: &str) {
    let root_path = dir.join(root_rel);
    if let Some(parent) = root_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&root_path, source).unwrap();
    let manifest = format!(
        "{{\"name\": \"{}\", \"version\": \"{}\", \"{}\": {{\"root\": \"{}\"}}}}",
        name, version, kind, root_rel
    );
    std::fs::write(dir.join("gruel.json"), manifest).unwrap();
}

/// Run the gruel CLI, capturing stdout / stderr / exit code.
fn run_gruel(cwd: Option<&Path>, args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(gruel_bin());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.args(args);
    let output = cmd.output().expect("run gruel");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const MAIN_SRC: &str = "fn main() -> i32 { 42 }\n";
const LIB_SRC: &str = "pub fn answer() -> i32 { 42 }\n";

#[test]
fn build_implicit_manifest_discovery_in_cwd() {
    let pkg = write_bin_package("hello", "0.1.0", "src/main.gruel", MAIN_SRC);
    let out_path = pkg.path().join("hello");

    let (code, _stdout, stderr) = run_gruel(
        Some(pkg.path()),
        &["build", "-o", out_path.to_str().unwrap()],
    );
    assert_eq!(code, 0, "build failed: stderr={}", stderr);
    assert!(out_path.exists(), "expected output binary at {:?}", out_path);
}

#[test]
fn build_explicit_manifest_dir() {
    let pkg = write_bin_package("hi", "0.1.0", "src/main.gruel", MAIN_SRC);
    let out_path = pkg.path().join("hi");

    let (code, _stdout, stderr) = run_gruel(
        None,
        &[
            "build",
            pkg.path().to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "build failed: stderr={}", stderr);
    assert!(out_path.exists());
}

#[test]
fn build_explicit_manifest_file() {
    let pkg = write_bin_package("hi", "0.1.0", "src/main.gruel", MAIN_SRC);
    let manifest = pkg.path().join("gruel.json");
    let out_path = pkg.path().join("hi");

    let (code, _stdout, stderr) = run_gruel(
        None,
        &[
            "build",
            manifest.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "build failed: stderr={}", stderr);
    assert!(out_path.exists());
}

#[test]
fn build_legacy_single_file_still_works() {
    // The `.gruel` positional branch bypasses manifests entirely.
    let pkg = write_bin_package("hi", "0.1.0", "src/main.gruel", MAIN_SRC);
    let src = pkg.path().join("src/main.gruel");
    let out_path = pkg.path().join("legacy");

    let (code, _stdout, stderr) = run_gruel(
        None,
        &[
            "build",
            src.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "build failed: stderr={}", stderr);
    assert!(out_path.exists());
}

#[test]
fn build_library_rejected() {
    let pkg = write_lib_package("math", "0.1.0", "src/lib.gruel", LIB_SRC);
    let (code, _stdout, stderr) = run_gruel(None, &["build", pkg.path().to_str().unwrap()]);
    assert_ne!(code, 0, "build should fail on library package");
    assert!(
        stderr.contains("library") && stderr.contains("cannot build"),
        "expected library-rejection message, got: {}",
        stderr
    );
}

#[test]
fn run_library_rejected() {
    let pkg = write_lib_package("math", "0.1.0", "src/lib.gruel", LIB_SRC);
    let (code, _stdout, stderr) = run_gruel(None, &["run", pkg.path().to_str().unwrap()]);
    assert_ne!(code, 0, "run should fail on library package");
    assert!(
        stderr.contains("library") && stderr.contains("cannot run"),
        "expected library-rejection message, got: {}",
        stderr
    );
}

#[test]
fn check_accepts_library() {
    let pkg = write_lib_package("math", "0.1.0", "src/lib.gruel", LIB_SRC);
    let (code, _stdout, stderr) = run_gruel(None, &["check", pkg.path().to_str().unwrap()]);
    assert_eq!(code, 0, "check failed on library: stderr={}", stderr);
}

#[test]
fn doc_accepts_library_and_uses_manifest_name_for_title() {
    let pkg = write_lib_package("mathlib", "0.1.0", "src/lib.gruel", LIB_SRC);
    let doc_dir = pkg.path().join("docs-out");

    let (code, _stdout, stderr) = run_gruel(
        None,
        &[
            "doc",
            "--output-dir",
            doc_dir.to_str().unwrap(),
            pkg.path().to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "doc failed: stderr={}", stderr);
    let index = std::fs::read_to_string(doc_dir.join("index.md")).expect("index.md");
    assert!(
        index.contains("mathlib"),
        "expected manifest name in doc index, got: {}",
        index
    );
}

#[test]
fn doc_accepts_bin_package_uses_manifest_name() {
    let pkg = write_bin_package("hello", "0.1.0", "src/main.gruel", MAIN_SRC);
    let doc_dir = pkg.path().join("docs-out");

    let (code, _stdout, stderr) = run_gruel(
        None,
        &[
            "doc",
            "--output-dir",
            doc_dir.to_str().unwrap(),
            pkg.path().to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "doc failed: stderr={}", stderr);
    let index = std::fs::read_to_string(doc_dir.join("index.md")).expect("index.md");
    assert!(
        index.contains("hello"),
        "expected manifest name in doc index, got: {}",
        index
    );
}

#[test]
fn extra_positional_on_manifest_branch_rejected() {
    let pkg = write_bin_package("hi", "0.1.0", "src/main.gruel", MAIN_SRC);
    let (code, _stdout, stderr) = run_gruel(
        None,
        &[
            "build",
            pkg.path().to_str().unwrap(),
            "extra-positional.gruel",
        ],
    );
    assert_ne!(code, 0, "extra positional should be rejected");
    assert!(
        stderr.contains("manifest-based invocation accepts only one positional"),
        "expected extra-positional rejection, got: {}",
        stderr
    );
}

#[test]
fn no_source_no_manifest_in_ancestors_reports_helpful_error() {
    let dir = TempDir::new().unwrap();
    // Run from inside a temp dir that has no gruel.json upstream. (The check
    // is best-effort — if the test host has a gruel.json on disk somewhere
    // above the system tempdir, this would be a false negative.)
    let (code, _stdout, stderr) = run_gruel(Some(dir.path()), &["build"]);
    assert_ne!(code, 0, "expected failure");
    assert!(
        stderr.contains("no gruel.json found") || stderr.contains("invalid manifest"),
        "expected manifest-discovery failure, got: {}",
        stderr
    );
}

#[test]
fn build_implicit_default_output_is_manifest_name() {
    let pkg = write_bin_package("widget", "0.1.0", "src/main.gruel", MAIN_SRC);

    let (code, _stdout, stderr) = run_gruel(Some(pkg.path()), &["build"]);
    assert_eq!(code, 0, "build failed: stderr={}", stderr);
    let expected = pkg.path().join("widget");
    assert!(
        expected.exists(),
        "expected default output named '{}' at {:?}",
        "widget",
        expected
    );
}

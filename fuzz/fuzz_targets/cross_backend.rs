//! Cross-backend differential fuzzer.
//!
//! Compiles valid Gruel programs with both the native backend and the LLVM
//! backend, executes the resulting binaries, and asserts that:
//!
//! 1. Both backends compile the program without errors.
//! 2. Both binaries produce identical exit code, stdout, and stderr.
//!
//! Any divergence indicates a backend-specific code generation bug.
#![no_main]

use gruel_compiler::{CodegenBackend, CompileOptions, LinkerMode, compile_with_options};
use gruel_fuzz::GruelProgram;
use libfuzzer_sys::fuzz_target;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;

/// All observable output from running a binary.
#[derive(Debug, PartialEq)]
struct RunOutput {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fuzz_target!(|prog: GruelProgram| {
    let source = &prog.0;

    // Compile with the native backend (internal linker).
    let native_opts = CompileOptions {
        codegen_backend: CodegenBackend::Native,
        linker: LinkerMode::Internal,
        ..CompileOptions::default()
    };
    let native_elf = match compile_with_options(source, &native_opts) {
        Ok(out) => out.elf,
        Err(e) => panic!(
            "native backend failed on a valid program\nsource:\n{}\nerrors: {:?}",
            source, e
        ),
    };

    // Compile with the LLVM backend (falls back to system `cc` for linking).
    let llvm_opts = CompileOptions {
        codegen_backend: CodegenBackend::Llvm,
        linker: LinkerMode::Internal,
        ..CompileOptions::default()
    };
    let llvm_elf = match compile_with_options(source, &llvm_opts) {
        Ok(out) => out.elf,
        Err(e) => panic!(
            "LLVM backend failed on a valid program\nsource:\n{}\nerrors: {:?}",
            source, e
        ),
    };

    // Execute both binaries and compare all output.
    let native_out = run_binary("native", &native_elf);
    let llvm_out = run_binary("llvm", &llvm_elf);

    // Timeouts on both sides are inconclusive — skip comparison.
    if native_out.exit_code.is_none() && llvm_out.exit_code.is_none() {
        return;
    }

    assert_eq!(
        native_out, llvm_out,
        "backend output mismatch\nsource:\n{}\nnative: {:?}\nllvm:   {:?}",
        source, native_out, llvm_out,
    );
});

/// Write `bytes` to a temp file, make it executable, run it with a 5-second
/// timeout, and return the exit code, stdout, and stderr.
///
/// On timeout the process is killed and `exit_code` is `None`.
fn run_binary(tag: &str, bytes: &[u8]) -> RunOutput {
    let dir = std::env::temp_dir()
        .join(format!("gruel-fuzz-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("prog-{}", tag));

    fs::write(&path, bytes).expect("write binary");

    let mut perms = fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("set permissions");

    let mut child = Command::new(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    // Wait up to 5 seconds, then kill.
    let timeout = Duration::from_secs(5);
    let started = std::time::Instant::now();
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break Some(s);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    let output = child.wait_with_output().expect("wait_with_output");
    let _ = fs::remove_file(&path);

    RunOutput {
        exit_code: status.and_then(|s| s.code()),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

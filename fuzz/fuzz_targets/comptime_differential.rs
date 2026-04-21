//! Differential fuzzer comparing comptime interpreter output against runtime execution.
//!
//! For each generated program, this target:
//! 1. Evaluates the body in a comptime block, collecting `@dbg` output from the compiler buffer
//! 2. Compiles the body as a normal runtime program, executes it, and captures stdout
//! 3. Asserts both outputs are identical
//!
//! Any divergence indicates a bug in the comptime interpreter.

#![no_main]
use gruel_compiler::CompileOptions;
use gruel_fuzz::ComptimeProgram;
use libfuzzer_sys::fuzz_target;
use std::io::Write;

fuzz_target!(|prog: ComptimeProgram| {
    let preview = gruel_error::PreviewFeatures::new();

    // Path A: comptime evaluation — @dbg output collected in compiler buffer.
    // Suppress on-the-fly stderr printing so the fuzzer doesn't flood the
    // terminal; the buffer is still populated.
    let comptime_source = prog.comptime_source();
    let comptime_dbg =
        match gruel_compiler::compile_frontend_with_options_full(&comptime_source, &preview, true) {
            Ok(state) => state.comptime_dbg_output.join("\n"),
            Err(_) => return, // Skip programs that don't compile
        };

    // Path B: runtime execution — @dbg output captured from stdout
    let runtime_source = prog.runtime_source();
    let options = CompileOptions {
        preview_features: preview,
        ..Default::default()
    };
    let runtime_dbg = match compile_and_run(&runtime_source, &options) {
        Some(stdout) => stdout,
        None => return, // Skip if compilation or execution fails
    };

    // Compare @dbg output line by line
    assert_eq!(
        comptime_dbg,
        runtime_dbg,
        "comptime/runtime divergence!\n\nBody:\n{}\n\nComptime source:\n{}\n\nRuntime source:\n{}",
        prog.body(),
        comptime_source,
        runtime_source,
    );
});

/// Compile source to a binary, execute it, and return captured stdout.
fn compile_and_run(source: &str, options: &CompileOptions) -> Option<String> {
    let output = gruel_compiler::compile_with_options(source, options).ok()?;

    let dir = tempfile::tempdir().ok()?;
    let binary_path = dir.path().join("test_bin");

    // Write binary
    let mut f = std::fs::File::create(&binary_path).ok()?;
    f.write_all(&output.elf).ok()?;
    drop(f);

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755)).ok()?;
    }

    // Execute and capture stdout
    let result = std::process::Command::new(&binary_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    // Trim trailing newline for comparison (runtime @dbg adds \n after each value)
    let stdout = String::from_utf8(result.stdout).ok()?;
    let trimmed = stdout.trim_end_matches('\n').to_string();
    Some(trimmed)
}

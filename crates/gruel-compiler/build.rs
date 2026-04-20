use std::path::PathBuf;
use std::process::Command;

/// Builds gruel-runtime as a staticlib and places it in OUT_DIR so it can be
/// embedded by `include_bytes!(concat!(env!("OUT_DIR"), "/libgruel_runtime.a"))`.
///
/// This mirrors what Buck2's `mapped_srcs` did: compile gruel-runtime with special
/// flags (`-Cpanic=abort`, `-Copt-level=z`, `-Crelocation-model=pic`, etc.) and
/// make the resulting archive available to the compiler crate at build time.
fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let runtime_src = manifest_dir
        .parent()
        .unwrap()
        .join("gruel-runtime")
        .join("src")
        .join("lib.rs");

    // Re-run if any runtime source changes.
    let runtime_src_dir = runtime_src.parent().unwrap();
    println!("cargo:rerun-if-changed={}", runtime_src_dir.display());

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let target = std::env::var("TARGET").unwrap();

    let status = Command::new(&rustc)
        .args([
            "--edition=2024",
            "--crate-type=staticlib",
            "--crate-name=gruel_runtime",
            // Minimize output size.
            "-Copt-level=z",
            // Abort on panic (no unwinding in embedded runtime).
            "-Cpanic=abort",
            // LTO for smaller output.
            "-Clto=true",
            // PIC is required for linking into PIE executables (the default
            // on modern Linux). The LLVM backend always uses the system linker
            // which expects position-independent code.
            "-Crelocation-model=pic",
            // Disable LSE atomics on aarch64 to avoid __aarch64_have_lse_atomics
            // runtime detection symbols from compiler-rt that we don't have.
            "-Ctarget-feature=-lse,-lse2,-outline-atomics",
        ])
        .args(["--target", &target])
        .args(["--out-dir", out_dir.to_str().unwrap()])
        .arg(&runtime_src)
        .status()
        .expect("failed to invoke rustc to build gruel-runtime");

    assert!(status.success(), "gruel-runtime staticlib build failed");
}

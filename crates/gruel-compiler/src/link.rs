//! Linking object files into the final executable.
//!
//! Driver for the system-linker path: collects object files into a temporary
//! directory, drops the embedded runtime archive next to them, and shells out
//! to `cc` / `clang` / `ld` (whatever the user picked). Cleanup is automatic
//! via `Drop` on `TempLinkDir`, so early returns from errors don't leak.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{info, info_span};

use gruel_util::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind, MultiErrorResult,
};

use crate::{CompileOptions, CompileOutput};

/// Build a `LinkError` from an `io::Error` and a context string.
fn io_link_error(context: &str, err: std::io::Error) -> CompileError {
    CompileError::without_span(ErrorKind::LinkError(format!("{}: {}", context, err)))
}

/// Counter for generating unique temp directory names across parallel test runs.
static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The gruel-runtime staticlib archive bytes, embedded at compile time.
/// Linked into every Gruel executable.
static RUNTIME_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libgruel_runtime.a"));

/// A temporary directory for linking that automatically cleans up on drop.
///
/// Holds object files plus the runtime archive. The directory is removed
/// when `TempLinkDir` is dropped (whether via normal completion or early
/// error return), so callers can use `?` freely without leaking files.
struct TempLinkDir {
    path: PathBuf,
    obj_paths: Vec<PathBuf>,
    runtime_path: PathBuf,
    output_path: PathBuf,
}

impl TempLinkDir {
    fn new() -> CompileResult<Self> {
        let unique_id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("gruel-{}-{}", std::process::id(), unique_id));
        std::fs::create_dir_all(&path)
            .map_err(|e| io_link_error("failed to create temp directory", e))?;

        let runtime_path = path.join("libgruel_runtime.a");
        let output_path = path.join("output");

        Ok(Self {
            path,
            obj_paths: Vec::new(),
            runtime_path,
            output_path,
        })
    }

    fn write_object_files(&mut self, object_files: &[Vec<u8>]) -> CompileResult<()> {
        for (i, obj_bytes) in object_files.iter().enumerate() {
            let obj_path = self.path.join(format!("obj{}.o", i));
            let mut file = std::fs::File::create(&obj_path)
                .map_err(|e| io_link_error("failed to create temp object file", e))?;
            file.write_all(obj_bytes)
                .map_err(|e| io_link_error("failed to write temp object file", e))?;
            self.obj_paths.push(obj_path);
        }
        Ok(())
    }

    fn write_runtime(&self, runtime_bytes: &[u8]) -> CompileResult<()> {
        std::fs::write(&self.runtime_path, runtime_bytes)
            .map_err(|e| io_link_error("failed to write runtime archive", e))
    }

    fn read_output(&self) -> CompileResult<Vec<u8>> {
        std::fs::read(&self.output_path)
            .map_err(|e| io_link_error("failed to read linked executable", e))
    }
}

impl Drop for TempLinkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Which linker to use for the final linking phase.
///
/// The Gruel compiler can either use its built-in ELF linker or delegate to
/// an external system linker like `clang`, `gcc`, or `ld`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LinkerMode {
    /// Use the internal linker (default).
    #[default]
    Internal,
    /// Use an external system linker (e.g., `"clang"`, `"ld"`, `"gcc"`).
    System(String),
}

/// Link object files into a final executable using an external system linker.
pub(crate) fn link_system_with_warnings(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    linker_cmd: &str,
    warnings: &[CompileWarning],
    extra_link_libraries: &[String],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("linker", mode = "system", command = linker_cmd).entered();

    let mut temp_dir = TempLinkDir::new().map_err(CompileErrors::from)?;
    temp_dir
        .write_object_files(object_files)
        .map_err(CompileErrors::from)?;
    temp_dir
        .write_runtime(RUNTIME_BYTES)
        .map_err(CompileErrors::from)?;

    let mut cmd = Command::new(linker_cmd);

    // We pass `-nostartfiles` (not `-nostdlib`) because the runtime provides
    // its own `_start` / `__main` entry but still relies on libc for
    // syscalls. On Linux, dynamic linking lets `ld.so` initialize libc
    // (TLS, malloc, stdio) before we jump into our entry.
    if options.target.is_macho() {
        cmd.arg("-nostartfiles");
        cmd.arg("-arch").arg("arm64");
        cmd.arg("-e").arg("__main");
    } else {
        cmd.arg("-nostartfiles");
    }

    cmd.arg("-o");
    cmd.arg(&temp_dir.output_path);
    for path in &temp_dir.obj_paths {
        cmd.arg(path);
    }
    cmd.arg(&temp_dir.runtime_path);

    if options.target.is_macho() {
        cmd.arg("-lSystem");
    }

    // ADR-0085: emit `-l<name>` for each user-declared library.
    for lib in extra_link_libraries {
        cmd.arg(format!("-l{}", lib));
    }

    let output = cmd.output().map_err(|e| {
        CompileErrors::from(CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to execute linker '{}': {}",
            linker_cmd, e
        ))))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::LinkError(format!("linker '{}' failed: {}", linker_cmd, stderr)),
        )));
    }

    let elf = temp_dir.read_output().map_err(CompileErrors::from)?;
    info!(
        object_count = object_files.len(),
        output_bytes = elf.len(),
        "linking complete"
    );

    Ok(CompileOutput {
        elf,
        warnings: warnings.to_vec(),
    })
}

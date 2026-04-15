//! LLVM-based code generation for the Gruel compiler.
//!
//! This crate converts CFG (Control Flow Graph) to native object code via LLVM IR,
//! using the `inkwell` crate as safe Rust bindings to LLVM's C API.
//!
//! ## Build requirements
//!
//! LLVM 22 must be installed and `LLVM_SYS_221_PREFIX` or a system `llvm-config`
//! must be available.
//!
//! - On macOS: `brew install llvm`
//! - On Linux: `apt install llvm-22-dev`
//!
//! ## Pipeline
//!
//! ```text
//! CFG → LLVM IR (via inkwell) → object file bytes
//! ```

mod codegen;
mod types;

use gruel_air::TypeInternPool;
use gruel_cfg::Cfg;
use gruel_error::CompileResult;
use lasso::ThreadedRodeo;

/// Generate a native object file from a collection of function CFGs using LLVM.
///
/// All functions are compiled into a single LLVM module, which is then lowered
/// to an object file via LLVM's backend. The returned bytes can be written to a
/// `.o` file and passed to a system linker.
///
/// # Errors
///
/// Returns an error if an LLVM compilation error occurs.
pub fn generate(
    functions: &[&Cfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
) -> CompileResult<Vec<u8>> {
    codegen::generate(functions, type_pool, strings, interner)
}

/// Generate LLVM textual IR from a collection of function CFGs.
///
/// Returns the LLVM IR in human-readable `.ll` format. Used by `--emit asm`
/// to produce inspectable IR in place of native assembly.
pub fn generate_ir(
    functions: &[&Cfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
) -> CompileResult<String> {
    codegen::generate_ir(functions, type_pool, strings, interner)
}

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
//! CFG → LLVM IR (via inkwell) → [opt passes] → object file bytes
//! ```

mod codegen;
mod types;

use gruel_air::TypeInternPool;
use gruel_cfg::{Cfg, OptLevel};
use gruel_error::CompileResult;
use lasso::ThreadedRodeo;

/// Inputs to LLVM codegen, bundled to keep function signatures readable.
///
/// All borrows are produced upstream by sema/CFG construction; codegen reads
/// from them but does not own or mutate them.
pub struct CodegenInputs<'a> {
    pub functions: &'a [&'a Cfg],
    pub type_pool: &'a TypeInternPool,
    pub strings: &'a [String],
    pub bytes: &'a [Vec<u8>],
    pub interner: &'a ThreadedRodeo,
    pub interface_defs: &'a [gruel_air::InterfaceDef],
    pub interface_vtables: &'a gruel_air::InterfaceVtables,
}

/// Generate a native object file from a collection of function CFGs using LLVM.
///
/// All functions are compiled into a single LLVM module, which is then lowered
/// to an object file via LLVM's backend. The returned bytes can be written to a
/// `.o` file and passed to a system linker.
///
/// At `-O1` and above the LLVM mid-end pipeline (`default<OX>`) is run before
/// emission, enabling InstCombine, GVN, SCCP, ADCE, SimplifyCFG, and more.
///
/// # Errors
///
/// Returns an error if an LLVM compilation error occurs.
pub fn generate(inputs: &CodegenInputs<'_>, opt_level: OptLevel) -> CompileResult<Vec<u8>> {
    codegen::generate(inputs, opt_level)
}

/// Generate LLVM textual IR from a collection of function CFGs.
///
/// Returns the LLVM IR in human-readable `.ll` format. Used by `--emit asm`
/// to produce inspectable IR in place of native assembly. At `-O1+` the
/// returned IR is the post-optimization form.
pub fn generate_ir(inputs: &CodegenInputs<'_>, opt_level: OptLevel) -> CompileResult<String> {
    codegen::generate_ir(inputs, opt_level)
}

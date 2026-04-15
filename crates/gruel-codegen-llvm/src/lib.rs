//! LLVM-based code generation for the Gruel compiler.
//!
//! This crate converts CFG (Control Flow Graph) to native object code via LLVM IR,
//! using the `inkwell` crate as safe Rust bindings to LLVM's C API.
//!
//! ## Build requirements
//!
//! LLVM 18 must be installed and `LLVM_SYS_180_PREFIX` or a system `llvm-config`
//! must be available. Enable the `llvm18` Cargo feature to activate LLVM support:
//!
//! ```text
//! cargo build --features gruel-codegen-llvm/llvm18
//! ```
//!
//! Without the feature, [`generate`] returns an error at runtime.
//!
//! ## Pipeline
//!
//! ```text
//! CFG → LLVM IR (via inkwell) → object file bytes
//! ```

#[cfg(feature = "llvm18")]
mod codegen;
#[cfg(feature = "llvm18")]
mod types;

use gruel_air::TypeInternPool;
use gruel_cfg::Cfg;
use gruel_error::CompileResult;
#[cfg(not(feature = "llvm18"))]
use gruel_error::{CompileError, ErrorKind};
use lasso::ThreadedRodeo;

/// Generate a native object file from a collection of function CFGs using LLVM.
///
/// All functions are compiled into a single LLVM module, which is then lowered
/// to an object file via LLVM's backend. The returned bytes can be written to a
/// `.o` file and passed to a system linker.
///
/// # Errors
///
/// Returns an error if:
/// - The crate was compiled without the `llvm18` feature (LLVM not available).
/// - An LLVM compilation error occurs.
pub fn generate(
    functions: &[&Cfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
) -> CompileResult<Vec<u8>> {
    #[cfg(feature = "llvm18")]
    return codegen::generate(functions, type_pool, strings, interner);

    #[cfg(not(feature = "llvm18"))]
    {
        let _ = (functions, type_pool, strings, interner);
        Err(CompileError::without_span(ErrorKind::InternalError(
            "LLVM backend is not available; \
             rebuild with --features gruel-codegen-llvm/llvm18 \
             after installing LLVM 18 (brew install llvm@18)"
                .into(),
        )))
    }
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
    #[cfg(feature = "llvm18")]
    return codegen::generate_ir(functions, type_pool, strings, interner);

    #[cfg(not(feature = "llvm18"))]
    {
        let _ = (functions, type_pool, strings, interner);
        Err(CompileError::without_span(ErrorKind::InternalError(
            "LLVM backend is not available; \
             rebuild with --features gruel-codegen-llvm/llvm18 \
             after installing LLVM 18 (brew install llvm@18)"
                .into(),
        )))
    }
}

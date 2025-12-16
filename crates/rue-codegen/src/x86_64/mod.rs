//! x86-64 backend for the Rue compiler.
//!
//! This module implements the full x86-64 code generation pipeline:
//!
//! ```text
//! AIR → X86Mir (virtual registers) → Register Allocation → Machine Code
//! ```
//!
//! The pipeline is split into distinct phases:
//! - `lower`: Converts AIR to X86Mir with virtual registers
//! - `regalloc`: Assigns physical registers to virtual registers
//! - `emit`: Encodes X86Mir instructions to machine code bytes

mod emit;
mod lower;
mod mir;
mod regalloc;

pub use emit::Emitter;
pub use lower::Lower;
pub use mir::{Operand, Reg, VReg, X86Mir, X86Inst};
pub use regalloc::RegAlloc;

use rue_air::Air;

/// A relocation emitted during code generation.
///
/// This represents a location in the generated machine code that needs to be
/// patched by the linker to reference an external symbol. The relocation type
/// is not included here - it's determined by the compiler when converting to
/// linker relocations based on the context (e.g., call instructions use PLT32).
#[derive(Debug, Clone)]
pub struct EmittedRelocation {
    /// Offset in the code section where the relocation applies.
    pub offset: u64,
    /// Symbol name to reference.
    pub symbol: String,
    /// Addend for the relocation (typically -4 for PC-relative relocations
    /// to account for the displacement being relative to the next instruction).
    pub addend: i64,
}

/// Generated machine code for a function.
pub struct MachineCode {
    /// The raw machine code bytes.
    pub code: Vec<u8>,
    /// Relocations needed in the code (for external symbol references).
    pub relocations: Vec<EmittedRelocation>,
}

/// Generate machine code from AIR.
///
/// This is the main entry point for x86-64 code generation.
pub fn generate(air: &Air) -> MachineCode {
    // Phase 1: Lower AIR to X86Mir with virtual registers
    let mir = Lower::new(air).lower();

    // Phase 2: Allocate physical registers
    let mir = RegAlloc::new(mir).allocate();

    // Phase 3: Emit machine code bytes
    let (code, relocations) = Emitter::new(&mir).emit();

    MachineCode { code, relocations }
}

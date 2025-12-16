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

/// Generated machine code for a function.
pub struct MachineCode {
    /// The raw machine code bytes
    pub code: Vec<u8>,
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
    let code = Emitter::new(&mir).emit();

    MachineCode { code }
}

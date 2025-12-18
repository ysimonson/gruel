//! x86-64 backend for the Rue compiler.
//!
//! This module implements the full x86-64 code generation pipeline:
//!
//! ```text
//! AIR → CFG → X86Mir (virtual registers) → Register Allocation → Machine Code
//! ```
//!
//! The pipeline is split into distinct phases:
//! - `cfg_lower`: Converts CFG to X86Mir with virtual registers
//! - `regalloc`: Assigns physical registers to virtual registers
//! - `emit`: Encodes X86Mir instructions to machine code bytes
//!
//! The old `lower` module (AIR → X86Mir) is kept for backward compatibility.

mod cfg_lower;
mod emit;
mod liveness;
mod lower;
mod mir;
mod regalloc;

pub use cfg_lower::CfgLower;
pub use emit::Emitter;
pub use lower::Lower;
pub use mir::{Operand, Reg, VReg, X86Mir, X86Inst};
pub use regalloc::RegAlloc;

use rue_air::{Air, StructDef};
use rue_cfg::CfgBuilder;

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
/// The pipeline is: AIR → CFG → X86Mir → Machine Code
pub fn generate(
    air: &Air,
    struct_defs: &[StructDef],
    num_locals: u32,
    num_params: u32,
    fn_name: &str,
) -> MachineCode {
    // Phase 1: Build CFG from AIR
    let cfg = CfgBuilder::build(air, num_locals, num_params, fn_name);

    // Phase 2: Lower CFG to X86Mir with virtual registers
    let mir = CfgLower::new(&cfg, struct_defs).lower();

    // Phase 3: Allocate physical registers (may add spill slots)
    // Spill slots go after both locals AND parameters to avoid conflicts
    let existing_slots = num_locals + num_params;
    let (mir, num_spills, used_callee_saved) = RegAlloc::new(mir, existing_slots).allocate_with_spills();

    // Phase 4: Emit machine code bytes (with prologue for stack frame setup)
    // Total local slots = local variables + spill slots (params handled separately)
    let total_locals = num_locals + num_spills;
    let (code, relocations) = Emitter::new(&mir, total_locals, num_params, &used_callee_saved).emit();

    MachineCode { code, relocations }
}

//! Register allocation.
//!
//! This phase assigns physical registers to virtual registers. Currently uses
//! a simple strategy suitable for the minimal instruction set:
//!
//! - Allocate vregs to available caller-saved registers (rax, rcx, rdx, etc.)
//! - Physical register operands are left unchanged
//!
//! As the language grows, this can be replaced with a proper allocator
//! (linear scan, graph coloring, etc.).

use super::mir::{Operand, Reg, X86Inst, X86Mir};

/// Available registers for allocation (caller-saved, not used for special purposes).
///
/// We avoid:
/// - rsp (stack pointer)
/// - rbp (frame pointer, if we use it)
/// - rdi, rsi, rdx, rcx, r8, r9 are used for syscall/function args, but can be
///   used as temporaries when not needed for that purpose
///
/// For now, we use a simple set of scratch registers.
const ALLOCATABLE_REGS: &[Reg] = &[
    Reg::R10, // First choice - caller-saved, not used for args
    Reg::R11, // Second choice - caller-saved, not used for args
    Reg::Rax, // Can use when not needed for syscall number
    Reg::Rcx, // Can use when not needed for args
    Reg::Rdx, // Can use when not needed for args
    Reg::Rsi, // Can use when not needed for args
    Reg::R8,  // Can use when not needed for args
    Reg::R9,  // Can use when not needed for args
];

/// Register allocator.
pub struct RegAlloc {
    mir: X86Mir,
    /// Maps virtual register index to physical register.
    allocation: Vec<Option<Reg>>,
}

impl RegAlloc {
    /// Create a new register allocator.
    pub fn new(mir: X86Mir) -> Self {
        let vreg_count = mir.vreg_count() as usize;
        Self {
            mir,
            allocation: vec![None; vreg_count],
        }
    }

    /// Perform register allocation and return the updated MIR.
    pub fn allocate(mut self) -> X86Mir {
        // Phase 1: Assign physical registers to virtual registers
        self.assign_registers();

        // Phase 2: Rewrite instructions to use physical registers
        self.rewrite_instructions();

        self.mir
    }

    /// Assign physical registers to all virtual registers.
    fn assign_registers(&mut self) {
        // Simple linear allocation: assign registers in order
        // This works because our current IR is very simple (no overlapping lifetimes)
        for vreg_idx in 0..self.mir.vreg_count() {
            let reg_idx = vreg_idx as usize % ALLOCATABLE_REGS.len();
            self.allocation[vreg_idx as usize] = Some(ALLOCATABLE_REGS[reg_idx]);
        }
    }

    /// Rewrite all instructions to use physical registers.
    fn rewrite_instructions(&mut self) {
        for inst in self.mir.instructions_mut() {
            Self::rewrite_inst(&self.allocation, inst);
        }
    }

    /// Rewrite a single instruction.
    fn rewrite_inst(allocation: &[Option<Reg>], inst: &mut X86Inst) {
        match inst {
            X86Inst::MovRI32 { dst, .. } => {
                *dst = Self::rewrite_operand(allocation, *dst);
            }
            X86Inst::MovRI64 { dst, .. } => {
                *dst = Self::rewrite_operand(allocation, *dst);
            }
            X86Inst::MovRR { dst, src } => {
                *dst = Self::rewrite_operand(allocation, *dst);
                *src = Self::rewrite_operand(allocation, *src);
            }
            X86Inst::CallRel { .. } | X86Inst::Syscall | X86Inst::Ret => {
                // No register operands to rewrite
            }
        }
    }

    /// Rewrite an operand, replacing virtual registers with physical ones.
    fn rewrite_operand(allocation: &[Option<Reg>], operand: Operand) -> Operand {
        match operand {
            Operand::Virtual(vreg) => {
                let reg = allocation[vreg.index() as usize]
                    .expect("virtual register should have been allocated");
                Operand::Physical(reg)
            }
            Operand::Physical(_) => operand, // Already physical
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_allocation() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();

        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        let mir = RegAlloc::new(mir).allocate();

        // v0 should be allocated to R10 (first allocatable)
        match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, imm } => {
                assert_eq!(*dst, Operand::Physical(Reg::R10));
                assert_eq!(*imm, 42);
            }
            _ => panic!("expected MovRI32"),
        }
    }

    #[test]
    fn test_physical_reg_preserved() {
        let mut mir = X86Mir::new();

        // Instruction with physical register should be unchanged
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rdi),
            imm: 60,
        });

        let mir = RegAlloc::new(mir).allocate();

        match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, imm } => {
                assert_eq!(*dst, Operand::Physical(Reg::Rdi));
                assert_eq!(*imm, 60);
            }
            _ => panic!("expected MovRI32"),
        }
    }

    #[test]
    fn test_mov_rr_both_operands_rewritten() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();

        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        let mir = RegAlloc::new(mir).allocate();

        // Check the mov r, r instruction
        match &mir.instructions()[1] {
            X86Inst::MovRR { dst, src } => {
                assert!(dst.is_physical());
                assert!(src.is_physical());
                // v0 -> R10, v1 -> R11
                assert_eq!(*src, Operand::Physical(Reg::R10));
                assert_eq!(*dst, Operand::Physical(Reg::R11));
            }
            _ => panic!("expected MovRR"),
        }
    }
}

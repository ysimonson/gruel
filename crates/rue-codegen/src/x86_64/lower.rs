//! AIR to X86Mir lowering.
//!
//! This phase converts AIR (typed, high-level IR) to X86Mir (x86-64 instructions
//! with virtual registers). The lowering is straightforward since AIR operations
//! map fairly directly to x86-64 instructions.

use rue_air::{Air, AirInstData, AirRef};

use super::mir::{Operand, Reg, VReg, X86Inst, X86Mir};

/// AIR to X86Mir lowering.
pub struct Lower<'a> {
    air: &'a Air,
    mir: X86Mir,
    /// Maps AIR instruction refs to the vreg holding their result.
    value_map: Vec<Option<VReg>>,
}

impl<'a> Lower<'a> {
    /// Create a new lowering pass.
    pub fn new(air: &'a Air) -> Self {
        Self {
            air,
            mir: X86Mir::new(),
            value_map: vec![None; air.len()],
        }
    }

    /// Lower AIR to X86Mir.
    pub fn lower(mut self) -> X86Mir {
        // Walk AIR instructions and lower each one
        for (air_ref, inst) in self.air.iter() {
            self.lower_inst(air_ref, &inst.data);
        }

        self.mir
    }

    /// Lower a single AIR instruction.
    fn lower_inst(&mut self, air_ref: AirRef, data: &AirInstData) {
        match data {
            AirInstData::Const(value) => {
                // Allocate a vreg and move the constant into it
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                // Use 32-bit move if value fits, otherwise 64-bit
                if *value >= i32::MIN as i64 && *value <= i32::MAX as i64 {
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: *value as i32,
                    });
                } else {
                    self.mir.push(X86Inst::MovRI64 {
                        dst: Operand::Virtual(vreg),
                        imm: *value,
                    });
                }
            }

            AirInstData::Ret(value_ref) => {
                // Get the vreg holding the return value
                let value_vreg = self.get_vreg(*value_ref);

                // For now, we emit an exit syscall with the return value.
                // In a real compiler, we'd emit a ret instruction and let
                // the caller handle the calling convention.

                // Move return value to edi (first arg to syscall)
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rdi),
                    src: Operand::Virtual(value_vreg),
                });

                // Move syscall number (60 = exit) to eax
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Physical(Reg::Rax),
                    imm: 60,
                });

                // syscall
                self.mir.push(X86Inst::Syscall);
            }
        }
    }

    /// Get the vreg holding the result of an AIR instruction.
    fn get_vreg(&self, air_ref: AirRef) -> VReg {
        self.value_map[air_ref.as_u32() as usize]
            .expect("AIR instruction should have been lowered before use")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::{AirInst, Type};
    use rue_span::Span;

    #[test]
    fn test_lower_const_and_ret() {
        let mut air = Air::new(Type::I32);

        // Add constant 42
        let const_ref = air.add_inst(AirInst {
            data: AirInstData::Const(42),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        // Add return
        air.add_inst(AirInst {
            data: AirInstData::Ret(const_ref),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        let mir = Lower::new(&air).lower();

        // Should have 4 instructions:
        // 1. mov v0, 42
        // 2. mov rdi, v0
        // 3. mov rax, 60
        // 4. syscall
        assert_eq!(mir.instructions().len(), 4);

        // Check first instruction is mov v0, 42
        match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, imm } => {
                assert!(matches!(dst, Operand::Virtual(v) if v.index() == 0));
                assert_eq!(*imm, 42);
            }
            _ => panic!("expected MovRI32"),
        }

        // Check syscall is last
        assert!(matches!(mir.instructions()[3], X86Inst::Syscall));
    }

    #[test]
    fn test_lower_large_constant() {
        let mut air = Air::new(Type::I32);

        // Add a constant that doesn't fit in i32
        let const_ref = air.add_inst(AirInst {
            data: AirInstData::Const(0x1_0000_0000), // 2^32
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        air.add_inst(AirInst {
            data: AirInstData::Ret(const_ref),
            ty: Type::I32,
            span: Span::new(0, 2),
        });

        let mir = Lower::new(&air).lower();

        // First instruction should be 64-bit move
        match &mir.instructions()[0] {
            X86Inst::MovRI64 { dst, imm } => {
                assert!(matches!(dst, Operand::Virtual(v) if v.index() == 0));
                assert_eq!(*imm, 0x1_0000_0000);
            }
            _ => panic!("expected MovRI64"),
        }
    }
}

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
    /// Label counter for generating unique labels.
    label_counter: u32,
}

impl<'a> Lower<'a> {
    /// Create a new lowering pass.
    pub fn new(air: &'a Air) -> Self {
        Self {
            air,
            mir: X86Mir::new(),
            value_map: vec![None; air.len()],
            label_counter: 0,
        }
    }

    /// Generate a unique label name.
    ///
    /// TODO: Labels are currently unique within a single function but could collide
    /// across multiple functions in the same object file. When we add multi-function
    /// support, we'll need function-scoped or globally unique label generation.
    fn new_label(&mut self, prefix: &str) -> String {
        let label = format!(".L{}_{}", prefix, self.label_counter);
        self.label_counter += 1;
        label
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

            AirInstData::Add(lhs, rhs) => {
                // Allocate result vreg
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // x86 add is dst = dst + src, so we need to copy lhs to dst first
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::AddRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
                // Check for overflow and call error handler if set
                let ok_label = self.new_label("add_ok");
                self.mir.push(X86Inst::Jno { label: ok_label.clone() });
                self.mir.push(X86Inst::CallRel { symbol: "__rue_overflow".to_string() });
                self.mir.push(X86Inst::Label { name: ok_label });
            }

            AirInstData::Sub(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::SubRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
                // Check for overflow and call error handler if set
                let ok_label = self.new_label("sub_ok");
                self.mir.push(X86Inst::Jno { label: ok_label.clone() });
                self.mir.push(X86Inst::CallRel { symbol: "__rue_overflow".to_string() });
                self.mir.push(X86Inst::Label { name: ok_label });
            }

            AirInstData::Mul(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // imul r32, r32 is dst = dst * src
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(lhs_vreg),
                });
                self.mir.push(X86Inst::ImulRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
                // Check for overflow and call error handler if set
                let ok_label = self.new_label("mul_ok");
                self.mir.push(X86Inst::Jno { label: ok_label.clone() });
                self.mir.push(X86Inst::CallRel { symbol: "__rue_overflow".to_string() });
                self.mir.push(X86Inst::Label { name: ok_label });
            }

            AirInstData::Div(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Check for division by zero before performing division
                let ok_label = self.new_label("div_ok");
                self.mir.push(X86Inst::TestRR {
                    src1: Operand::Virtual(rhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Jnz { label: ok_label.clone() });
                self.mir.push(X86Inst::CallRel { symbol: "__rue_div_by_zero".to_string() });
                self.mir.push(X86Inst::Label { name: ok_label });

                // Division on x86 uses EDX:EAX / divisor -> quotient in EAX, remainder in EDX
                // TODO: The register allocator doesn't know RAX/RDX are clobbered here.
                // Once we have real liveness analysis, we'll need register constraints or
                // explicit clobber sets to prevent the allocator from placing live values
                // in RAX/RDX across division operations.
                // 1. Move dividend to EAX
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rax),
                    src: Operand::Virtual(lhs_vreg),
                });
                // 2. Sign-extend EAX to EDX:EAX
                self.mir.push(X86Inst::Cdq);
                // 3. Perform division
                self.mir.push(X86Inst::IdivR {
                    src: Operand::Virtual(rhs_vreg),
                });
                // 4. Move quotient (EAX) to result vreg
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Physical(Reg::Rax),
                });
            }

            AirInstData::Mod(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Check for division by zero before performing modulo
                let ok_label = self.new_label("mod_ok");
                self.mir.push(X86Inst::TestRR {
                    src1: Operand::Virtual(rhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Jnz { label: ok_label.clone() });
                self.mir.push(X86Inst::CallRel { symbol: "__rue_div_by_zero".to_string() });
                self.mir.push(X86Inst::Label { name: ok_label });

                // Modulo uses the same idiv instruction, but takes remainder from EDX
                // 1. Move dividend to EAX
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rax),
                    src: Operand::Virtual(lhs_vreg),
                });
                // 2. Sign-extend EAX to EDX:EAX
                self.mir.push(X86Inst::Cdq);
                // 3. Perform division
                self.mir.push(X86Inst::IdivR {
                    src: Operand::Virtual(rhs_vreg),
                });
                // 4. Move remainder (EDX) to result vreg
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Physical(Reg::Rdx),
                });
            }

            AirInstData::Neg(operand) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let operand_vreg = self.get_vreg(*operand);

                // neg r32 modifies in place, so copy first
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(operand_vreg),
                });
                self.mir.push(X86Inst::Neg {
                    dst: Operand::Virtual(vreg),
                });
                // Check for overflow (only happens when negating i32::MIN)
                let ok_label = self.new_label("neg_ok");
                self.mir.push(X86Inst::Jno { label: ok_label.clone() });
                self.mir.push(X86Inst::CallRel { symbol: "__rue_overflow".to_string() });
                self.mir.push(X86Inst::Label { name: ok_label });
            }

            AirInstData::Ret(value_ref) => {
                // Get the vreg holding the return value
                let value_vreg = self.get_vreg(*value_ref);

                // Move return value to rdi (first argument per System V AMD64 ABI).
                // We emit a 64-bit mov (mov rdi, src) even though __rue_exit takes
                // an i32 status code. The upper 32 bits are ignored by the callee.
                // Using rdi instead of edi avoids needing a separate 32-bit mov path.
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Physical(Reg::Rdi),
                    src: Operand::Virtual(value_vreg),
                });

                // Call the runtime's __rue_exit function.
                // This function never returns (it calls the exit syscall).
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_exit".to_string(),
                });
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

        // Should have 3 instructions:
        // 1. mov v0, 42
        // 2. mov rdi, v0
        // 3. call __rue_exit
        assert_eq!(mir.instructions().len(), 3);

        // Check first instruction is mov v0, 42
        match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, imm } => {
                assert!(matches!(dst, Operand::Virtual(v) if v.index() == 0));
                assert_eq!(*imm, 42);
            }
            _ => panic!("expected MovRI32"),
        }

        // Check call __rue_exit is last
        match &mir.instructions()[2] {
            X86Inst::CallRel { symbol } => {
                assert_eq!(symbol, "__rue_exit");
            }
            _ => panic!("expected CallRel"),
        }
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

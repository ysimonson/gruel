//! AIR to X86Mir lowering.
//!
//! This phase converts AIR (typed, high-level IR) to X86Mir (x86-64 instructions
//! with virtual registers). The lowering is demand-driven: instructions are only
//! lowered when their values are needed, enabling short-circuit evaluation of
//! logical operators (&&, ||).

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
    /// Whether this function has a stack frame (num_locals > 0).
    /// Used to emit proper epilogue before returns.
    has_frame: bool,
}

impl<'a> Lower<'a> {
    /// Create a new lowering pass.
    pub fn new(air: &'a Air, num_locals: u32) -> Self {
        Self {
            air,
            mir: X86Mir::new(),
            value_map: vec![None; air.len()],
            label_counter: 0,
            has_frame: num_locals > 0,
        }
    }

    /// Calculate the stack offset for a local variable slot.
    /// Slot 0 is at [rbp - 8], slot 1 is at [rbp - 16], etc.
    fn local_offset(&self, slot: u32) -> i32 {
        -((slot as i32 + 1) * 8)
    }

    /// Emit function epilogue to restore the stack frame.
    ///
    /// This tears down the RBP-based stack frame:
    /// ```asm
    /// mov rsp, rbp
    /// pop rbp
    /// ```
    fn emit_epilogue(&mut self) {
        // mov rsp, rbp
        self.mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rsp),
            src: Operand::Physical(Reg::Rbp),
        });
        // pop rbp
        self.mir.push(X86Inst::Pop {
            dst: Operand::Physical(Reg::Rbp),
        });
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

    /// Lower AIR to X86Mir using demand-driven lowering.
    ///
    /// Instead of lowering all instructions in order, we start from the root
    /// (the Ret instruction at the end) and recursively demand-lower dependencies.
    /// This enables short-circuit evaluation: And/Or can conditionally skip
    /// lowering their RHS if the LHS determines the result.
    pub fn lower(mut self) -> X86Mir {
        // Start from the root (last instruction, which should be Ret)
        // and demand-lower recursively
        let root = AirRef::from_raw((self.air.len() - 1) as u32);
        let inst = self.air.get(root);
        self.lower_inst(root, &inst.data.clone());

        self.mir
    }

    /// Lower a single AIR instruction.
    fn lower_inst(&mut self, air_ref: AirRef, data: &AirInstData) {
        // Skip if already lowered
        if self.value_map[air_ref.as_u32() as usize].is_some() {
            return;
        }
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

            AirInstData::Alloc { slot, init } => {
                // Get the initialized value
                let init_vreg = self.get_vreg(*init);

                // Store to stack slot
                let offset = self.local_offset(*slot);
                self.mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Virtual(init_vreg),
                });

                // Alloc doesn't produce a value that can be used directly
                // (it's a statement, not an expression)
            }

            AirInstData::Load { slot } => {
                // Allocate vreg for the loaded value
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                // Load from stack slot
                let offset = self.local_offset(*slot);
                self.mir.push(X86Inst::MovRM {
                    dst: Operand::Virtual(vreg),
                    base: Reg::Rbp,
                    offset,
                });
            }

            AirInstData::Store { slot, value } => {
                // Get the value to store
                let value_vreg = self.get_vreg(*value);

                // Store to stack slot
                let offset = self.local_offset(*slot);
                self.mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Virtual(value_vreg),
                });

                // Store doesn't produce a value
            }

            AirInstData::BoolConst(value) => {
                // Booleans are represented as 0 or 1
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(vreg),
                    imm: if *value { 1 } else { 0 },
                });
            }

            // Comparison operators
            AirInstData::Eq(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                // Compare and set result
                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Sete {
                    dst: Operand::Virtual(vreg),
                });
                // Zero-extend byte to dword
                self.mir.push(X86Inst::Movzx {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(vreg),
                });
            }

            AirInstData::Ne(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Setne {
                    dst: Operand::Virtual(vreg),
                });
                self.mir.push(X86Inst::Movzx {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(vreg),
                });
            }

            AirInstData::Lt(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Setl {
                    dst: Operand::Virtual(vreg),
                });
                self.mir.push(X86Inst::Movzx {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(vreg),
                });
            }

            AirInstData::Gt(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Setg {
                    dst: Operand::Virtual(vreg),
                });
                self.mir.push(X86Inst::Movzx {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(vreg),
                });
            }

            AirInstData::Le(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Setle {
                    dst: Operand::Virtual(vreg),
                });
                self.mir.push(X86Inst::Movzx {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(vreg),
                });
            }

            AirInstData::Ge(lhs, rhs) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let lhs_vreg = self.get_vreg(*lhs);
                let rhs_vreg = self.get_vreg(*rhs);

                self.mir.push(X86Inst::CmpRR {
                    src1: Operand::Virtual(lhs_vreg),
                    src2: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Setge {
                    dst: Operand::Virtual(vreg),
                });
                self.mir.push(X86Inst::Movzx {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(vreg),
                });
            }

            // Logical operators
            AirInstData::Not(operand) => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let operand_vreg = self.get_vreg(*operand);

                // Booleans are 0 or 1, so !x is x ^ 1
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(operand_vreg),
                });
                self.mir.push(X86Inst::XorRI {
                    dst: Operand::Virtual(vreg),
                    imm: 1,
                });
            }

            AirInstData::And(lhs, rhs) => {
                // Short-circuit evaluation: a && b
                // If LHS is false, result is false (skip RHS)
                // If LHS is true, result is RHS
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                // Always evaluate LHS
                let lhs_vreg = self.get_vreg(*lhs);

                let false_label = self.new_label("and_false");
                let end_label = self.new_label("and_end");

                // If LHS is false (zero), short-circuit to false
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(lhs_vreg),
                    imm: 0,
                });
                self.mir.push(X86Inst::Jz { label: false_label.clone() });

                // LHS was true - evaluate RHS (demand-driven, only lowered here)
                let rhs_vreg = self.get_vreg(*rhs);
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Jmp { label: end_label.clone() });

                // Short-circuit path: result is false
                self.mir.push(X86Inst::Label { name: false_label });
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(vreg),
                    imm: 0,
                });

                self.mir.push(X86Inst::Label { name: end_label });
            }

            AirInstData::Or(lhs, rhs) => {
                // Short-circuit evaluation: a || b
                // If LHS is true, result is true (skip RHS)
                // If LHS is false, result is RHS
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                // Always evaluate LHS
                let lhs_vreg = self.get_vreg(*lhs);

                let true_label = self.new_label("or_true");
                let end_label = self.new_label("or_end");

                // If LHS is true (non-zero), short-circuit to true
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(lhs_vreg),
                    imm: 0,
                });
                self.mir.push(X86Inst::Jnz { label: true_label.clone() });

                // LHS was false - evaluate RHS (demand-driven, only lowered here)
                let rhs_vreg = self.get_vreg(*rhs);
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(vreg),
                    src: Operand::Virtual(rhs_vreg),
                });
                self.mir.push(X86Inst::Jmp { label: end_label.clone() });

                // Short-circuit path: result is true
                self.mir.push(X86Inst::Label { name: true_label });
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(vreg),
                    imm: 1,
                });

                self.mir.push(X86Inst::Label { name: end_label });
            }

            AirInstData::Branch { cond, then_value, else_value } => {
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                let cond_vreg = self.get_vreg(*cond);

                if let Some(else_v) = else_value {
                    // if-else: result is either then_value or else_value
                    let else_label = self.new_label("else");
                    let end_label = self.new_label("end_if");

                    // Test condition: if zero (false), jump to else
                    self.mir.push(X86Inst::CmpRI {
                        src: Operand::Virtual(cond_vreg),
                        imm: 0,
                    });
                    self.mir.push(X86Inst::Jz { label: else_label.clone() });

                    // Then branch
                    let then_vreg = self.get_vreg(*then_value);
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(then_vreg),
                    });
                    self.mir.push(X86Inst::Jmp { label: end_label.clone() });

                    // Else branch
                    self.mir.push(X86Inst::Label { name: else_label });
                    let else_vreg = self.get_vreg(*else_v);
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(else_vreg),
                    });

                    // End
                    self.mir.push(X86Inst::Label { name: end_label });
                } else {
                    // if without else: result is Unit (we can just use then_value)
                    let end_label = self.new_label("end_if");

                    // Test condition: if zero (false), skip then branch
                    self.mir.push(X86Inst::CmpRI {
                        src: Operand::Virtual(cond_vreg),
                        imm: 0,
                    });
                    self.mir.push(X86Inst::Jz { label: end_label.clone() });

                    // Then branch
                    let then_vreg = self.get_vreg(*then_value);
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(then_vreg),
                    });

                    // End
                    self.mir.push(X86Inst::Label { name: end_label });
                }
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

                // Emit epilogue to restore stack frame before the call.
                // This is technically not needed since __rue_exit never returns,
                // but it's good practice for when we add real function returns.
                if self.has_frame {
                    self.emit_epilogue();
                }

                // Call the runtime's __rue_exit function.
                // This function never returns (it calls the exit syscall).
                self.mir.push(X86Inst::CallRel {
                    symbol: "__rue_exit".to_string(),
                });
            }

            AirInstData::Block { statements, value } => {
                // Execute all statements in order (for side effects).
                // This is demand-driven: statements are lowered now, inside whatever
                // control flow context we're in (e.g., inside the RHS of &&).
                //
                // We call lower_inst directly instead of get_vreg because statements
                // like Alloc and Store don't produce values (they have no entry in
                // value_map). The lower_inst function handles this correctly by not
                // setting value_map for these instructions.
                for stmt_ref in statements {
                    self.demand_lower(*stmt_ref);
                }

                // The block's value is the result - use the value's vreg directly
                let value_vreg = self.get_vreg(*value);
                self.value_map[air_ref.as_u32() as usize] = Some(value_vreg);
            }
        }
    }

    /// Demand-lower an AIR instruction if not already lowered.
    ///
    /// This is used for instructions that don't produce values (like Alloc, Store)
    /// where we need to ensure they're lowered but don't need a vreg back.
    fn demand_lower(&mut self, air_ref: AirRef) {
        // Skip if already lowered
        if self.value_map[air_ref.as_u32() as usize].is_some() {
            return;
        }

        // Lower now - we need to clone the data because lower_inst borrows self mutably
        let data = self.air.get(air_ref).data.clone();
        self.lower_inst(air_ref, &data);
    }

    /// Get the vreg holding the result of an AIR instruction.
    ///
    /// This is demand-driven: if the instruction hasn't been lowered yet,
    /// it will be lowered now. This enables short-circuit evaluation where
    /// we only lower instructions when their values are actually needed.
    fn get_vreg(&mut self, air_ref: AirRef) -> VReg {
        // Check if already lowered
        if let Some(vreg) = self.value_map[air_ref.as_u32() as usize] {
            return vreg;
        }

        // Not yet lowered - lower it now (demand-driven)
        // We need to clone the data because lower_inst borrows self mutably
        let data = self.air.get(air_ref).data.clone();
        self.lower_inst(air_ref, &data);

        // Should be lowered now
        self.value_map[air_ref.as_u32() as usize]
            .expect("instruction should have been lowered")
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

        let mir = Lower::new(&air, 0).lower();

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

        let mir = Lower::new(&air, 0).lower();

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
